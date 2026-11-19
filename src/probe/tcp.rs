//! TCP SYN probes with a chosen TTL (hop limit), without a packet driver (Npcap): Windows builds and sends the SYN
//! of an ordinary non-blocking `connect()` whose socket has the TTL set; a raw socket in `SIO_RCVALL` mode sees the
//! packets the computer receives, so it catches the routers' ICMP "TTL expired" / "unreachable" (they quote our
//! SYN's addresses and ports) and the target's own SYN-ACK or RST. IPv4 and IPv6; the raw socket needs
//! administrator rights.
//!
//! An IPv4 raw socket receives whole packets. An IPv6 one receives only what follows the IPv6 header (Windows
//! doesn't hand that header to raw sockets): the sender comes from `recvfrom`, the protocol from the content — an
//! ICMPv6 error quoting our SYN, or a TCP segment from the target between the probe's ports.
//!
//! The listener sees our own SYN leave, too, and keeps its sequence number: an answer counts only if it echoes that
//! number (quoted in the ICMP error, or acknowledged by the target), so neither a stranger guessing the ports nor
//! other traffic of the target (UDP between the same ports looks like TCP to an IPv6 raw socket) passes for one.

use std::collections::HashMap;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6, UdpSocket};
use std::os::windows::io::AsRawSocket as _;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use socket2::{Domain, Protocol, Socket, Type};
use windows::Win32::NetworkManagement::IpHelper::{GetBestRoute2, MIB_IPFORWARD_ROW2};
use windows::Win32::Networking::WinSock::{
    AF_INET, AF_INET6, IN_ADDR, IN_ADDR_0, IN6_ADDR, IN6_ADDR_0, RCVALL_IPLEVEL, SIO_RCVALL, SIO_TCP_INITIAL_RTO,
    SOCKADDR_IN, SOCKADDR_IN6, SOCKADDR_INET, SOCKET, TCP_INITIAL_RTO_DEFAULT_RTT,
    TCP_INITIAL_RTO_NO_SYN_RETRANSMISSIONS, TCP_INITIAL_RTO_PARAMETERS, WSAIoctl,
};
use windows::core::HRESULT;

use crate::i18n::{Message, n};
use crate::probe::firewall::{self, AnswerWatch};
use crate::probe::icmp;
use crate::probe::route::ProbeOutcome;
use crate::sync::lock;

/// Receive buffer of the raw socket: room for bursts of traffic while the listener thread is busy.
const RECEIVE_BUFFER: usize = 4 * 1024 * 1024;
/// A listener with no probe waiting for this long closes, so the computer's packets aren't copied into Ping Watch
/// while no TCP trace runs; the next probe opens a new one.
const IDLE_CLOSE: Duration = Duration::from_secs(10);
/// How often the listener thread looks whether it is idle, also while no packet arrives.
const IDLE_CHECK: Duration = Duration::from_secs(1);
/// IP protocol number of ICMP.
const PROTOCOL_ICMP: u8 = 1;
/// IP protocol number of TCP.
const PROTOCOL_TCP: u8 = 6;
/// ICMP "destination unreachable".
const ICMP_UNREACHABLE: u8 = 3;
/// ICMP "time exceeded" (TTL expired in transit).
const ICMP_TIME_EXCEEDED: u8 = 11;
/// ICMPv6 "destination unreachable".
const ICMP6_UNREACHABLE: u8 = 1;
/// ICMPv6 "time exceeded" (hop limit exceeded in transit).
const ICMP6_TIME_EXCEEDED: u8 = 3;
/// Length of an IPv6 header (without extension headers; our SYN has none).
const IPV6_HEADER: usize = 40;
/// Length of a TCP header without options.
const TCP_HEADER: usize = 20;
/// TCP flag SYN.
const TCP_SYN: u8 = 0x02;
/// TCP flag RST.
const TCP_RST: u8 = 0x04;
/// TCP flag ACK.
const TCP_ACK: u8 = 0x10;
/// Windows' "access denied" socket error: the raw socket needs administrator rights.
const WSAEACCES: i32 = 10013;
/// Message of [`TcpError::NotElevated`], translated where it is shown.
pub const NOT_ELEVATED: &str = n!("TCP probes need administrator rights");
/// Message of [`TcpError::Blocked`], translated where it is shown.
pub const BLOCKED: &str = n!("A firewall on this computer blocks Ping Watch's TCP connections");

/// Error of a TCP probe itself (not a lost probe).
#[derive(Debug, thiserror::Error)]
pub enum TcpError {
    /// The raw socket that catches the answers can't be opened without administrator rights.
    #[error("{}", NOT_ELEVATED)]
    NotElevated,
    /// The raw socket couldn't be opened or switched to receive the computer's packets.
    #[error("cannot listen for TCP probe answers: {0}")]
    Listen(io::Error),
    /// No local address leads to the target (no network).
    #[error("no route to {0}: {1}")]
    Route(IpAddr, windows::core::Error),
    /// A firewall on this computer doesn't let Ping Watch open TCP connections.
    #[error("{}", BLOCKED)]
    Blocked,
    /// The probe's socket couldn't be set up.
    #[error("cannot send a TCP probe: {0}")]
    Socket(io::Error),
}

impl TcpError {
    /// The error as shown to the user; Windows' own text stays as Windows wrote it.
    pub fn message(&self) -> Message {
        match self {
            Self::NotElevated => Message::new(NOT_ELEVATED),
            Self::Blocked => Message::new(BLOCKED),
            Self::Listen(error) => {
                Message::new(n!("Cannot listen for TCP probe answers: {error}")).with("error", error)
            }
            Self::Route(address, error) if is_no_route_error(error) => {
                Message::new(icmp::NO_ROUTE).with("address", address)
            }
            Self::Route(address, error) => Message::new(n!("No route to {address}: {error}"))
                .with("address", address)
                .with("error", error.message()),
            Self::Socket(error) => Message::new(n!("Cannot send a TCP probe: {error}")).with("error", error),
        }
    }
}

