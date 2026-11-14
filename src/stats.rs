//! Running statistics of one hop: the numbers shown in the hop table (sent, received, loss, best, average,
//! worst, last, jitter).

use std::time::Duration;

/// Accumulated probe results of a single hop.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HopStats {
    /// Number of probes sent.
    sent: u64,
    /// Number of probes answered.
    received: u64,
    /// Sum of all round-trip times, used for the average.
    total_rtt: Duration,
    /// Fastest round-trip time seen.
    best: Option<Duration>,
    /// Slowest round-trip time seen.
    worst: Option<Duration>,
    /// Round-trip time of the most recent answered probe.
    last: Option<Duration>,
    /// Sum of absolute differences between consecutive round-trip times, used for jitter.
    total_jitter: Duration,
    /// Number of differences summed in `total_jitter`.
    jitter_samples: u64,
}

impl HopStats {
    /// Records a probe that was answered after `rtt`.
    pub fn record_reply(&mut self, rtt: Duration) {
        self.sent = self.sent.saturating_add(1);
        self.received = self.received.saturating_add(1);
        self.total_rtt = self.total_rtt.saturating_add(rtt);
        self.best = Some(self.best.map_or(rtt, |best| best.min(rtt)));
        self.worst = Some(self.worst.map_or(rtt, |worst| worst.max(rtt)));
        if let Some(previous) = self.last {
            self.total_jitter = self.total_jitter.saturating_add(previous.abs_diff(rtt));
            self.jitter_samples = self.jitter_samples.saturating_add(1);
        }
        self.last = Some(rtt);
    }

    /// Records a probe that got no answer.
    pub fn record_loss(&mut self) {
        self.sent = self.sent.saturating_add(1);
    }

    /// Number of probes sent.
    pub fn sent(&self) -> u64 {
        self.sent
    }

    /// Number of probes answered.
    pub fn received(&self) -> u64 {
        self.received
    }

    /// Share of unanswered probes in percent, `0.0` before the first probe.
    #[expect(clippy::cast_precision_loss, reason = "counters stay far below 2^52")]
    pub fn loss_percent(&self) -> f64 {
        if self.sent == 0 {
            return 0.0;
        }
        let lost = self.sent.saturating_sub(self.received);
        lost as f64 * 100.0 / self.sent as f64
    }

    /// Fastest round-trip time, `None` until the first answer.
    pub fn best(&self) -> Option<Duration> {
        self.best
    }

    /// Slowest round-trip time, `None` until the first answer.
    pub fn worst(&self) -> Option<Duration> {
        self.worst
    }

    /// Most recent round-trip time, `None` until the first answer.
    pub fn last(&self) -> Option<Duration> {
        self.last
    }

    /// Mean round-trip time of answered probes, `None` until the first answer.
    pub fn average(&self) -> Option<Duration> {
        mean(self.total_rtt, self.received)
    }

    /// Mean absolute difference between consecutive round-trip times, `None` until two answers.
    pub fn jitter(&self) -> Option<Duration> {
        mean(self.total_jitter, self.jitter_samples)
    }
}

/// Divides `total` by `count`, `None` when `count` is zero.
fn mean(total: Duration, count: u64) -> Option<Duration> {
    let count = u32::try_from(count).ok()?;
    total.checked_div(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shorthand for a duration in milliseconds.
    fn ms(value: u64) -> Duration {
        Duration::from_millis(value)
    }

    #[test]
    fn empty_stats_have_no_values() {
        let stats = HopStats::default();
        assert_eq!(stats.loss_percent(), 0.0);
        assert_eq!(stats.average(), None);
        assert_eq!(stats.best(), None);
        assert_eq!(stats.jitter(), None);
    }

    #[test]
    fn replies_give_best_worst_last_and_average() {
        let mut stats = HopStats::default();
        for rtt in [30, 10, 20] {
            stats.record_reply(ms(rtt));
        }
        assert_eq!(stats.sent(), 3);
        assert_eq!(stats.received(), 3);
        assert_eq!(stats.best(), Some(ms(10)));
        assert_eq!(stats.worst(), Some(ms(30)));
        assert_eq!(stats.last(), Some(ms(20)));
        assert_eq!(stats.average(), Some(ms(20)));
    }

    #[test]
    fn losses_count_as_sent_only() {
        let mut stats = HopStats::default();
        stats.record_reply(ms(10));
        stats.record_loss();
        stats.record_loss();
        stats.record_loss();
        assert_eq!(stats.sent(), 4);
        assert_eq!(stats.received(), 1);
        assert_eq!(stats.loss_percent(), 75.0);
        assert_eq!(stats.average(), Some(ms(10)));
    }

    #[test]
    fn jitter_is_mean_difference_between_consecutive_replies() {
        let mut stats = HopStats::default();
        stats.record_reply(ms(10));
        assert_eq!(stats.jitter(), None);
        stats.record_reply(ms(30)); // diff 20
        stats.record_loss(); // losses don't break the chain
        stats.record_reply(ms(20)); // diff 10
        assert_eq!(stats.jitter(), Some(ms(15)));
    }
}
