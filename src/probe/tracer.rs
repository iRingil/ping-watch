//! Continuous tracing of one target: one worker thread per TTL probes its hop every interval (by ICMP echo or a
//! TCP SYN) and records the outcome into a shared [`Route`] (the UI reads snapshots of it) and, optionally, sends it
//! to the history. The trace can be paused and its interval changed while it runs.

use std::net::{IpAddr, Ipv4Addr};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

use crate::probe::icmp::{IcmpError, Pinger};
use crate::probe::route::{MAX_TTL, ProbeOutcome, Route, RouteState, Sample};
use crate::probe::tcp::{self, TcpError, TcpProber};
use crate::store::history::unix_ms;
use crate::sync::lock;

/// What the hops are probed with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Probe {
    /// ICMP echo, as `ping` and `tracert`; needs no rights.
    #[default]
    Icmp,
    /// A TCP SYN to this port, as a connection would start: passes firewalls that drop ICMP, and reaches servers
    /// that don't answer ping. Needs administrator rights; IPv4 only.
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
    /// TCP probes were asked for an IPv6 target.
    #[error("TCP probes trace IPv4 addresses only")]
    TcpIpv6,
}

/// What a worker probes its hop with.
#[derive(Debug)]
enum Prober {
    /// Its own ICMP handle.
    Icmp(Pinger),
    /// The app's TCP prober, for this target and port.
    Tcp {
        /// Shared by all traces.
        prober: Arc<TcpProber>,
        /// The target.
        target: Ipv4Addr,
        /// Its port.
        port: u16,
    },
}

impl Prober {
    /// The prober of `probe` for `target`.
    fn new(probe: Probe, target: IpAddr, tcp: &Arc<TcpProber>) -> Result<Self, TraceError> {
        match (probe, target) {
            (Probe::Icmp, _) => Ok(Self::Icmp(Pinger::for_target(target)?)),
            (Probe::Tcp(port), IpAddr::V4(target)) => Ok(Self::Tcp { prober: Arc::clone(tcp), target, port }),
            (Probe::Tcp(_), IpAddr::V6(_)) => Err(TraceError::TcpIpv6),
        }
    }

    /// Probes hop `ttl` towards `target`, waiting up to `timeout`; the error is shown to the user.
    fn probe(&self, target: IpAddr, ttl: u8, timeout: Duration) -> Result<ProbeOutcome, ProbeError> {
        match self {
            Self::Icmp(pinger) => {
                pinger.probe(target, ttl, timeout).map_err(|error| ProbeError::Text(error.to_string()))
            }
            Self::Tcp { prober, target, port } => {
                prober.probe(*target, *port, ttl, timeout).map_err(|error| match error {
                    TcpError::NotElevated => ProbeError::Known(tcp::NOT_ELEVATED),
                    TcpError::Blocked => ProbeError::Known(tcp::BLOCKED),
                    error => ProbeError::Text(error.to_string()),
                })
            }
        }
    }
}

/// Why probes fail, as shown to the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeError {
    /// One of the app's own messages (marked for translation), shown in the user's language.
    Known(&'static str),
    /// A message from Windows, shown as it is.
    Text(String),
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
    /// Error of the latest failed probe, cleared by the next answer.
    last_error: Mutex<Option<ProbeError>>,
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

/// Running trace of one target; stops its workers on drop.
#[derive(Debug)]
pub struct Tracer {
    /// State shared with the workers.
    shared: Arc<Shared>,
    /// Worker threads, one per TTL.
    workers: Vec<JoinHandle<()>>,
}

impl Tracer {
    /// Starts tracing `target`; every probe outcome is also sent to `history`, if given. TCP probes go through
    /// `tcp`, the app's prober.
    ///
    /// # Errors
    ///
    /// [`TraceError`] if an ICMP handle can't be opened, TCP probes are asked for an IPv6 target or a thread can't
    /// be started.
    pub fn start(
        target: IpAddr,
        config: TraceConfig,
        history: Option<Sender<Sample>>,
        tcp: &Arc<TcpProber>,
    ) -> Result<Self, TraceError> {
        let shared = Arc::new(Shared {
            route: Mutex::new(RouteState::new(target, config.max_ttl)),
            control: Mutex::new(Control { stop: false, paused: false, interval: config.interval }),
            wake: Condvar::new(),
            last_error: Mutex::new(None),
        });
        // Assigned before spawning, so that an early error drops (and stops) the already started workers.
        let mut tracer = Self { shared, workers: Vec::new() };
        for ttl in 1..=config.max_ttl {
            let prober = Prober::new(config.probe, target, tcp)?;
            let shared = Arc::clone(&tracer.shared);
            let history = history.clone();
            let worker = thread::Builder::new()
                .name(format!("probe-ttl-{ttl}"))
                .spawn(move || probe_loop(&shared, &prober, history.as_ref(), target, ttl, config))?;
            tracer.workers.push(worker);
        }
        Ok(tracer)
    }

    /// Current route with statistics over `focus` = `(from, to)` Unix ms, or since the start if `None`.
    pub fn snapshot(&self, focus: Option<(i64, i64)>) -> Route {
        lock(&self.shared.route).snapshot(focus)
    }

    /// Error of the latest failed probe (network down, ...), `None` once probes get answers again.
    pub fn last_error(&self) -> Option<ProbeError> {
        lock(&self.shared.last_error).clone()
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
    fn drop(&mut self) {
        self.shared.update(|control| control.stop = true);
        for worker in self.workers.drain(..) {
            if worker.join().is_err() {
                log::error!("probe thread panicked");
            }
        }
    }
}

/// Worker body: probes hop `ttl` every interval until stop is requested.
fn probe_loop(
    shared: &Shared,
    prober: &Prober,
    history: Option<&Sender<Sample>>,
    target: IpAddr,
    ttl: u8,
    config: TraceConfig,
) {
    loop {
        let started = Instant::now();
        if lock(&shared.route).should_probe(ttl) {
            let at_ms = unix_ms(SystemTime::now());
            let outcome = match prober.probe(target, ttl, config.timeout) {
                Ok(outcome) => {
                    if matches!(outcome, ProbeOutcome::Reply { .. }) {
                        *lock(&shared.last_error) = None;
                    }
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
            let (in_route, at_target) = {
                let mut route = lock(&shared.route);
                route.record(ttl, at_ms, outcome);
                (route.in_route(ttl), route.is_target_ttl(ttl))
            };
            // TTLs beyond the target, or past the end of an unreachable route, only add noise to the history.
            if in_route
                && let Some(history) = history
                && history.send(Sample { at_ms, ttl, outcome, at_target }).is_err()
            {
                log::debug!("history recorder is gone; sample of ttl {ttl} dropped");
            }
        }
        if !shared.wait_turn(started) {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
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
