//! One target tab: the host is resolved in the background, then traced; the tab draws its toolbar (pause with its
//! menu, interval, focus) and the three panes.

use std::net::IpAddr;
use std::path::Path;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, SystemTime};

use eframe::egui::{self, Align, Button, Color32, Key, KeyboardShortcut, Layout, Modifiers, RichText, TextEdit, Ui};

use crate::focus::Focus;
use crate::history::{self, Recorder, Store};
use crate::hop_table;
use crate::names::{NameCache, resolve};
use crate::route::{Hop, Route};
use crate::timeline::Timeline;
use crate::tracer::{TraceConfig, Tracer};

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
pub const RESTART_SHORTCUT: KeyboardShortcut = KeyboardShortcut::new(Modifiers::CTRL, Key::R);
/// Opens a tab for a new target.
pub const NEW_TARGET_SHORTCUT: KeyboardShortcut = KeyboardShortcut::new(Modifiers::CTRL, Key::N);
/// Stops the trace and closes its tab.
pub const CLOSE_SHORTCUT: KeyboardShortcut = KeyboardShortcut::new(Modifiers::CTRL, Key::Q);

/// What the user asked for in a tab's menu or with a shortcut; carried out by the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Restart the current trace with fresh statistics.
    Restart,
    /// Open a tab for a new target.
    NewTarget,
    /// Stop the current trace and close its tab.
    Close,
}

/// Resources the tabs share, owned by the app.
#[derive(Debug, Clone, Copy)]
pub struct Context<'a> {
    /// Reverse-DNS cache for hop names.
    pub names: &'a NameCache,
    /// Read connection to the history, `None` if history is off.
    pub reader: Option<&'a Store>,
    /// History database file, `None` if history is off.
    pub history_path: Option<&'a Path>,
}

/// A running trace of one target.
#[derive(Debug)]
struct Session {
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

/// Where a tab is in its life.
#[derive(Debug)]
enum State {
    /// Waiting for the user to enter a host (or after a failed start, with the error).
    Editing(Option<String>),
    /// The host is being resolved in the background.
    Resolving(Receiver<Result<IpAddr, String>>),
    /// The trace runs.
    Running(Session),
}

/// The final-hop numbers of a tab for the All Targets overview.
#[derive(Debug, Clone, PartialEq)]
pub struct Overview {
    /// Host as entered, or a placeholder for an empty tab.
    pub title: String,
    /// Traced address, once resolved.
    pub target: Option<IpAddr>,
    /// Short state description: running, paused, resolving, error.
    pub status: String,
    /// Number of hops to the target, once it answered.
    pub hops: Option<usize>,
    /// The target hop, once it answered.
    pub target_hop: Option<Hop>,
}

/// One target tab.
#[derive(Debug)]
pub struct Tab {
    /// Contents of the host input of an editing tab.
    host_input: String,
    /// Host being resolved or traced.
    host: String,
    /// Life state.
    state: State,
    /// Non-fatal problem shown in the toolbar (history is off).
    warning: Option<String>,
    /// Hop chosen for the timeline by clicking its row; `None` means the target hop.
    selected_ttl: Option<u8>,
    /// Timeline pane state.
    timeline: Timeline,
    /// Probe interval.
    interval: Duration,
    /// Period the hop table summarizes.
    focus: Focus,
    /// Move keyboard focus to the host input on the next frame.
    focus_input: bool,
}

impl Tab {
    /// An empty tab asking for a host.
    pub fn new() -> Self {
        Self {
            host_input: String::new(),
            host: String::new(),
            state: State::Editing(None),
            warning: None,
            selected_ttl: None,
            timeline: Timeline::default(),
            interval: Duration::from_secs(1),
            focus: Focus::default(),
            focus_input: true,
        }
    }

    /// A tab that starts tracing `host` right away.
    pub fn tracing(host: String) -> Self {
        let mut tab = Self::new();
        tab.host_input.clone_from(&host);
        tab.begin(host);
        tab
    }

