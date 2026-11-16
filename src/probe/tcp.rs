//! TCP SYN probes with a chosen TTL, without a packet driver (Npcap): Windows builds and sends the SYN of an
//! ordinary non-blocking `connect()` whose socket has the TTL set; a raw socket in `SIO_RCVALL` mode sees every
//! IPv4 packet the interface receives, so it catches the routers' ICMP "TTL expired" / "unreachable" (they quote our
//! SYN's ports) and the target's own SYN-ACK or RST. The raw socket needs administrator rights; IPv4 only.

use std::collections::HashMap;
use std::io::{self, Read as _};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::os::windows::io::AsRawSocket as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use socket2::{Domain, Protocol, Socket, Type};
use windows::Win32::Networking::WinSock::{RCVALL_ON, SIO_RCVALL, SOCKET, WSAIoctl};

use crate::probe::route::ProbeOutcome;
use crate::sync::lock;

/// Receive buffer of the raw socket: room for bursts of traffic while the listener thread is busy.
const RECEIVE_BUFFER: usize = 4 * 1024 * 1024;
/// IP protocol number of ICMP.
const PROTOCOL_ICMP: u8 = 1;
/// IP protocol number of TCP.
const PROTOCOL_TCP: u8 = 6;
/// ICMP "destination unreachable".
const ICMP_UNREACHABLE: u8 = 3;
/// ICMP "time exceeded" (TTL expired in transit).
const ICMP_TIME_EXCEEDED: u8 = 11;
/// TCP flag RST.
const TCP_RST: u8 = 0x04;
/// TCP flag ACK.
const TCP_ACK: u8 = 0x10;
/// Windows' "access denied" socket error: the raw socket needs administrator rights.
const WSAEACCES: i32 = 10013;

/// Error of a TCP probe itself (not a lost probe).
#[derive(Debug, thiserror::Error)]
pub enum TcpError {
    /// The raw socket that catches the answers can't be opened without administrator rights.
    #[error("TCP probes need administrator rights")]
    NotElevated,
    /// The raw socket couldn't be opened or switched to receive everything.
    #[error("cannot listen for TCP probe answers: {0}")]
    Listen(io::Error),
    /// No local address leads to the target (no network).
    #[error("no route to {0}: {1}")]
    Route(Ipv4Addr, io::Error),
    /// A firewall on this computer doesn't let Ping Watch open TCP connections.
    #[error("A firewall on this computer blocks Ping Watch's TCP connections")]
    Blocked,
    /// The probe's socket couldn't be set up.
    #[error("cannot send a TCP probe: {0}")]
    Socket(io::Error),
}

/// Which probe an answer belongs to: the target's address and the probe's local port.
type Key = (Ipv4Addr, u16);

/// An answer to a probe: who sent it and when it arrived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Answer {
    /// The router or the target that answered.
    from: Ipv4Addr,
    /// When the listener received it.
    at: Instant,
}

/// The raw socket of one local address with its receiving thread, and the probes waiting for answers.
#[derive(Debug)]
struct Listener {
    /// Probes waiting, by key; the thread sends each its answer.
    waiting: Mutex<HashMap<Key, Sender<Answer>>>,
    /// Cleared when the thread ends (the interface went away); a new listener is opened then.
    alive: AtomicBool,
}

/// Sends TCP probes and matches their answers; one for the whole app, shared by all traces.
#[derive(Debug, Default)]
pub struct TcpProber {
    /// Listeners by local address (one per network interface in use).
    listeners: Mutex<HashMap<Ipv4Addr, Arc<Listener>>>,
}

