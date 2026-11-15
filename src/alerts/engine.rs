//! Checking the rules: once a second the app asks the engine to look at every traced target; the engine keeps which
//! alerts are on (since when, worst value) and returns what changed.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use super::route_watch::{Path, RouteChange, RouteWatch};
use super::rules::{Condition, Rule};

/// Fewest probes in the window before loss or latency is judged (a couple of probes say nothing).
const MIN_PROBES: u64 = 3;
/// Share of a condition's window the trace must already cover before the condition is judged.
const MIN_COVERAGE: f64 = 0.95;

/// What a target's own probes (the last hop) showed over a period.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowStats {
    /// Probes sent in the period.
    pub sent: u64,
    /// Probes answered.
    pub received: u64,
    /// Average latency of the answers, milliseconds.
    pub average_ms: Option<f64>,
    /// How much of the period the trace has run (a trace started a minute ago covers one minute of five).
    pub covered: Duration,
}

impl WindowStats {
    /// Loss, percent of the probes sent.
    fn loss_percent(&self) -> f64 {
        if self.sent == 0 {
            return 0.0;
        }
        #[expect(clippy::cast_precision_loss, reason = "probe counts are far below 2^52")]
        let share = self.sent.saturating_sub(self.received) as f64 / self.sent as f64;
        share * 100.0
    }
}

/// Where the engine gets its measurements: the running traces. A trait so the tests can feed made-up numbers.
pub trait Observe {
    /// The traced targets (hosts as entered); paused or not yet resolved ones are left out.
    fn hosts(&self) -> Vec<String>;
    /// The target's own probes over the last `window`, `None` if it isn't traced.
    fn window(&self, host: &str, window: Duration) -> Option<WindowStats>;
    /// The answering hops of the target's route.
    fn path(&self, host: &str) -> Path;
}

/// What happened at a check.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// A rule's condition became true for a target.
    Started {
        /// The rule.
        rule_id: i64,
        /// The target.
        host: String,
        /// The measured value: loss percent, average ms or seconds without an answer.
        value: f64,
    },
    /// The condition is over.
    Ended {
        /// The rule.
        rule_id: i64,
        /// The target.
        host: String,
        /// When it started, Unix ms.
        started_ms: i64,
        /// The worst value while it lasted.
        worst: f64,
        /// It really got better (`false`: the rule was turned off or the target closed, so nothing to announce).
        recovered: bool,
    },
    /// The route of a target changed.
    RouteChanged {
        /// The rule that watches it.
        rule_id: i64,
        /// The target.
        host: String,
        /// Before and after.
        change: RouteChange,
    },
}

/// An alert that is on.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Active {
    /// When it started, Unix ms.
    started_ms: i64,
    /// The worst value so far.
    worst: f64,
}

/// The rule checker and its state.
#[derive(Debug, Default)]
pub struct Engine {
    /// Alerts that are on, by rule and target.
    active: HashMap<(i64, String), Active>,
    /// Route watches by target.
    routes: HashMap<String, RouteWatch>,
}

impl Engine {
    /// Number of alerts that are on.
    pub fn active_count(&self) -> usize {
        self.active.len()
    }

    /// Whether an alert is on for `host`.
    pub fn is_alerting(&self, host: &str) -> bool {
        self.active.keys().any(|(_, active_host)| active_host.eq_ignore_ascii_case(host))
    }

    /// Checks `rules` against what `observe` sees at `now_ms`; returns what started, ended or changed.
    pub fn check(&mut self, rules: &[Rule], observe: &dyn Observe, now_ms: i64) -> Vec<Event> {
        let hosts = observe.hosts();
        let mut events = Vec::new();
        let mut checked = HashSet::new();
        for rule in rules.iter().filter(|rule| rule.enabled) {
            for host in hosts.iter().filter(|host| rule.targets.contains(host)) {
                if let Some(window) = rule.condition.window() {
                    checked.insert((rule.id, host.clone()));
                    let stats = observe.window(host, window);
                    self.judge(rule, host, window, stats, now_ms, &mut events);
                }
            }
        }
        // Alerts whose rule was turned off or whose target went away end quietly.
        let gone: Vec<(i64, String)> = self.active.keys().filter(|key| !checked.contains(*key)).cloned().collect();
        for (rule_id, host) in gone {
            if let Some(active) = self.active.remove(&(rule_id, host.clone())) {
                events.push(Event::Ended {
                    rule_id,
                    host,
                    started_ms: active.started_ms,
                    worst: active.worst,
                    recovered: false,
                });
            }
        }
        self.check_routes(rules, &hosts, observe, &mut events);
        events
    }

