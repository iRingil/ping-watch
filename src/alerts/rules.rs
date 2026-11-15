//! What an alert rule is: which targets, which condition, what to tell the user.

use std::time::Duration;

/// Kind of condition, without its numbers (for menus and storage).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConditionKind {
    /// Packet loss above a share.
    Loss,
    /// Average latency above a time.
    Latency,
    /// No answer at all for a while.
    Unreachable,
    /// The route to the target changed.
    RouteChanged,
}

impl ConditionKind {
    /// All kinds in menu order.
    pub const ALL: [Self; 4] = [Self::Loss, Self::Latency, Self::Unreachable, Self::RouteChanged];

    /// Stable key for storing the kind.
    pub fn key(self) -> &'static str {
        match self {
            Self::Loss => "loss",
            Self::Latency => "latency",
            Self::Unreachable => "unreachable",
            Self::RouteChanged => "route",
        }
    }

    /// The kind stored under `key`, if it is one.
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.key() == key)
    }

    /// Menu label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Loss => "Packet loss",
            Self::Latency => "Average latency",
            Self::Unreachable => "No answer",
            Self::RouteChanged => "Route changed",
        }
    }
}

/// When a rule fires.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Condition {
    /// Packet loss of the target above `percent` over the last `window`.
    Loss {
        /// Threshold, percent of the probes sent.
        percent: f64,
        /// Period the loss is measured over.
        window: Duration,
    },
    /// Average latency of the target above `ms` over the last `window`.
    Latency {
        /// Threshold, milliseconds.
        ms: f64,
        /// Period the average is taken over.
        window: Duration,
    },
    /// The target hasn't answered a single probe for `after`.
    Unreachable {
        /// How long without an answer.
        after: Duration,
    },
    /// The route to the target changed (and the new one held for a while).
    RouteChanged,
}

impl Condition {
    /// Kind of this condition.
    pub fn kind(&self) -> ConditionKind {
        match self {
            Self::Loss { .. } => ConditionKind::Loss,
            Self::Latency { .. } => ConditionKind::Latency,
            Self::Unreachable { .. } => ConditionKind::Unreachable,
            Self::RouteChanged => ConditionKind::RouteChanged,
        }
    }

    /// The default condition of `kind`.
    pub fn default_of(kind: ConditionKind) -> Self {
        match kind {
            ConditionKind::Loss => Self::Loss { percent: 10.0, window: Duration::from_secs(120) },
            ConditionKind::Latency => Self::Latency { ms: 150.0, window: Duration::from_secs(120) },
            ConditionKind::Unreachable => Self::Unreachable { after: Duration::from_secs(30) },
            ConditionKind::RouteChanged => Self::RouteChanged,
        }
    }

    /// The period the condition looks back over (`None` for a route change, which has no window).
    pub fn window(&self) -> Option<Duration> {
        match self {
            Self::Loss { window, .. } | Self::Latency { window, .. } => Some(*window),
            Self::Unreachable { after } => Some(*after),
            Self::RouteChanged => None,
        }
    }

    /// The condition as a short phrase: "loss above 10 % over 2 minutes".
    pub fn describe(&self) -> String {
        match self {
            Self::Loss { percent, window } => format!("loss above {percent:.0} % over {}", span(*window)),
            Self::Latency { ms, window } => format!("average latency above {ms:.0} ms over {}", span(*window)),
            Self::Unreachable { after } => format!("no answer for {}", span(*after)),
            Self::RouteChanged => "route changed".to_owned(),
        }
    }
}

/// "30 seconds", "1 minute", "2 minutes", "1 hour".
pub fn span(duration: Duration) -> String {
    let seconds = duration.as_secs();
    let (count, unit) = match seconds {
        s if s >= 3_600 && s % 3_600 == 0 => (s / 3_600, "hour"),
        s if s >= 60 && s % 60 == 0 => (s / 60, "minute"),
        s => (s, "second"),
    };
    if count == 1 { format!("1 {unit}") } else { format!("{count} {unit}s") }
}

/// Which targets a rule watches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Targets {
    /// Every traced target.
    All,
    /// These hosts (as entered, compared without case).
    Only(Vec<String>),
}

impl Targets {
    /// Whether `host` is one of them.
    pub fn contains(&self, host: &str) -> bool {
        match self {
            Self::All => true,
            Self::Only(hosts) => hosts.iter().any(|listed| listed.eq_ignore_ascii_case(host)),
        }
    }
}

/// One alert rule.
#[derive(Debug, Clone, PartialEq)]
pub struct Rule {
    /// Row id; 0 for a rule not stored yet.
    pub id: i64,
    /// Watched targets.
    pub targets: Targets,
    /// When it fires.
    pub condition: Condition,
    /// Show a Windows notification when it fires.
    pub notify: bool,
    /// Show another notification when the problem is over.
    pub notify_recovery: bool,
    /// Whether the rule is checked at all.
    pub enabled: bool,
}

impl Rule {
    /// The rule as a phrase for lists: "Loss above 10 % over 2 minutes, all targets".
    pub fn describe(&self) -> String {
        let mut text = self.condition.describe();
        if let Some(first) = text.get(..1) {
            text = first.to_uppercase() + text.get(1..).unwrap_or_default();
        }
        let targets = match &self.targets {
            Targets::All => "all targets".to_owned(),
            Targets::Only(hosts) => hosts.join(", "),
        };
        format!("{text}, {targets}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans_read_naturally() {
        assert_eq!(span(Duration::from_secs(30)), "30 seconds");
        assert_eq!(span(Duration::from_secs(60)), "1 minute");
        assert_eq!(span(Duration::from_secs(120)), "2 minutes");
        assert_eq!(span(Duration::from_secs(90)), "90 seconds");
        assert_eq!(span(Duration::from_secs(3_600)), "1 hour");
    }

    #[test]
    fn rules_describe_themselves() {
        let rule = Rule {
            id: 0,
            targets: Targets::Only(vec!["1.1.1.1".to_owned(), "github.com".to_owned()]),
            condition: Condition::Loss { percent: 10.0, window: Duration::from_secs(120) },
            notify: true,
            notify_recovery: true,
            enabled: true,
        };
        assert_eq!(rule.describe(), "Loss above 10 % over 2 minutes, 1.1.1.1, github.com");
        assert!(rule.targets.contains("GitHub.com"));
        assert!(!rule.targets.contains("8.8.8.8"));
        assert!(Targets::All.contains("8.8.8.8"));
    }

    #[test]
    fn kinds_round_trip_through_keys() {
        assert!(ConditionKind::ALL.into_iter().all(|kind| ConditionKind::from_key(kind.key()) == Some(kind)));
        assert!(ConditionKind::ALL.into_iter().all(|kind| Condition::default_of(kind).kind() == kind));
    }
}