impl TcpProber {
    /// Sends a SYN to `target`:`port` with `ttl` and waits up to `timeout` for a router's ICMP answer or the
    /// target's SYN-ACK / RST.
    ///
    /// # Errors
    ///
    /// [`TcpError`] if the answers can't be listened for (no administrator rights) or the probe can't be sent; no
    /// answer is [`ProbeOutcome::Lost`], not an error.
    pub fn probe(&self, target: Ipv4Addr, port: u16, ttl: u8, timeout: Duration) -> Result<ProbeOutcome, TcpError> {
        let local = local_address(target, port)?;
        let listener = self.listener(local)?;
        let socket = Socket::new(Domain::IPV4, Type::STREAM, Some(Protocol::TCP)).map_err(TcpError::Socket)?;
        socket.set_ttl_v4(u32::from(ttl)).map_err(TcpError::Socket)?;
        // Closing then resets the connection instead of leaving it in TIME_WAIT: a trace opens dozens a second.
        socket.set_linger(Some(Duration::ZERO)).map_err(TcpError::Socket)?;
        // Bound first, so the local port is known before the SYN can be answered.
        socket.bind(&SocketAddr::V4(SocketAddrV4::new(local, 0)).into()).map_err(TcpError::Socket)?;
        let local_port = socket
            .local_addr()
            .ok()
            .and_then(|address| address.as_socket())
            .map(|address| address.port())
            .ok_or_else(|| TcpError::Socket(io::Error::other("the probe socket has no local port")))?;
        socket.set_nonblocking(true).map_err(TcpError::Socket)?;
        let key = (target, local_port);
        let (sender, receiver) = mpsc::channel();
        lock(&listener.waiting).insert(key, sender);
        let sent = Instant::now();
        let outcome = match socket.connect(&SocketAddr::V4(SocketAddrV4::new(target, port)).into()) {
            // Non-blocking: the SYN is on its way.
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => match receiver.recv_timeout(timeout) {
                Ok(answer) => {
                    Ok(ProbeOutcome::Reply { from: answer.from.into(), rtt: answer.at.saturating_duration_since(sent) })
                }
                Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => match socket.take_error() {
                    // A firewall on this computer refused the connection: no SYN left at all.
                    Ok(Some(error)) if error.raw_os_error() == Some(WSAEACCES) => Err(TcpError::Blocked),
                    _ => Ok(ProbeOutcome::Lost),
                },
            },
            // A local network reaches it at once; the SYN-ACK came before the call returned.
            Ok(()) => Ok(ProbeOutcome::Reply { from: target.into(), rtt: sent.elapsed() }),
            Err(error) => Err(TcpError::Socket(error)),
        };
        lock(&listener.waiting).remove(&key);
        outcome
    }

    /// The live listener of `local`, opened if there is none.
    fn listener(&self, local: Ipv4Addr) -> Result<Arc<Listener>, TcpError> {
        let mut listeners = lock(&self.listeners);
        if let Some(listener) = listeners.get(&local).filter(|listener| listener.alive.load(Ordering::Relaxed)) {
            return Ok(Arc::clone(listener));
        }
        let listener = Listener::open(local)?;
        listeners.insert(local, Arc::clone(&listener));
        Ok(listener)
    }
}

impl Listener {
    /// Opens the raw socket of `local` in `SIO_RCVALL` mode and starts its receiving thread.
    fn open(local: Ipv4Addr) -> Result<Arc<Self>, TcpError> {
        let socket = Socket::new(Domain::IPV4, Type::RAW, Some(Protocol::from(0))).map_err(|error| {
            if error.raw_os_error() == Some(WSAEACCES) { TcpError::NotElevated } else { TcpError::Listen(error) }
        })?;
        socket.bind(&SocketAddr::V4(SocketAddrV4::new(local, 0)).into()).map_err(TcpError::Listen)?;
        if let Err(error) = socket.set_recv_buffer_size(RECEIVE_BUFFER) {
            log::debug!("cannot enlarge the TCP listener's buffer: {error}");
        }
        receive_all(&socket).map_err(TcpError::Listen)?;
        let listener = Arc::new(Self { waiting: Mutex::new(HashMap::new()), alive: AtomicBool::new(true) });
        let shared = Arc::clone(&listener);
        thread::Builder::new()
            .name("tcp-answers".to_owned())
            .spawn(move || shared.receive_loop(&socket, local))
            .map_err(TcpError::Listen)?;
        Ok(listener)
    }

    /// Thread body: hands every answer to the probe waiting for it, until the socket fails.
    fn receive_loop(&self, socket: &Socket, local: Ipv4Addr) {
        let mut packet = vec![0_u8; 65_535];
        let mut reader = socket;
        loop {
            let length = match reader.read(&mut packet) {
                Ok(length) => length,
                Err(error) => {
                    log::warn!("TCP probe listener of {local} stopped: {error}");
                    self.alive.store(false, Ordering::Relaxed);
                    return;
                }
            };
            let at = Instant::now();
            let Some((key, from)) = packet.get(..length).and_then(|packet| answer_of(packet, local)) else {
                continue;
            };
            if let Some(sender) = lock(&self.waiting).get(&key) {
                // The probe may have timed out meanwhile; then nobody listens.
                sender.send(Answer { from, at }).ok();
            }
        }
    }
}

