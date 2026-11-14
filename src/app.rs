//! Main window: target input with Start/Stop, and the panes showing the running trace.

use std::net::Ipv4Addr;
use std::time::Duration;

use eframe::egui::{self, Color32, Key, RichText, TextEdit};

use crate::hop_table;
use crate::names::{NameCache, resolve_ipv4};
use crate::tracer::{TraceConfig, Tracer};

/// How often the window redraws to show fresh numbers.
const REFRESH: Duration = Duration::from_millis(250);

/// A running trace of one target.
#[derive(Debug)]
struct Session {
    /// Host as the user entered it.
    host: String,
    /// Resolved address being traced.
    target: Ipv4Addr,
    /// Background tracer; dropping the session stops it.
    tracer: Tracer,
}

/// Application state.
#[derive(Debug)]
pub struct PingWatchApp {
    /// Contents of the target input field.
    host_input: String,
    /// Running trace, `None` when stopped.
    session: Option<Session>,
    /// Last start error, shown next to the input.
    error: Option<String>,
    /// Reverse-DNS cache for hop names.
    names: NameCache,
}

impl PingWatchApp {
    /// Creates the app; starts tracing `host` right away if given.
    pub fn new(host: Option<String>) -> Self {
        let mut app =
            Self { host_input: host.unwrap_or_default(), session: None, error: None, names: NameCache::default() };
        if !app.host_input.is_empty() {
            app.start();
        }
        app
    }

    /// (Re)starts tracing the host from the input field; on failure keeps the error for display.
    fn start(&mut self) {
        // Stop the old trace first, so it doesn't keep probing while the new one starts.
        self.session = None;
        let host = self.host_input.trim().to_owned();
        let started = resolve_ipv4(&host).map_err(|error| error.to_string()).and_then(|target| {
            let tracer = Tracer::start(target, TraceConfig::default()).map_err(|error| error.to_string())?;
            Ok(Session { host, target, tracer })
        });
        match started {
            Ok(session) => {
                self.session = Some(session);
                self.error = None;
            }
            Err(error) => {
                log::warn!("{error}");
                self.error = Some(error);
            }
        }
    }

    /// Top bar: target input, Start/Stop button and the last error.
    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("Target:");
            let input = ui.add(TextEdit::singleline(&mut self.host_input).hint_text("host or IP").desired_width(260.0));
            let enter_pressed = input.lost_focus() && ui.input(|input| input.key_pressed(Key::Enter));
            let running = self.session.is_some();
            if ui.button(if running { "Stop" } else { "Start" }).clicked() {
                if running {
                    self.session = None;
                } else {
                    self.start();
                }
            } else if enter_pressed {
                self.start();
            }
            if let Some(error) = &self.error {
                ui.colored_label(Color32::LIGHT_RED, error);
            }
        });
    }
}

impl eframe::App for PingWatchApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::top("toolbar").show(ui, |ui| self.toolbar(ui));
        egui::CentralPanel::default().show(ui, |ui| match &self.session {
            Some(session) => {
                ui.label(RichText::new(format!("{} / {}", session.target, session.host)).strong());
                ui.separator();
                hop_table::show(ui, &session.tracer.snapshot(), &self.names);
            }
            None => {
                ui.label("Enter a host and press Start.");
            }
        });
        ui.ctx().request_repaint_after(REFRESH);
    }
}
