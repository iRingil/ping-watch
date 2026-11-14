//! Main window: toolbar (target input, pause with its menu, interval) and the panes showing the running trace.

use std::net::IpAddr;
use std::path::PathBuf;
use std::time::Duration;

use eframe::egui::{self, Button, Color32, Key, KeyboardShortcut, Modifiers, RichText, TextEdit};

use crate::history::{self, Recorder, Store};
use crate::hop_table;
use crate::names::{NameCache, resolve};
use crate::route::Route;
use crate::timeline::Timeline;
use crate::tracer::{TraceConfig, Tracer};

/// How often the window redraws to show fresh numbers.
const REFRESH: Duration = Duration::from_millis(250);
/// Probe intervals offered in the toolbar.
const INTERVALS: [(Duration, &str); 7] = [
    (Duration::from_millis(500), "0.5 seconds"),
    (Duration::from_secs(1), "1 second"),
    (Duration::from_millis(2_500), "2.5 seconds"),
    (Duration::from_secs(5), "5 seconds"),
    (Duration::from_secs(10), "10 seconds"),
    (Duration::from_secs(30), "30 seconds"),
    (Duration::from_secs(60), "60 seconds"),
];
/// Restarts the trace with fresh statistics.
const RESTART_SHORTCUT: KeyboardShortcut = KeyboardShortcut::new(Modifiers::CTRL, Key::R);
/// Starts entering a new target.
const NEW_TARGET_SHORTCUT: KeyboardShortcut = KeyboardShortcut::new(Modifiers::CTRL, Key::N);
/// Stops and closes the trace.
const CLOSE_SHORTCUT: KeyboardShortcut = KeyboardShortcut::new(Modifiers::CTRL, Key::Q);

/// What the user asked for in the toolbar or with a shortcut; applied after drawing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    /// Trace the host in the input field.
    Start,
    /// Restart the current trace with fresh statistics.
    Restart,
    /// Clear and focus the input field for a new target.
    NewTarget,
    /// Stop the current trace.
    Close,
}

/// Menu label of a probe interval.
fn interval_label(interval: Duration) -> String {
    INTERVALS
        .iter()
        .find(|(known, _)| *known == interval)
        .map_or_else(|| format!("{:.1} seconds", interval.as_secs_f64()), |(_, label)| (*label).to_owned())
}

/// A running trace of one target.
#[derive(Debug)]
struct Session {
    /// Host as the user entered it.
    host: String,
    /// Resolved address being traced.
    target: IpAddr,
    /// Id of the target in the history, `None` if history is off.
    target_id: Option<i64>,
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
    /// Read connection to the history for the timeline, `None` if history is off.
    reader: Option<Store>,
    /// Hop chosen for the timeline by clicking its row; `None` means the target hop.
    selected_ttl: Option<u8>,
    /// Timeline pane state.
    timeline: Timeline,
    /// Probe interval for the running and new traces.
    interval: Duration,
    /// Move keyboard focus to the target input on the next frame.
    focus_input: bool,
}

impl PingWatchApp {
    /// Creates the app; starts tracing `host` right away if given.
    pub fn new(host: Option<String>) -> Self {
        let history_path = history::default_path().inspect_err(|error| log::warn!("{error}")).ok();
        let reader = history_path
            .as_deref()
            .and_then(|path| Store::open(path).inspect_err(|error| log::warn!("{error}")).ok());
        let mut app = Self {
            host_input: host.unwrap_or_default(),
            session: None,
            error: None,
            names: NameCache::default(),
            history_path,
            reader,
            selected_ttl: None,
            timeline: Timeline::default(),
            interval: Duration::from_secs(1),
            focus_input: false,
        };
        if !app.host_input.is_empty() {
            app.start(app.host_input.trim().to_owned());
        }
        app
    }

