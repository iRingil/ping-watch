//! State of the route to one target: per-hop addresses, statistics and recent probes, and how many hops the route
//! has. [`RouteState`] is what the tracer updates; [`Route`] is a snapshot of it for the UI, with statistics over
//! the chosen focus period.

use std::collections::VecDeque;
use std::net::IpAddr;
use std::time::Duration;

use crate::probe::stats::HopStats;

/// Largest TTL probed: routes up to this long are seen whole (how many hops are shown is a separate setting).
pub const MAX_TTL: u8 = 40;

/// While the target doesn't answer, the TTLs up to this many past the farthest hop that answered are probed every
/// interval; the farther ones take turns, one per interval, so a silent target isn't sent every TTL each time.
pub const NEAR_EXTRA: u8 = 3;

/// How far back each hop keeps individual probes: the longest focus period that isn't "all".
pub const RECENT_WINDOW: Duration = Duration::from_secs(60 * 60);

/// Result of one probe sent with a given TTL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeOutcome {
    /// Someone answered: a router on the way (TTL expired) or the target itself.
    Reply {
        /// Address that sent the answer.
        from: IpAddr,
        /// Measured round-trip time.
        rtt: Duration,
    },
    /// No answer within the timeout, or an error answer.
    Lost,
}

/// Outcome of one probe with its time and TTL, as stored in the history.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sample {
    /// When the probe was sent, in milliseconds since the Unix epoch.
    pub at_ms: i64,
    /// TTL the probe was sent with (the hop number).
    pub ttl: u8,
    /// What came back.
    pub outcome: ProbeOutcome,
    /// Whether the probe was meant for the target itself: its TTL was the route length when it was recorded.
    /// The target's history is read by this flag, so it survives the route getting longer or shorter.
    pub at_target: bool,
}

/// One hop of a route snapshot.
#[derive(Debug, Clone, PartialEq)]
pub struct Hop {
    /// TTL the hop is probed with; also its position in the route, starting at 1.
    pub ttl: u8,
    /// Address that answered most recently, `None` if the hop never answered.
    pub address: Option<IpAddr>,
    /// Statistics over the snapshot's focus period.
    pub stats: HopStats,
    /// Send time of the hop's first probe still counted, Unix ms: since the trace started, or since the hop was
    /// forgotten as lying past the target; `None` before its first probe.
    pub since_ms: Option<i64>,
}

/// Snapshot of a route for display.
#[derive(Debug, Clone, PartialEq)]
pub struct Route {
    /// Address being traced.
    target: IpAddr,
    /// Hops up to the target, or, while the target doesn't answer, up to one past the farthest hop that answered.
    hops: Vec<Hop>,
    /// Start of the period the statistics cover, Unix ms; `None` before the first probe.
    from_ms: Option<i64>,
    /// End of the period the statistics cover, Unix ms; `None` for "until now".
    to_ms: Option<i64>,
}

impl Route {
    /// Address being traced.
    pub fn target(&self) -> IpAddr {
        self.target
    }

    /// Hops up to the target, or, while the target doesn't answer, up to one past the farthest hop that answered.
    pub fn hops(&self) -> &[Hop] {
        &self.hops
    }

    /// How many hops are hidden so that at most `limit` are shown: those right after the first hop (the router
    /// telling whether we are online at all), as the provider's hops matter less than the end of the route.
    pub fn hidden(&self, limit: u8) -> usize {
        self.hops.len().saturating_sub(usize::from(limit.max(2)))
    }

    /// The hops shown under `limit`, before and after the hidden ones: the first hop and the last `limit - 1`,
    /// or the whole route and nothing if it fits.
    pub fn shown(&self, limit: u8) -> (&[Hop], &[Hop]) {
        let hidden = self.hidden(limit);
        if hidden == 0 {
            return (&self.hops, &[]);
        }
        let head = self.hops.get(..1).unwrap_or_default();
        let tail = self.hops.get(hidden.saturating_add(1)..).unwrap_or_default();
        (head, tail)
    }

    /// Start of the period the statistics cover, Unix ms; `None` before the first probe.
    pub fn focus_start_ms(&self) -> Option<i64> {
        self.from_ms
    }

