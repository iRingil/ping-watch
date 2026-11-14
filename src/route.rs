//! State of the route to one target: per-hop addresses and statistics, and how many hops the route has.

use std::net::IpAddr;
use std::time::Duration;

use crate::stats::HopStats;

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
}

/// One hop of the route.
#[derive(Debug, Clone, PartialEq)]
pub struct Hop {
    /// TTL the hop is probed with; also its position in the route, starting at 1.
    pub ttl: u8,
    /// Address that answered most recently, `None` if the hop never answered.
    pub address: Option<IpAddr>,
    /// Accumulated statistics.
    pub stats: HopStats,
}

/// Route to one target, built from probe outcomes.
#[derive(Debug, Clone)]
pub struct Route {
    /// Address being traced.
    target: IpAddr,
    /// One entry per TTL from 1 to the maximum hop count.
    hops: Vec<Hop>,
    /// Smallest TTL at which the target itself answered: the route length.
    target_ttl: Option<u8>,
}

impl Route {
    /// Creates an empty route to `target` that probes TTLs `1..=max_hops`.
    pub fn new(target: IpAddr, max_hops: u8) -> Self {
        let hops = (1..=max_hops).map(|ttl| Hop { ttl, address: None, stats: HopStats::default() }).collect();
        Self { target, hops, target_ttl: None }
    }

    /// Address being traced.
    pub fn target(&self) -> IpAddr {
        self.target
    }

    /// Whether probes with `ttl` are still useful: TTLs beyond the target are not.
    pub fn should_probe(&self, ttl: u8) -> bool {
        self.target_ttl.is_none_or(|target_ttl| ttl <= target_ttl)
    }

    /// Hops up to the target, or all hops while the target hasn't answered yet.
    pub fn hops(&self) -> &[Hop] {
        let len = self.target_ttl.map_or(self.hops.len(), usize::from);
        self.hops.get(..len).unwrap_or(&self.hops)
    }

    /// Records the outcome of a probe sent with `ttl`; TTLs outside the route are ignored.
    pub fn record(&mut self, ttl: u8, outcome: ProbeOutcome) {
        let Some(hop) = usize::from(ttl).checked_sub(1).and_then(|index| self.hops.get_mut(index)) else {
            return;
        };
        match outcome {
            ProbeOutcome::Reply { from, rtt } => {
                hop.address = Some(from);
                hop.stats.record_reply(rtt);
                if from == self.target {
                    // The target answered: the route is at most this long.
                    self.target_ttl = Some(self.target_ttl.map_or(ttl, |known| known.min(ttl)));
                } else if self.target_ttl == Some(ttl) {
                    // A router answers where the target used to: the route got longer.
                    self.target_ttl = None;
                }
            }
            ProbeOutcome::Lost => hop.stats.record_loss(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use super::*;

    /// Address of the traced target in tests.
    const TARGET: IpAddr = IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8));
    /// Address of an intermediate router in tests.
    const ROUTER: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1));

    /// Reply outcome from `from` with a 10 ms round trip.
    fn reply(from: IpAddr) -> ProbeOutcome {
        ProbeOutcome::Reply { from, rtt: Duration::from_millis(10) }
    }

    #[test]
    fn all_hops_visible_until_target_answers() {
        let mut route = Route::new(TARGET, 30);
        route.record(1, reply(ROUTER));
        assert_eq!(route.hops().len(), 30);
        assert!(route.should_probe(30));
    }

    #[test]
    fn target_reply_cuts_route() {
        let mut route = Route::new(TARGET, 30);
        route.record(5, reply(TARGET));
        // Later TTLs also reach the target; they must not extend the route.
        route.record(7, reply(TARGET));
        assert_eq!(route.hops().len(), 5);
        assert!(route.should_probe(5));
        assert!(!route.should_probe(6));
    }

    #[test]
    fn shorter_route_replaces_longer() {
        let mut route = Route::new(TARGET, 30);
        route.record(7, reply(TARGET));
        route.record(5, reply(TARGET));
        assert_eq!(route.hops().len(), 5);
    }

    #[test]
    fn router_at_target_ttl_reopens_route() {
        let mut route = Route::new(TARGET, 30);
        route.record(5, reply(TARGET));
        route.record(5, reply(ROUTER));
        assert_eq!(route.hops().len(), 30);
        assert!(route.should_probe(30));
    }

    #[test]
    fn records_go_to_their_hop() {
        let mut route = Route::new(TARGET, 3);
        route.record(2, reply(ROUTER));
        route.record(2, ProbeOutcome::Lost);
        let hop = route.hops().get(1).expect("route has 3 hops");
        assert_eq!(hop.ttl, 2);
        assert_eq!(hop.address, Some(ROUTER));
        assert_eq!(hop.stats.sent(), 2);
        assert_eq!(hop.stats.received(), 1);
    }

    #[test]
    fn out_of_range_ttl_is_ignored() {
        let mut route = Route::new(TARGET, 3);
        route.record(0, ProbeOutcome::Lost);
        route.record(4, ProbeOutcome::Lost);
        assert!(route.hops().iter().all(|hop| hop.stats.sent() == 0));
    }
}
