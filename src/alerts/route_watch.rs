//! Route change detection: the answering hops of a target are compared from check to check, and a different route
//! counts only once it has held for [`STABLE_CHECKS`] checks in a row. Some providers balance traffic over several
//! routers, so a hop answers from two addresses in turn, each for longer than that; a route seen before (one of the
//! last [`KNOWN_ROUTES`]) is therefore no change either. A hop that didn't answer on one of the routes compared is
//! not compared at all.

use std::net::IpAddr;

use crate::i18n::{Language, tr, trf};

/// Checks (one a second) a new route must hold before it counts.
pub const STABLE_CHECKS: u32 = 10;
/// Routes remembered per target; a return to one of them is load balancing, not a change.
const KNOWN_ROUTES: usize = 8;

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
    /// What changed, without words: the first hop answering from another address and the new hop count.
    pub fn note(&self) -> RouteNote {
        let hop = self.before.iter().find_map(|(ttl, old)| {
            let new = address_at(&self.after, *ttl)?;
            (new != *old).then_some((*ttl, *old, new))
        });
        let (old_len, new_len) = (hop_count(&self.before), hop_count(&self.after));
        RouteNote { hop, hops: (old_len != new_len).then_some((old_len, new_len)) }
    }
}

/// What a route change was, kept language-neutral (the journal stores it and words it when shown).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RouteNote {
    /// The first hop answering from another address: TTL, old and new address.
    pub hop: Option<(u8, IpAddr, IpAddr)>,
    /// The hop count before and after, if it changed.
    pub hops: Option<(u8, u8)>,
}

impl RouteNote {
    /// The change as a phrase: "hop 3 10.74.232.97 → 192.168.1.1, hops 12 → 11".
    pub fn describe(&self, language: Language) -> String {
        let mut parts = Vec::new();
        if let Some((ttl, old, new)) = self.hop {
            parts.push(trf!(language, "hop {ttl} {old} \u{2192} {new}", ttl = ttl, old = old, new = new));
        }
        if let Some((old, new)) = self.hops {
            parts.push(trf!(language, "hops {old} \u{2192} {new}", old = old, new = new));
        }
        if parts.is_empty() { tr!(language, "the route changed").to_owned() } else { parts.join(", ") }
    }

    /// The note as stored: "hop 3 10.0.0.1 10.0.0.2;hops 12 11", each part only if there is one.
    pub fn to_text(self) -> String {
        let mut parts = Vec::new();
        if let Some((ttl, old, new)) = self.hop {
            parts.push(format!("hop {ttl} {old} {new}"));
        }
        if let Some((old, new)) = self.hops {
            parts.push(format!("hops {old} {new}"));
        }
        parts.join(";")
    }

    /// The note stored as `text` (see [`Self::to_text`]); unreadable parts are left out.
    pub fn from_text(text: &str) -> Self {
        let mut note = Self { hop: None, hops: None };
        for part in text.split(';') {
            let words: Vec<&str> = part.split_whitespace().collect();
            match words.as_slice() {
                ["hop", ttl, old, new] => {
                    note.hop =
                        ttl.parse().ok().zip(old.parse().ok()).zip(new.parse().ok()).map(|((t, o), n)| (t, o, n));
                }
                ["hops", old, new] => note.hops = old.parse().ok().zip(new.parse().ok()),
                _ => {}
            }
        }
        note
    }
}

/// Number of hops of a path: the TTL of its last answering hop.
fn hop_count(path: &Path) -> u8 {
    path.last().map_or(0, |(ttl, _)| *ttl)
}

/// The address hop `ttl` of `path` answered from, if it answered.
fn address_at(path: &Path, ttl: u8) -> Option<IpAddr> {
    path.iter().find(|(hop, _)| *hop == ttl).map(|(_, address)| *address)
}

/// Whether two routes differ: another hop count, or a hop that answered on both from different addresses.
fn differs(one: &Path, other: &Path) -> bool {
    hop_count(one) != hop_count(other)
        || one.iter().any(|(ttl, address)| address_at(other, *ttl).is_some_and(|there| there != *address))
}

/// Route watch of one target.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RouteWatch {
    /// The route known to hold, `None` before the first check.
    stable: Option<Path>,
    /// A different route seen lately, with how many checks in a row it has held.
    candidate: Option<(Path, u32)>,
    /// Routes that held lately, newest last.
    known: Vec<Path>,
}

