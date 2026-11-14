//! ICMP echo with a chosen TTL through the Windows ICMP API (`IcmpSendEcho`); needs no admin rights.
//!
//! This is the only module that talks to the OS; everything above it works with [`ProbeOutcome`].

use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{GetLastError, HANDLE};
use windows::Win32::NetworkManagement::IpHelper::{
    ICMP_ECHO_REPLY, IP_OPTION_INFORMATION, IP_REQ_TIMED_OUT, IP_SUCCESS, IP_TTL_EXPIRED_TRANSIT, IcmpCloseHandle,
    IcmpCreateFile, IcmpSendEcho,
};

use crate::route::ProbeOutcome;

/// Payload sent in every echo request.
const PAYLOAD: [u8; 32] = [0; 32];
/// Size of the reply buffer in `u64` words (8-byte alignment for `ICMP_ECHO_REPLY`): 1 KiB, far more than one
/// reply with our payload or an ICMP error message needs.
const REPLY_BUFFER_WORDS: usize = 128;
/// Range of `IP_STATUS` codes: the request went out but got an error answer or none (timeout, unreachable, ...).
const IP_STATUS_RANGE: std::ops::RangeInclusive<u32> = 11000..=11050;

/// Error of the ICMP API itself (not a lost probe).
#[derive(Debug, thiserror::Error)]
pub enum IcmpError {
    /// `IcmpCreateFile` failed.
    #[error("cannot open ICMP handle: {0}")]
    Open(windows::core::Error),
    /// `IcmpSendEcho` failed with a non-`IP_STATUS` error code.
    #[error("ICMP send failed with error {0}")]
    Send(u32),
}

/// Open ICMP handle; one per thread, as probes on it are blocking.
#[derive(Debug)]
pub struct Pinger {
    /// Handle from `IcmpCreateFile`, closed on drop.
    handle: HANDLE,
}

// SAFETY: an ICMP handle is a kernel object handle, not tied to the thread that created it; `Pinger` owns it
// exclusively, so moving it to another thread is sound. (`HANDLE` wraps a raw pointer, hence not `Send` by default.)
unsafe impl Send for Pinger {}

impl Pinger {
    /// Opens an ICMP handle.
    ///
    /// # Errors
    ///
    /// [`IcmpError::Open`] if Windows refuses to create the handle.
    pub fn new() -> Result<Self, IcmpError> {
        // SAFETY: `IcmpCreateFile` has no preconditions; an invalid handle is turned into `Err` by the wrapper.
        let handle = unsafe { IcmpCreateFile() }.map_err(IcmpError::Open)?;
        Ok(Self { handle })
    }

    /// Sends one echo request to `target` with `ttl` and waits up to `timeout` for the answer.
    ///
    /// The round-trip time is measured locally around the call, which gives sub-millisecond precision
    /// (the API itself reports whole milliseconds).
    ///
    /// # Errors
    ///
    /// [`IcmpError::Send`] if the API fails for a reason other than the network (no answer is
    /// [`ProbeOutcome::Lost`], not an error).
    pub fn probe(&self, target: Ipv4Addr, ttl: u8, timeout: Duration) -> Result<ProbeOutcome, IcmpError> {
        let options = IP_OPTION_INFORMATION { Ttl: ttl, ..Default::default() };
        let mut buffer = [0u64; REPLY_BUFFER_WORDS];
        let timeout_ms = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX);

        let started = Instant::now();
        // SAFETY: the handle is open for the lifetime of `self`; payload, options and buffer are live locals whose
        // pointers and sizes match; the buffer is 8-byte aligned and larger than one reply plus payload.
        let replies = unsafe {
            IcmpSendEcho(
                self.handle,
                u32::from_ne_bytes(target.octets()),
                PAYLOAD.as_ptr().cast(),
                PAYLOAD.len() as u16,
                Some(&raw const options),
                buffer.as_mut_ptr().cast(),
                size_of_val(&buffer) as u32,
                timeout_ms,
            )
        };
        let rtt = started.elapsed();

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

        // SAFETY: the buffer is initialized (zeroed, then possibly written by the API), 8-byte aligned and larger
        // than `ICMP_ECHO_REPLY`, a plain C struct for which any bit pattern is valid.
        let reply = unsafe { buffer.as_ptr().cast::<ICMP_ECHO_REPLY>().read() };
        Ok(match reply.Status {
            IP_SUCCESS | IP_TTL_EXPIRED_TRANSIT => {
                ProbeOutcome::Reply { from: Ipv4Addr::from(reply.Address.to_ne_bytes()), rtt }
            }
            status => {
                log::debug!("probe {target} ttl {ttl}: reply status {status}");
                ProbeOutcome::Lost
            }
        })
    }
}

impl Drop for Pinger {
    fn drop(&mut self) {
        // SAFETY: the handle came from `IcmpCreateFile` and is closed only here, once.
        if let Err(error) = unsafe { IcmpCloseHandle(self.handle) } {
            log::warn!("cannot close ICMP handle: {error}");
        }
    }
}
