//! Host names: forward lookup of the target (IPv4 preferred) and cached background reverse lookups for the hop
//! "Name" column.

use std::collections::HashMap;

use std::net::{IpAddr, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::sync::lock;

/// Longest host name DNS allows (RFC 1035); longer input is no host.
pub const MAX_HOST_LENGTH: usize = 253;
/// Most addresses the reverse-DNS cache keeps; when it is full, the answered ones are forgotten (and asked again
/// when shown).
const MAX_CACHED_NAMES: usize = 4_096;
/// Most reverse lookups running at once; an address asked for while all run is looked up on a later call.
const MAX_LOOKUPS: usize = 16;
/// A failed reverse lookup (no name, or no network at the time) is asked again after this long.
const RETRY_FAILED: Duration = Duration::from_secs(5 * 60);

/// What the cache knows about one address.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Lookup {
    /// The lookup runs.
    Running,
    /// The address has this name.
    Named(String),
    /// The lookup failed at this moment.
    Failed(Instant),
}

/// Error resolving the target host.
#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    /// The name lookup itself failed.
    #[error("cannot resolve {host}: {source}")]
    Lookup {
        /// Host as entered.
        host: String,
        /// Underlying error.
        source: std::io::Error,
    },
    /// The name exists but has no address.
    #[error("{0} has no address")]
    NoAddress(String),
    /// The text can't be a host: too long or with control characters.
    #[error("{0:?} is not a host name or address")]
    Invalid(String),
}

/// Whether `a` and `b` name the same host: letter case doesn't matter, in any script (not only ASCII).
pub fn same_host(a: &str, b: &str) -> bool {
    a == b || a.to_lowercase() == b.to_lowercase()
}

/// Resolves `host` (name or address) to its first IPv4 address, or its first IPv6 address if it has no IPv4 one.
/// Blocks while DNS answers.
///
/// # Errors
///
/// [`ResolveError`] if `host` can't be a host name, can't be resolved or has no address.
pub fn resolve(host: &str) -> Result<IpAddr, ResolveError> {
    if !is_plausible_host(host) {
        return Err(ResolveError::Invalid(host.to_owned()));
    }
    let addresses: Vec<IpAddr> = (host, 0)
        .to_socket_addrs()
        .map_err(|source| ResolveError::Lookup { host: host.to_owned(), source })?
        .map(|address| address.ip())
        .collect();
    addresses
        .iter()
        .find(|ip| ip.is_ipv4())
        .or_else(|| addresses.first())
        .copied()
        .ok_or_else(|| ResolveError::NoAddress(host.to_owned()))
}

/// Whether `host` can be a host name or address at all: not empty, at most [`MAX_HOST_LENGTH`] bytes, no control
/// characters (a NUL would also break the resolver thread's name and the C resolver).
pub fn is_plausible_host(host: &str) -> bool {
    !host.is_empty() && host.len() <= MAX_HOST_LENGTH && !host.chars().any(char::is_control)
}

/// Reverse-DNS cache; lookups run in background threads so the UI never waits for DNS. Both the cache and the
/// number of lookups at once are bounded. Lookups can be turned off: then no address has a name.
#[derive(Debug, Clone, Default)]
pub struct NameCache {
    /// Lookups are off: no new ones start and known names aren't given out.
    off: Arc<AtomicBool>,
    /// Addresses looked up.
    names: Arc<Mutex<HashMap<IpAddr, Lookup>>>,
    /// Lookups running now.
    running: Arc<AtomicUsize>,
}

impl NameCache {
    /// Turns reverse lookups on or off.
    pub fn set_enabled(&self, enabled: bool) {
        self.off.store(!enabled, Ordering::Relaxed);
    }