/// Whether `error` of a route lookup is Windows' "the network location cannot be reached".
fn is_no_route_error(error: &windows::core::Error) -> bool {
    [1231, 1232]
        .into_iter()
        .any(|code| icmp::is_no_route(code) && error.code() == HRESULT::from_win32(code))
}

/// Which probe an answer belongs to: the target's address, its port, and the probe's local port.
type Key = (IpAddr, u16, u16);

/// What an answer says about its probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Heard {
    /// This router (TTL expired) or the target itself answered.
    Reply(IpAddr),
    /// A router on the way reported the target unreachable: lost, as an ICMP probe would be, with no hop to show.
    Lost,
}

/// An answer to a probe: what it says and when it arrived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Answer {
    /// What it says.
    heard: Heard,
    /// When the listener received it.
    at: Instant,
}

/// The raw socket of one local address with its receiving thread, and the probes waiting for answers.
#[derive(Debug)]
struct Listener {
    /// The probes waiting and whether the thread still runs, under one lock: a probe is registered only with a
    /// listener that is open, and the thread closes only while no probe waits.
    waiting: Mutex<Waiting>,
}

/// How an answer proves it is about our SYN and not made up or another protocol's: by our SYN's sequence number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Echo {
    /// An ICMP error quotes the SYN, its sequence number included.
    Quoted(u32),
    /// The target's SYN-ACK or RST acknowledges the SYN: its sequence number plus one.
    Acknowledged(u32),
}

impl Echo {
    /// Whether this is the echo of a SYN sent with sequence number `sequence`.
    fn of(self, sequence: u32) -> bool {
        match self {
            Self::Quoted(quoted) => quoted == sequence,
            Self::Acknowledged(acknowledged) => acknowledged == sequence.wrapping_add(1),
        }
    }
}

/// A probe waiting for its answer.
#[derive(Debug)]
struct Pending {
    /// Where the listener sends the answer.
    sender: Sender<Answer>,
    /// Sequence number of the probe's SYN, once the listener saw it leave. Until then (the SYN always leaves
    /// before its answer comes, so only where outgoing packets aren't shown to the listener) answers are taken
    /// by addresses and ports alone.
    sequence: Option<u32>,
}

/// The probes a listener answers.
#[derive(Debug)]
struct Waiting {
    /// Probes waiting, by key; the thread sends each its answer.
    probes: HashMap<Key, Pending>,
    /// Cleared when the thread ends (idle, or the interface went away); a new listener is opened then.
    open: bool,
}

/// Sends TCP probes and matches their answers; one for the whole app, shared by all traces.
#[derive(Debug, Default)]
pub struct TcpProber {
    /// Listeners by local address (one per network interface and address family in use).
    listeners: Mutex<HashMap<IpAddr, Arc<Listener>>>,
    /// What the probes of each target show about a firewall on this computer keeping their answers away.
    watches: Mutex<HashMap<IpAddr, AnswerWatch>>,
}

impl TcpProber {
    /// Sends a SYN to `target`:`port` with `ttl` and waits up to `timeout` for a router's ICMP answer or the
    /// target's SYN-ACK / RST.
    ///
    /// # Errors
    ///
    /// [`TcpError`] if the answers can't be listened for (no administrator rights) or the probe can't be sent; no
    /// answer is [`ProbeOutcome::Lost`], not an error.
    pub fn probe(&self, target: IpAddr, port: u16, ttl: u8, timeout: Duration) -> Result<ProbeOutcome, TcpError> {
        let local = local_address(target)?;
        let socket =
            Socket::new(Domain::for_address(local), Type::STREAM, Some(Protocol::TCP)).map_err(TcpError::Socket)?;
        match target {
            IpAddr::V4(_) => socket.set_ttl_v4(u32::from(ttl)),
            IpAddr::V6(_) => socket.set_unicast_hops_v6(u32::from(ttl)),
        }
        .map_err(TcpError::Socket)?;
        // Closing then resets the connection instead of leaving it in TIME_WAIT: a trace opens dozens a second.
        socket.set_linger(Some(Duration::ZERO)).map_err(TcpError::Socket)?;
        // Windows would send the SYN again after a second, with the same TTL and port: an answer to that one would
        // count for this probe, a loss turning into a round trip of a second.
        if let Err(error) = no_syn_retransmissions(&socket) {
            log::debug!("cannot turn off SYN retransmissions: {error}");
        }
        // Bound first, so the local port is known before the SYN can be answered.
        socket.bind(&local.into()).map_err(TcpError::Socket)?;
        let local_port = socket
            .local_addr()
            .ok()
            .and_then(|address| address.as_socket())
            .map(|address| address.port())
            .ok_or_else(|| TcpError::Socket(io::Error::other("the probe socket has no local port")))?;
        socket.set_nonblocking(true).map_err(TcpError::Socket)?;
        let key = (target, port, local_port);
        let (sender, receiver) = mpsc::channel();
        let listener = self.register(local, key, sender)?;
        let sent = Instant::now();
        // Set if the probe is lost although Windows made the connection: the target's SYN-ACK came, and something
        // on this computer kept it from the listener.
        let mut target_hidden = false;
        let outcome = match socket.connect(&SocketAddr::new(target, port).into()) {
            // Non-blocking: the SYN is on its way.
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => match receiver.recv_timeout(timeout) {
                Ok(Answer { heard: Heard::Reply(from), at }) => {
                    Ok(ProbeOutcome::Reply { from, rtt: at.saturating_duration_since(sent) })
                }
                Ok(Answer { heard: Heard::Lost, .. }) => Ok(ProbeOutcome::Lost),
                Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => match socket.take_error() {
                    // A firewall on this computer refused the connection: no SYN left at all.
                    Ok(Some(error)) if error.raw_os_error() == Some(WSAEACCES) => Err(TcpError::Blocked),
                    _ => {
                        target_hidden = is_connected(&socket);
                        Ok(ProbeOutcome::Lost)
                    }
                },
            },
            // A local network reaches it at once; the SYN-ACK came before the call returned.
            Ok(()) => Ok(ProbeOutcome::Reply { from: target, rtt: sent.elapsed() }),
            Err(error) => Err(TcpError::Socket(error)),
        };
        lock(&listener.waiting).probes.remove(&key);
        if let Ok(outcome) = &outcome {
            self.watch_answers(target, *outcome, target_hidden);
        }
        outcome
    }