/// Switches the raw `socket` to receive every IPv4 packet of its interface.
fn receive_all(socket: &Socket) -> io::Result<()> {
    let value = RCVALL_ON.0;
    let mut returned = 0_u32;
    let handle = SOCKET(usize::try_from(socket.as_raw_socket()).map_err(io::Error::other)?);
    // SAFETY: `handle` is the open raw socket owned by `socket`; the input buffer is `value`, which lives across
    // the call, with its exact size; there is no output buffer, overlapped structure or completion routine.
    let result = unsafe {
        WSAIoctl(
            handle,
            SIO_RCVALL,
            Some((&raw const value).cast()),
            u32::try_from(size_of_val(&value)).unwrap_or(u32::MAX),
            None,
            0,
            &raw mut returned,
            None,
            None,
        )
    };
    if result == 0 { Ok(()) } else { Err(io::Error::last_os_error()) }
}

/// The local address Windows would send from to reach `target`:`port` (no packet is sent).
fn local_address(target: Ipv4Addr, port: u16) -> Result<Ipv4Addr, TcpError> {
    let route = |error| TcpError::Route(target, error);
    let socket = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0)).map_err(route)?;
    socket.connect(SocketAddrV4::new(target, port)).map_err(route)?;
    match socket.local_addr().map_err(route)? {
        SocketAddr::V4(address) => Ok(*address.ip()),
        SocketAddr::V6(_) => Err(route(io::Error::other("an IPv6 route to an IPv4 target"))),
    }
}

/// If `packet` (an IPv4 packet received on `local`) answers a TCP probe: the probe's key and who answered. A
/// router's ICMP "TTL expired" or "unreachable" quotes the probe's IP header and ports; the target answers the SYN
/// with SYN-ACK (port open) or RST (closed), both proof that it was reached.
fn answer_of(packet: &[u8], local: Ipv4Addr) -> Option<(Key, Ipv4Addr)> {
    let (protocol, source, destination, payload) = ipv4(packet)?;
    if destination != local {
        return None;
    }
    match protocol {
        PROTOCOL_ICMP => {
            let icmp_type = *payload.first()?;
            if icmp_type != ICMP_TIME_EXCEEDED && icmp_type != ICMP_UNREACHABLE {
                return None;
            }
            let (quoted_protocol, quoted_source, target, quoted) = ipv4(payload.get(8..)?)?;
            if quoted_protocol != PROTOCOL_TCP || quoted_source != local {
                return None;
            }
            let local_port = u16::from_be_bytes([*quoted.first()?, *quoted.get(1)?]);
            Some(((target, local_port), source))
        }
        PROTOCOL_TCP => {
            let local_port = u16::from_be_bytes([*payload.get(2)?, *payload.get(3)?]);
            let flags = *payload.get(13)?;
            let answers = flags & TCP_RST != 0 || flags & TCP_ACK != 0;
            answers.then_some(((source, local_port), source))
        }
        _ => None,
    }
}