    /// End of the period the statistics cover, Unix ms; `None` for "until now".
    pub fn focus_end_ms(&self) -> Option<i64> {
        self.to_ms
    }
}

/// Live state of one hop.
#[derive(Debug, Clone, Default)]
struct HopState {
    /// Address that answered most recently.
    address: Option<IpAddr>,
    /// Statistics since the trace started.
    total: HopStats,
    /// Probes of the last [`RECENT_WINDOW`]: send time (Unix ms) and round trip, `None` if lost. Oldest first.
    recent: VecDeque<(i64, Option<Duration>)>,
    /// Send time of the first probe recorded in this state, Unix ms.
    since_ms: Option<i64>,
}

/// Live state of the route to one target, built from probe outcomes.
#[derive(Debug, Clone)]
pub struct RouteState {
    /// Address being traced.
    target: IpAddr,
    /// One entry per TTL from 1 to the largest probed.
    hops: Vec<HopState>,
    /// Smallest TTL at which the target itself answered: the route length.
    target_ttl: Option<u8>,
    /// Send time of the first recorded probe, Unix ms.
    started_ms: Option<i64>,
    /// Samples waiting for the TTLs before them to be probed (the first round): only then is it known whether they
    /// lie on the route.
    pending: Vec<Sample>,
    /// The far TTL whose turn is next while the target doesn't answer ([`NEAR_EXTRA`]); 0 before the first turn.
    next_far: u8,
    /// When the next far TTL may be probed, Unix ms.
    far_due_ms: i64,
}

impl RouteState {
    /// Creates an empty route to `target` that probes the TTLs from 1 to `max_ttl`.
    pub fn new(target: IpAddr, max_ttl: u8) -> Self {
        Self {
            target,
            hops: vec![HopState::default(); usize::from(max_ttl)],
            target_ttl: None,
            started_ms: None,
            pending: Vec::new(),
            next_far: 0,
            far_due_ms: i64::MIN,
        }
    }

    /// Last TTL of the route: the target's once it answered; while it doesn't (unreachable since the trace
    /// started), one past the farthest hop that answered — where the target would stand, as PingPlotter shows it,
    /// instead of rows of silent TTLs up to the largest probed.
    fn end(&self) -> u8 {
        self.target_ttl.unwrap_or_else(|| self.farthest_answer().saturating_add(1).min(self.max_ttl()))
    }

    /// TTL of the farthest hop that answered, 0 if none did.
    fn farthest_answer(&self) -> u8 {
        // The farthest answering hop is at index `i`, TTL `i + 1`.
        let ttl = self.hops.iter().rposition(|hop| hop.address.is_some()).map_or(0, |i| i.saturating_add(1));
        u8::try_from(ttl).unwrap_or(u8::MAX)
    }

    /// Largest TTL probed.
    fn max_ttl(&self) -> u8 {
        u8::try_from(self.hops.len()).unwrap_or(u8::MAX)
    }

    /// Whether probes with `ttl` are the ones meant for the target: the route length, or, while the target doesn't
    /// answer, the TTL standing for it ([`Self::end`]), so the target's history records its losses.
    pub fn is_target_ttl(&self, ttl: u8) -> bool {
        self.end() == ttl
    }

    /// Whether every TTL before `ttl` has been probed: only then is it known whether `ttl` lies on the route.
    fn heard_before(&self, ttl: u8) -> bool {
        self.hops.iter().take(usize::from(ttl).saturating_sub(1)).all(|hop| hop.total.sent() > 0)
    }