    /// Registers the probe `key` with the open listener of `local` (opened if there is none), which sends its
    /// answer to `sender`; returns that listener.
    fn register(&self, local: SocketAddr, key: Key, sender: Sender<Answer>) -> Result<Arc<Listener>, TcpError> {
        loop {
            let listener = self.listener(local)?;
            let mut waiting = lock(&listener.waiting);
            // It may have closed since it was looked up; then the next lookup opens a new one.
            if waiting.open {
                waiting.probes.insert(key, Pending { sender, sequence: None });
                drop(waiting);
                return Ok(listener);
            }
        }
    }

    /// The open listener of `local`, opened if there is none.
    fn listener(&self, local: SocketAddr) -> Result<Arc<Listener>, TcpError> {
        let mut listeners = lock(&self.listeners);
        if let Some(listener) = listeners.get(&local.ip()).filter(|listener| lock(&listener.waiting).open) {
            return Ok(Arc::clone(listener));
        }
        let listener = Listener::open(local)?;
        listeners.insert(local.ip(), Arc::clone(&listener));
        Ok(listener)
    }

    /// Whether a firewall on this computer keeps the answers to the probes of `target` from Ping Watch.
    pub fn answers_blocked(&self, target: IpAddr) -> bool {
        lock(&self.watches).get(&target).is_some_and(|watch| watch.blocked(Instant::now()))
    }

    /// Forgets what the probes showed so far: firewall rules were just added, the next probes tell anew.
    pub fn rules_added(&self) {
        let now = SystemTime::now();
        lock(&self.watches).values_mut().for_each(|watch| *watch = AnswerWatch::after_rules(now));
    }

    /// Takes the `outcome` of a probe of `target` into what is known about a firewall on this computer;
    /// `target_hidden` if the probe was lost although its connection was made (the target answered). While
    /// answers are missing, a lost probe also has the log of dropped packets read, now and then.
    fn watch_answers(&self, target: IpAddr, outcome: ProbeOutcome, target_hidden: bool) {
        let now = Instant::now();
        let since = {
            let mut watches = lock(&self.watches);
            let watch = watches.entry(target).or_default();
            match outcome {
                ProbeOutcome::Reply { from, .. } if from == target => watch.target_heard(),
                ProbeOutcome::Reply { .. } => watch.router_heard(now),
                ProbeOutcome::Lost if target_hidden => watch.target_hidden(now),
                ProbeOutcome::Lost => {}
            }
            (outcome == ProbeOutcome::Lost && watch.check_due(now)).then(|| watch.log_since(SystemTime::now()))
        };
        let Some(since) = since else {
            return;
        };
        // Read without the lock: other probes go on meanwhile.
        let dropped = firewall::hop_answers_dropped(target, since).unwrap_or_else(|error| {
            log::debug!("cannot read the log of dropped packets: {error}");
            false
        });
        lock(&self.watches).entry(target).or_default().log_read(dropped, now);
    }
}

impl Listener {
    /// Opens the raw socket of `local` in `SIO_RCVALL` mode and starts its receiving thread.
    fn open(local: SocketAddr) -> Result<Arc<Self>, TcpError> {
        // Protocol 0 for both families: for IPv6 it receives the ICMPv6 errors and the target's TCP segments too
        // (checked live, `a_web_server_answers_an_ipv6_tcp_probe`).
        let socket = Socket::new(Domain::for_address(local), Type::RAW, Some(Protocol::from(0))).map_err(|error| {
            if error.raw_os_error() == Some(WSAEACCES) { TcpError::NotElevated } else { TcpError::Listen(error) }
        })?;
        socket.bind(&local.into()).map_err(TcpError::Listen)?;
        if let Err(error) = socket.set_recv_buffer_size(RECEIVE_BUFFER) {
            log::debug!("cannot enlarge the TCP listener's buffer: {error}");
        }
        receive_all(&socket).map_err(TcpError::Listen)?;
        let waiting = Waiting { probes: HashMap::new(), open: true };
        let listener = Arc::new(Self { waiting: Mutex::new(waiting) });
        let shared = Arc::clone(&listener);
        // std's `recv_from` works on any datagram-like socket, raw ones included, and needs no unsafe buffer.
        let socket = UdpSocket::from(socket);
        socket.set_read_timeout(Some(IDLE_CHECK)).map_err(TcpError::Listen)?;
        thread::Builder::new()
            .name("tcp-answers".to_owned())
            .spawn(move || shared.receive_loop(&socket, local.ip()))
            .map_err(TcpError::Listen)?;
        Ok(listener)
    }