/// Protocol, source, destination and payload of an IPv4 packet (or of the start of one quoted in ICMP: its
/// payload may be cut short).
fn ipv4(packet: &[u8]) -> Option<(u8, Ipv4Addr, Ipv4Addr, &[u8])> {
    let first = *packet.first()?;
    if first >> 4 != 4 {
        return None;
    }
    let header = usize::from(first & 0x0F).checked_mul(4)?;
    let address = |at: usize| -> Option<Ipv4Addr> {
        let bytes: [u8; 4] = packet.get(at..at.checked_add(4)?)?.try_into().ok()?;
        Some(Ipv4Addr::from(bytes))
    };
    Some((*packet.get(9)?, address(12)?, address(16)?, packet.get(header..)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Our address.
    const LOCAL: Ipv4Addr = Ipv4Addr::new(192, 168, 1, 10);
    /// The traced target.
    const TARGET: Ipv4Addr = Ipv4Addr::new(94, 156, 128, 52);
    /// A router on the way.
    const ROUTER: Ipv4Addr = Ipv4Addr::new(10, 0, 0, 1);

    /// A 20-byte IPv4 header for `protocol` from `source` to `destination`, followed by `payload`.
    fn ip(protocol: u8, source: Ipv4Addr, destination: Ipv4Addr, payload: &[u8]) -> Vec<u8> {
        let mut packet = vec![0x45, 0, 0, 0, 0, 0, 0, 0, 64, protocol, 0, 0];
        packet.extend(source.octets());
        packet.extend(destination.octets());
        packet.extend(payload);
        packet
    }

    /// The start of a TCP header from `source_port` to `destination_port` with `flags`.
    fn tcp(source_port: u16, destination_port: u16, flags: u8) -> Vec<u8> {
        let mut header = Vec::new();
        header.extend(source_port.to_be_bytes());
        header.extend(destination_port.to_be_bytes());
        header.extend([0; 9]);
        header.push(flags);
        header.extend([0; 6]);
        header
    }

    /// A router's ICMP message of `icmp_type` quoting our SYN from `local_port` to the target.
    fn icmp(icmp_type: u8, local_port: u16) -> Vec<u8> {
        let syn = ip(PROTOCOL_TCP, LOCAL, TARGET, tcp(local_port, 443, 0x02).get(..8).unwrap_or_default());
        let mut message = vec![icmp_type, 0, 0, 0, 0, 0, 0, 0];
        message.extend(syn);
        ip(PROTOCOL_ICMP, ROUTER, LOCAL, &message)
    }

    #[test]
    fn a_routers_ttl_expired_answers_the_quoted_probe() {
        assert_eq!(answer_of(&icmp(ICMP_TIME_EXCEEDED, 50_001), LOCAL), Some(((TARGET, 50_001), ROUTER)));
        assert_eq!(answer_of(&icmp(ICMP_UNREACHABLE, 50_002), LOCAL), Some(((TARGET, 50_002), ROUTER)));
    }

    #[test]
    fn the_targets_syn_ack_or_rst_answers_the_probe() {
        let syn_ack = ip(PROTOCOL_TCP, TARGET, LOCAL, &tcp(443, 50_003, 0x12));
        assert_eq!(answer_of(&syn_ack, LOCAL), Some(((TARGET, 50_003), TARGET)));
        let rst = ip(PROTOCOL_TCP, TARGET, LOCAL, &tcp(443, 50_004, 0x14));
        assert_eq!(answer_of(&rst, LOCAL), Some(((TARGET, 50_004), TARGET)));
    }

    #[test]
    fn other_traffic_is_ignored() {
        // Our own outgoing SYN, seen by the listener too.
        assert_eq!(answer_of(&ip(PROTOCOL_TCP, LOCAL, TARGET, &tcp(50_005, 443, 0x02)), LOCAL), None);
        // An echo reply, and an ICMP message for another host.
        assert_eq!(answer_of(&ip(PROTOCOL_ICMP, ROUTER, LOCAL, &[0; 36]), LOCAL), None);
        assert_eq!(answer_of(&icmp(ICMP_TIME_EXCEEDED, 50_006), Ipv4Addr::new(192, 168, 1, 11)), None);
        // Not IPv4, and cut short.
        assert_eq!(answer_of(&[0x60; 40], LOCAL), None);
        assert_eq!(answer_of(&[0x45, 0, 0], LOCAL), None);
    }
}

/// Probes a real server; run by hand with administrator rights: `cargo test -- --ignored live`.
#[cfg(test)]
mod live {
    use super::*;

    #[test]
    #[ignore = "needs administrator rights, the network, and a firewall that lets the test program connect"]
    fn a_web_server_answers_a_tcp_probe() {
        let prober = TcpProber::default();
        // one.one.one.one: answers on 443 a few hops away.
        let outcome = prober.probe(Ipv4Addr::new(1, 1, 1, 1), 443, 64, Duration::from_secs(3));
        assert!(
            matches!(outcome, Ok(ProbeOutcome::Reply { from, .. }) if from == Ipv4Addr::new(1, 1, 1, 1)),
            "{outcome:?}"
        );
        let first = prober.probe(Ipv4Addr::new(1, 1, 1, 1), 443, 1, Duration::from_secs(3));
        assert!(matches!(first, Ok(ProbeOutcome::Reply { .. })), "the first router answers: {first:?}");
    }
}
