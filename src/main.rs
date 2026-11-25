//! Ping Watch — traceroute monitor in the style of PingPlotter. (`ping-watch` is only the project / executable
//! name; everything the user sees says "Ping Watch".)
//!
//! Usage: `ping-watch [--tray] [host...]` — opens the window with one tab per given host, each tracing right away;
//! with `--tray` (as Windows starts it at sign-in) the window starts hidden to the tray.
// Release builds are GUI-only; debug builds keep the console for `RUST_LOG` output.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod alerts;
mod com;
mod elevation;
mod i18n;
mod probe;
mod snapshot;
mod store;
mod sync;
mod traces;
mod ui;
mod update;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::anyhow;
use eframe::egui;

use crate::store::window_state::{WindowState, WindowStore};
use crate::ui::app::PingWatchApp;

/// Name of the app as users see it: window title, data folder, executable properties, documentation.
pub const APP_NAME: &str = "Ping Watch";
/// The app's website.
pub const SITE_URL: &str = "https://iringil.github.io/ping-watch/";

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

/// Renderer setup: Direct3D 12 only, unless `WGPU_BACKEND` names others. The default set includes Vulkan, and
/// merely listing Vulkan adapters loads every installed Vulkan driver; the legacy Intel one for older integrated
/// GPUs (`igvk64.dll`, 31.0.101.2xxx) crashes the process there before the window opens. Direct3D 12 is present
/// on every Windows 10 and 11, with the WARP software renderer where no GPU supports it.
fn renderer_options() -> eframe::egui_wgpu::WgpuConfiguration {
    let mut setup = eframe::egui_wgpu::WgpuSetupCreateNew::without_display_handle();
    setup.instance_descriptor.backends = eframe::wgpu::Backends::from_env().unwrap_or(eframe::wgpu::Backends::DX12);
    eframe::egui_wgpu::WgpuConfiguration {
        wgpu_setup: eframe::egui_wgpu::WgpuSetup::CreateNew(setup),
        ..Default::default()
    }
}

/// Has the DLLs loaded later (by Windows' own libraries, on demand) come from System32 only, not from the
/// program's folder, which any program of the user may write to while Ping Watch may run as administrator. The
/// DLLs the executable imports get the same rule from the linker (`build.rs`).
fn load_dlls_from_system32() {
    // SAFETY: no preconditions; it only changes where this process looks for DLLs.
    if let Err(error) = unsafe {
        windows::Win32::System::LibraryLoader::SetDefaultDllDirectories(
            windows::Win32::System::LibraryLoader::LOAD_LIBRARY_SEARCH_SYSTEM32,
        )
    } {
        log::warn!("cannot restrict where DLLs are loaded from: {error}");
    }
}

/// Program entry point.
fn main() -> anyhow::Result<()> {
    env_logger::init();
    load_dlls_from_system32();
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    // Started only to make or remove the sign-in task, with the administrator rights that takes: no window.
    if let Some(on) = store::autostart::take_task_option(&mut args) {
        return store::autostart::set_task(on).map_err(|error| anyhow!("the sign-in task wasn't changed: {error}"));
    }
    // Started only to remove the task and the firewall rules of a portable copy, likewise.
    if traces::take_option(&mut args) {
        return traces::remove_privileged().map_err(|error| anyhow!("the traces weren't removed: {error}"));
    }
    // An instance started to replace this one with administrator rights waits until the old one is gone.
    let mut hosts = elevation::wait_for_replaced(args);
    let in_tray = ui::tray::take_start_option(&mut hosts);
    let add_rules = elevation::take_rules_option(&mut hosts);
    // A second start shows the running window instead (its hosts, if any, are not opened).
    let first_instance = match ui::single_instance::claim() {
        ui::single_instance::Claim::First(first) => Some(first),
        ui::single_instance::Claim::Other => {
            log::info!("Ping Watch is already running; its window was asked to show");
            return Ok(());
        }
        ui::single_instance::Claim::Unknown => None,
    };
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
    let options = eframe::NativeOptions {
        viewport: window_icon(viewport),
        wgpu_options: renderer_options(),
        ..Default::default()
    };
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
            let quit = Arc::new(AtomicBool::new(false));
            if let Some(first) = first_instance {
                let context = creation.egui_ctx.clone();
                let quit_context = creation.egui_ctx.clone();
                let quit_on_request = Arc::clone(&quit);
                first.watch(
                    move || {
                        context.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                        context.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                        context.send_viewport_cmd(egui::ViewportCommand::Focus);
                        context.request_repaint();
                    },
                    // The installer replaces the program: quit as the tray's Exit does, saving tabs and history.
                    move || {
                        quit_on_request.store(true, Ordering::Relaxed);
                        quit_context.send_viewport_cmd(egui::ViewportCommand::Close);
                        quit_context.request_repaint();
                    },
                );
            }
            let context = creation.egui_ctx.clone();
            let quit_on_signal = Arc::clone(&quit);
            if let Err(error) = ctrlc::set_handler(move || {
                quit_on_signal.store(true, Ordering::Relaxed);
                context.send_viewport_cmd(egui::ViewportCommand::Close);
                context.request_repaint();
            }) {
                log::warn!("cannot handle Ctrl+C: {error}");
            }
            Ok(Box::new(PingWatchApp::new(hosts, in_tray, add_rules, tray, window_store, saved_window, quit)))
        }),
    )
    // `eframe::Error` isn't `Send + Sync`, so it can't go into `anyhow` directly.
    .map_err(|error| anyhow!("{error}"))
}