    /// Whether the worker of `ttl` probes now, at `at_ms` (Unix ms), with `interval_ms` between its probes. TTLs
    /// beyond the target are not probed. While the target doesn't answer, every TTL is probed once (the first
    /// round), then those up to [`NEAR_EXTRA`] past the farthest answer every interval and the farther ones in
    /// turns, one per interval: a silent hop may hide routers and the target behind it, and a turn finds them.
    pub fn take_turn(&mut self, ttl: u8, at_ms: i64, interval_ms: i64) -> bool {
        if let Some(target_ttl) = self.target_ttl {
            return ttl <= target_ttl;
        }
        let Some(hop) = usize::from(ttl).checked_sub(1).and_then(|index| self.hops.get(index)) else {
            return false;
        };
        let near_end = self.farthest_answer().saturating_add(NEAR_EXTRA);
        if hop.total.sent() == 0 || ttl <= near_end {
            return true;
        }
        let first_far = near_end.saturating_add(1);
        let next = if (first_far..=self.max_ttl()).contains(&self.next_far) { self.next_far } else { first_far };
        self.next_far = next;
        if ttl != next || at_ms < self.far_due_ms {
            return false;
        }
        self.next_far = if next >= self.max_ttl() { first_far } else { next.saturating_add(1) };
        self.far_due_ms = at_ms.saturating_add(interval_ms);
        true
    }

    /// Records the outcome of a probe sent with `ttl` at `at_ms` (Unix ms) and returns the samples that now belong
    /// in the history: those on the route as shown, once every TTL before them has been probed. The silent TTLs
    /// past the end are only noise; so is a target's answer at a far TTL in the first round, before the nearer
    /// TTLs (one of which the target will answer too) are heard from. TTLs outside the route and beyond the target
    /// are ignored.
    pub fn record(&mut self, ttl: u8, at_ms: i64, outcome: ProbeOutcome) -> Vec<Sample> {
        if self.target_ttl.is_some_and(|target_ttl| ttl > target_ttl) {
            return Vec::new();
        }
        let Some(hop) = usize::from(ttl).checked_sub(1).and_then(|index| self.hops.get_mut(index)) else {
            return Vec::new();
        };
        self.started_ms = Some(self.started_ms.map_or(at_ms, |started| started.min(at_ms)));
        hop.since_ms = Some(hop.since_ms.map_or(at_ms, |since| since.min(at_ms)));
        let mut cut = false;
        let rtt = match outcome {
            ProbeOutcome::Reply { from, rtt } => {
                hop.address = Some(from);
                hop.total.record_reply(rtt);
                if from == self.target {
                    // The target answered: the route is at most this long. TTLs beyond it are forgotten (they
                    // reached the target too), so they don't come back if the route gets longer.
                    if self.target_ttl.is_none_or(|known| ttl < known) {
                        self.target_ttl = Some(ttl);
                        cut = true;
                    }
                } else if self.target_ttl == Some(ttl) {
                    // A router answers where the target used to: the route got longer.
                    self.target_ttl = None;
                }
                Some(rtt)
            }
            ProbeOutcome::Lost => {
                hop.total.record_loss();
                None
            }
        };
        hop.recent.push_back((at_ms, rtt));
        let horizon = at_ms.saturating_sub(millis(RECENT_WINDOW));
        while hop.recent.front().is_some_and(|(sent_ms, _)| *sent_ms < horizon) {
            hop.recent.pop_front();
        }
        if cut {
            self.hops.iter_mut().skip(usize::from(ttl)).for_each(|hop| *hop = HopState::default());
        }
        self.pending.push(Sample { at_ms, ttl, outcome, at_target: false });
        self.settle()
    }

    /// Takes the pending samples whose turn has come ([`Self::record`]); those past the target are dropped.
    fn settle(&mut self) -> Vec<Sample> {
        let mut ready = Vec::new();
        for sample in std::mem::take(&mut self.pending) {
            if self.target_ttl.is_some_and(|target_ttl| sample.ttl > target_ttl) {
                continue;
            }
            if !self.heard_before(sample.ttl) {
                self.pending.push(sample);
            } else if sample.ttl <= self.end() {
                ready.push(Sample { at_target: self.is_target_ttl(sample.ttl), ..sample });
            }
        }
        ready
    }

