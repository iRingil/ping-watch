//! Continuous tracing of one target: one worker thread per TTL probes its hop every interval and records the
//! outcome into a shared [`Route`] (the UI reads snapshots of it) and, optionally, sends it to the history.
//! The trace can be paused and its interval changed while it runs.

use std::net::IpAddr;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

use crate::history::unix_ms;
use crate::icmp::{IcmpError, Pinger};
use crate::route::{ProbeOutcome, Route, RouteState, Sample};
use crate::sync::lock;

/// Tracing parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TraceConfig {
    /// Initial time between probes of the same hop.
    pub interval: Duration,
    /// How long to wait for an answer before counting the probe as lost.
    pub timeout: Duration,
    /// Largest TTL probed.
    pub max_hops: u8,
}

impl Default for TraceConfig {
    fn default() -> Self {
        Self { interval: Duration::from_secs(1), timeout: Duration::from_secs(1), max_hops: 30 }
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
    last_error: Mutex<Option<String>>,
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
    /// Starts tracing `target`; every probe outcome is also sent to `history`, if given.
    ///
    /// # Errors
    ///
    /// [`TraceError`] if an ICMP handle can't be opened or a thread can't be started.
    pub fn start(target: IpAddr, config: TraceConfig, history: Option<Sender<Sample>>) -> Result<Self, TraceError> {
        let shared = Arc::new(Shared {
            route: Mutex::new(RouteState::new(target, config.max_hops)),
            control: Mutex::new(Control { stop: false, paused: false, interval: config.interval }),
            wake: Condvar::new(),
            last_error: Mutex::new(None),
        });
        // Assigned before spawning, so that an early error drops (and stops) the already started workers.
        let mut tracer = Self { shared, workers: Vec::new() };
        for ttl in 1..=config.max_hops {
            let pinger = Pinger::for_target(target)?;
            let shared = Arc::clone(&tracer.shared);
            let history = history.clone();
            let worker = thread::Builder::new()
                .name(format!("probe-ttl-{ttl}"))
                .spawn(move || probe_loop(&shared, &pinger, history.as_ref(), target, ttl, config))?;
            tracer.workers.push(worker);
        }
        Ok(tracer)
    }

    /// Current route with statistics over `focus` = `(from, to)` Unix ms, or since the start if `None`.
    pub fn snapshot(&self, focus: Option<(i64, i64)>) -> Route {
        lock(&self.shared.route).snapshot(focus)
    }

    /// Error of the latest failed probe (network down, ...), `None` once probes get answers again.
    pub fn last_error(&self) -> Option<String> {
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
    pinger: &Pinger,
    history: Option<&Sender<Sample>>,
    target: IpAddr,
    ttl: u8,
    config: TraceConfig,
) {
    loop {
        let started = Instant::now();
        if lock(&shared.route).should_probe(ttl) {
            let at_ms = unix_ms(SystemTime::now());
            let outcome = match pinger.probe(target, ttl, config.timeout) {
                Ok(outcome) => {
                    if matches!(outcome, ProbeOutcome::Reply { .. }) {
                        *lock(&shared.last_error) = None;
                    }
                    outcome
                }
                Err(error) => {
                    // No answer is a lost probe whatever the reason (with the network down Windows fails at once
                    // instead of timing out); the error itself is shown until a probe gets through again.
                    log::debug!("probe {target} ttl {ttl}: {error}");
                    *lock(&shared.last_error) = Some(error.to_string());
                    ProbeOutcome::Lost
                }
            };
            let (in_route, at_target) = {
                let mut route = lock(&shared.route);
                route.record(ttl, at_ms, outcome);
                (route.should_probe(ttl), route.is_target_ttl(ttl))
            };
            // Before the route length is known every TTL is probed once; TTLs that turn out to lie beyond the
            // target only add noise to the history.
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
    use std::net::Ipv4Addr;

    use super::*;

    /// Probes sent to the first hop so far.
    fn sent(tracer: &Tracer) -> u64 {
        tracer.snapshot(None).hops().first().map_or(0, |hop| hop.stats.sent())
    }

    /// Runs against the loopback address, so it needs no network.
    #[test]
    fn pause_stops_probing_and_interval_changes_live() {
        let config =
            TraceConfig { interval: Duration::from_millis(50), timeout: Duration::from_millis(200), max_hops: 2 };
        let tracer = Tracer::start(IpAddr::V4(Ipv4Addr::LOCALHOST), config, None).expect("tracer starts");
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