    /// Tab title: the host, or a placeholder while none is entered.
    pub fn title(&self) -> String {
        if self.host.is_empty() { "New target".to_owned() } else { self.host.clone() }
    }

    /// Starts resolving `host` in the background (stopping any running trace); tracing begins once it resolves.
    fn begin(&mut self, host: String) {
        self.state = State::Editing(None);
        self.host = host.trim().to_owned();
        self.selected_ttl = None;
        self.warning = None;
        if self.host.is_empty() {
            return;
        }
        let (sender, receiver) = mpsc::channel();
        let name = self.host.clone();
        let spawned = thread::Builder::new().name(format!("resolve-{name}")).spawn(move || {
            // The tab may be gone by now; then nobody needs the answer.
            sender.send(resolve(&name).map_err(|error| error.to_string())).ok();
        });
        self.state = match spawned {
            Ok(_) => State::Resolving(receiver),
            Err(error) => State::Editing(Some(format!("cannot resolve {}: {error}", self.host))),
        };
    }

    /// Restarts the trace of the current host with fresh statistics.
    pub fn restart(&mut self) {
        let host = self.host.clone();
        self.begin(host);
    }

    /// Moves a resolving tab on once the address is known: starts the history recorder and the tracer.
    /// Called every frame for every tab, shown or not.
    pub fn poll(&mut self, context: Context<'_>) {
        let State::Resolving(receiver) = &self.state else {
            return;
        };
        let target = match receiver.try_recv() {
            Ok(Ok(target)) => target,
            Ok(Err(error)) => return self.fail(error),
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => return self.fail(format!("cannot resolve {}", self.host)),
        };
        let key = history_key(&self.host);
        let target_id = context
            .reader
            .and_then(|reader| reader.target_id(&key).inspect_err(|error| log::warn!("{error}")).ok());
        let recorder = match context.history_path {
            None => Err("history is off: no data folder".to_owned()),
            Some(path) => Recorder::start(path, &key).map_err(|error| format!("history is off: {error}")),
        };
        if let Err(warning) = &recorder {
            log::warn!("{warning}");
        }
        let recorder = recorder.map_err(|warning| self.warning = Some(warning)).ok();
        let config = TraceConfig { interval: self.interval, ..TraceConfig::default() };
        match Tracer::start(target, config, recorder.as_ref().and_then(Recorder::sender)) {
            Ok(tracer) => self.state = State::Running(Session { target, target_id, tracer, recorder }),
            Err(error) => self.fail(error.to_string()),
        }
    }

    /// Logs a start failure and returns to editing with the error shown.
    fn fail(&mut self, error: String) {
        log::warn!("{error}");
        self.state = State::Editing(Some(error));
    }

    /// Current route with the tab's focus, if tracing.
    fn route(&self) -> Option<Route> {
        let State::Running(session) = &self.state else {
            return None;
        };
        Some(session.tracer.snapshot(self.focus.window(self.timeline.period_length())))
    }

    /// Final-hop numbers for the All Targets overview.
    pub fn overview(&self) -> Overview {
        let route = self.route();
        let target_hop = route
            .as_ref()
            .and_then(|route| route.hops().last().filter(|hop| hop.address == Some(route.target())));
        let status = match &self.state {
            State::Editing(Some(error)) => error.clone(),
            State::Editing(None) => "no target".to_owned(),
            State::Resolving(_) => "resolving…".to_owned(),
            State::Running(session) if session.tracer.is_paused() => "paused".to_owned(),
            State::Running(_) => "running".to_owned(),
        };
        Overview {
            title: self.title(),
            target: route.as_ref().map(Route::target),
            status,
            hops: target_hop.map(|hop| usize::from(hop.ttl)),
            target_hop: target_hop.cloned(),
        }
    }