    /// Snapshot with statistics over `focus` = `(from, to)` Unix ms, or since the start if `None`. Only the last
    /// [`RECENT_WINDOW`] of probes is kept, so an older range covers less.
    pub fn snapshot(&self, focus: Option<(i64, i64)>) -> Route {
        let len = usize::from(self.end());
        let since_ms = focus.map(|(from, _)| from);
        let until_ms = focus.map_or(i64::MAX, |(_, to)| to);
        let hops = self
            .hops
            .iter()
            .take(len)
            .zip(1..)
            .map(|(hop, ttl)| {
                let stats = match since_ms {
                    None => hop.total.clone(),
                    Some(since) => HopStats::from_probes(
                        hop.recent
                            .iter()
                            .filter(|(sent_ms, _)| (since..=until_ms).contains(sent_ms))
                            .map(|(_, rtt)| *rtt),
                    ),
                };
                Hop { ttl, address: hop.address, stats, since_ms: hop.since_ms }
            })
            .collect();
        let from_ms = match (since_ms, self.started_ms) {
            (Some(since), Some(started)) => Some(since.max(started)),
            (None, started) => started,
            (Some(_), None) => None,
        };
        Route { target: self.target, hops, from_ms, to_ms: focus.map(|(_, to)| to) }
    }
}

