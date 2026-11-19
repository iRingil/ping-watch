//! Windows Firewall against the answers to TCP probes. The answers come to a raw socket, which the firewall treats
//! as unasked-for traffic: without a rule it drops the routers' ICMP errors ("TTL expired", "unreachable") and
//! hides the target's SYN-ACK / RST, and no error tells the program so.
//!
//! What is done about it, only once probes do go unanswered: [`AnswerWatch`] keeps, per target, what the probes
//! show; the filtering platform's log of dropped packets ([`hop_answers_dropped`]) proves that the hops' answers
//! were dropped on this computer; [`windows_firewall_on`] tells whether Windows Firewall is the one to change;
//! [`allow_answers`] adds the rules that let the answers in, when the user asks for them.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::os::windows::ffi::OsStrExt as _;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use windows::Win32::Foundation::{HANDLE, VARIANT_TRUE, WIN32_ERROR};
use windows::Win32::NetworkManagement::WindowsFilteringPlatform::{
    FWP_IP_VERSION_V4, FWP_IP_VERSION_V6, FWPM_LAYER_INBOUND_ICMP_ERROR_V4, FWPM_LAYER_INBOUND_ICMP_ERROR_V6,
    FWPM_LAYER0, FWPM_NET_EVENT_FLAG_IP_PROTOCOL_SET, FWPM_NET_EVENT_FLAG_IP_VERSION_SET,
    FWPM_NET_EVENT_FLAG_REMOTE_ADDR_SET, FWPM_NET_EVENT_TYPE_CLASSIFY_DROP, FWPM_NET_EVENT0, FwpmEngineClose0,
    FwpmEngineOpen0, FwpmFreeMemory0, FwpmLayerGetByKey0, FwpmNetEventCreateEnumHandle0,
    FwpmNetEventDestroyEnumHandle0, FwpmNetEventEnum0,
};
use windows::Win32::NetworkManagement::WindowsFirewall::{
    INetFwPolicy2, INetFwRule, INetFwRules, NET_FW_ACTION_ALLOW, NET_FW_PROFILE2_ALL, NET_FW_PROFILE2_DOMAIN,
    NET_FW_PROFILE2_PRIVATE, NET_FW_PROFILE2_PUBLIC, NET_FW_RULE_DIR_IN, NetFwPolicy2, NetFwRule,
};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
};
use windows::Win32::System::Rpc::RPC_C_AUTHN_WINNT;
use windows::core::{BSTR, GUID, PCWSTR};

use crate::i18n::n;

/// Hop answers count as missing once no router has answered a target's probes for this long.
const HOPS_SILENT: Duration = Duration::from_secs(10);
/// The log of dropped packets is read at most this often per target, and only while answers are missing.
const CHECK_EVERY: Duration = Duration::from_secs(5);
/// How far back the first reading of the log looks; later readings continue where the last one ended.
const FIRST_LOOK: Duration = Duration::from_secs(10);
/// A dropped hop answer counts for this long: probes may be a minute apart, and the next drop renews it.
const EVIDENCE_KEPT: Duration = Duration::from_secs(150);
/// The target's answer must stay hidden this many probes in a row: one may just be missed by a busy listener.
const HIDDEN_ANSWERS: u8 = 3;
/// Events asked from the log at a time; it holds a few hundred.
const LOG_BATCH: u32 = 512;
/// IP protocol number of TCP.
const PROTOCOL_TCP: u8 = 6;
/// 100 ns ticks between 1601 (the start of Windows file times) and 1970 (the Unix epoch).
const FILETIME_UNIX_EPOCH: u64 = 116_444_736_000_000_000;
/// Group of the rules of Ping Watch in Windows Firewall.
const RULE_GROUP: &str = "Ping Watch";
/// The rules that let the answers to TCP probes in.
const RULES: [Rule; 3] = [
    Rule {
        name: "Ping Watch - Hop answers (ICMPv4-In)",
        description: "Inbound rule for Ping Watch to allow the routers' answers to its TCP probes [ICMPv4 3, 11]",
        protocol: 1,
        icmp: Some("3:*,11:*"),
        own_program: false,
    },
    Rule {
        name: "Ping Watch - Hop answers (ICMPv6-In)",
        description: "Inbound rule for Ping Watch to allow the routers' answers to its TCP probes [ICMPv6 1, 3]",
        protocol: 58,
        icmp: Some("1:*,3:*"),
        own_program: false,
    },
    Rule {
        name: "Ping Watch - Target answers (TCP-In)",
        description: "Inbound rule for Ping Watch to allow the targets' answers to its TCP probes [TCP]",
        protocol: 6,
        icmp: None,
        own_program: true,
    },
];

