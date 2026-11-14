//! Ping Watch — traceroute monitor in the style of PingPlotter. (`ping-watch` is only the project / executable
//! name; everything the user sees says "Ping Watch".)
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

/// Name of the app as users see it: window title, data folder, executable properties, documentation.
pub const APP_NAME: &str = "Ping Watch";

/// Window and taskbar icon (the executable's own icon comes from `build.rs`).
const ICON_PNG: &[u8] = include_bytes!("../assets/icon-256.png");

/// Adds the app icon to `viewport`; without it (bad PNG) the window just gets the default icon.
fn window_icon(viewport: egui::ViewportBuilder) -> egui::ViewportBuilder {
    match eframe::icon_data::from_png_bytes(ICON_PNG) {
        Ok(icon) => viewport.with_icon(icon),
        Err(error) => {
            log::warn!("cannot load the window icon: {error}");
            viewport
        }
    }
}

/// Program entry point.
fn main() -> anyhow::Result<()> {
    env_logger::init();
    let hosts: Vec<String> = std::env::args().skip(1).collect();
    let options = eframe::NativeOptions {
        viewport: window_icon(egui::ViewportBuilder::default().with_title(APP_NAME).with_inner_size([1200.0, 760.0])),
        ..Default::default()
    };
    eframe::run_native(
        APP_NAME,
        options,
        Box::new(move |creation| {
            theme::apply(&creation.egui_ctx);
            Ok(Box::new(PingWatchApp::new(hosts)))
        }),
    )
    // `eframe::Error` isn't `Send + Sync`, so it can't go into `anyhow` directly.
    .map_err(|error| anyhow!("{error}"))
}