/// Duration in whole milliseconds, saturating.
pub fn millis(duration: Duration) -> i64 {
    i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use super::*;

    /// Address of the traced target in tests.
    const TARGET: IpAddr = IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8));
    /// Address of an intermediate router in tests.
    const ROUTER: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1));

    /// Reply outcome from `from` with a round trip of `ms` milliseconds.
    fn reply_ms(from: IpAddr, ms: u64) -> ProbeOutcome {
        ProbeOutcome::Reply { from, rtt: Duration::from_millis(ms) }
    }

    /// Reply outcome from `from` with a 10 ms round trip.
    fn reply(from: IpAddr) -> ProbeOutcome {
        reply_ms(from, 10)
    }

    /// Snapshot with all-time statistics.
    fn all(route: &RouteState) -> Route {
        route.snapshot(None)
    }

    #[test]
    fn unreachable_route_ends_one_past_the_farthest_answer() {
        let mut route = RouteState::new(TARGET, 30);
        assert_eq!(all(&route).hops().len(), 1);
        route.record(1, 0, reply(ROUTER));
        route.record(3, 0, reply(ROUTER));
        route.record(2, 0, ProbeOutcome::Lost);
        assert_eq!(all(&route).hops().len(), 4);
        assert_eq!(ttls(&route.record(4, 0, ProbeOutcome::Lost)), [4], "the end of the route is in the history");
        assert!(route.record(5, 0, ProbeOutcome::Lost).is_empty(), "past the end it isn't");
    }

    /// TTLs of `samples`.
    fn ttls(samples: &[Sample]) -> Vec<u8> {
        samples.iter().map(|sample| sample.ttl).collect()
    }

    #[test]
    fn a_far_answer_of_the_target_waits_for_the_nearer_hops() {
        let mut route = RouteState::new(TARGET, 30);
        // The first round: the target answers at 27 before the TTLs below it report.
        assert!(route.record(27, 0, reply(TARGET)).is_empty(), "not in the history yet");
        for ttl in 1..10 {
            assert_eq!(ttls(&route.record(ttl, 0, reply(ROUTER))), [ttl]);
        }
        let at_ten = route.record(10, 0, reply(TARGET));
        assert_eq!(ttls(&at_ten), [10], "the far answer is past the route");
        assert!(at_ten.first().is_some_and(|sample| sample.at_target));
    }

    #[test]
    fn answers_behind_a_silent_hop_wait_for_it_in_the_first_round() {
        let mut route = RouteState::new(TARGET, 30);
        assert_eq!(ttls(&route.record(1, 0, reply(ROUTER))), [1]);
        // Hop 2 is silent and times out last; the hops behind it answered before.
        assert!(route.record(3, 0, reply(ROUTER)).is_empty());
        assert!(route.record(4, 0, reply(TARGET)).is_empty());
        let released = route.record(2, 0, ProbeOutcome::Lost);
        assert_eq!(ttls(&released), [3, 4, 2]);
        let at_target: Vec<bool> = released.iter().map(|sample| sample.at_target).collect();
        assert_eq!(at_target, [false, true, false]);
    }

    /// The TTLs of `route` up to 10 that take their turn at `at_ms`, with probes every second.
    fn round(route: &mut RouteState, at_ms: i64) -> Vec<u8> {
        (1..=10).filter(|ttl| route.take_turn(*ttl, at_ms, 1_000)).collect()
    }

    #[test]
    fn a_silent_target_gets_the_far_ttls_in_turns() {
        let mut route = RouteState::new(TARGET, 10);
        // The first round probes every TTL; hops 1 and 2 answer.
        assert_eq!(round(&mut route, 0), (1..=10).collect::<Vec<_>>());
        route.record(1, 0, reply(ROUTER));
        route.record(2, 0, reply(ROUTER));
        for ttl in 3..=10 {
            route.record(ttl, 0, ProbeOutcome::Lost);
        }
        // Hops up to 2 + 3 every interval; of 6 to 10 one per interval, in turn.
        let far: Vec<Vec<u8>> = (1..=6).map(|i| round(&mut route, i * 1_000)).collect();
        let near = [1, 2, 3, 4, 5];
        let with = |ttl: u8| near.iter().copied().chain([ttl]).collect::<Vec<_>>();
        assert_eq!(far, [with(6), with(7), with(8), with(9), with(10), with(6)]);
        // A turn too early waits for the next interval.
        assert_eq!(round(&mut route, 6_500), near);
        // Once the target answers, every TTL up to it is probed, none past it.
        route.record(4, 7_000, reply(TARGET));
        assert_eq!(round(&mut route, 8_000), [1, 2, 3, 4]);
    }

    #[test]
    fn hops_beyond_the_target_are_forgotten() {
        let mut route = RouteState::new(TARGET, 30);
        route.record(9, 0, reply(TARGET));
        route.record(5, 0, reply(TARGET));
        route.record(7, 0, reply(TARGET));
        // The route got longer: hop 5 is a router now; the old replies at 7 and 9 don't stretch it.
        route.record(5, 0, reply(ROUTER));
        assert_eq!(all(&route).hops().len(), 6);
    }

    #[test]
    fn a_forgotten_hop_counts_from_its_next_probe() {
        let mut route = RouteState::new(TARGET, 30);
        route.record(6, 1_000, ProbeOutcome::Lost);
        route.record(5, 2_000, reply(TARGET));
        // The route got longer again: hop 6 starts afresh, not at the trace's start.
        route.record(5, 60_000, reply(ROUTER));
        route.record(6, 61_000, ProbeOutcome::Lost);
        let snapshot = all(&route);
        let since = |ttl: usize| snapshot.hops().get(ttl - 1).and_then(|hop| hop.since_ms);
        assert_eq!(since(5), Some(2_000));
        assert_eq!(since(6), Some(61_000));
    }

    #[test]
    fn the_first_hops_are_hidden_beyond_the_limit() {
        let mut route = RouteState::new(TARGET, 30);
        route.record(22, 0, reply(TARGET));
        let snapshot = all(&route);
        let ttls = |(head, tail): (&[Hop], &[Hop])| head.iter().chain(tail).map(|hop| hop.ttl).collect::<Vec<_>>();
        assert_eq!(snapshot.hidden(20), 2);
        let shown = ttls(snapshot.shown(20));
        assert_eq!(shown.len(), 20);
        assert_eq!(shown.get(..2), Some(&[1, 4][..]), "the first hop stays, hops 2 and 3 are hidden");
        assert_eq!(shown.last(), Some(&22));
        assert_eq!(snapshot.hidden(30), 0);
        assert_eq!(ttls(snapshot.shown(30)).len(), 22);
    }

    #[test]
    fn target_reply_cuts_route() {
        let mut route = RouteState::new(TARGET, 30);
        route.record(5, 0, reply(TARGET));
        // Later TTLs also reach the target; they must not extend the route.
        route.record(7, 0, reply(TARGET));
        assert_eq!(all(&route).hops().len(), 5);
        assert!(route.take_turn(5, 0, 1_000));
        assert!(!route.take_turn(6, 0, 1_000));
    }

    #[test]
    fn shorter_route_replaces_longer() {
        let mut route = RouteState::new(TARGET, 30);
        route.record(7, 0, reply(TARGET));
        route.record(5, 0, reply(TARGET));
        assert_eq!(all(&route).hops().len(), 5);
    }

    #[test]
    fn router_at_target_ttl_reopens_route() {
        let mut route = RouteState::new(TARGET, 30);
        route.record(5, 0, reply(TARGET));
        route.record(5, 0, reply(ROUTER));
        assert_eq!(all(&route).hops().len(), 6);
        assert!(route.take_turn(30, 0, 1_000), "not probed since it was forgotten");
    }

    #[test]
    fn the_target_ttl_follows_the_farthest_answer_until_the_target_answers() {
        let mut route = RouteState::new(TARGET, 30);
        assert!(route.is_target_ttl(1));
        route.record(3, 0, reply(ROUTER));
        assert!(route.is_target_ttl(4));
        assert!(!route.is_target_ttl(30));
        route.record(5, 0, reply(TARGET));
        assert!(route.is_target_ttl(5));
        assert!(!route.is_target_ttl(4));
    }

    #[test]
    fn records_go_to_their_hop() {
        let mut route = RouteState::new(TARGET, 3);
        route.record(2, 0, reply(ROUTER));
        route.record(2, 0, ProbeOutcome::Lost);
        let snapshot = all(&route);
        let hop = snapshot.hops().get(1).expect("route has 3 hops");
        assert_eq!(hop.ttl, 2);
        assert_eq!(hop.address, Some(ROUTER));
        assert_eq!(hop.stats.sent(), 2);
        assert_eq!(hop.stats.received(), 1);
    }

    #[test]
    fn out_of_range_ttl_is_ignored() {
        let mut route = RouteState::new(TARGET, 3);
        route.record(0, 0, ProbeOutcome::Lost);
        route.record(4, 0, ProbeOutcome::Lost);
        assert!(all(&route).hops().iter().all(|hop| hop.stats.sent() == 0));
    }

    #[test]
    fn focus_limits_statistics_to_recent_probes() {
        let mut route = RouteState::new(TARGET, 1);
        route.record(1, 1_000, reply_ms(TARGET, 100));
        route.record(1, 9_000, reply_ms(TARGET, 10));
        route.record(1, 10_000, ProbeOutcome::Lost);

        let recent = route.snapshot(Some((5_000, 10_000)));
        let stats = &recent.hops().first().expect("one hop").stats;
        assert_eq!((stats.sent(), stats.received()), (2, 1));
        assert_eq!(stats.worst(), Some(Duration::from_millis(10)));
        assert_eq!(recent.focus_start_ms(), Some(5_000));

        let everything = route.snapshot(None);
        assert_eq!(everything.hops().first().map(|hop| hop.stats.sent()), Some(3));
        assert_eq!(everything.focus_start_ms(), Some(1_000));
    }

    #[test]
    fn focus_never_starts_before_the_first_probe() {
        let mut route = RouteState::new(TARGET, 1);
        route.record(1, 9_000, reply(TARGET));
        assert_eq!(route.snapshot(Some((0, 10_000))).focus_start_ms(), Some(9_000));
    }

    #[test]
    fn focus_range_can_lie_in_the_past() {
        let mut route = RouteState::new(TARGET, 1);
        for at_ms in [1_000, 2_000, 3_000, 4_000] {
            route.record(1, at_ms, reply(TARGET));
        }
        let past = route.snapshot(Some((1_500, 3_000)));
        assert_eq!(past.hops().first().map(|hop| hop.stats.sent()), Some(2));
        assert_eq!(past.focus_end_ms(), Some(3_000));
    }

    #[test]
    fn old_probes_leave_the_recent_window() {
        let mut route = RouteState::new(TARGET, 1);
        let window = millis(RECENT_WINDOW);
        route.record(1, 0, ProbeOutcome::Lost);
        route.record(1, window.saturating_add(1), reply(TARGET));
        let snapshot = route.snapshot(Some((i64::MIN, window.saturating_add(1))));
        assert_eq!(snapshot.hops().first().map(|hop| hop.stats.sent()), Some(1));
    }
}