    /// Thread body: hands every answer to the probe waiting for it, until no probe has waited for [`IDLE_CLOSE`]
    /// or the socket fails; the socket closes with the thread.
    fn receive_loop(&self, socket: &UdpSocket, local: IpAddr) {
        let mut packet = vec![0_u8; 65_535];
        let mut busy_at = Instant::now();
        let mut checked_at = Instant::now();
        loop {
            match socket.recv_from(&mut packet) {
                Ok((length, from)) => {
                    let at = Instant::now();
                    let packet = packet.get(..length).unwrap_or_default();
                    if let Some((local_port, target_port, sequence)) = own_syn(packet, from.ip(), local) {
                        // A local port belongs to one socket at a time: the ports name the probe.
                        lock(&self.waiting)
                            .probes
                            .iter_mut()
                            .filter(|((_, target, source), _)| *target == target_port && *source == local_port)
                            .for_each(|(_, probe)| probe.sequence = Some(sequence));
                    } else if let Some((key, heard, echo)) = echoed_answer(packet, from.ip(), local)
                        && let Some(probe) = lock(&self.waiting).probes.get(&key)
                        && probe.sequence.is_none_or(|sequence| echo.of(sequence))
                    {
                        // The probe may have timed out meanwhile; then nobody listens.
                        probe.sender.send(Answer { heard, at }).ok();
                    }
                }
                Err(error) if matches!(error.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {}
                Err(error) => {
                    log::warn!("TCP probe listener of {local} stopped: {error}");
                    lock(&self.waiting).open = false;
                    return;
                }
            }
            if checked_at.elapsed() < IDLE_CHECK {
                continue;
            }
            checked_at = Instant::now();
            let mut waiting = lock(&self.waiting);
            if !waiting.probes.is_empty() {
                busy_at = checked_at;
            } else if busy_at.elapsed() >= IDLE_CLOSE {
                waiting.open = false;
                log::debug!("TCP probe listener of {local} closed: no TCP probes");
                return;
            }
        }
    }
}

/// Whether the non-blocking `connect()` of `socket` was answered: Windows made the connection. Sending nothing
/// tells: it succeeds on a connection and fails ("not connected") while the SYN is unanswered — the peer's address
/// doesn't, Windows gives it as soon as `connect()` is called.
fn is_connected(socket: &Socket) -> bool {
    socket.send(&[]).is_ok()
}

/// Keeps Windows from sending the SYN of `socket` again when no answer comes: one probe, one SYN.
fn no_syn_retransmissions(socket: &Socket) -> io::Result<()> {
    // The header's value is `(UCHAR) -2`; the crate gives it as a wider integer.
    let no_retransmissions = u8::try_from(TCP_INITIAL_RTO_NO_SYN_RETRANSMISSIONS & 0xFF).map_err(io::Error::other)?;
    let parameters = TCP_INITIAL_RTO_PARAMETERS {
        Rtt: u16::try_from(TCP_INITIAL_RTO_DEFAULT_RTT).map_err(io::Error::other)?,
        MaxSynRetransmissions: no_retransmissions,
    };
    socket_ioctl(socket, SIO_TCP_INITIAL_RTO, &parameters)
}

/// Runs the input-only `WSAIoctl` `code` on `socket` with `input`.
fn socket_ioctl<T>(socket: &Socket, code: u32, input: &T) -> io::Result<()> {
    let mut returned = 0_u32;
    let handle = SOCKET(usize::try_from(socket.as_raw_socket()).map_err(io::Error::other)?);
    // SAFETY: `handle` is the open socket owned by `socket`; the input buffer is `input`, which lives across the
    // call, with its exact size; there is no output buffer, overlapped structure or completion routine.
    let result = unsafe {
        WSAIoctl(
            handle,
            code,
            Some((&raw const *input).cast()),
            u32::try_from(size_of_val(input)).unwrap_or(u32::MAX),
            None,
            0,
            &raw mut returned,
            None,
            None,
        )
    };
    if result == 0 { Ok(()) } else { Err(io::Error::last_os_error()) }
}

/// Switches the raw `socket` to receive every packet of its address family that the computer sends or receives
/// (`RCVALL_IPLEVEL`: the network card is not put in promiscuous mode, other computers' traffic is not seen).
fn receive_all(socket: &Socket) -> io::Result<()> {
    socket_ioctl(socket, SIO_RCVALL, &RCVALL_IPLEVEL.0)
}

/// The local address (port 0) Windows sends from to reach `target`, from its routing table (no socket, no
/// packet); an IPv6 one keeps its scope (interface) for binding.
fn local_address(target: IpAddr) -> Result<SocketAddr, TcpError> {
    let destination = match target {
        IpAddr::V4(ip) => SOCKADDR_INET {
            Ipv4: SOCKADDR_IN {
                sin_family: AF_INET,
                sin_port: 0,
                sin_addr: IN_ADDR { S_un: IN_ADDR_0 { S_addr: u32::from_ne_bytes(ip.octets()) } },
                sin_zero: [0; 8],
            },
        },
        IpAddr::V6(ip) => SOCKADDR_INET {
            Ipv6: SOCKADDR_IN6 {
                sin6_family: AF_INET6,
                sin6_addr: IN6_ADDR { u: IN6_ADDR_0 { Byte: ip.octets() } },
                ..SOCKADDR_IN6::default()
            },
        },
    };
    let mut route = MIB_IPFORWARD_ROW2::default();
    let mut source = SOCKADDR_INET::default();
    // SAFETY: `destination` is a filled IPv4 or IPv6 address; `route` and `source` are writable structs of the
    // types the function fills; no interface or source address is imposed.
    let result = unsafe { GetBestRoute2(None, 0, None, &raw const destination, 0, &raw mut route, &raw mut source) };
    result.ok().map_err(|error| TcpError::Route(target, error))?;
    match target {
        IpAddr::V4(_) => {
            // SAFETY: for an IPv4 destination the function writes an IPv4 source address, so `Ipv4` is the
            // variant set; reading the `u32` of an `IN_ADDR` is valid for any bits.
            let bits = unsafe { source.Ipv4.sin_addr.S_un.S_addr };
            Ok(SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::from(bits.to_ne_bytes()), 0)))
        }
        IpAddr::V6(_) => {
            // SAFETY: for an IPv6 destination the function writes an IPv6 source address, so `Ipv6` is the
            // variant set; its address bytes and scope id are plain integers, valid for any bits.
            let (octets, scope) = unsafe { (source.Ipv6.sin6_addr.u.Byte, source.Ipv6.Anonymous.sin6_scope_id) };
            Ok(SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::from(octets), 0, 0, scope)))
        }
    }
}

