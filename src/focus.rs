//! Focus: the period the hop table and the latency bars summarize, as in PingPlotter.

use std::time::Duration;

use crate::route::RECENT_WINDOW;

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
    /// Same period as the timeline below, if the probes are still kept in memory; otherwise everything.
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

    /// The period to summarize, `None` for everything; `timeline` is the period the timeline shows.
    pub fn window(self, timeline: Duration) -> Option<Duration> {
        let seconds = match self {
            Self::Seconds5 => 5,
            Self::Seconds30 => 30,
            Self::Seconds60 => 60,
            Self::Minutes5 => 5 * 60,
            Self::Minutes30 => 30 * 60,
            Self::Minutes60 => 60 * 60,
            Self::All => return None,
            Self::Auto => return (timeline <= RECENT_WINDOW).then_some(timeline),
        };
        Some(Duration::from_secs(seconds))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_choices_ignore_the_timeline() {
        assert_eq!(Focus::Seconds30.window(Duration::from_secs(600)), Some(Duration::from_secs(30)));
        assert_eq!(Focus::All.window(Duration::from_secs(600)), None);
    }

    #[test]
    fn auto_follows_the_timeline_while_probes_are_in_memory() {
        assert_eq!(Focus::Auto.window(Duration::from_secs(600)), Some(Duration::from_secs(600)));
        assert_eq!(Focus::Auto.window(RECENT_WINDOW), Some(RECENT_WINDOW));
        assert_eq!(Focus::Auto.window(Duration::from_secs(24 * 3_600)), None);
    }
}
