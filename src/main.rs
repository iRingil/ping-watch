//! `ping-watch` — traceroute monitor in the style of PingPlotter.
//!
//! For now a console front end: traces the host given on the command line and redraws the hop table every second.

mod icmp;
mod route;
mod stats;
mod tracer;

use std::net::{IpAddr, Ipv4Addr, ToSocketAddrs};
use std::thread;
use std::time::Duration;

use anyhow::Context;

use crate::route::Route;
use crate::tracer::{TraceConfig, Tracer};

/// Program entry point.
fn main() -> anyhow::Result<()> {
    env_logger::init();
    let host = std::env::args().nth(1).context("usage: ping-watch <host>")?;
    let target = resolve(&host)?;
    let tracer = Tracer::start(target, TraceConfig::default()).context("cannot start trace")?;
    loop {
        thread::sleep(Duration::from_secs(1));
        print_table(&host, &tracer.snapshot());
    }
}

/// Resolves `host` (name or address) to its first IPv4 address.
///
/// # Errors
///
/// If the name can't be resolved or has no IPv4 address.
fn resolve(host: &str) -> anyhow::Result<Ipv4Addr> {
    (host, 0)
        .to_socket_addrs()
        .with_context(|| format!("cannot resolve {host}"))?
        .find_map(|address| match address.ip() {
            IpAddr::V4(ip) => Some(ip),
            IpAddr::V6(_) => None,
        })
        .with_context(|| format!("{host} has no IPv4 address"))
}

/// Clears the console and prints the hop table of `route`.
fn print_table(host: &str, route: &Route) {
    print!("\x1b[2J\x1b[H");
    println!("{} / {host}\n", route.target());
    println!(
        "{:>3}  {:<15} {:>6} {:>5} {:>5} {:>7} {:>7} {:>7} {:>7} {:>7}",
        "Hop", "IP", "Loss%", "Sent", "Recv", "Best", "Avg", "Worst", "Last", "Jitter"
    );
    for hop in route.hops() {
        let stats = &hop.stats;
        let address = hop.address.map_or_else(|| "*".to_owned(), |ip| ip.to_string());
        println!(
            "{:>3}  {:<15} {:>6.1} {:>5} {:>5} {:>7} {:>7} {:>7} {:>7} {:>7}",
            hop.ttl,
            address,
            stats.loss_percent(),
            stats.sent(),
            stats.received(),
            ms(stats.best()),
            ms(stats.average()),
            ms(stats.worst()),
            ms(stats.last()),
            ms(stats.jitter())
        );
    }
}

/// Formats a duration as milliseconds with one decimal, `-` for none.
fn ms(value: Option<Duration>) -> String {
    value.map_or_else(|| "-".to_owned(), |duration| format!("{:.1}", duration.as_secs_f64() * 1000.0))
}