impl RouteWatch {
    /// Takes the route seen at this check; returns the change once a different route, not seen lately, has held
    /// long enough. An empty path (nothing answers) is ignored: that is the "no answer" condition's business.
    pub fn check(&mut self, path: Path) -> Option<RouteChange> {
        if path.is_empty() {
            return None;
        }
        let Some(stable) = &self.stable else {
            self.known.push(path.clone());
            self.stable = Some(path);
            return None;
        };
        if !differs(stable, &path) {
            // The same route, perhaps with a hop more or less answering: the latest view is compared next time,
            // and remembered in its place among the known routes.
            if let Some(known) = self.known.iter_mut().find(|known| *known == stable) {
                known.clone_from(&path);
            }
            self.stable = Some(path);
            self.candidate = None;
            return None;
        }
        let held = match &self.candidate {
            Some((candidate, held)) if !differs(candidate, &path) => held.saturating_add(1),
            _ => 1,
        };
        if held < STABLE_CHECKS {
            self.candidate = Some((path, held));
            return None;
        }
        self.candidate = None;
        let before = self.stable.replace(path.clone()).unwrap_or_default();
        if self.known.iter().any(|known| !differs(known, &path)) {
            return None;
        }
        if self.known.len() >= KNOWN_ROUTES {
            self.known.remove(0);
        }
        self.known.push(path.clone());
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

    /// Feeds `route` to `watch` for STABLE_CHECKS checks; returns the change reported, if any.
    fn hold(watch: &mut RouteWatch, route: &[u8]) -> Option<RouteChange> {
        (0..STABLE_CHECKS).filter_map(|_| watch.check(path(route))).last()
    }

    #[test]
    fn a_new_route_counts_once_it_holds() {
        let mut watch = RouteWatch::default();
        assert_eq!(watch.check(path(&[1, 2, 3])), None, "the first route is just remembered");
        for _ in 1..STABLE_CHECKS {
            assert_eq!(watch.check(path(&[1, 5, 3])), None);
        }
        let change = watch.check(path(&[1, 5, 3])).expect("held long enough");
        assert_eq!(change.note().describe(Language::English), "hop 2 10.0.0.2 \u{2192} 10.0.0.5");
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
    fn a_return_to_a_known_route_is_load_balancing() {
        let mut watch = RouteWatch::default();
        watch.check(path(&[1, 2, 3]));
        assert!(hold(&mut watch, &[1, 5, 3]).is_some(), "a new route");
        assert_eq!(hold(&mut watch, &[1, 2, 3]), None, "back to the first one");
        assert_eq!(hold(&mut watch, &[1, 5, 3]), None, "and again");
        assert!(hold(&mut watch, &[1, 7, 3]).is_some(), "a third route is new");
    }

    #[test]
    fn a_hop_that_starts_or_stops_answering_is_not_compared() {
        let mut watch = RouteWatch::default();
        watch.check(vec![(1, ip(1)), (3, ip(3))]);
        assert_eq!(hold(&mut watch, &[1, 2, 3]), None, "hop 2 answers now");
        assert_eq!(hold(&mut watch, &[1, 9, 3]).map(|change| change.note().hop), Some(Some((2, ip(2), ip(9)))));
    }

    #[test]
    fn silence_and_shorter_routes() {
        let mut watch = RouteWatch::default();
        watch.check(path(&[1, 2, 3, 4]));
        assert_eq!(watch.check(Vec::new()), None, "nothing answering is no route");
        let change = hold(&mut watch, &[1, 2, 4]);
        assert_eq!(
            change.map(|change| change.note().describe(Language::English)),
            Some("hop 3 10.0.0.3 \u{2192} 10.0.0.4, hops 4 \u{2192} 3".to_owned())
        );
    }

    #[test]
    fn notes_round_trip_through_text() {
        let note = RouteNote { hop: Some((5, ip(1), "2620:fe::fe".parse().expect("valid"))), hops: Some((12, 11)) };
        assert_eq!(RouteNote::from_text(&note.to_text()), note);
        let empty = RouteNote { hop: None, hops: None };
        assert_eq!(RouteNote::from_text(&empty.to_text()), empty);
        assert_eq!(empty.describe(Language::English), "the route changed");
        assert_eq!(RouteNote::from_text("garbage;hop x y z"), empty);
    }
}
