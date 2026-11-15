//! Route change detection: the answering hops of a target are compared from check to check, and a different route
//! counts only once it has held for [`STABLE_CHECKS`] checks in a row — some providers balance traffic over
//! several routers, so one hop answers from two addresses in turn, and that is not a change.

use std::net::IpAddr;

use crate::i18n::{Language, tr, trf};

/// Checks (one a second) a new route must hold before it counts.
pub const STABLE_CHECKS: u32 = 10;

/// The answering hops of a route: TTL and address, in order. Hops without an answer are left out, so a hop that
/// sometimes stays silent doesn't look like a change.
pub type Path = Vec<(u8, IpAddr)>;

/// A route change that held.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteChange {
    /// The route before.
    pub before: Path,
    /// The route now.
    pub after: Path,
}

impl RouteChange {
    /// The change as a phrase: "hop 3 10.74.232.97 → 192.168.1.1, hops 12 → 11".
    pub fn describe(&self, language: Language) -> String {
        let mut parts = Vec::new();
        let first_difference = self.before.iter().zip(&self.after).find(|(old, new)| old != new);
        match first_difference {
            Some(((old_ttl, old_ip), (new_ttl, new_ip))) if old_ttl == new_ttl => {
                parts.push(trf!(language, "hop {ttl} {old} \u{2192} {new}", ttl = old_ttl, old = old_ip, new = new_ip));
            }
            Some(((_, old_ip), (new_ttl, new_ip))) => {
                parts.push(trf!(language, "hop {ttl} {old} \u{2192} {new}", ttl = new_ttl, old = old_ip, new = new_ip));
            }
            None => {}
        }
        let (old_len, new_len) = (hop_count(&self.before), hop_count(&self.after));
        if old_len != new_len {
            parts.push(trf!(language, "hops {old} \u{2192} {new}", old = old_len, new = new_len));
        }
        if parts.is_empty() { tr!(language, "the route changed").to_owned() } else { parts.join(", ") }
    }
}

/// Number of hops of a path: the TTL of its last answering hop.
fn hop_count(path: &Path) -> u8 {
    path.last().map_or(0, |(ttl, _)| *ttl)
}

/// Route watch of one target.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RouteWatch {
    /// The route known to hold, `None` before the first check.
    stable: Option<Path>,
    /// A different route seen lately, with how many checks in a row it has held.
    candidate: Option<(Path, u32)>,
}

impl RouteWatch {
    /// Takes the route seen at this check; returns the change once a different route has held long enough. An
    /// empty path (nothing answers) is ignored: that is the "no answer" condition's business.
    pub fn check(&mut self, path: Path) -> Option<RouteChange> {
        if path.is_empty() {
            return None;
        }
        let Some(stable) = &self.stable else {
            self.stable = Some(path);
            return None;
        };
        if *stable == path {
            self.candidate = None;
            return None;
        }
        let held = match &self.candidate {
            Some((candidate, held)) if *candidate == path => held.saturating_add(1),
            _ => 1,
        };
        if held < STABLE_CHECKS {
            self.candidate = Some((path, held));
            return None;
        }
        self.candidate = None;
        let before = self.stable.replace(path.clone()).unwrap_or_default();
        Some(RouteChange { before, after: path })
    }
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use super::*;

    /// Address 10.0.0.`last`.
    fn ip(last: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(10, 0, 0, last))
    }

    /// A path of hops 1, 2, … answering from `lasts`.
    fn path(lasts: &[u8]) -> Path {
        (1..).zip(lasts.iter().map(|last| ip(*last))).collect()
    }

    #[test]
    fn a_new_route_counts_once_it_holds() {
        let mut watch = RouteWatch::default();
        assert_eq!(watch.check(path(&[1, 2, 3])), None, "the first route is just remembered");
        for _ in 1..STABLE_CHECKS {
            assert_eq!(watch.check(path(&[1, 5, 3])), None);
        }
        let change = watch.check(path(&[1, 5, 3])).expect("held long enough");
        assert_eq!(change.describe(Language::English), "hop 2 10.0.0.2 \u{2192} 10.0.0.5");
        assert_eq!(watch.check(path(&[1, 5, 3])), None, "reported once");
    }

    #[test]
    fn alternating_routers_are_not_a_change() {
        let mut watch = RouteWatch::default();
        watch.check(path(&[1, 2, 3]));
        for round in 0..(STABLE_CHECKS * 3) {
            let hop2 = if round % 2 == 0 { 5 } else { 2 };
            assert_eq!(watch.check(path(&[1, hop2, 3])), None);
        }
    }

    #[test]
    fn silence_and_shorter_routes() {
        let mut watch = RouteWatch::default();
        watch.check(path(&[1, 2, 3, 4]));
        assert_eq!(watch.check(Vec::new()), None, "nothing answering is no route");
        let mut change = None;
        for _ in 0..STABLE_CHECKS {
            change = watch.check(path(&[1, 2, 4]));
        }
        assert_eq!(
            change.map(|change| change.describe(Language::English)),
            Some("hop 3 10.0.0.3 \u{2192} 10.0.0.4, hops 4 \u{2192} 3".to_owned())
        );
    }
}
