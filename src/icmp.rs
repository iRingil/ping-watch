//! ICMP echo with a chosen TTL (hop limit) through the Windows ICMP API — `IcmpSendEcho` for IPv4,
//! `Icmp6SendEcho2` for IPv6; needs no admin rights.
//!
//! This is the only module that talks to the OS; everything above it works with [`ProbeOutcome`].

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{GetLastError, HANDLE};
use windows::Win32::NetworkManagement::IpHelper::{
    ICMP_ECHO_REPLY, ICMPV6_ECHO_REPLY_LH, IP_OPTION_INFORMATION, IP_REQ_TIMED_OUT, IP_SUCCESS, IP_TTL_EXPIRED_TRANSIT,
    Icmp6CreateFile, Icmp6SendEcho2, IcmpCloseHandle, IcmpCreateFile, IcmpSendEcho,
};
use windows::Win32::Networking::WinSock::{AF_INET6, IN6_ADDR, IN6_ADDR_0, SOCKADDR_IN6};

use crate::route::ProbeOutcome;

/// Payload sent in every echo request.
const PAYLOAD: [u8; 32] = [0; 32];
/// Size of the reply buffer in `u64` words (8-byte alignment for the reply structs): 1 KiB, far more than one
/// reply with our payload or an ICMP error message needs.
const REPLY_BUFFER_WORDS: usize = 128;
/// Range of `IP_STATUS` codes: the request went out but got an error answer or none (timeout, unreachable, ...).
/// `IP_TTL_EXPIRED_TRANSIT` is also IPv6's `IP_HOP_LIMIT_EXCEEDED`.
const IP_STATUS_RANGE: std::ops::RangeInclusive<u32> = 11000..=11050;

/// Error of the ICMP API itself (not a lost probe).
#[derive(Debug, thiserror::Error)]
pub enum IcmpError {
    /// `IcmpCreateFile` / `Icmp6CreateFile` failed.
    #[error("cannot open ICMP handle: {0}")]
    Open(windows::core::Error),
    /// The send call failed with a non-`IP_STATUS` error code.
    #[error("ICMP send failed with error {0}")]
    Send(u32),
    /// The target's address family doesn't match the handle's.
    #[error("{0} doesn't match the ICMP handle's address family")]
    Family(IpAddr),
}

/// Open ICMP handle for one address family; one per thread, as probes on it are blocking.
#[derive(Debug)]
pub struct Pinger {
    /// Handle from `IcmpCreateFile` or `Icmp6CreateFile`, closed on drop.
    handle: HANDLE,
    /// Whether the handle is for IPv6.
    ipv6: bool,
}

// SAFETY: an ICMP handle is a kernel object handle, not tied to the thread that created it; `Pinger` owns it
// exclusively, so moving it to another thread is sound. (`HANDLE` wraps a raw pointer, hence not `Send` by default.)
unsafe impl Send for Pinger {}

impl Pinger {
    /// Opens an ICMP handle for probing addresses of `target`'s family.
    ///
    /// # Errors
    ///
    /// [`IcmpError::Open`] if Windows refuses to create the handle.
    pub fn for_target(target: IpAddr) -> Result<Self, IcmpError> {
        let ipv6 = target.is_ipv6();
        // SAFETY: both functions have no preconditions; an invalid handle is turned into `Err` by the wrapper.
        let handle = unsafe { if ipv6 { Icmp6CreateFile() } else { IcmpCreateFile() } }.map_err(IcmpError::Open)?;
        Ok(Self { handle, ipv6 })
    }

    /// Sends one echo request to `target` with `ttl` and waits up to `timeout` for the answer.
    ///
    /// The round-trip time combines a local measurement (sub-millisecond) with the kernel's (whole milliseconds,
    /// free of scheduling delays); see [`round_trip`].
    ///
    /// # Errors
    ///
    /// [`IcmpError::Send`] if the API fails for a reason other than the network (no answer is
    /// [`ProbeOutcome::Lost`], not an error); [`IcmpError::Family`] for a target of the other family.
    pub fn probe(&self, target: IpAddr, ttl: u8, timeout: Duration) -> Result<ProbeOutcome, IcmpError> {
        if target.is_ipv6() != self.ipv6 {
            return Err(IcmpError::Family(target));
        }
        let options = IP_OPTION_INFORMATION { Ttl: ttl, ..Default::default() };
        let mut buffer = [0u64; REPLY_BUFFER_WORDS];
        let timeout_ms = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX);

        let started = Instant::now();
        let replies = match target {
            IpAddr::V4(ip) => self.send_v4(ip, &options, &mut buffer, timeout_ms),
            IpAddr::V6(ip) => self.send_v6(ip, &options, &mut buffer, timeout_ms),
        };
        let measured = started.elapsed();

        if replies == 0 {
            // SAFETY: no preconditions; reads the error of the call just made on this thread.
            let code = unsafe { GetLastError() }.0;
            match code {
                // Some Windows versions report a router's answer as an error but still fill the buffer.
                IP_TTL_EXPIRED_TRANSIT => {}
                code if IP_STATUS_RANGE.contains(&code) => {
                    if code != IP_REQ_TIMED_OUT {
                        log::debug!("probe {target} ttl {ttl}: IP status {code}");
                    }
                    return Ok(ProbeOutcome::Lost);
                }
                code => return Err(IcmpError::Send(code)),
            }
        }

