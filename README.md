# Ping Watch

A traceroute monitor for Windows: traces the route to your targets, shows each hop's latency and loss, and keeps
the history in a local database, so you can look back at what happened.

- several targets in tabs, plus an All targets overview with charts
- per-hop latency bars and a timeline of latency, jitter and loss (1 minute to 7 days)
- IPv4 and IPv6

Website: https://iringil.github.io/ping-watch/

## Build

Rust (edition 2024) on Windows:

```
cargo build --release
```

The executable is `target/release/ping-watch.exe`. History is kept in `%LOCALAPPDATA%\Ping Watch`.

## License

[PolyForm Noncommercial 1.0.0](https://polyformproject.org/licenses/noncommercial/1.0.0/) (see `LICENSE.md`): you may
use, change and share Ping Watch for any noncommercial purpose; commercial use is not allowed.

The Barlow font is embedded under the SIL Open Font License (`assets/fonts/OFL.txt`).