/// Who keeps the answers to TCP probes from Ping Watch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Blocker {
    /// Windows Firewall, which is on: [`allow_answers`] helps.
    WindowsFirewall,
    /// Another firewall on this computer; its rules are the user's to change.
    Other,
}

impl Blocker {
    /// The problem as shown to the user, translated where it is shown.
    pub fn message(self) -> &'static str {
        match self {
            Self::WindowsFirewall => n!("Windows Firewall blocks the answers to TCP probes"),
            Self::Other => n!("A firewall on this computer blocks the answers to TCP probes"),
        }
    }
}

/// What the TCP probes of one target show about a firewall on this computer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AnswerWatch {
    /// When a router last answered a probe.
    router_heard: Option<Instant>,
    /// When the log of dropped packets was last read for this target.
    checked: Option<Instant>,
    /// Up to when the log was read: its older events are counted already (or forgiven, after new rules).
    log_read_to: Option<SystemTime>,
    /// When the log last showed a hop answer dropped.
    dropped_at: Option<Instant>,
    /// Probes in a row that the target answered (the connection was made) while the listener heard nothing.
    target_hidden: u8,
    /// Whether Windows Firewall was on when a problem was last found.
    windows: bool,
}

impl AnswerWatch {
    /// A watch that takes the log's events up to `now` as dealt with: rules were just added.
    pub fn after_rules(now: SystemTime) -> Self {
        Self { log_read_to: Some(now), ..Self::default() }
    }

    /// A router answered a probe: hop answers get through.
    pub fn router_heard(&mut self, now: Instant) {
        self.router_heard = Some(now);
        self.dropped_at = None;
    }

    /// The listener heard the target's answer.
    pub fn target_heard(&mut self) {
        self.target_hidden = 0;
    }

    /// The target answered a probe, but the listener didn't hear it.
    pub fn target_hidden(&mut self) {
        self.target_hidden = self.target_hidden.saturating_add(1);
    }

    /// Whether the log of dropped packets is to be read now, after a lost probe: answers are missing (no router
    /// heard for a while, or the target's answers stay hidden) and the last reading isn't recent. Marks it read.
    pub fn check_due(&mut self, now: Instant) -> bool {
        let silent = self.router_heard.is_none_or(|at| now.saturating_duration_since(at) >= HOPS_SILENT);
        let rested = self.checked.is_none_or(|at| now.saturating_duration_since(at) >= CHECK_EVERY);
        let due = (silent || self.target_is_hidden()) && rested;
        if due {
            self.checked = Some(now);
        }
        due
    }

    /// From when the log is to be read now, at `now`: where the last reading ended, or a little back the first
    /// time. The next reading starts at `now`.
    pub fn log_since(&mut self, now: SystemTime) -> SystemTime {
        let since = self.log_read_to.unwrap_or_else(|| now.checked_sub(FIRST_LOOK).unwrap_or(UNIX_EPOCH));
        self.log_read_to = Some(now);
        since
    }

    /// Takes a reading of the log: whether it showed a hop answer `dropped`.
    pub fn log_read(&mut self, dropped: bool, now: Instant) {
        if dropped {
            self.dropped_at = Some(now);
        }
    }

    /// Whether answers are kept from Ping Watch as of `now`, by whoever.
    pub fn blocked(&self, now: Instant) -> bool {
        let dropped = self.dropped_at.is_some_and(|at| now.saturating_duration_since(at) < EVIDENCE_KEPT);
        dropped || self.target_is_hidden()
    }

    /// Notes whether Windows Firewall is on, asked when a problem is found.
    pub fn set_windows(&mut self, on: bool) {
        self.windows = on;
    }

    /// Who keeps answers from Ping Watch as of `now`, if anyone.
    pub fn blocker(&self, now: Instant) -> Option<Blocker> {
        self.blocked(now).then_some(if self.windows { Blocker::WindowsFirewall } else { Blocker::Other })
    }

    /// Whether the target's answers have stayed hidden long enough to count.
    fn target_is_hidden(&self) -> bool {
        self.target_hidden >= HIDDEN_ANSWERS
    }
}

