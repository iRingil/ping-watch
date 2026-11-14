//! `ping-watch` — traceroute monitor in the style of PingPlotter.
//!
//! Usage: `ping-watch [host]` — opens the window and, if a host is given, starts tracing it.
// Release builds are GUI-only; debug builds keep the console for `RUST_LOG` output.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod focus;
mod history;
mod hop_table;
mod icmp;
mod latency_bars;
mod names;
mod route;
mod stats;
mod sync;
mod timeline;
mod tracer;

use anyhow::anyhow;
use eframe::egui;

use crate::app::PingWatchApp;

/// Program entry point.
fn main() -> anyhow::Result<()> {
    env_logger::init();
    let host = std::env::args().nth(1);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_title("ping-watch").with_inner_size([1200.0, 760.0]),
        ..Default::default()
    };
    eframe::run_native(
        "ping-watch",
        options,
        Box::new(move |creation| {
            creation.egui_ctx.set_theme(egui::Theme::Dark);
            Ok(Box::new(PingWatchApp::new(host)))
        }),
    )
    // `eframe::Error` isn't `Send + Sync`, so it can't go into `anyhow` directly.
    .map_err(|error| anyhow!("{error}"))
}