/// If `packet`, seen on `local` from `from`, is a SYN of ours on its way out: its local port, the target's port
/// and its sequence number. `packet` is a whole IPv4 packet, or, for IPv6, what follows the IPv6 header.
fn own_syn(packet: &[u8], from: IpAddr, local: IpAddr) -> Option<(u16, u16, u32)> {
    let segment = match (local, from) {
        (IpAddr::V4(local), _) => {
            let (protocol, source, _, payload) = ipv4(packet)?;
            (protocol == PROTOCOL_TCP && source == local).then_some(payload)?
        }
        (IpAddr::V6(local), IpAddr::V6(from)) => (from == local).then_some(packet)?,
        (IpAddr::V6(_), IpAddr::V4(_)) => return None,
    };
    let (local_port, target_port) = ports(segment)?;
    let flags = *segment.get(13)?;
    (flags & TCP_SYN != 0 && flags & TCP_ACK == 0).then_some((local_port, target_port, number_at(segment, 4)?))
}

/// If `packet`, received on `local` from `from`, answers a TCP probe: the probe's key, what it says and how it
/// echoes the probe's SYN. A router's
/// ICMP "TTL expired" or "unreachable" quotes the probe's IP header and ports; the target answers the SYN with
/// SYN-ACK (port open) or RST (closed), both proof that it was reached. `packet` is a whole IPv4 packet, or, for
/// IPv6, what follows the IPv6 header.
fn echoed_answer(packet: &[u8], from: IpAddr, local: IpAddr) -> Option<(Key, Heard, Echo)> {
    match (local, from) {
        (IpAddr::V4(local), _) => answer_v4(packet, local),
        (IpAddr::V6(local), IpAddr::V6(from)) => answer_v6(packet, from, local),
        (IpAddr::V6(_), IpAddr::V4(_)) => None,
    }
}

/// [`echoed_answer`] for a whole IPv4 `packet` received on `local`.
fn answer_v4(packet: &[u8], local: Ipv4Addr) -> Option<(Key, Heard, Echo)> {
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
            let (local_port, target_port) = ports(quoted)?;
            let key = (target.into(), target_port, local_port);
            let echo = Echo::Quoted(number_at(quoted, 4)?);
            Some((key, error_heard(icmp_type == ICMP_UNREACHABLE, source.into(), key), echo))
        }
        PROTOCOL_TCP => target_answer(payload, source.into()),
        _ => None,
    }
}

/// [`echoed_answer`] for an IPv6 `payload` (what follows the IPv6 header) from `from`, received on `local`: an
/// ICMPv6 error quoting our SYN, else a TCP segment from the target.
fn answer_v6(payload: &[u8], from: Ipv6Addr, local: Ipv6Addr) -> Option<(Key, Heard, Echo)> {
    // Our own outgoing packets are seen too.
    if from == local {
        return None;
    }
    icmp6_answer(payload, local)
        .map(|(key, unreachable, echo)| (key, error_heard(unreachable, from.into(), key), echo))
        .or_else(|| target_answer(payload, from.into()))
}

/// What an ICMP error from `from` about the probe of `key` says: "unreachable" from a router on the way means the
/// probe is lost (that router isn't the hop of this TTL, it answers every TTL past it); from the target itself, and
/// "TTL expired" from anyone, it is an answer.
fn error_heard(unreachable: bool, from: IpAddr, key: Key) -> Heard {
    let (target, ..) = key;
    if unreachable && from != target { Heard::Lost } else { Heard::Reply(from) }
}

/// The key of the probe an ICMPv6 `message` to `local` quotes, whether it is "unreachable" (else "time
/// exceeded") and the quoted sequence number, if it is either about our SYN.
fn icmp6_answer(message: &[u8], local: Ipv6Addr) -> Option<(Key, bool, Echo)> {
    let icmp_type = *message.first()?;
    if icmp_type != ICMP6_TIME_EXCEEDED && icmp_type != ICMP6_UNREACHABLE {
        return None;
    }
    let quoted = message.get(8..)?;
    if *quoted.first()? >> 4 != 6 || *quoted.get(6)? != PROTOCOL_TCP || ipv6_at(quoted, 8)? != local {
        return None;
    }
    let target = ipv6_at(quoted, 24)?;
    let syn = quoted.get(IPV6_HEADER..)?;
    let (local_port, target_port) = ports(syn)?;
    Some(((target.into(), target_port, local_port), icmp_type == ICMP6_UNREACHABLE, Echo::Quoted(number_at(syn, 4)?)))
}

/// The key of the probe a TCP `segment` from `from` (the target) answers, if it is a SYN-ACK or RST. An IPv6
/// listener doesn't see the protocol of what it gets, so the segment must also look like a TCP header: whole, with
/// a plausible header length — another protocol's packet between the same ports is unlikely to pass.
fn target_answer(segment: &[u8], from: IpAddr) -> Option<(Key, Heard, Echo)> {
    let (target_port, local_port) = ports(segment)?;
    let header = usize::from(*segment.get(12)? >> 4).checked_mul(4)?;
    if segment.len() < TCP_HEADER || header < TCP_HEADER {
        return None;
    }
    let flags = *segment.get(13)?;
    let syn_ack = TCP_SYN | TCP_ACK;
    let answers = flags & TCP_RST != 0 || flags & syn_ack == syn_ack;
    let echo = Echo::Acknowledged(number_at(segment, 8)?);
    answers.then_some(((from, target_port, local_port), Heard::Reply(from), echo))
}