    /// Draws the tab; returns the action asked for in its menu.
    pub fn ui(&mut self, ui: &mut Ui, context: Context<'_>) -> Option<Action> {
        match &self.state {
            State::Editing(error) => {
                let error = error.clone();
                self.editor(ui, error.as_deref());
                None
            }
            State::Resolving(_) => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(format!("Resolving {}…", self.host));
                });
                None
            }
            State::Running(_) => self.running(ui, context),
        }
    }

    /// Host entry of an editing tab.
    fn editor(&mut self, ui: &mut Ui, error: Option<&str>) {
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label("Target:");
            let input = ui.add(TextEdit::singleline(&mut self.host_input).hint_text("host or IP").desired_width(320.0));
            if std::mem::take(&mut self.focus_input) {
                input.request_focus();
            }
            let enter_pressed = input.lost_focus() && ui.input(|input| input.key_pressed(Key::Enter));
            if ui.button("Trace").clicked() || enter_pressed {
                let host = self.host_input.clone();
                self.begin(host);
            }
        });
        if let Some(error) = error {
            ui.colored_label(Color32::LIGHT_RED, error);
        }
    }

    /// Toolbar and panes of a running tab.
    fn running(&mut self, ui: &mut Ui, context: Context<'_>) -> Option<Action> {
        let route = self.route()?;
        let action = egui::Panel::top("tab-toolbar").show(ui, |ui| self.toolbar(ui, &route)).inner;
        let State::Running(session) = &self.state else {
            return action;
        };
        // The clicked hop while it's still on the route, otherwise the target (last) hop.
        let ttl = self
            .selected_ttl
            .filter(|ttl| route.hops().iter().any(|hop| hop.ttl == *ttl))
            .or_else(|| route.hops().last().map(|hop| hop.ttl));
        if let Some(ttl) = ttl {
            let title = hop_title(&route, ttl, context.names);
            egui::Panel::bottom("timeline").resizable(true).default_size(300.0).show(ui, |ui| {
                self.timeline.show(ui, context.reader, session.target_id, ttl, &title);
            });
        }
        egui::CentralPanel::default().show(ui, |ui| {
            if let Some(clicked) = hop_table::show(ui, &route, context.names, ttl) {
                self.selected_ttl = Some(clicked);
            }
        });
        action
    }

    /// Target line with pause and its menu, interval, focus and the focus range. Returns the action asked.
    fn toolbar(&mut self, ui: &mut Ui, route: &Route) -> Option<Action> {
        let State::Running(session) = &self.state else {
            return None;
        };
        let mut action = None;
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("{} / {}", session.target, self.host)).strong());
            ui.separator();
            let paused = session.tracer.is_paused();
            let pause = ui.add(Button::new(if paused { "\u{25b6} Resume" } else { "\u{23f8} Pause" }).selected(paused));
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
            egui::ComboBox::from_id_salt("interval")
                .selected_text(interval_label(self.interval))
                .show_ui(ui, |ui| {
                    for (interval, label) in INTERVALS {
                        ui.selectable_value(&mut self.interval, interval, label);
                    }
                });
            if self.interval != before {
                session.tracer.set_interval(self.interval);
            }
            ui.label("Focus");
            egui::ComboBox::from_id_salt("focus").selected_text(self.focus.label()).show_ui(ui, |ui| {
                for focus in Focus::ALL {
                    ui.selectable_value(&mut self.focus, focus, focus.label());
                }
            });
            if let Some(warning) = &self.warning {
                ui.colored_label(Color32::LIGHT_RED, warning);
            }
            if let Some(from_ms) = route.focus_start_ms() {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let now_ms = history::unix_ms(SystemTime::now());
                    ui.label(format!("Focus: {} - {}", clock(from_ms), clock(now_ms)));
                });
            }
        });
        action
    }
}

/// Menu label of a probe interval.
fn interval_label(interval: Duration) -> String {
    INTERVALS
        .iter()
        .find(|(known, _)| *known == interval)
        .map_or_else(|| format!("{:.1} seconds", interval.as_secs_f64()), |(_, label)| (*label).to_owned())
}

/// Local wall-clock time `HH:MM:SS` of Unix milliseconds.
fn clock(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|time| time.with_timezone(&chrono::Local).format("%H:%M:%S").to_string())
        .unwrap_or_default()
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
