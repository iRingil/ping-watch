<!--suppress ALL -->
<p align="center">
    <img src="https://repository-images.githubusercontent.com/1458618272/f76bcbfe-1965-418a-9b3a-ad29c432b0c5" alt="Ping Watch" width="640">
</p>
<p align="center">
    <a href="https://doc.rust-lang.org/edition-guide/rust-2024/">
        <img src="https://img.shields.io/badge/rust-2024_edition-informational" alt="Rust edition">
    </a>
    <a href="https://crates.io/crates/eframe/0.37.0">
        <img src="https://img.shields.io/badge/eframe-v0.37.0-informational" alt="eframe version">
    </a>
    <a href="https://crates.io/crates/egui_plot/0.38.0">
        <img src="https://img.shields.io/badge/egui__plot-v0.38.0-informational" alt="egui_plot version">
    </a>
    <a href="https://crates.io/crates/rusqlite/0.42.0">
        <img src="https://img.shields.io/badge/rusqlite-v0.42.0-informational" alt="rusqlite version">
    </a>
    <a href="https://crates.io/crates/windows/0.62.2">
        <img src="https://img.shields.io/badge/windows-v0.62.2-informational" alt="windows version">
    </a>
    <a href="https://crates.io/crates/tray-icon/0.26.1">
        <img src="https://img.shields.io/badge/tray--icon-v0.26.1-informational" alt="tray-icon version">
    </a>
</p>
<p align="center">
    <a href="https://github.com/iRingil/ping-watch/actions/workflows/dependabot/dependabot-updates">
        <img src="https://github.com/iRingil/ping-watch/actions/workflows/dependabot/dependabot-updates/badge.svg" alt="Dependabot Updates">
    </a>
    <a href="https://github.com/iRingil/ping-watch/actions/workflows/github-code-scanning/codeql">
        <img src="https://github.com/iRingil/ping-watch/actions/workflows/github-code-scanning/codeql/badge.svg" alt="CodeQL">
    </a>
    <a href="https://github.com/iRingil/ping-watch/actions/workflows/lint.yml">
        <img src="https://github.com/iRingil/ping-watch/actions/workflows/lint.yml/badge.svg" alt="Lint">
    </a>
    <a href="https://github.com/iRingil/ping-watch/actions/workflows/test.yml">
        <img src="https://github.com/iRingil/ping-watch/actions/workflows/test.yml/badge.svg" alt="Test">
    </a>
    <a href="https://github.com/iRingil/ping-watch/actions/workflows/test.yml">
        <img src="https://img.shields.io/endpoint?url=https%3A%2F%2Fraw.githubusercontent.com%2FiRingil%2Fping-watch%2Fbadges%2Fcoverage.json" alt="Coverage">
    </a>
</p>
<p align="center">
    <a href="https://github.com/iRingil/ping-watch/releases/latest">
        <img src="https://img.shields.io/github/v/release/iRingil/ping-watch" alt="Latest release">
    </a>
    <a href="LICENSE.md">
        <img src="https://img.shields.io/badge/license-PolyForm_Noncommercial_1.0.0-informational" alt="License">
    </a>
    <a href="https://github.com/rust-lang/rust-clippy">
        <img src="https://img.shields.io/badge/linted_with-clippy-informational" alt="Clippy">
    </a>
    <a href="https://deepwiki.com/iRingil/ping-watch">
        <img src="https://img.shields.io/badge/Ask-DeepWiki-informational" alt="Ask DeepWiki">
    </a>
</p>

**Ping Watch** keeps an eye on your connection: it traces the route to the sites and servers you choose and shows,
hop by hop, where the delays and the lost packets are.

Enter an address, and Ping Watch probes every router on the way to it, again and again, so a slow or dropping hop
stands out at once. It keeps the history, so you can look back at what happened while you were away.

<p align="center">
    <img src="docs/main-window.webp" alt="A target tab: the hops to OpenDNS with their latency and loss, the latency bars and the timeline of the last ten minutes" width="880">
</p>

## Features

- **Hop by hop.** Each router on the route gets a row: its address and name, the average, best, worst and current
  latency, the jitter and the packet loss, with a bar that shows the spread over green, amber and red zones.
- **History.** Every probe is stored on this PC. The timeline shows latency, jitter and loss of any hop over the
  last minute to the last seven days; a slider goes back in time, and comments can be pinned to a moment.
- **Several targets.** Each target has its own tab. "All targets" shows them side by side, each with its latency bar,
  and charts the ones you tick, all over the same period.
- **Alerts.** Rules for loss and latency, for one target or all of them, such as "loss above 5 % for two minutes".
  Windows shows a notification when a rule fires and another when the connection is fine again; every alert is kept
  in a log and marked on the timeline.
- **Snapshots.** One click saves the shown target, or All targets, as a single HTML page: the hop table, the
  latency and loss chart and the alerts of the period, ready to send to your provider or to print.
- **IPv4 and IPv6**, host names or addresses, with a list of public DNS servers to start from.
- **In the background.** The close button can hide Ping Watch to the notification area, where it keeps tracing, and
  it can start with Windows.
