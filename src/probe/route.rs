//! State of the route to one target: per-hop addresses, statistics and recent probes, and how many hops the route
//! has. [`RouteState`] is what the tracer updates; [`Route`] is a snapshot of it for the UI, with statistics over
//! the chosen focus period.

use std::collections::VecDeque;
use std::net::IpAddr;
use std::time::Duration;

use crate::probe::stats::HopStats;

/// Largest TTL probed: routes up to this long are seen whole (how many hops are shown is a separate setting).
pub const MAX_TTL: u8 = 40;

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
}

impl RouteState {
    /// Creates an empty route to `target` that probes the TTLs from 1 to `max_ttl`.
    pub fn new(target: IpAddr, max_ttl: u8) -> Self {
        Self { target, hops: vec![HopState::default(); usize::from(max_ttl)], target_ttl: None, started_ms: None }
    }

    /// Last TTL of the route: the target's once it answered; while it doesn't (unreachable since the trace
    /// started), one past the farthest hop that answered — where the target would stand, as PingPlotter shows it,
    /// instead of rows of silent TTLs up to the largest probed.
    fn end(&self) -> u8 {
        self.target_ttl.unwrap_or_else(|| {
            // The farthest answering hop is at index `i`, TTL `i + 1`; the end is the TTL after it.
            let end = self.hops.iter().rposition(|hop| hop.address.is_some()).map_or(1, |i| i.saturating_add(2));
            u8::try_from(end.min(self.hops.len())).unwrap_or(u8::MAX)
        })
    }

    /// Whether probes with `ttl` are the ones meant for the target: the route length, or, while the target doesn't
    /// answer, the TTL standing for it ([`Self::end`]), so the target's history records its losses.
    pub fn is_target_ttl(&self, ttl: u8) -> bool {
        self.end() == ttl
    }

    /// Whether `ttl` lies on the route as shown: its probes belong in the history; the silent TTLs past the end
    /// are only noise.
    pub fn in_route(&self, ttl: u8) -> bool {
        ttl <= self.end()
    }

    /// Whether probes with `ttl` are still useful: TTLs beyond the target are not.
    pub fn should_probe(&self, ttl: u8) -> bool {
        self.target_ttl.is_none_or(|target_ttl| ttl <= target_ttl)
    }

    /// Records the outcome of a probe sent with `ttl` at `at_ms` (Unix ms); TTLs outside the route and beyond the
    /// target are ignored.
    pub fn record(&mut self, ttl: u8, at_ms: i64, outcome: ProbeOutcome) {
        if self.target_ttl.is_some_and(|target_ttl| ttl > target_ttl) {
            return;
        }
        let Some(hop) = usize::from(ttl).checked_sub(1).and_then(|index| self.hops.get_mut(index)) else {
            return;
        };
        self.started_ms = Some(self.started_ms.map_or(at_ms, |started| started.min(at_ms)));
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
                Hop { ttl, address: hop.address, stats }
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
        // Every TTL is still probed: a silent hop may hide routers and the target behind it.
        assert!(route.should_probe(30));
        assert!(route.in_route(4));
        assert!(!route.in_route(5));
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
        assert!(route.should_probe(5));
        assert!(!route.should_probe(6));
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
        assert!(route.should_probe(30));
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
