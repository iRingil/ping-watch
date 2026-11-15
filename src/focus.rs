//! Focus: the period the hop table and the latency bars summarize, as in PingPlotter.

use crate::route::{RECENT_WINDOW, millis};

/// Period whose probes the hop table and latency bars summarize.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Focus {
    /// Last 5 seconds.
    Seconds5,
    /// Last 30 seconds.
    Seconds30,
    /// Last 60 seconds.
    Seconds60,
    /// Last 5 minutes.
    Minutes5,
    /// Last 30 minutes.
    Minutes30,
    /// Last 60 minutes.
    Minutes60,
    /// Everything since the trace started.
    All,
    /// The time range the timeline below shows (also when scrolled back), if its probes are still kept in memory;
    /// otherwise everything.
    #[default]
    Auto,
}

impl Focus {
    /// All choices in menu order.
    pub const ALL: [Self; 8] = [
        Self::Seconds5,
        Self::Seconds30,
        Self::Seconds60,
        Self::Minutes5,
        Self::Minutes30,
        Self::Minutes60,
        Self::All,
        Self::Auto,
    ];

    /// Menu label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Seconds5 => "5 seconds",
            Self::Seconds30 => "30 seconds",
            Self::Seconds60 => "60 seconds",
            Self::Minutes5 => "5 minutes",
            Self::Minutes30 => "30 minutes",
            Self::Minutes60 => "60 minutes",
            Self::All => "All",
            Self::Auto => "Auto",
        }
    }

    /// Stable key for storing the choice.
    pub fn key(self) -> &'static str {
        match self {
            Self::Seconds5 => "5s",
            Self::Seconds30 => "30s",
            Self::Seconds60 => "60s",
            Self::Minutes5 => "5m",
            Self::Minutes30 => "30m",
            Self::Minutes60 => "60m",
            Self::All => "all",
            Self::Auto => "auto",
        }
    }

    /// The choice stored under `key`, if it is one.
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|focus| focus.key() == key)
    }

    /// The time range to summarize as `(from, to)` Unix ms, `None` for everything. `timeline` is the range the
    /// timeline shows, `now_ms` the current time.
    pub fn range(self, timeline: (i64, i64), now_ms: i64) -> Option<(i64, i64)> {
        let seconds: i64 = match self {
            Self::Seconds5 => 5,
            Self::Seconds30 => 30,
            Self::Seconds60 => 60,
            Self::Minutes5 => 5 * 60,
            Self::Minutes30 => 30 * 60,
            Self::Minutes60 => 60 * 60,
            Self::All => return None,
            Self::Auto => {
                let kept_since = now_ms.saturating_sub(millis(RECENT_WINDOW));
                return (timeline.0 >= kept_since).then_some(timeline);
            }
        };
        Some((now_ms.saturating_sub(seconds.saturating_mul(1_000)), now_ms))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// "Now" in the tests: 10 hours after the epoch.
    const NOW: i64 = 36_000_000;

    #[test]
    fn keys_round_trip() {
        assert!(Focus::ALL.into_iter().all(|focus| Focus::from_key(focus.key()) == Some(focus)));
        assert_eq!(Focus::from_key("nonsense"), None);
    }

    #[test]
    fn fixed_choices_end_now_and_ignore_the_timeline() {
        let timeline = (NOW - 600_000, NOW - 300_000);
        assert_eq!(Focus::Seconds30.range(timeline, NOW), Some((NOW - 30_000, NOW)));
        assert_eq!(Focus::All.range(timeline, NOW), None);
    }

    #[test]
    fn auto_follows_the_timeline_while_its_probes_are_in_memory() {
        let scrolled_back = (NOW - 1_200_000, NOW - 600_000);
        assert_eq!(Focus::Auto.range(scrolled_back, NOW), Some(scrolled_back));
        let hour = (NOW - 3_600_000, NOW);
        assert_eq!(Focus::Auto.range(hour, NOW), Some(hour));
        assert_eq!(Focus::Auto.range((NOW - 24 * 3_600_000, NOW), NOW), None);
    }
}