- **Five languages.** English, Russian, German, French and Spanish.
- **Light and dark themes**, or the one Windows uses.
- **Normal or portable installation**: for one Windows user, or into any folder such as a USB stick.

## Installation

Download the latest version from [Releases](https://github.com/iRingil/ping-watch/releases/latest). It runs on
64-bit Windows 10 and 11.

- `PingWatch-<version>-setup.exe` is the installer. It needs no administrator rights and offers two types:
  - **Normal**: installs into `%LOCALAPPDATA%\Programs\Ping Watch` for the current Windows user, with a Start menu
    shortcut and an entry in Installed apps to uninstall it.
  - **Portable**: copies the app into any folder, without shortcuts or an uninstaller.
- `PingWatch-<version>-portable.zip` is the portable copy: unpack it anywhere and run `PingWatch.exe`.

The app is not code-signed yet, so Windows SmartScreen may warn about it: click **More info → Run anyway**.

### Where the app keeps its files

|                                                    | Normal installation                                          | Portable copy                                 |
|----------------------------------------------------|--------------------------------------------------------------|-----------------------------------------------|
| Probe history, settings, targets, window and tabs  | `%LOCALAPPDATA%\Ping Watch\history.sqlite`                   | `userdata\history.sqlite` in the app's folder |
| Start with Windows, when turned on in the settings | `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`         | the same                                      |
| Name and icon of the app's notifications           | `HKCU\Software\Classes\AppUserModelId\iRingil.PingWatch`     | the same                                      |

The history keeps the last seven days by default (Settings → Tracing → Keep history for). A portable copy is a folder
with `portable.txt` next to `PingWatch.exe`: delete that file, and the copy keeps its data in the user profile
instead.

A portable copy writes the same two registry entries as a normal installation, both for the current user only:
starting with Windows is a value under `Run` (not a shortcut in the Startup folder), and Windows shows notifications
under the app's name and icon only when they are registered under `AppUserModelId`. To remove a portable copy, turn
off "Start with Windows" in the settings, delete the folder and, if you like, the `AppUserModelId\iRingil.PingWatch`
key. Uninstalling a normal installation removes both entries and asks whether to delete the history and the settings
as well.

## Privacy

Ping Watch has no accounts, telemetry, ads or analytics. It sends only what tracing needs, and one request you ask
for:

- **ICMP echo requests** (pings) to the targets you enter and, with growing hop limits, to the routers on the way.
- **DNS lookups**: the address of a target given by name, and the names of the routers on the route.
- **GitHub** (`api.github.com`): once a day, a request for the latest release of this repository, to tell you when a
  new version is out (Settings → General → Updates turns it off), and when you click About → Check for updates.

The history, the settings and the list of targets stay on this PC, in the file listed above.

## How releases are built

Every release is built from this repository by [GitHub Actions](.github/workflows/build.yml) and published by the
[Release workflow](.github/workflows/release.yml) from the files of a tested build. The app's network use is described
in [Privacy](#privacy).

## Building from source

With Rust (stable, edition 2024) on Windows, from the repository root:

```shell
cargo run                                             # run from source
cargo run -- 1.1.1.1 github.com                       # open a tab per target
cargo fmt                                             # format
cargo clippy --all-targets                            # lint
cargo test                                            # tests
cargo audit                                           # advisories (cargo install cargo-audit)
infrastructure\inno\build.cmd                         # release build, installer and portable zip (needs Inno Setup 6.6 or 7)
```

## License

Ping Watch is released under the [PolyForm Noncommercial License 1.0.0](LICENSE.md): you may use, change and share it
for any noncommercial purpose. It is built with these third-party components:

- [egui, eframe, egui_extras](https://github.com/emilk/egui) and [egui_plot](https://github.com/emilk/egui_plot):
  MIT or Apache-2.0
- [rusqlite](https://github.com/rusqlite/rusqlite): MIT; [SQLite](https://sqlite.org): public domain
- [windows, windows-registry](https://github.com/microsoft/windows-rs): MIT or Apache-2.0
- [tray-icon](https://github.com/tauri-apps/tray-icon): MIT or Apache-2.0
- [dns-lookup](https://github.com/keeperofdakeys/dns-lookup), [chrono](https://github.com/chronotope/chrono),
  [ctrlc](https://github.com/Detegr/rust-ctrlc): MIT or Apache-2.0
- [tauri-winrt-notification](https://github.com/tauri-apps/winrt-notification): MIT or Apache-2.0
- [ureq](https://github.com/algesten/ureq), [serde_json](https://github.com/serde-rs/json): MIT or Apache-2.0
- [anyhow, thiserror](https://github.com/dtolnay), [log, env_logger](https://github.com/rust-lang/log):
  MIT or Apache-2.0
- [Roboto Condensed](https://github.com/googlefonts/roboto-classic) font: SIL Open Font License 1.1
- [Rust](https://www.rust-lang.org) standard library: MIT or Apache-2.0
