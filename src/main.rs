//! Ping Watch — traceroute monitor in the style of PingPlotter. (`ping-watch` is only the project / executable
//! name; everything the user sees says "Ping Watch".)
//!
//! Usage: `ping-watch [host...]` — opens the window with one tab per given host, each tracing right away.
// Release builds are GUI-only; debug builds keep the console for `RUST_LOG` output.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod probe;
mod store;
mod sync;
mod ui;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::anyhow;
use eframe::egui;

use crate::store::window_state::{WindowState, WindowStore};
use crate::ui::app::PingWatchApp;

/// Name of the app as users see it: window title, data folder, executable properties, documentation.
pub const APP_NAME: &str = "Ping Watch";

/// Inner size of the window on the very first start, logical points.
const DEFAULT_WINDOW_SIZE: [f32; 2] = [1200.0, 760.0];
/// Smallest inner size of the window, logical points: the toolbar and the table columns still fit, and it fits a
/// 1366 × 768 screen with the taskbar.
const MIN_WINDOW_SIZE: [f32; 2] = [1000.0, 640.0];

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
    // The window comes back where and as it was left.
    let window_store = store::history::default_path()
        .ok()
        .and_then(|path| WindowStore::open(&path).inspect_err(|error| log::warn!("{error}")).ok());
    let saved_window = window_store
        .as_ref()
        .and_then(|store| store.load().inspect_err(|error| log::warn!("{error}")).ok().flatten())
        .filter(WindowState::is_plausible);
    let mut viewport = egui::ViewportBuilder::default()
        .with_title(APP_NAME)
        .with_inner_size(DEFAULT_WINDOW_SIZE)
        .with_min_inner_size(MIN_WINDOW_SIZE);
    if let Some(state) = saved_window {
        viewport = viewport
            .with_position([state.x, state.y])
            // A size saved before the minimum existed (or under it) comes back at least as large: Windows applies
            // the minimum only while the user resizes, not to the size the window is created with.
            .with_inner_size([state.width.max(MIN_WINDOW_SIZE[0]), state.height.max(MIN_WINDOW_SIZE[1])])
            .with_maximized(state.maximized);
    }
    let options = eframe::NativeOptions { viewport: window_icon(viewport), ..Default::default() };
    eframe::run_native(
        APP_NAME,
        options,
        Box::new(move |creation| {
            ui::theme::apply(&creation.egui_ctx);
            // Created here, on the event loop's thread, as the tray needs. Without it the app still works.
            let tray = ui::tray::Tray::new(&creation.egui_ctx, ICON_PNG, APP_NAME)
                .inspect_err(|error| log::warn!("{error}"))
                .ok();
            // Ctrl+C, Ctrl+Break, a closed console, logoff and shutdown (an IDE's Stop button sends Ctrl+C) close
            // the window like its close button: tabs and history are saved and the process exits normally.
            let context = creation.egui_ctx.clone();
            let quit = Arc::new(AtomicBool::new(false));
            let quit_on_signal = Arc::clone(&quit);
            if let Err(error) = ctrlc::set_handler(move || {
                quit_on_signal.store(true, Ordering::Relaxed);
                context.send_viewport_cmd(egui::ViewportCommand::Close);
                context.request_repaint();
            }) {
                log::warn!("cannot handle Ctrl+C: {error}");
            }
            Ok(Box::new(PingWatchApp::new(hosts, tray, window_store, saved_window, quit)))
        }),
    )
    // `eframe::Error` isn't `Send + Sync`, so it can't go into `anyhow` directly.
    .map_err(|error| anyhow!("{error}"))
}
