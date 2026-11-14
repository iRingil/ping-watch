//! Continuous tracing of one target: one worker thread per TTL probes its hop every interval and records the
//! outcome into a shared [`Route`] (the UI reads snapshots of it) and, optionally, sends it to the history.

use std::net::Ipv4Addr;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

use crate::history::unix_ms;
use crate::icmp::{IcmpError, Pinger};
use crate::route::{Route, Sample};
use crate::sync::lock;

/// Tracing parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TraceConfig {
    /// Time between probes of the same hop.
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

/// State shared between the [`Tracer`] and its workers.
#[derive(Debug)]
struct Shared {
    /// Route being built.
    route: Mutex<Route>,
    /// Set to `true` to stop the workers.
    stop: Mutex<bool>,
    /// Wakes sleeping workers when `stop` is set.
    wake: Condvar,
}

impl Shared {
    /// Sleeps for `duration` or until stop is requested; returns whether to stop.
    fn sleep_or_stop(&self, duration: Duration) -> bool {
        let stop = lock(&self.stop);
        let (stop, _) =
            self.wake.wait_timeout_while(stop, duration, |stop| !*stop).unwrap_or_else(PoisonError::into_inner);
        *stop
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
    pub fn start(target: Ipv4Addr, config: TraceConfig, history: Option<Sender<Sample>>) -> Result<Self, TraceError> {
        let shared = Arc::new(Shared {
            route: Mutex::new(Route::new(target, config.max_hops)),
            stop: Mutex::new(false),
            wake: Condvar::new(),
        });
        // Assigned before spawning, so that an early error drops (and stops) the already started workers.
        let mut tracer = Self { shared, workers: Vec::new() };
        for ttl in 1..=config.max_hops {
            let pinger = Pinger::new()?;
            let shared = Arc::clone(&tracer.shared);
            let history = history.clone();
            let worker = thread::Builder::new()
                .name(format!("probe-ttl-{ttl}"))
                .spawn(move || probe_loop(&shared, &pinger, history.as_ref(), target, ttl, config))?;
            tracer.workers.push(worker);
        }
        Ok(tracer)
    }

    /// Copy of the current route state.
    pub fn snapshot(&self) -> Route {
        lock(&self.shared.route).clone()
    }
}

impl Drop for Tracer {
    fn drop(&mut self) {
        *lock(&self.shared.stop) = true;
        self.shared.wake.notify_all();
        for worker in self.workers.drain(..) {
            if worker.join().is_err() {
                log::error!("probe thread panicked");
            }
        }
    }
}

/// Worker body: probes hop `ttl` every `config.interval` until stop is requested.
fn probe_loop(
    shared: &Shared,
    pinger: &Pinger,
    history: Option<&Sender<Sample>>,
    target: Ipv4Addr,
    ttl: u8,
    config: TraceConfig,
) {
    loop {
        let started = Instant::now();
        if lock(&shared.route).should_probe(ttl) {
            let at_ms = unix_ms(SystemTime::now());
            match pinger.probe(target, ttl, config.timeout) {
                Ok(outcome) => {
                    let in_route = {
                        let mut route = lock(&shared.route);
                        route.record(ttl, outcome);
                        route.should_probe(ttl)
                    };
                    // Before the route length is known every TTL is probed once; TTLs that turn out to lie beyond
                    // the target only add noise to the history.
                    if in_route
                        && let Some(history) = history
                        && history.send(Sample { at_ms, ttl, outcome }).is_err()
                    {
                        log::debug!("history recorder is gone; sample of ttl {ttl} dropped");
                    }
                }
                Err(error) => log::warn!("probe {target} ttl {ttl}: {error}"),
            }
        }
        if shared.sleep_or_stop(config.interval.saturating_sub(started.elapsed())) {
            break;
        }
    }
}
