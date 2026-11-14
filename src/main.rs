//! `ping-watch` — traceroute monitor in the style of PingPlotter.
//!
//! Usage: `ping-watch [host...]` — opens the window with one tab per given host, each tracing right away.
// Release builds are GUI-only; debug builds keep the console for `RUST_LOG` output.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod focus;
mod history;
mod hop_table;
mod icmp;
mod latency_bars;
mod names;
mod presets;
mod route;
mod stats;
mod sync;
mod tab;
mod theme;
mod timeline;
mod tracer;

use anyhow::anyhow;
use eframe::egui;

use crate::app::PingWatchApp;

/// Program entry point.
fn main() -> anyhow::Result<()> {
    env_logger::init();
    let hosts: Vec<String> = std::env::args().skip(1).collect();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_title("ping-watch").with_inner_size([1200.0, 760.0]),
        ..Default::default()
    };
    eframe::run_native(
        "ping-watch",
        options,
        Box::new(move |creation| {
            theme::apply(&creation.egui_ctx);
            Ok(Box::new(PingWatchApp::new(hosts)))
        }),
    )
    // `eframe::Error` isn't `Send + Sync`, so it can't go into `anyhow` directly.
    .map_err(|error| anyhow!("{error}"))
}
