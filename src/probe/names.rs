//! Host names: forward lookup of the target (IPv4 preferred) and cached background reverse lookups for the hop
//! "Name" column.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::net::{IpAddr, ToSocketAddrs};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use crate::sync::lock;

/// Longest host name DNS allows (RFC 1035); longer input is no host.
pub const MAX_HOST_LENGTH: usize = 253;
/// Most addresses the reverse-DNS cache keeps; when it is full, the answered ones are forgotten (and asked again
/// when shown).
const MAX_CACHED_NAMES: usize = 4_096;
/// Most reverse lookups running at once; an address asked for while all run is looked up on a later call.
const MAX_LOOKUPS: usize = 16;

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
/// number of lookups at once are bounded.
#[derive(Debug, Clone, Default)]
pub struct NameCache {
    /// Known addresses: `None` while the lookup runs or if it failed.
    names: Arc<Mutex<HashMap<IpAddr, Option<String>>>>,
    /// Lookups running now.
    running: Arc<AtomicUsize>,
}

impl NameCache {
    /// Name of `ip` if already known; the first call for an address starts its lookup and returns `None`.
    pub fn get(&self, ip: IpAddr) -> Option<String> {
        let mut names = lock(&self.names);
        if let Some(known) = names.get(&ip) {
            return known.clone();
        }
        if self.running.load(Ordering::Relaxed) >= MAX_LOOKUPS {
            return None;
        }
        if names.len() >= MAX_CACHED_NAMES {
            // Lookups still running keep their entries, so they are not started twice.
            names.retain(|_, name| name.is_none());
        }
        if let Entry::Vacant(entry) = names.entry(ip) {
            entry.insert(None);
            self.spawn_lookup(ip);
        }
        None
    }

    /// Starts a background reverse lookup of `ip` that stores its result in the cache.
    fn spawn_lookup(&self, ip: IpAddr) {
        let (names, running) = (Arc::clone(&self.names), Arc::clone(&self.running));
        running.fetch_add(1, Ordering::Relaxed);
        let spawned = thread::Builder::new().name("rdns".to_owned()).spawn(move || {
            let name = dns_lookup::lookup_addr(&ip)
                .inspect_err(|error| log::debug!("reverse lookup of {ip} failed: {error}"))
                .ok();
            lock(&names).insert(ip, name);
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