        let (status, from, kernel_ms) = match target {
            IpAddr::V4(_) => parse_v4(&buffer),
            IpAddr::V6(_) => parse_v6(&buffer),
        };
        Ok(match status {
            IP_SUCCESS | IP_TTL_EXPIRED_TRANSIT => ProbeOutcome::Reply { from, rtt: round_trip(measured, kernel_ms) },
            status => {
                log::debug!("probe {target} ttl {ttl}: reply status {status}");
                ProbeOutcome::Lost
            }
        })
    }

    /// Sends an IPv4 echo request; returns the number of replies written to `buffer`.
    fn send_v4(&self, target: Ipv4Addr, options: &IP_OPTION_INFORMATION, buffer: &mut [u64], timeout_ms: u32) -> u32 {
        // SAFETY: the handle is an open IPv4 handle for the lifetime of `self`; payload, options and buffer are live
        // for the call and their pointers and sizes match; the buffer is 8-byte aligned and larger than one reply
        // plus payload.
        unsafe {
            IcmpSendEcho(
                self.handle,
                u32::from_ne_bytes(target.octets()),
                PAYLOAD.as_ptr().cast(),
                PAYLOAD.len() as u16,
                Some(options),
                buffer.as_mut_ptr().cast(),
                size_of_val(buffer) as u32,
                timeout_ms,
            )
        }
    }

    /// Sends an IPv6 echo request synchronously (no event, no APC); returns the number of replies in `buffer`.
    fn send_v6(&self, target: Ipv6Addr, options: &IP_OPTION_INFORMATION, buffer: &mut [u64], timeout_ms: u32) -> u32 {
        let source = SOCKADDR_IN6 { sin6_family: AF_INET6, ..Default::default() };
        let destination = SOCKADDR_IN6 {
            sin6_family: AF_INET6,
            sin6_addr: IN6_ADDR { u: IN6_ADDR_0 { Byte: target.octets() } },
            ..Default::default()
        };
        // SAFETY: the handle is an open IPv6 handle for the lifetime of `self`; with no event and no APC routine
        // the call is synchronous, so all pointers (locals and `buffer`) only need to live for the call; sizes
        // match; the buffer is 8-byte aligned and larger than one reply plus payload.
        unsafe {
            Icmp6SendEcho2(
                self.handle,
                None,
                None,
                None,
                &raw const source,
                &raw const destination,
                PAYLOAD.as_ptr().cast(),
                PAYLOAD.len() as u16,
                Some(options),
                buffer.as_mut_ptr().cast(),
                size_of_val(buffer) as u32,
                timeout_ms,
            )
        }
    }
}

/// Round-trip time of a reply: the local measurement `measured` (sub-millisecond, but inflated by however long the
/// thread waited to be scheduled around the call) clamped into `[kernel_ms, kernel_ms + 1)`, the interval given by
/// the kernel's own whole-millisecond measurement. Keeps the precision without the scheduling stalls.
fn round_trip(measured: Duration, kernel_ms: u32) -> Duration {
    let low = Duration::from_millis(u64::from(kernel_ms));
    let high = low.saturating_add(Duration::from_micros(999));
    measured.clamp(low, high)
}

/// Status, sender and kernel round-trip time (ms) of the IPv4 reply at the start of `buffer`.
fn parse_v4(buffer: &[u64; REPLY_BUFFER_WORDS]) -> (u32, IpAddr, u32) {
    // SAFETY: the buffer is initialized (zeroed, then possibly written by the API), 8-byte aligned and larger than
    // `ICMP_ECHO_REPLY`, a plain C struct for which any bit pattern is valid.
    let reply = unsafe { buffer.as_ptr().cast::<ICMP_ECHO_REPLY>().read() };
    (reply.Status, IpAddr::V4(Ipv4Addr::from(reply.Address.to_ne_bytes())), reply.RoundTripTime)
}

/// Status, sender and kernel round-trip time (ms) of the IPv6 reply at the start of `buffer`.
fn parse_v6(buffer: &[u64; REPLY_BUFFER_WORDS]) -> (u32, IpAddr, u32) {
    // SAFETY: as in `parse_v4`; `ICMPV6_ECHO_REPLY_LH` is a plain C struct (its address part is packed, which only
    // lowers its alignment requirement).
    let reply = unsafe { buffer.as_ptr().cast::<ICMPV6_ECHO_REPLY_LH>().read() };
    // The address words are in network byte order as stored in memory.
    let words = reply.Address.sin6_addr;
    let mut octets = [0u8; 16];
    for (pair, word) in octets.as_chunks_mut::<2>().0.iter_mut().zip(words) {
        *pair = word.to_ne_bytes();
    }
    (reply.Status, IpAddr::V6(Ipv6Addr::from(octets)), reply.RoundTripTime)
}

impl Drop for Pinger {
    fn drop(&mut self) {
        // SAFETY: the handle came from `IcmpCreateFile`/`Icmp6CreateFile` and is closed only here, once
        // (`IcmpCloseHandle` closes both kinds).
        if let Err(error) = unsafe { IcmpCloseHandle(self.handle) } {
            log::warn!("cannot close ICMP handle: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measurement_inside_the_kernel_interval_is_kept() {
        assert_eq!(round_trip(Duration::from_micros(44_380), 44), Duration::from_micros(44_380));
    }

    #[test]
    fn scheduling_stall_is_cut_to_the_kernel_interval() {
        // The thread got the reply 140 ms late; the kernel says 44 ms.
        assert_eq!(round_trip(Duration::from_micros(184_500), 44), Duration::from_micros(44_999));
    }

    #[test]
    fn measurement_below_the_kernel_value_is_raised() {
        assert_eq!(round_trip(Duration::from_micros(900), 1), Duration::from_millis(1));
    }
}