    /// Judges one rule for one target and records the start or end of its alert.
    fn judge(
        &mut self,
        rule: &Rule,
        host: &str,
        window: Duration,
        stats: Option<WindowStats>,
        now_ms: i64,
        events: &mut Vec<Event>,
    ) {
        // No probes in the window (paused, just started): nothing to judge, the alert stays as it is.
        let Some(stats) = stats.filter(|stats| stats.sent > 0) else {
            return;
        };
        if stats.covered.as_secs_f64() < window.as_secs_f64() * MIN_COVERAGE {
            return;
        }
        // The value if the condition holds, `None` if it doesn't; too few probes or answers to judge leave the
        // alert as it is (a latency alert can't end while nothing answers: there is no average to look at).
        let bad = match rule.condition {
            Condition::Loss { percent, .. } => {
                if stats.sent < MIN_PROBES {
                    return;
                }
                let loss = stats.loss_percent();
                (loss > percent).then_some(loss)
            }
            Condition::Latency { ms, .. } => {
                let Some(average) = stats.average_ms.filter(|_| stats.received >= MIN_PROBES) else {
                    return;
                };
                (average > ms).then_some(average)
            }
            Condition::Unreachable { after } => (stats.received == 0).then_some(after.as_secs_f64()),
            Condition::RouteChanged => return,
        };
        let key = (rule.id, host.to_owned());
        match (bad, self.active.get_mut(&key)) {
            (Some(value), Some(active)) => active.worst = active.worst.max(value),
            (Some(value), None) => {
                self.active.insert(key, Active { started_ms: now_ms, worst: value });
                events.push(Event::Started { rule_id: rule.id, host: host.to_owned(), value });
            }
            (None, Some(_)) => {
                if let Some(active) = self.active.remove(&key) {
                    events.push(Event::Ended {
                        rule_id: rule.id,
                        host: host.to_owned(),
                        started_ms: active.started_ms,
                        worst: active.worst,
                        recovered: true,
                    });
                }
            }
            (None, None) => {}
        }
    }

