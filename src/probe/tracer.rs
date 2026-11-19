//! Continuous tracing of one target: one worker thread per TTL that may lie on the route (every TTL until the
//! target answers, then those up to a few past it) probes its hop every interval (by ICMP echo or a
//! TCP SYN) and records the outcome into a shared [`Route`] (the UI reads snapshots of it) and, optionally, sends it
//! to the history. The trace can be paused and its interval changed while it runs.

use std::net::IpAddr;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use crate::i18n::{Message, n};
use crate::probe::icmp::{IcmpError, Pinger};
use crate::probe::route::{MAX_TTL, ProbeOutcome, Route, RouteState, millis};
use crate::probe::tcp::TcpProber;
use crate::store::history::{HistorySender, unix_ms};
use crate::sync::lock;

/// What the hops are probed with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Probe {
    /// ICMP echo, as `ping` and `tracert`; needs no rights.
    #[default]
    Icmp,
    /// A TCP SYN to this port, as a connection would start: passes firewalls that drop ICMP, and reaches servers
    /// that don't answer ping. Needs administrator rights.
    Tcp(u16),
}

impl Probe {
    /// Port of a TCP probe until another is chosen: HTTPS, open on every web server.
    pub const DEFAULT_TCP_PORT: u16 = 443;

    /// Text stored for this probe.
    pub fn key(self) -> String {
        match self {
            Self::Icmp => "icmp".to_owned(),
            Self::Tcp(port) => format!("tcp:{port}"),
        }
    }

    /// The probe stored as `key`; `None` if unknown.
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "icmp" => Some(Self::Icmp),
            key => key.strip_prefix("tcp:")?.parse().ok().filter(|port| *port != 0).map(Self::Tcp),
        }
    }
}

/// Tracing parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TraceConfig {
    /// Initial time between probes of the same hop.
    pub interval: Duration,
    /// How long to wait for an answer before counting the probe as lost.
    pub timeout: Duration,
    /// Largest TTL probed.
    pub max_ttl: u8,
    /// What the hops are probed with.
    pub probe: Probe,
}

impl Default for TraceConfig {
    fn default() -> Self {
        Self { interval: Duration::from_secs(1), timeout: Duration::from_secs(1), max_ttl: MAX_TTL, probe: Probe::Icmp }
    }
}

/// Error starting a trace.
#[derive(Debug, thiserror::Error)]
pub enum TraceError {
    /// The ICMP API is unavailable.
    #[error(transparent)]
    Icmp(#[from] IcmpError),
    /// A worker thread couldn't be started.
    #[error("cannot start probe thread: {0}")]
    Spawn(#[from] std::io::Error),
}

impl TraceError {
    /// The error as shown to the user.
    pub fn message(&self) -> Message {
        match self {
            Self::Icmp(error) => error.message(),
            Self::Spawn(error) => Message::new(n!("Cannot start a probe thread: {error}")).with("error", error),
        }
    }
}

/// What a worker probes its hop with.
#[derive(Debug)]
enum Prober {
    /// Its own ICMP handle.
    Icmp(Pinger),
    /// The app's TCP prober, for this port of the target.
    Tcp {
        /// Shared by all traces.
        prober: Arc<TcpProber>,
        /// The target's port.
        port: u16,
    },
}

impl Prober {
    /// The prober of `probe` for `target`.
    fn new(probe: Probe, target: IpAddr, tcp: &Arc<TcpProber>) -> Result<Self, TraceError> {
        match probe {
            Probe::Icmp => Ok(Self::Icmp(Pinger::for_target(target)?)),
            Probe::Tcp(port) => Ok(Self::Tcp { prober: Arc::clone(tcp), port }),
        }
    }

    /// Probes hop `ttl` towards `target`, waiting up to `timeout`; the error is shown to the user.
    fn probe(&self, target: IpAddr, ttl: u8, timeout: Duration) -> Result<ProbeOutcome, Message> {
        match self {
            Self::Icmp(pinger) => pinger.probe(target, ttl, timeout).map_err(|error| error.message()),
            Self::Tcp { prober, port } => prober.probe(target, *port, ttl, timeout).map_err(|error| error.message()),
        }
    }
}

/// Settings the workers follow, changeable while they run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Control {
    /// Workers exit as soon as they see this.
    stop: bool,
    /// Workers don't probe while this is set.
    paused: bool,
    /// Time between probes of the same hop.
    interval: Duration,
}

/// State shared between the [`Tracer`] and its workers.
#[derive(Debug)]
struct Shared {
    /// Route being built.
    route: Mutex<RouteState>,
    /// Current settings.
    control: Mutex<Control>,
    /// Wakes waiting workers when `control` changes.
    wake: Condvar,
    /// Error of the latest failed probe, cleared by the next probe that could be sent.
    last_error: Mutex<Option<Message>>,
    /// Which TTLs have a worker, by index `ttl - 1`. Locked before `route` where both are.
    workers: Mutex<Vec<bool>>,
    /// Address being traced.
    target: IpAddr,
    /// The trace's parameters.
    config: TraceConfig,
    /// Where every probe outcome is also sent, if anywhere.
    history: Option<HistorySender>,
    /// The app's TCP prober.
    tcp: Arc<TcpProber>,
}

impl Shared {
    /// Waits until the next probe of a cycle that began at `started` is due: the interval has passed, or a pause
    /// has just ended (resuming probes right away). Returns `false` if stop was requested instead.
    fn wait_turn(&self, started: Instant) -> bool {
        let mut control = lock(&self.control);
        let mut was_paused = false;
        loop {
            if control.stop {
                return false;
            }
            if control.paused {
                was_paused = true;
                control = self.wake.wait(control).unwrap_or_else(PoisonError::into_inner);
                continue;
            }
            let remaining = control.interval.saturating_sub(started.elapsed());
            if was_paused || remaining.is_zero() {
                return true;
            }
            control = self.wake.wait_timeout(control, remaining).unwrap_or_else(PoisonError::into_inner).0;
        }
    }