/// Source and destination ports at the start of a TCP header.
fn ports(tcp: &[u8]) -> Option<(u16, u16)> {
    let port = |at: usize| -> Option<u16> { Some(u16::from_be_bytes([*tcp.get(at)?, *tcp.get(at.checked_add(1)?)?])) };
    Some((port(0)?, port(2)?))
}

/// The 32-bit number (sequence or acknowledgment) at byte `at` of a TCP header.
fn number_at(tcp: &[u8], at: usize) -> Option<u32> {
    let bytes: [u8; 4] = tcp.get(at..at.checked_add(4)?)?.try_into().ok()?;
    Some(u32::from_be_bytes(bytes))
}

/// The IPv6 address at byte `at` of `bytes`.
fn ipv6_at(bytes: &[u8], at: usize) -> Option<Ipv6Addr> {
    let octets: [u8; 16] = bytes.get(at..at.checked_add(16)?)?.try_into().ok()?;
    Some(Ipv6Addr::from(octets))
}

/// Protocol, source, destination and payload of an IPv4 packet (or of the start of one quoted in ICMP: its
/// payload may be cut short). A header shorter than the minimum, or a fragment after the first (no TCP or ICMP
/// header in it), is not one.
fn ipv4(packet: &[u8]) -> Option<(u8, Ipv4Addr, Ipv4Addr, &[u8])> {
    let first = *packet.first()?;
    if first >> 4 != 4 {
        return None;
    }
    let header = usize::from(first & 0x0F).checked_mul(4)?;
    let fragment_offset = u16::from_be_bytes([*packet.get(6)?, *packet.get(7)?]) & 0x1FFF;
    if header < 20 || fragment_offset != 0 {
        return None;
    }
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
    /// Our IPv6 address.
    const LOCAL6: Ipv6Addr = Ipv6Addr::new(0x2a00, 0x1e98, 0, 0, 0, 0, 0, 0x10);
    /// The traced IPv6 target.
    const TARGET6: Ipv6Addr = Ipv6Addr::new(0x2606, 0x4700, 0x4700, 0, 0, 0, 0, 0x1111);
    /// An IPv6 router on the way.
    const ROUTER6: Ipv6Addr = Ipv6Addr::new(0x2a02, 0x2d8, 0, 0, 0, 0, 0, 1);

    /// What a packet answers, without the echo.
    fn answer_of(packet: &[u8], from: IpAddr, local: IpAddr) -> Option<(Key, Heard)> {
        echoed_answer(packet, from, local).map(|(key, heard, _)| (key, heard))
    }

    /// A 20-byte IPv4 header for `protocol` from `source` to `destination`, followed by `payload`.
    fn ip(protocol: u8, source: Ipv4Addr, destination: Ipv4Addr, payload: &[u8]) -> Vec<u8> {
        let mut packet = vec![0x45, 0, 0, 0, 0, 0, 0, 0, 64, protocol, 0, 0];
        packet.extend(source.octets());
        packet.extend(destination.octets());
        packet.extend(payload);
        packet
    }

    /// A 40-byte IPv6 header for `next` (protocol) from `source` to `destination`, followed by `payload`.
    fn ip6(next: u8, source: Ipv6Addr, destination: Ipv6Addr, payload: &[u8]) -> Vec<u8> {
        let mut packet = vec![0x60, 0, 0, 0, 0, 0, next, 64];
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
        header.extend([0; 8]);
        // Header length: 5 words, no options.
        header.push(0x50);
        header.push(flags);
        header.extend([0; 6]);
        header
    }

    /// A router's ICMP message of `icmp_type` quoting our SYN from `local_port` to the target's port 443.
    fn icmp(icmp_type: u8, local_port: u16) -> Vec<u8> {
        icmp_from(ROUTER, icmp_type, local_port)
    }

    /// [`icmp`] sent by `sender`.
    fn icmp_from(sender: Ipv4Addr, icmp_type: u8, local_port: u16) -> Vec<u8> {
        let syn = ip(PROTOCOL_TCP, LOCAL, TARGET, tcp(local_port, 443, 0x02).get(..8).unwrap_or_default());
        let mut message = vec![icmp_type, 0, 0, 0, 0, 0, 0, 0];
        message.extend(syn);
        ip(PROTOCOL_ICMP, sender, LOCAL, &message)
    }

    /// A router's ICMPv6 message of `icmp_type` quoting our SYN from `source` and `local_port` to the target's
    /// port 443, as an IPv6 raw socket receives it (no IPv6 header of its own).
    fn icmp6(icmp_type: u8, source: Ipv6Addr, local_port: u16) -> Vec<u8> {
        let mut message = vec![icmp_type, 0, 0, 0, 0, 0, 0, 0];
        message.extend(ip6(PROTOCOL_TCP, source, TARGET6, &tcp(local_port, 443, 0x02)));
        message
    }

    #[test]
    fn the_local_address_comes_from_the_routing_table() {
        let local = |ip: IpAddr| local_address(ip).ok().map(|address| address.ip());
        assert_eq!(local(Ipv4Addr::LOCALHOST.into()), Some(Ipv4Addr::LOCALHOST.into()));
        assert_eq!(local(Ipv6Addr::LOCALHOST.into()), Some(Ipv6Addr::LOCALHOST.into()));
    }

    #[test]
    fn a_routers_ttl_expired_answers_the_quoted_probe() {
        let answer = |packet: &[u8]| answer_of(packet, ROUTER.into(), LOCAL.into());
        let reply = Heard::Reply(ROUTER.into());
        assert_eq!(answer(&icmp(ICMP_TIME_EXCEEDED, 50_001)), Some(((TARGET.into(), 443, 50_001), reply)));
    }

    #[test]
    fn unreachable_is_lost_from_a_router_and_an_answer_from_the_target() {
        let answer = |packet: &[u8]| answer_of(packet, ROUTER.into(), LOCAL.into());
        assert_eq!(answer(&icmp(ICMP_UNREACHABLE, 50_002)), Some(((TARGET.into(), 443, 50_002), Heard::Lost)));
        let from_target = icmp_from(TARGET, ICMP_UNREACHABLE, 50_009);
        assert_eq!(answer(&from_target), Some(((TARGET.into(), 443, 50_009), Heard::Reply(TARGET.into()))));
    }

    #[test]
    fn the_targets_syn_ack_or_rst_answers_the_probe() {
        let answer = |packet: &[u8]| answer_of(packet, TARGET.into(), LOCAL.into());
        let syn_ack = ip(PROTOCOL_TCP, TARGET, LOCAL, &tcp(443, 50_003, 0x12));
        assert_eq!(answer(&syn_ack), Some(((TARGET.into(), 443, 50_003), Heard::Reply(TARGET.into()))));
        let rst = ip(PROTOCOL_TCP, TARGET, LOCAL, &tcp(443, 50_004, 0x14));
        assert_eq!(answer(&rst), Some(((TARGET.into(), 443, 50_004), Heard::Reply(TARGET.into()))));
    }

    #[test]
    fn an_answer_echoes_the_sequence_number_of_our_syn() {
        // Our SYN leaves with sequence number 0x01020304; the listener sees it, on IPv4 and IPv6.
        let mut syn = tcp(50_030, 443, TCP_SYN);
        syn.splice(4..8, [1, 2, 3, 4]);
        let sent = Some((50_030, 443, 0x0102_0304));
        assert_eq!(own_syn(&ip(PROTOCOL_TCP, LOCAL, TARGET, &syn), LOCAL.into(), LOCAL.into()), sent);
        assert_eq!(own_syn(&syn, LOCAL6.into(), LOCAL6.into()), sent);
        // Not ours: the target's SYN-ACK, a segment from elsewhere.
        let answer = ip(PROTOCOL_TCP, TARGET, LOCAL, &tcp(443, 50_030, 0x12));
        assert_eq!(own_syn(&answer, TARGET.into(), LOCAL.into()), None);
        assert_eq!(own_syn(&syn, TARGET6.into(), LOCAL6.into()), None);
        // A router quotes it; the target acknowledges it plus one.
        let mut quoting = icmp(ICMP_TIME_EXCEEDED, 50_030);
        let length = quoting.len();
        quoting.splice(length.saturating_sub(4).., [1, 2, 3, 4]);
        let echo =
            |packet: &[u8], from: Ipv4Addr| echoed_answer(packet, from.into(), LOCAL.into()).map(|answer| answer.2);
        assert_eq!(echo(&quoting, ROUTER), Some(Echo::Quoted(0x0102_0304)));
        let mut syn_ack = tcp(443, 50_030, 0x12);
        syn_ack.splice(8..12, [1, 2, 3, 5]);
        assert_eq!(echo(&ip(PROTOCOL_TCP, TARGET, LOCAL, &syn_ack), TARGET), Some(Echo::Acknowledged(0x0102_0305)));
        assert!(Echo::Quoted(0x0102_0304).of(0x0102_0304) && !Echo::Quoted(7).of(0x0102_0304));
        assert!(Echo::Acknowledged(0x0102_0305).of(0x0102_0304) && !Echo::Acknowledged(0x0102_0304).of(0x0102_0304));
        assert!(Echo::Acknowledged(0).of(u32::MAX), "the number wraps");
    }

    #[test]
    fn other_traffic_is_ignored() {
        let answer = |packet: &[u8], local: Ipv4Addr| answer_of(packet, ROUTER.into(), local.into());
        // Our own outgoing SYN, seen by the listener too.
        assert_eq!(answer(&ip(PROTOCOL_TCP, LOCAL, TARGET, &tcp(50_005, 443, 0x02)), LOCAL), None);
        // An echo reply, and an ICMP message for another host.
        assert_eq!(answer(&ip(PROTOCOL_ICMP, ROUTER, LOCAL, &[0; 36]), LOCAL), None);
        assert_eq!(answer(&icmp(ICMP_TIME_EXCEEDED, 50_006), Ipv4Addr::new(192, 168, 1, 11)), None);
        // Not IPv4, cut short, a header length below the minimum, a later fragment.
        assert_eq!(answer(&[0x60; 40], LOCAL), None);
        assert_eq!(answer(&[0x45, 0, 0], LOCAL), None);
        let mut short = icmp(ICMP_TIME_EXCEEDED, 50_007);
        short[0] = 0x40;
        assert_eq!(answer(&short, LOCAL), None);
        let mut fragment = icmp(ICMP_TIME_EXCEEDED, 50_008);
        fragment[7] = 1;
        assert_eq!(answer(&fragment, LOCAL), None);
    }

    #[test]
    fn an_ipv6_routers_hop_limit_exceeded_answers_the_quoted_probe() {
        let answer = |message: &[u8], from: Ipv6Addr| answer_of(message, from.into(), LOCAL6.into());
        let expected = |port, heard| Some(((TARGET6.into(), 443, port), heard));
        let reply = Heard::Reply(ROUTER6.into());
        assert_eq!(answer(&icmp6(ICMP6_TIME_EXCEEDED, LOCAL6, 50_011), ROUTER6), expected(50_011, reply));
        assert_eq!(answer(&icmp6(ICMP6_UNREACHABLE, LOCAL6, 50_012), ROUTER6), expected(50_012, Heard::Lost));
        let from_target = Heard::Reply(TARGET6.into());
        assert_eq!(answer(&icmp6(ICMP6_UNREACHABLE, LOCAL6, 50_019), TARGET6), expected(50_019, from_target));
    }

    #[test]
    fn the_ipv6_targets_syn_ack_or_rst_answers_the_probe() {
        let answer = |segment: &[u8]| answer_of(segment, TARGET6.into(), LOCAL6.into());
        let reply = Heard::Reply(TARGET6.into());
        assert_eq!(answer(&tcp(443, 50_013, 0x12)), Some(((TARGET6.into(), 443, 50_013), reply)));
        assert_eq!(answer(&tcp(443, 50_014, 0x14)), Some(((TARGET6.into(), 443, 50_014), reply)));
    }

    #[test]
    fn other_ipv6_traffic_is_ignored() {
        // Our own outgoing SYN.
        assert_eq!(answer_of(&tcp(50_015, 443, 0x02), LOCAL6.into(), LOCAL6.into()), None);
        // An ICMPv6 error about another host's packet, a neighbor solicitation, a bare SYN, cut short.
        let other = Ipv6Addr::new(0x2a00, 0x1e98, 0, 0, 0, 0, 0, 0x11);
        assert_eq!(answer_of(&icmp6(ICMP6_TIME_EXCEEDED, other, 50_016), ROUTER6.into(), LOCAL6.into()), None);
        assert_eq!(answer_of(&[0x87, 0, 0, 0, 0, 0, 0, 0], ROUTER6.into(), LOCAL6.into()), None);
        assert_eq!(answer_of(&tcp(443, 50_017, 0x02), TARGET6.into(), LOCAL6.into()), None);
        assert_eq!(answer_of(&[1, 0xbb], TARGET6.into(), LOCAL6.into()), None);
        // Not a TCP header: a bare ACK, a header length below the minimum, a segment cut short.
        assert_eq!(answer_of(&tcp(443, 50_020, 0x10), TARGET6.into(), LOCAL6.into()), None);
        let mut short_header = tcp(443, 50_021, 0x12);
        short_header[12] = 0x40;
        assert_eq!(answer_of(&short_header, TARGET6.into(), LOCAL6.into()), None);
        let cut = tcp(443, 50_022, 0x12);
        assert_eq!(answer_of(cut.get(..19).unwrap_or_default(), TARGET6.into(), LOCAL6.into()), None);
        // An IPv4 sender on an IPv6 listener.
        assert_eq!(answer_of(&tcp(443, 50_018, 0x12), TARGET.into(), LOCAL6.into()), None);
    }
}