    /// Feeds every target's route to its watch and reports held changes to the rules watching routes.
    fn check_routes(&mut self, rules: &[Rule], hosts: &[String], observe: &dyn Observe, events: &mut Vec<Event>) {
        self.routes.retain(|host, _| hosts.contains(host));
        for host in hosts {
            let change = self.routes.entry(host.clone()).or_default().check(observe.path(host));
            let Some(change) = change else {
                continue;
            };
            let watching = rules.iter().filter(|rule| {
                rule.enabled && rule.condition == Condition::RouteChanged && rule.targets.contains(host)
            });
            for rule in watching {
                events.push(Event::RouteChanged { rule_id: rule.id, host: host.clone(), change: change.clone() });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::net::{IpAddr, Ipv4Addr};

    use super::super::route_watch::STABLE_CHECKS;
    use super::super::rules::Targets;
    use super::*;

    /// Made-up measurements: the same window for every period asked.
    #[derive(Default)]
    struct Fake {
        /// Hosts and their window.
        hosts: RefCell<Vec<(String, WindowStats, Path)>>,
    }

    impl Fake {
        /// Sets what `host` shows.
        fn set(&self, host: &str, stats: WindowStats, path: Path) {
            let mut hosts = self.hosts.borrow_mut();
            hosts.retain(|(known, _, _)| known != host);
            hosts.push((host.to_owned(), stats, path));
        }
    }

    impl Observe for Fake {
        fn hosts(&self) -> Vec<String> {
            self.hosts.borrow().iter().map(|(host, _, _)| host.clone()).collect()
        }
        fn window(&self, host: &str, _window: Duration) -> Option<WindowStats> {
            self.hosts.borrow().iter().find(|(known, _, _)| known == host).map(|(_, stats, _)| *stats)
        }
        fn path(&self, host: &str) -> Path {
            self.hosts
                .borrow()
                .iter()
                .find(|(known, _, _)| known == host)
                .map(|(_, _, path)| path.clone())
                .unwrap_or_default()
        }
    }

    /// A window of `sent` probes with `received` answers averaging `average_ms`, fully covered.
    fn stats(sent: u64, received: u64, average_ms: f64) -> WindowStats {
        WindowStats { sent, received, average_ms: Some(average_ms), covered: Duration::from_secs(3_600) }
    }

    /// An enabled rule `id` on all targets.
    fn rule(id: i64, condition: Condition) -> Rule {
        Rule { id, targets: Targets::All, condition, notify: true, notify_recovery: true, enabled: true }
    }

    /// A one-hop path answering from 10.0.0.`last`.
    fn path(last: u8) -> Path {
        vec![(1, IpAddr::V4(Ipv4Addr::new(10, 0, 0, last)))]
    }

    #[test]
    fn loss_starts_once_then_recovers() {
        let fake = Fake::default();
        let rules = [rule(1, Condition::Loss { percent: 10.0, window: Duration::from_secs(120) })];
        let mut engine = Engine::default();
        fake.set("a", stats(100, 95, 20.0), path(1));
        assert!(engine.check(&rules, &fake, 0).is_empty(), "5 % is fine");
        fake.set("a", stats(100, 80, 20.0), path(1));
        assert_eq!(
            engine.check(&rules, &fake, 1_000),
            [Event::Started { rule_id: 1, host: "a".to_owned(), value: 20.0 }]
        );
        fake.set("a", stats(100, 70, 20.0), path(1));
        assert!(engine.check(&rules, &fake, 2_000).is_empty(), "announced once, not every second");
        assert!(engine.is_alerting("A"));
        fake.set("a", stats(100, 100, 20.0), path(1));
        assert_eq!(
            engine.check(&rules, &fake, 3_000),
            [Event::Ended { rule_id: 1, host: "a".to_owned(), started_ms: 1_000, worst: 30.0, recovered: true }]
        );
        assert_eq!(engine.active_count(), 0);
    }

    #[test]
    fn latency_and_no_answer() {
        let fake = Fake::default();
        let rules = [
            rule(1, Condition::Latency { ms: 100.0, window: Duration::from_secs(60) }),
            rule(2, Condition::Unreachable { after: Duration::from_secs(30) }),
        ];
        let mut engine = Engine::default();
        fake.set("a", stats(60, 60, 150.0), path(1));
        assert_eq!(engine.check(&rules, &fake, 0), [Event::Started { rule_id: 1, host: "a".to_owned(), value: 150.0 }]);
        fake.set(
            "a",
            WindowStats { sent: 30, received: 0, average_ms: None, covered: Duration::from_secs(60) },
            path(1),
        );
        let events = engine.check(&rules, &fake, 1_000);
        assert!(events.contains(&Event::Started { rule_id: 2, host: "a".to_owned(), value: 30.0 }));
        assert!(
            !events.iter().any(|event| matches!(event, Event::Ended { rule_id: 1, .. })),
            "without answers there is no average to judge, so the latency alert stays"
        );
    }

    #[test]
    fn nothing_is_judged_before_the_window_is_covered_or_without_probes() {
        let fake = Fake::default();
        let rules = [rule(1, Condition::Unreachable { after: Duration::from_secs(30) })];
        let mut engine = Engine::default();
        fake.set("a", WindowStats { sent: 5, received: 0, average_ms: None, covered: Duration::from_secs(5) }, path(1));
        assert!(engine.check(&rules, &fake, 0).is_empty(), "the trace runs only 5 s of 30");
        fake.set(
            "a",
            WindowStats { sent: 0, received: 0, average_ms: None, covered: Duration::from_secs(60) },
            path(1),
        );
        assert!(engine.check(&rules, &fake, 1_000).is_empty(), "paused: no probes");
    }

    #[test]
    fn turned_off_rules_and_closed_targets_end_quietly() {
        let fake = Fake::default();
        let mut rules = [rule(1, Condition::Loss { percent: 10.0, window: Duration::from_secs(60) })];
        let mut engine = Engine::default();
        fake.set("a", stats(100, 50, 20.0), path(1));
        engine.check(&rules, &fake, 0);
        if let Some(first) = rules.first_mut() {
            first.enabled = false;
        }
        assert_eq!(
            engine.check(&rules, &fake, 1_000),
            [Event::Ended { rule_id: 1, host: "a".to_owned(), started_ms: 0, worst: 50.0, recovered: false }]
        );
    }

    #[test]
    fn only_the_listed_targets_are_watched() {
        let fake = Fake::default();
        let mut only = rule(1, Condition::Loss { percent: 10.0, window: Duration::from_secs(60) });
        only.targets = Targets::Only(vec!["b".to_owned()]);
        let mut engine = Engine::default();
        fake.set("a", stats(100, 50, 20.0), path(1));
        assert!(engine.check(&[only], &fake, 0).is_empty());
    }

    #[test]
    fn route_changes_go_to_the_route_rules() {
        let fake = Fake::default();
        let rules = [rule(7, Condition::RouteChanged)];
        let mut engine = Engine::default();
        fake.set("a", stats(10, 10, 20.0), path(1));
        engine.check(&rules, &fake, 0);
        fake.set("a", stats(10, 10, 20.0), path(2));
        let mut events = Vec::new();
        for second in 1..=i64::from(STABLE_CHECKS) {
            events.extend(engine.check(&rules, &fake, second.saturating_mul(1_000)));
        }
        assert!(matches!(events.as_slice(), [Event::RouteChanged { rule_id: 7, .. }]));
    }
}