    /// Changes the settings and wakes the workers so they follow at once.
    fn update(&self, change: impl FnOnce(&mut Control)) {
        change(&mut lock(&self.control));
        self.wake.notify_all();
    }
}

/// Running trace of one target, by one worker thread per TTL the route may need; stops its workers on drop.
#[derive(Debug)]
pub struct Tracer {
    /// State shared with the workers.
    shared: Arc<Shared>,
    /// For a trace by TCP probes: the app's prober and the target, to ask about a firewall on this computer.
    tcp: Option<(Arc<TcpProber>, IpAddr)>,
}

impl Tracer {
    /// Starts tracing `target`; every probe outcome is also sent to `history`, if given. TCP probes go through
    /// `tcp`, the app's prober.
    ///
    /// # Errors
    ///
    /// [`TraceError`] if an ICMP handle can't be opened or a thread can't be started.
    pub fn start(
        target: IpAddr,
        config: TraceConfig,
        history: Option<HistorySender>,
        tcp: &Arc<TcpProber>,
    ) -> Result<Self, TraceError> {
        let shared = Arc::new(Shared {
            route: Mutex::new(RouteState::new(target, config.max_ttl)),
            control: Mutex::new(Control { stop: false, paused: false, interval: config.interval }),
            wake: Condvar::new(),
            last_error: Mutex::new(None),
            workers: Mutex::new(vec![false; usize::from(config.max_ttl)]),
            target,
            config,
            history,
            tcp: Arc::clone(tcp),
        });
        // Made before spawning, so that an early error drops it and stops the workers already started.
        let tracer = Self { shared, tcp: matches!(config.probe, Probe::Tcp(_)).then(|| (Arc::clone(tcp), target)) };
        spawn_wanted(&tracer.shared)?;
        Ok(tracer)
    }

    /// Current route with statistics over `focus` = `(from, to)` Unix ms, or since the start if `None`.
    pub fn snapshot(&self, focus: Option<(i64, i64)>) -> Route {
        lock(&self.shared.route).snapshot(focus)
    }

    /// Error of the latest failed probe (network down, ...), `None` once probes can be sent again.
    pub fn last_error(&self) -> Option<Message> {
        lock(&self.shared.last_error).clone()
    }

    /// Whether a firewall on this computer keeps the answers to this trace's TCP probes from Ping Watch.
    pub fn answers_blocked(&self) -> bool {
        self.tcp.as_ref().is_some_and(|(prober, target)| prober.answers_blocked(*target))
    }

    /// Pauses or resumes probing; statistics are kept.
    pub fn set_paused(&self, paused: bool) {
        self.shared.update(|control| control.paused = paused);
    }

    /// Whether probing is paused.
    pub fn is_paused(&self) -> bool {
        lock(&self.shared.control).paused
    }