/// A packet the filtering platform dropped, as its log tells it. For an ICMP error the log describes the packet
/// the error is about — our SYN — so `remote` is the probe's target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Dropped {
    /// When, in 100 ns ticks since 1601 (a Windows file time).
    at: u64,
    /// IP protocol, if logged.
    protocol: Option<u8>,
    /// The remote address, if logged.
    remote: Option<IpAddr>,
    /// Run-time ID of the filtering layer that dropped it.
    layer: u16,
}

impl Dropped {
    /// Whether this is a router's answer to a TCP probe of `target`, dropped at `layer` (that of incoming ICMP
    /// errors) at `since` or later.
    fn hides_hop_of(&self, target: IpAddr, layer: u16, since: u64) -> bool {
        self.layer == layer && self.protocol == Some(PROTOCOL_TCP) && self.remote == Some(target) && self.at >= since
    }
}

/// `time` as a Windows file time: 100 ns ticks since 1601.
fn filetime(time: SystemTime) -> u64 {
    let since_unix = time.duration_since(UNIX_EPOCH).unwrap_or_default();
    let ticks = u64::try_from(since_unix.as_nanos() / 100).unwrap_or(u64::MAX);
    ticks.saturating_add(FILETIME_UNIX_EPOCH)
}

/// Whether the filtering platform's log shows a router's answer to a TCP probe of `target` dropped at `since` or
/// later. Windows keeps the log by default (the last few hundred drops); reading it needs administrator rights,
/// which TCP probes have.
///
/// # Errors
///
/// Windows' error if the filtering engine can't be opened or asked (no rights, the service is off).
pub fn hop_answers_dropped(target: IpAddr, since: SystemTime) -> windows::core::Result<bool> {
    let engine = Engine::open()?;
    let layer = engine.layer_id(match target {
        IpAddr::V4(_) => &FWPM_LAYER_INBOUND_ICMP_ERROR_V4,
        IpAddr::V6(_) => &FWPM_LAYER_INBOUND_ICMP_ERROR_V6,
    })?;
    let since = filetime(since);
    Ok(engine.drops()?.iter().any(|dropped| dropped.hides_hop_of(target, layer, since)))
}

/// A session with the filtering engine, closed on drop.
#[derive(Debug)]
struct Engine(HANDLE);

impl Engine {
    /// Opens a session with this computer's filtering engine.
    fn open() -> windows::core::Result<Self> {
        let mut handle = HANDLE::default();
        // SAFETY: a null server name is the local computer; no identity and no session options are given; `handle`
        // is a writable handle the function fills on success.
        let code = unsafe { FwpmEngineOpen0(PCWSTR::null(), RPC_C_AUTHN_WINNT, None, None, &raw mut handle) };
        WIN32_ERROR(code).ok()?;
        Ok(Self(handle))
    }

    /// Run-time ID of the filtering layer `key`, as the log names layers.
    fn layer_id(&self, key: &GUID) -> windows::core::Result<u16> {
        let mut layer: *mut FWPM_LAYER0 = std::ptr::null_mut();
        // SAFETY: the session is open; `key` is a GUID that lives across the call; `layer` is a writable pointer
        // the function sets to memory it allocates.
        let code = unsafe { FwpmLayerGetByKey0(self.0, key, &raw mut layer) };
        WIN32_ERROR(code).ok()?;
        // SAFETY: on success `layer` is null or points to a `FWPM_LAYER0` valid until freed below.
        let id = unsafe { layer.as_ref() }.map(|layer| layer.layerId);
        // SAFETY: `layer` was allocated by the call above (or is null, which the function accepts) and is not
        // used after that.
        unsafe { FwpmFreeMemory0((&raw mut layer).cast()) };
        id.ok_or_else(windows::core::Error::empty)
    }