    /// Name of `ip` if already known; the first call for an address starts its lookup and returns `None`, as does
    /// the first call [`RETRY_FAILED`] after a failed lookup, which starts it again. Always `None` while lookups
    /// are off.
    pub fn get(&self, ip: IpAddr) -> Option<String> {
        if self.off.load(Ordering::Relaxed) {
            return None;
        }
        let mut names = lock(&self.names);
        match names.get(&ip) {
            Some(Lookup::Named(name)) => return Some(name.clone()),
            Some(Lookup::Running) => return None,
            Some(Lookup::Failed(at)) if at.elapsed() < RETRY_FAILED => return None,
            Some(Lookup::Failed(_)) | None => {}
        }
        if self.running.load(Ordering::Relaxed) >= MAX_LOOKUPS {
            return None;
        }
        if names.len() >= MAX_CACHED_NAMES {
            // Lookups still running keep their entries, so they are not started twice.
            names.retain(|_, lookup| *lookup == Lookup::Running);
        }
        names.insert(ip, Lookup::Running);
        self.spawn_lookup(ip);
        None
    }

    /// Starts a background reverse lookup of `ip` that stores its result in the cache.
    fn spawn_lookup(&self, ip: IpAddr) {
        let (names, running) = (Arc::clone(&self.names), Arc::clone(&self.running));
        running.fetch_add(1, Ordering::Relaxed);
        let spawned = thread::Builder::new().name("rdns".to_owned()).spawn(move || {
            let lookup = match dns_lookup::lookup_addr(&ip) {
                Ok(name) => Lookup::Named(name),
                Err(error) => {
                    log::debug!("reverse lookup of {ip} failed: {error}");
                    Lookup::Failed(Instant::now())
                }
            };
            lock(&names).insert(ip, lookup);
            running.fetch_sub(1, Ordering::Relaxed);
        });
        if let Err(error) = spawned {
            log::warn!("cannot start reverse lookup of {ip}: {error}");
            self.running.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::net::{Ipv4Addr, Ipv6Addr};

    #[test]
    fn host_names_match_in_any_case() {
        assert!(same_host("GitHub.com", "github.com"));
        assert!(same_host("ПРИМЕР.РФ", "пример.рф"));
        assert!(!same_host("example.com", "example.org"));
    }

    #[test]
    fn no_names_while_lookups_are_off() {
        let names = NameCache::default();
        names.set_enabled(false);
        let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
        assert_eq!(names.get(ip), None);
        assert!(lock(&names.names).is_empty(), "no lookup started");
    }

    #[test]
    fn a_failed_lookup_is_asked_again_later() {
        let names = NameCache::default();
        let ip = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1));
        let failed = |ago: Duration| Instant::now().checked_sub(ago).map(Lookup::Failed);
        lock(&names.names).extend(failed(Duration::from_secs(1)).map(|lookup| (ip, lookup)));
        assert_eq!(names.get(ip), None);
        assert!(matches!(lock(&names.names).get(&ip), Some(Lookup::Failed(_))), "not asked again yet");
        lock(&names.names).extend(failed(RETRY_FAILED).map(|lookup| (ip, lookup)));
        assert_eq!(names.get(ip), None);
        let stale = matches!(lock(&names.names).get(&ip), Some(Lookup::Failed(at)) if at.elapsed() >= RETRY_FAILED);
        assert!(!stale, "asked again");
    }

    #[test]
    fn ipv4_literal_resolves_to_itself() {
        assert_eq!(resolve("192.0.2.7").ok(), Some(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 7))));
    }

    #[test]
    fn ipv6_literal_resolves_to_itself() {
        assert_eq!(resolve("::1").ok(), Some(IpAddr::V6(Ipv6Addr::LOCALHOST)));
    }

    #[test]
    fn implausible_hosts_are_refused_without_a_lookup() {
        assert!(matches!(resolve("dns\0google"), Err(ResolveError::Invalid(_))));
        assert!(matches!(resolve(&"a".repeat(MAX_HOST_LENGTH + 1)), Err(ResolveError::Invalid(_))));
        assert!(matches!(resolve(""), Err(ResolveError::Invalid(_))));
        assert!(is_plausible_host(&"a".repeat(MAX_HOST_LENGTH)));
        assert!(is_plausible_host("dns.google"));
    }
}