    /// Changes the time between probes of the same hop; takes effect immediately.
    pub fn set_interval(&self, interval: Duration) {
        self.shared.update(|control| control.interval = interval);
    }
}

impl Drop for Tracer {
    /// Tells the workers to stop and doesn't wait for them: one waiting for an answer ends when its timeout does,
    /// and its outcome goes nowhere. Closing a tab or the app is immediate.
    fn drop(&mut self) {
        self.shared.update(|control| control.stop = true);
    }
}

/// Starts a worker for every TTL the route needs one for ([`RouteState::wanted`]) and has none.
fn spawn_wanted(shared: &Arc<Shared>) -> Result<(), TraceError> {
    let mut workers = lock(&shared.workers);
    for ttl in 1..=shared.config.max_ttl {
        let Some(running) = workers.get_mut(usize::from(ttl).saturating_sub(1)) else {
            continue;
        };
        if *running || !lock(&shared.route).wanted(ttl) {
            continue;
        }
        let prober = Prober::new(shared.config.probe, shared.target, &shared.tcp)?;
        let worker = Arc::clone(shared);
        thread::Builder::new()
            .name(format!("probe-ttl-{ttl}"))
            .spawn(move || probe_loop(&worker, &prober, ttl))?;
        *running = true;
    }
    Ok(())
}

/// Ends the worker of `ttl` if the route no longer needs it (the target answers well before it): `true` if so.
/// Decided under the workers' lock, so a route reopened meanwhile starts a new one.
fn retire(shared: &Shared, ttl: u8) -> bool {
    let mut workers = lock(&shared.workers);
    if lock(&shared.route).wanted(ttl) {
        return false;
    }
    if let Some(running) = workers.get_mut(usize::from(ttl).saturating_sub(1)) {
        *running = false;
    }
    true
}

/// Worker body: probes hop `ttl` every interval until stop is requested or the route doesn't reach that far.
fn probe_loop(shared: &Arc<Shared>, prober: &Prober, ttl: u8) {
    let (target, config, history) = (shared.target, shared.config, shared.history.as_ref());
    loop {
        let started = Instant::now();
        let at_ms = unix_ms(SystemTime::now());
        let interval_ms = millis(lock(&shared.control).interval);
        if lock(&shared.route).take_turn(ttl, at_ms, interval_ms) {
            let outcome = match prober.probe(target, ttl, config.timeout) {
                Ok(outcome) => {
                    // It went out: whatever kept the probes from being sent is over, answered or not.
                    *lock(&shared.last_error) = None;
                    outcome
                }
                Err(error) => {
                    // No answer is a lost probe whatever the reason (with the network down Windows fails at once
                    // instead of timing out); the error itself is shown until a probe gets through again.
                    log::debug!("probe {target} ttl {ttl}: {error:?}");
                    *lock(&shared.last_error) = Some(error);
                    ProbeOutcome::Lost
                }
            };
            // A trace stopped while this probe was out (its tab closed, its probe changed) records nothing more.
            if lock(&shared.control).stop {
                break;
            }
            let ready = lock(&shared.route).record(ttl, at_ms, outcome);
            if let Some(history) = history
                && !ready.into_iter().all(|sample| history.send(sample))
            {
                log::debug!("history recorder is gone; samples of ttl {ttl} dropped");
            }
            // The outcome may have made the route longer: the TTLs it now reaches get their workers back.
            if let Err(error) = spawn_wanted(shared) {
                log::warn!("cannot start a probe worker: {error}");
                *lock(&shared.last_error) = Some(error.message());
            }
        }
        if retire(shared, ttl) || !shared.wait_turn(started) {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use super::*;

    #[test]
    fn probes_round_trip_through_their_keys() {
        for probe in [Probe::Icmp, Probe::Tcp(443), Probe::Tcp(22)] {
            assert_eq!(Probe::from_key(&probe.key()), Some(probe));
        }
        assert_eq!(Probe::from_key("tcp:0"), None);
        assert_eq!(Probe::from_key("tcp:99999"), None);
        assert_eq!(Probe::from_key("udp"), None);
    }

    /// Probes sent to the first hop so far.
    fn sent(tracer: &Tracer) -> u64 {
        tracer.snapshot(None).hops().first().map_or(0, |hop| hop.stats.sent())
    }

    /// Runs against the loopback address, so it needs no network.
    #[test]
    fn pause_stops_probing_and_interval_changes_live() {
        let config = TraceConfig {
            interval: Duration::from_millis(50),
            timeout: Duration::from_millis(200),
            max_ttl: 2,
            probe: Probe::Icmp,
        };
        let tcp = Arc::new(TcpProber::default());
        let tracer = Tracer::start(IpAddr::V4(Ipv4Addr::LOCALHOST), config, None, &tcp).expect("tracer starts");
        thread::sleep(Duration::from_millis(400));
        assert!(sent(&tracer) >= 4, "probes every 50 ms");

        tracer.set_paused(true);
        thread::sleep(Duration::from_millis(100)); // let a probe in flight finish
        let paused_at = sent(&tracer);
        thread::sleep(Duration::from_millis(300));
        assert_eq!(sent(&tracer), paused_at, "no probes while paused");

        tracer.set_interval(Duration::from_secs(10));
        tracer.set_paused(false);
        thread::sleep(Duration::from_millis(300));
        // Resuming probes once right away, then waits the new, long interval.
        assert_eq!(sent(&tracer), paused_at.saturating_add(1));
    }
}