    /// (Re)starts tracing `host`; on failure keeps the error for display.
    /// Without a history database the trace still runs, and the error says history is off.
    fn start(&mut self, host: String) {
        // Stop the old trace first, so it doesn't keep probing while the new one starts.
        self.session = None;
        let target = match resolve(&host) {
            Ok(target) => target,
            Err(error) => return self.fail(error.to_string()),
        };
        self.selected_ttl = None;
        let target_id = self
            .reader
            .as_ref()
            .and_then(|reader| reader.target_id(&history_key(&host)).inspect_err(|error| log::warn!("{error}")).ok());
        let (recorder, history_error) = match self.start_recorder(&host) {
            Ok(recorder) => (Some(recorder), None),
            Err(error) => (None, Some(error)),
        };
        let config = TraceConfig { interval: self.interval, ..TraceConfig::default() };
        match Tracer::start(target, config, recorder.as_ref().and_then(Recorder::sender)) {
            Ok(tracer) => {
                self.session = Some(Session { host, target, target_id, tracer, recorder });
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
        Recorder::start(path, &history_key(host)).map_err(|error| {
            log::warn!("{error}");
            format!("history is off: {error}")
        })
    }

    /// Carries out `action`.
    fn apply(&mut self, action: Action) {
        match action {
            Action::Start => self.start(self.host_input.trim().to_owned()),
            Action::Restart => {
                if let Some(host) = self.session.as_ref().map(|session| session.host.clone()) {
                    self.start(host);
                }
            }
            Action::NewTarget => {
                self.host_input.clear();
                self.focus_input = true;
            }
            Action::Close => self.session = None,
        }
    }

    /// Top bar: target input, Start, pause with its menu, interval and the last error. Returns the action asked.
    fn toolbar(&mut self, ui: &mut egui::Ui) -> Option<Action> {
        let mut action = None;
        ui.horizontal(|ui| {
            ui.label("Target:");
            let input = ui.add(TextEdit::singleline(&mut self.host_input).hint_text("host or IP").desired_width(260.0));
            if std::mem::take(&mut self.focus_input) {
                input.request_focus();
            }
            let enter_pressed = input.lost_focus() && ui.input(|input| input.key_pressed(Key::Enter));
            if ui.button("Start").clicked() || enter_pressed {
                action = Some(Action::Start);
            }
            if let Some(session) = &self.session {
                ui.separator();
                let paused = session.tracer.is_paused();
                let pause =
                    ui.add(Button::new(if paused { "\u{25b6} Resume" } else { "\u{23f8} Pause" }).selected(paused));
                if pause.clicked() {
                    session.tracer.set_paused(!paused);
                }
                ui.menu_button("\u{25bc}", |ui| {
                    let items = [
                        ("Reset and Restart", RESTART_SHORTCUT, Action::Restart),
                        ("New Target", NEW_TARGET_SHORTCUT, Action::NewTarget),
                        ("Close (and stop)", CLOSE_SHORTCUT, Action::Close),
                    ];
                    for (label, shortcut, item) in items {
                        let shortcut = ui.ctx().format_shortcut(&shortcut);
                        if ui.add(Button::new(label).shortcut_text(shortcut)).clicked() {
                            action = Some(item);
                        }
                    }
                });
                ui.label("Interval");
                let before = self.interval;
                egui::ComboBox::from_id_salt("interval").selected_text(interval_label(self.interval)).show_ui(
                    ui,
                    |ui| {
                        for (interval, label) in INTERVALS {
                            ui.selectable_value(&mut self.interval, interval, label);
                        }
                    },
                );
                if self.interval != before {
                    session.tracer.set_interval(self.interval);
                }
            }
            if let Some(error) = &self.error {
                ui.colored_label(Color32::LIGHT_RED, error);
            }
        });
        action
    }
}

/// The action of a pressed keyboard shortcut, if any (consumed so widgets don't see it).
fn shortcut_action(ctx: &egui::Context) -> Option<Action> {
    ctx.input_mut(|input| {
        [(RESTART_SHORTCUT, Action::Restart), (NEW_TARGET_SHORTCUT, Action::NewTarget), (CLOSE_SHORTCUT, Action::Close)]
            .into_iter()
            .find(|(shortcut, _)| input.consume_shortcut(shortcut))
            .map(|(_, action)| action)
    })
}

/// Name under which a host's history is stored.
fn history_key(host: &str) -> String {
    host.to_lowercase()
}

/// Timeline title of hop `ttl`: name, address and hop number, as in PingPlotter.
fn hop_title(route: &Route, ttl: u8, names: &NameCache) -> String {
    let address = route.hops().iter().find(|hop| hop.ttl == ttl).and_then(|hop| hop.address);
    match address {
        Some(ip) => {
            let name = names.get(ip).unwrap_or_else(|| ip.to_string());
            format!("{name} ({ip}) hop {ttl}")
        }
        None => format!("hop {ttl} (no response)"),
    }
}

impl eframe::App for PingWatchApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let shortcut = shortcut_action(ui.ctx());
        let clicked = egui::Panel::top("toolbar").show(ui, |ui| self.toolbar(ui)).inner;
        if let Some(action) = shortcut.or(clicked) {
            self.apply(action);
        }
        let Some(session) = &self.session else {
            egui::CentralPanel::default().show(ui, |ui| ui.label("Enter a host and press Start."));
            return;
        };
        let route = session.tracer.snapshot();
        // The clicked hop while it's still on the route, otherwise the target (last) hop.
        let ttl = self
            .selected_ttl
            .filter(|ttl| route.hops().iter().any(|hop| hop.ttl == *ttl))
            .or_else(|| route.hops().last().map(|hop| hop.ttl));
        if let Some(ttl) = ttl {
            let title = hop_title(&route, ttl, &self.names);
            egui::Panel::bottom("timeline").resizable(true).default_size(300.0).show(ui, |ui| {
                self.timeline.show(ui, self.reader.as_ref(), session.target_id, ttl, &title);
            });
        }
        egui::CentralPanel::default().show(ui, |ui| {
            ui.label(RichText::new(format!("{} / {}", session.target, session.host)).strong());
            ui.separator();
            if let Some(clicked) = hop_table::show(ui, &route, &self.names, ttl) {
                self.selected_ttl = Some(clicked);
            }
        });
        ui.ctx().request_repaint_after(REFRESH);
    }
}