/// Probes real servers; run by hand with administrator rights: `cargo test -- --ignored live`.
#[cfg(test)]
mod live {
    use super::*;

    /// one.one.one.one answers on `target`:443 a few hops away, and the first router answers hop 1.
    fn a_web_server_answers(target: IpAddr) {
        let prober = TcpProber::default();
        let outcome = prober.probe(target, 443, 64, Duration::from_secs(3));
        assert!(matches!(outcome, Ok(ProbeOutcome::Reply { from, .. }) if from == target), "{outcome:?}");
        let first = prober.probe(target, 443, 1, Duration::from_secs(3));
        assert!(matches!(first, Ok(ProbeOutcome::Reply { .. })), "the first router answers: {first:?}");
    }

    #[test]
    #[ignore = "needs administrator rights, the network, and a firewall that lets the test program connect"]
    fn a_web_server_answers_a_tcp_probe() {
        a_web_server_answers(Ipv4Addr::new(1, 1, 1, 1).into());
    }

    #[test]
    #[ignore = "needs administrator rights, the network, a firewall that lets the test program connect, and 12 s"]
    fn an_idle_listener_closes_and_a_new_probe_opens_another() {
        let prober = TcpProber::default();
        let target = Ipv4Addr::new(1, 1, 1, 1).into();
        let open = |prober: &TcpProber| lock(&prober.listeners).values().any(|listener| lock(&listener.waiting).open);
        assert!(matches!(prober.probe(target, 443, 64, Duration::from_secs(3)), Ok(ProbeOutcome::Reply { .. })));
        assert!(open(&prober));
        thread::sleep(IDLE_CLOSE + IDLE_CHECK * 2);
        assert!(!open(&prober), "closed while idle");
        assert!(matches!(prober.probe(target, 443, 64, Duration::from_secs(3)), Ok(ProbeOutcome::Reply { .. })));
        assert!(open(&prober));
    }