    /// The dropped packets in the log.
    fn drops(&self) -> windows::core::Result<Vec<Dropped>> {
        let mut enumeration = HANDLE::default();
        // SAFETY: the session is open; no template restricts the events; `enumeration` is a writable handle the
        // function fills on success.
        let code = unsafe { FwpmNetEventCreateEnumHandle0(self.0, None, &raw mut enumeration) };
        WIN32_ERROR(code).ok()?;
        let mut found = Vec::new();
        let result = loop {
            let mut entries: *mut *mut FWPM_NET_EVENT0 = std::ptr::null_mut();
            let mut count = 0_u32;
            // SAFETY: the session and the enumeration are open; `entries` and `count` are writable and filled by
            // the function, which allocates the array.
            let code = unsafe { FwpmNetEventEnum0(self.0, enumeration, LOG_BATCH, &raw mut entries, &raw mut count) };
            if code == 0 && !entries.is_null() {
                // SAFETY: on success `entries` points to `count` event pointers, valid until freed below.
                let events = unsafe { std::slice::from_raw_parts(entries, usize::try_from(count).unwrap_or(0)) };
                // SAFETY: each pointer of the array is null or points to an event valid until freed below.
                found.extend(events.iter().filter_map(|event| dropped(unsafe { event.as_ref() }?)));
            }
            // SAFETY: `entries` was allocated by the call above (or is null, which the function accepts); the
            // events it held are not used after that.
            unsafe { FwpmFreeMemory0((&raw mut entries).cast()) };
            if code != 0 {
                break WIN32_ERROR(code).ok();
            }
            if count < LOG_BATCH {
                break Ok(());
            }
        };
        // SAFETY: the enumeration was opened above in this session and is not used after that.
        unsafe { FwpmNetEventDestroyEnumHandle0(self.0, enumeration) };
        result.map(|()| found)
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        // SAFETY: the handle was opened by `FwpmEngineOpen0` and is not used after that.
        unsafe { FwpmEngineClose0(self.0) };
    }
}

/// What the log's `event` says, if it is a packet dropped by a filter.
fn dropped(event: &FWPM_NET_EVENT0) -> Option<Dropped> {
    if event.r#type != FWPM_NET_EVENT_TYPE_CLASSIFY_DROP {
        return None;
    }
    // SAFETY: for an event of this type the union holds `classifyDrop`, null or a pointer valid as long as the
    // event.
    let drop = unsafe { event.Anonymous.classifyDrop.as_ref() }?;
    let header = &event.header;
    let has = |flag: u32| header.flags & flag != 0;
    let remote = if !has(FWPM_NET_EVENT_FLAG_REMOTE_ADDR_SET) || !has(FWPM_NET_EVENT_FLAG_IP_VERSION_SET) {
        None
    } else if header.ipVersion == FWP_IP_VERSION_V4 {
        // SAFETY: for an IPv4 event the union holds the address as a `u32` (host byte order); any bits are valid.
        Some(IpAddr::V4(Ipv4Addr::from(unsafe { header.Anonymous2.remoteAddrV4 })))
    } else if header.ipVersion == FWP_IP_VERSION_V6 {
        // SAFETY: for an IPv6 event the union holds the address as 16 bytes; any bits are valid.
        Some(IpAddr::V6(Ipv6Addr::from(unsafe { header.Anonymous2.remoteAddrV6.byteArray16 })))
    } else {
        None
    };
    let at = u64::from(header.timeStamp.dwHighDateTime)
        .checked_shl(32)
        .map(|high| high | u64::from(header.timeStamp.dwLowDateTime))?;
    Some(Dropped {
        at,
        protocol: has(FWPM_NET_EVENT_FLAG_IP_PROTOCOL_SET).then_some(header.ipProtocol),
        remote,
        layer: drop.layerId,
    })
}

/// Keeps COM initialized for this thread as long as the value lives.
#[derive(Debug)]
struct Com {
    /// Whether this value initialized COM, and so uninitializes it.
    initialized: bool,
}

impl Com {
    /// Initializes COM for this thread, unless the thread has it already in another mode (the window's thread):
    /// then that one is used.
    fn init() -> Self {
        // SAFETY: plain initialization of COM for the calling thread, paired with `CoUninitialize` on drop.
        let result = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        Self { initialized: result.is_ok() }
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        if self.initialized {
            // SAFETY: pairs with the successful `CoInitializeEx` of `init` on this thread.
            unsafe { CoUninitialize() };
        }
    }
}

/// Windows Firewall's policy object.
fn policy() -> windows::core::Result<INetFwPolicy2> {
    // SAFETY: COM is initialized on this thread by the callers' `Com`; the class and the interface belong
    // together.
    unsafe { CoCreateInstance(&NetFwPolicy2, None, CLSCTX_INPROC_SERVER) }
}

