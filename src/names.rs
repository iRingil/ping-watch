//! Host names: forward lookup of the target (IPv4 preferred) and cached background reverse lookups for the hop
//! "Name" column.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::net::{IpAddr, ToSocketAddrs};
use std::sync::{Arc, Mutex};
use std::thread;

use crate::sync::lock;

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
}

/// Resolves `host` (name or address) to its first IPv4 address, or its first IPv6 address if it has no IPv4 one.
/// Blocks while DNS answers.
///
/// # Errors
///
/// [`ResolveError`] if the name can't be resolved or has no address.
pub fn resolve(host: &str) -> Result<IpAddr, ResolveError> {
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

/// Reverse-DNS cache; lookups run in background threads so the UI never waits for DNS.
#[derive(Debug, Clone, Default)]
pub struct NameCache {
    /// Known addresses: `None` while the lookup runs or if it failed.
    names: Arc<Mutex<HashMap<IpAddr, Option<String>>>>,
}

impl NameCache {
    /// Name of `ip` if already known; the first call for an address starts its lookup and returns `None`.
    pub fn get(&self, ip: IpAddr) -> Option<String> {
        match lock(&self.names).entry(ip) {
            Entry::Occupied(entry) => entry.get().clone(),
            Entry::Vacant(entry) => {
                entry.insert(None);
                self.spawn_lookup(ip);
                None
            }
        }
    }

    /// Starts a background reverse lookup of `ip` that stores its result in the cache.
    fn spawn_lookup(&self, ip: IpAddr) {
        let names = Arc::clone(&self.names);
        let spawned = thread::Builder::new().name(format!("rdns-{ip}")).spawn(move || {
            let name = dns_lookup::lookup_addr(&ip)
                .inspect_err(|error| log::debug!("reverse lookup of {ip} failed: {error}"))
                .ok();
            lock(&names).insert(ip, name);
        });
        if let Err(error) = spawned {
            log::warn!("cannot start reverse lookup of {ip}: {error}");
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
}