    #[test]
    #[ignore = "needs administrator rights, the network, a firewall that lets the test program connect, and 5 s"]
    fn lost_probes_of_a_silent_target_are_not_hidden_answers() {
        let prober = TcpProber::default();
        // TEST-NET-1: routed nowhere, nobody answers.
        let target = Ipv4Addr::new(192, 0, 2, 1).into();
        for _ in 0..5 {
            let outcome = prober.probe(target, 443, 64, Duration::from_secs(1));
            assert!(matches!(outcome, Ok(ProbeOutcome::Lost)), "{outcome:?}");
        }
        assert!(!prober.answers_blocked(target), "silence isn't a firewall");
    }

    #[test]
    #[ignore = "needs the network"]
    fn only_an_answered_connect_is_connected() {
        let connecting = |target: SocketAddr| {
            let socket = Socket::new(Domain::IPV4, Type::STREAM, Some(Protocol::TCP)).expect("a socket");
            socket.set_nonblocking(true).expect("non-blocking");
            socket.connect(&target.into()).ok();
            thread::sleep(Duration::from_secs(1));
            socket
        };
        assert!(is_connected(&connecting(SocketAddr::from(([1, 1, 1, 1], 443)))));
        assert!(!is_connected(&connecting(SocketAddr::from(([192, 0, 2, 1], 443)))));
    }

    #[test]
    #[ignore = "needs administrator rights, IPv6 internet, and a firewall that lets the test program connect"]
    fn a_web_server_answers_an_ipv6_tcp_probe() {
        a_web_server_answers(Ipv6Addr::new(0x2606, 0x4700, 0x4700, 0, 0, 0, 0, 0x1111).into());
    }
}