/// Whether Windows Firewall is on for a network profile in use now.
///
/// # Errors
///
/// Windows' error if the firewall can't be asked (its service is off or removed).
pub fn windows_firewall_on() -> windows::core::Result<bool> {
    let _com = Com::init();
    let policy = policy()?;
    // SAFETY: a method call on a live COM object.
    let active = unsafe { policy.CurrentProfileTypes() }?;
    for profile in [NET_FW_PROFILE2_DOMAIN, NET_FW_PROFILE2_PRIVATE, NET_FW_PROFILE2_PUBLIC] {
        // SAFETY: a method call on a live COM object.
        if active & profile.0 != 0 && unsafe { policy.get_FirewallEnabled(profile) }?.as_bool() {
            return Ok(true);
        }
    }
    Ok(false)
}

/// A Windows Firewall rule of Ping Watch: incoming, allowing, on every network profile.
#[derive(Debug, Clone, Copy)]
struct Rule {
    /// Name, by which the rule is found again.
    name: &'static str,
    /// What it is for, shown by Windows.
    description: &'static str,
    /// IP protocol number.
    protocol: i32,
    /// ICMP types and codes (`type:code`, `*` for any code), for an ICMP protocol.
    icmp: Option<&'static str>,
    /// Whether it is for the executable of Ping Watch only. The ICMP rules aren't: Windows doesn't apply a program's
    /// rule to ICMP errors (checked: such a rule lets nothing in).
    own_program: bool,
}

/// Error of [`allow_answers`].
#[derive(Debug, thiserror::Error)]
pub enum AllowError {
    /// Ping Watch can't tell where its executable is.
    #[error("{0}")]
    Program(#[from] std::io::Error),
    /// Windows Firewall refused or can't be reached.
    #[error("{0}")]
    Windows(#[from] windows::core::Error),
}

/// Adds the Windows Firewall rules that let the answers to TCP probes in, replacing those of an earlier run (a
/// portable copy may have moved): the routers' ICMP errors for every program, incoming TCP for Ping Watch. Needs
/// administrator rights.
///
/// # Errors
///
/// [`AllowError`] if Windows Firewall refuses (no rights, its service is off).
pub fn allow_answers() -> Result<(), AllowError> {
    let program = std::env::current_exe()?;
    let _com = Com::init();
    // SAFETY: a method call on a live COM object.
    let rules = unsafe { policy()?.Rules() }?;
    RULES.iter().try_for_each(|rule| rule.add(&rules, &program))?;
    Ok(())
}

impl Rule {
    /// Puts the rule into `rules`, in place of one of the same name; `program` is the executable of Ping Watch.
    fn add(&self, rules: &INetFwRules, program: &Path) -> windows::core::Result<()> {
        let name = BSTR::from(self.name);
        // SAFETY: COM is initialized on this thread by the caller; the class and the interface belong together.
        let rule: INetFwRule = unsafe { CoCreateInstance(&NetFwRule, None, CLSCTX_INPROC_SERVER) }?;
        // SAFETY: method calls on live COM objects with strings that live across each call. The protocol is set
        // before what depends on it (the ICMP types).
        unsafe {
            rule.SetName(&name)?;
            rule.SetDescription(&BSTR::from(self.description))?;
            rule.SetGrouping(&BSTR::from(RULE_GROUP))?;
            rule.SetProtocol(self.protocol)?;
            if let Some(icmp) = self.icmp {
                rule.SetIcmpTypesAndCodes(&BSTR::from(icmp))?;
            }
            if self.own_program {
                let path: Vec<u16> = program.as_os_str().encode_wide().collect();
                rule.SetApplicationName(&BSTR::from_wide(&path))?;
            }
            rule.SetDirection(NET_FW_RULE_DIR_IN)?;
            rule.SetAction(NET_FW_ACTION_ALLOW)?;
            rule.SetProfiles(NET_FW_PROFILE2_ALL.0)?;
            rule.SetEnabled(VARIANT_TRUE)?;
            // No rule of this name is fine.
            rules.Remove(&name).ok();
            rules.Add(&rule)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The traced target.
    const TARGET: IpAddr = IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1));

    #[test]
    fn a_dropped_hop_answer_is_told_from_other_drops() {
        let drop = Dropped { at: 1_000, protocol: Some(PROTOCOL_TCP), remote: Some(TARGET), layer: 28 };
        assert!(drop.hides_hop_of(TARGET, 28, 1_000));
        assert!(!drop.hides_hop_of(TARGET, 28, 1_001), "older than asked");
        assert!(!drop.hides_hop_of(TARGET, 44, 1_000), "another layer");
        assert!(!drop.hides_hop_of(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)), 28, 1_000), "another target");
        assert!(!Dropped { protocol: Some(17), ..drop }.hides_hop_of(TARGET, 28, 1_000), "not about a TCP packet");
        assert!(!Dropped { remote: None, ..drop }.hides_hop_of(TARGET, 28, 1_000));
    }

