//! Main window: target input with Start/Stop, and the panes showing the running trace.

use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::time::Duration;

use eframe::egui::{self, Color32, Key, RichText, TextEdit};

use crate::history::{self, Recorder};
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
    /// Background tracer; dropping the session stops it. Declared before `recorder` so it stops first and the
    /// recorder can flush everything the tracer sent.
    tracer: Tracer,
    /// History writer, `None` if the database is unavailable.
    #[expect(dead_code, reason = "held only to keep recording until the session is dropped")]
    recorder: Option<Recorder>,
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
    /// History database file, `None` if there is no place for it.
    history_path: Option<PathBuf>,
}

impl PingWatchApp {
    /// Creates the app; starts tracing `host` right away if given.
    pub fn new(host: Option<String>) -> Self {
        let history_path = history::default_path().inspect_err(|error| log::warn!("{error}")).ok();
        let mut app = Self {
            host_input: host.unwrap_or_default(),
            session: None,
            error: None,
            names: NameCache::default(),
            history_path,
        };
        if !app.host_input.is_empty() {
            app.start();
        }
        app
    }

    /// (Re)starts tracing the host from the input field; on failure keeps the error for display.
    /// Without a history database the trace still runs, and the error says history is off.
    fn start(&mut self) {
        // Stop the old trace first, so it doesn't keep probing while the new one starts.
        self.session = None;
        let host = self.host_input.trim().to_owned();
        let target = match resolve_ipv4(&host) {
            Ok(target) => target,
            Err(error) => return self.fail(error.to_string()),
        };
        let (recorder, history_error) = match self.start_recorder(&host) {
            Ok(recorder) => (Some(recorder), None),
            Err(error) => (None, Some(error)),
        };
        match Tracer::start(target, TraceConfig::default(), recorder.as_ref().and_then(Recorder::sender)) {
            Ok(tracer) => {
                self.session = Some(Session { host, target, tracer, recorder });
                self.error = history_error;
            }
            Err(error) => self.fail(error.to_string()),
        }
    }

    /// Logs a start failure and keeps it for display.
    fn fail(&mut self, error: String) {
        log::warn!("{error}");
        self.error = Some(error);
    }

    /// Starts the history recorder for `host`; the error text says history is off.
    fn start_recorder(&self, host: &str) -> Result<Recorder, String> {
        let path = self.history_path.as_ref().ok_or_else(|| "history is off: no data folder".to_owned())?;
        Recorder::start(path, &host.to_lowercase()).map_err(|error| {
            log::warn!("{error}");
            format!("history is off: {error}")
        })
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