    #[test]
    fn file_times_count_from_1601() {
        assert_eq!(filetime(UNIX_EPOCH), FILETIME_UNIX_EPOCH);
        let second = UNIX_EPOCH.checked_add(Duration::from_secs(1)).unwrap_or(UNIX_EPOCH);
        assert_eq!(filetime(second), FILETIME_UNIX_EPOCH.saturating_add(10_000_000));
    }

    #[test]
    fn the_log_is_read_only_while_answers_are_missing_and_not_too_often() {
        let start = Instant::now();
        let after = |seconds| start.checked_add(Duration::from_secs(seconds)).unwrap_or(start);
        let mut watch = AnswerWatch::default();
        assert!(watch.check_due(start), "no router heard yet");
        assert!(!watch.check_due(after(1)), "just read");
        assert!(watch.check_due(after(5)));
        watch.router_heard(after(6));
        assert!(!watch.check_due(after(12)), "a router answered lately");
        assert!(watch.check_due(after(16)), "the routers went silent");
        // The target's hidden answers ask for a reading even while routers answer.
        watch.router_heard(after(30));
        (0..HIDDEN_ANSWERS).for_each(|_| watch.target_hidden());
        assert!(watch.check_due(after(31)));
    }

    #[test]
    fn dropped_hop_answers_block_until_a_router_is_heard() {
        let start = Instant::now();
        let after = |seconds| start.checked_add(Duration::from_secs(seconds)).unwrap_or(start);
        let mut watch = AnswerWatch::default();
        watch.log_read(false, start);
        assert_eq!(watch.blocker(start), None);
        watch.log_read(true, start);
        assert_eq!(watch.blocker(after(1)), Some(Blocker::Other));
        watch.set_windows(true);
        assert_eq!(watch.blocker(after(60)), Some(Blocker::WindowsFirewall));
        // A reading without drops doesn't clear it (probes may be far apart); time and a router's answer do.
        watch.log_read(false, after(61));
        assert!(watch.blocked(after(62)));
        assert!(!watch.blocked(after(151)), "the evidence got old");
        watch.log_read(true, after(152));
        watch.router_heard(after(153));
        assert!(!watch.blocked(after(153)));
    }

    #[test]
    fn a_hidden_target_answer_counts_after_a_few_in_a_row() {
        let now = Instant::now();
        let mut watch = AnswerWatch::default();
        watch.target_hidden();
        watch.target_hidden();
        assert!(!watch.blocked(now), "may be a busy listener");
        watch.target_hidden();
        assert!(watch.blocked(now));
        watch.target_heard();
        assert!(!watch.blocked(now));
    }

    #[test]
    fn the_log_is_read_on_from_where_the_last_reading_ended() {
        let now = SystemTime::now();
        let later = now.checked_add(Duration::from_secs(5)).unwrap_or(now);
        let mut watch = AnswerWatch::default();
        assert_eq!(watch.log_since(now), now.checked_sub(FIRST_LOOK).unwrap_or(now));
        assert_eq!(watch.log_since(later), now);
        // After new rules the drops before them don't count.
        assert_eq!(AnswerWatch::after_rules(later).log_since(later), later);
    }
}

/// Reads this computer's real firewall state; run by hand with administrator rights:
/// `cargo test -- --ignored firewall::live --nocapture`.
#[cfg(test)]
mod live {
    use super::*;

    #[test]
    #[ignore = "needs administrator rights and the filtering engine; prints what it finds"]
    fn the_log_and_the_firewall_state_can_be_read() {
        let engine = Engine::open().expect("the filtering engine opens");
        let layer = engine.layer_id(&FWPM_LAYER_INBOUND_ICMP_ERROR_V4).expect("the ICMP error layer is known");
        let drops = engine.drops().expect("the log is read");
        let at_layer: Vec<&Dropped> = drops.iter().filter(|dropped| dropped.layer == layer).collect();
        println!(
            "layer {layer}; {} drops, {} of ICMP errors; last: {:?}",
            drops.len(),
            at_layer.len(),
            at_layer.last()
        );
        println!("Windows Firewall on: {:?}", windows_firewall_on());
    }
}
