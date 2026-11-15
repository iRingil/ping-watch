//! One target tab: the host is resolved in the background, then traced; the tab draws its toolbar (pause with its
//! menu, interval, focus) and the three panes.

use std::net::IpAddr;
use std::path::Path;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, SystemTime};

use eframe::egui::{
    self, Align, Frame, Key, KeyboardShortcut, Layout, Margin, Modifiers, Popup, Rect, RichText, Stroke, Ui, pos2,
};

use crate::probe::names::{NameCache, resolve};
use crate::probe::route::{Hop, Route};
use crate::probe::tracer::{TraceConfig, Tracer};
use crate::store::history::{self, HopKey, Recorder, Store};
use crate::store::targets::{TargetList, TraceSettings};
use crate::store::window_state::SavedTab;
use crate::ui::focus::Focus;
use crate::ui::hop_table;
use crate::ui::icons::{self, Icon, Style};
use crate::ui::latency_bars;
use crate::ui::target_entry::TargetEntry;
use crate::ui::theme;
use crate::ui::timeline::{Period, Timeline};

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
/// Width of the running trace's menu.
const TRACE_MENU_WIDTH: f32 = 230.0;
/// Share of the pane area the hop table gets at first; the timeline gets the rest.
pub const TABLE_SHARE: f32 = 0.66;
/// Limits of the table's share when the splitter is dragged, so neither pane disappears.
const TABLE_SHARE_RANGE: std::ops::RangeInclusive<f32> = 0.2..=0.85;
/// Height of the draggable bar between the panes.
const SPLITTER_HEIGHT: f32 = 10.0;
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
    /// The user's target list, `None` if history is off.
    pub targets: Option<&'a TargetList>,
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
    /// The hop standing for the target while tracing: the target itself once it answered, else the farthest probe
    /// (all lost while the target is unreachable).
    pub target_hop: Option<Hop>,
    /// Whether the target itself answers at the end of the route.
    pub reached: bool,
}

/// One target tab.
#[derive(Debug)]
pub struct Tab {
    /// Target field of an editing tab.
    entry: TargetEntry,
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
    /// The target's chart on the All targets page, `None` while it isn't charted there.
    chart: Option<Timeline>,
    /// Settings as last stored for this target; compared each frame to store changes.
    saved_settings: Option<TraceSettings>,
    /// Pause the trace as soon as it starts (a tab restored from a paused one).
    start_paused: bool,
    /// Name given to the host in the target list.
    name: Option<String>,
    /// Reverse-DNS name of the traced address, once known.
    reverse: Option<String>,
}

impl Tab {
    /// An empty tab asking for a host.
    pub fn new() -> Self {
        Self {
            entry: TargetEntry::new(true),
            host: String::new(),
            state: State::Editing(None),
            warning: None,
            selected_ttl: None,
            timeline: Timeline::default(),
            interval: Duration::from_secs(1),
            focus: Focus::default(),
            chart: None,
            saved_settings: None,
            start_paused: false,
            name: None,
            reverse: None,
        }
    }

    /// A tab that starts tracing `host` right away.
    pub fn tracing(host: String) -> Self {
        let mut tab = Self::new();
        tab.entry.set_text(&host);
        tab.begin(host);
        tab
    }

    /// A tab that starts tracing `host` right away with `interval`, unless the target has remembered settings.
    pub fn tracing_every(host: String, interval: Duration) -> Self {
        let mut tab = Self::tracing(host);
        tab.interval = interval;
        tab
    }

    /// A tab restored from the last run: traces `saved.host` again, starting paused if it wasn't tracing then,
    /// with its chart on the All targets page if it had one.
    pub fn restored(saved: &SavedTab) -> Self {
        let mut tab = Self::tracing(saved.host.clone());
        tab.start_paused = !saved.tracing;
        tab.set_charted(saved.charted);
        tab
    }

    /// What to remember about this tab: its host, whether it is tracing (resolving counts as about to trace) and
    /// charted; `None` for a tab without a host. `shown` is left for the caller.
    pub fn remembered(&self) -> Option<SavedTab> {
        if self.host.is_empty() {
            return None;
        }
        let tracing = match &self.state {
            State::Running(session) => !session.tracer.is_paused(),
            State::Resolving(_) => !self.start_paused,
            State::Editing(_) => false,
        };
        Some(SavedTab { host: self.host.clone(), tracing, charted: self.is_charted(), shown: false })
    }

    /// Changes the probe interval, also of the running trace.
    pub fn set_interval(&mut self, interval: Duration) {
        self.interval = interval;
        if let State::Running(session) = &self.state {
            session.tracer.set_interval(interval);
        }
    }

    /// Whether the target is charted on the All targets page.
    pub fn is_charted(&self) -> bool {
        self.chart.is_some()
    }

    /// Shows or hides the target's chart on the All targets page.
    pub fn set_charted(&mut self, charted: bool) {
        if charted != self.is_charted() {
            self.chart = charted.then(Timeline::compact);
        }
    }

    /// Draws the target's chart for the All targets page (the target hop over `period`, which the chart's own
    /// controls may change).
    pub fn chart(&mut self, ui: &mut Ui, history_path: Option<&Path>, period: &mut Period) {
        let route = self.route();
        let title = self.label().unwrap_or_else(|| self.title());
        let Some(chart) = &mut self.chart else {
            return;
        };
        chart.set_period(*period);
        match (&self.state, route.as_ref().and_then(|route| route.hops().last())) {
            (State::Running(session), Some(_)) => {
                chart.show(ui, history_path, session.target_id, HopKey::Target, &title, self.interval);
            }
            _ => {
                ui.label(RichText::new(title).font(theme::heading_font(15.0)));
                ui.weak("The chart appears once the trace runs.");
            }
        }
        *period = chart.period();
    }

    /// Stores the running trace's interval, focus and timeline period for its target whenever they changed.
    fn save_settings(&mut self, targets: Option<&TargetList>) {
        let (State::Running(_), Some(targets)) = (&self.state, targets) else {
            return;
        };
        let current = TraceSettings { interval: self.interval, focus: self.focus, period: self.timeline.period() };
        if self.saved_settings == Some(current) {
            return;
        }
        if let Err(error) = targets.save_settings(&self.host, current) {
            log::warn!("cannot save the settings of {}: {error}", self.host);
        }
        // Marked as handled even after a failure: one attempt per change, not one per frame.
        self.saved_settings = Some(current);
    }

    /// The traced target as shown on its tab and in headings (see [`target_label`]); `None` until the trace runs.
    pub fn label(&self) -> Option<String> {
        self.target()
            .map(|target| target_label(target, &self.host, self.name.as_deref(), self.reverse.as_deref()))
    }

    /// Looks up the names of the traced target: its name in the target list (it may be edited at any time) and
    /// the reverse-DNS name of its address.
    fn refresh_names(&mut self, context: Context<'_>) {
        let Some(target) = self.target() else {
            return;
        };
        if let Some(targets) = context.targets {
            self.name = targets.name(&self.host).inspect_err(|error| log::warn!("{error}")).ok().flatten();
        }
        // Without a PTR record the lookup gives back the address itself, which names nothing.
        self.reverse = context.names.get(target).filter(|name| *name != target.to_string());
    }

    /// Address being traced, `None` until the trace runs.
    pub fn target(&self) -> Option<IpAddr> {
        match &self.state {
            State::Running(session) => Some(session.target),
            State::Editing(_) | State::Resolving(_) => None,
        }
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
        self.refresh_names(context);
        let State::Resolving(receiver) = &self.state else {
            return;
        };
        let target = match receiver.try_recv() {
            Ok(Ok(target)) => target,
            Ok(Err(error)) => return self.fail(error),
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => return self.fail(format!("cannot resolve {}", self.host)),
        };
        if let Some(targets) = context.targets
            && let Err(error) = targets.remember(&self.host, target)
        {
            log::warn!("cannot add {} to the target list: {error}", self.host);
        }
        // The settings this target was last traced with come back with it.
        self.saved_settings = context
            .targets
            .and_then(|targets| targets.settings(&self.host).inspect_err(|error| log::warn!("{error}")).ok())
            .flatten();
        if let Some(settings) = self.saved_settings {
            self.interval = settings.interval;
            self.focus = settings.focus;
            self.timeline.set_period(settings.period);
        }
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
            Ok(tracer) => {
                if std::mem::take(&mut self.start_paused) {
                    tracer.set_paused(true);
                }
                self.state = State::Running(Session { target, target_id, tracer, recorder });
            }
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
        let now_ms = history::unix_ms(SystemTime::now());
        self.route_with(self.focus, self.timeline.visible_range(now_ms))
    }

    /// Current route summarized over `focus`, where Auto means `visible` (Unix ms), if tracing.
    fn route_with(&self, focus: Focus, visible: (i64, i64)) -> Option<Route> {
        let State::Running(session) = &self.state else {
            return None;
        };
        let now_ms = history::unix_ms(SystemTime::now());
        Some(session.tracer.snapshot(focus.range(visible, now_ms)))
    }

    /// Problem to show in the status bar, if any: a failed start, a probing failure (network down) — which matters
    /// more than the history warning — or the history warning.
    pub fn problem(&self) -> Option<String> {
        match &self.state {
            State::Editing(Some(error)) => Some(error.clone()),
            State::Running(session) => session.tracer.last_error().or_else(|| self.warning.clone()),
            State::Editing(None) | State::Resolving(_) => self.warning.clone(),
        }
    }

    /// Whether the trace is paused.
    pub fn is_paused(&self) -> bool {
        matches!(&self.state, State::Running(session) if session.tracer.is_paused())
    }

    /// Final-hop numbers with the tab's own focus.
    pub fn overview(&self) -> Overview {
        let now_ms = history::unix_ms(SystemTime::now());
        self.overview_with(self.focus, self.timeline.visible_range(now_ms))
    }

    /// Final-hop numbers summarized over `focus`, where Auto means `visible` (Unix ms) — for the All targets page,
    /// which has its own focus.
    pub fn overview_with(&self, focus: Focus, visible: (i64, i64)) -> Overview {
        let route = self.route_with(focus, visible);
        // The last hop stands for the target: the target itself once it answered, otherwise the farthest probe,
        // whose losses are the target's (an unreachable target is shown as lost, not as "no data").
        let target_hop = route.as_ref().and_then(|route| route.hops().last());
        let reached = route
            .as_ref()
            .is_some_and(|route| target_hop.is_some_and(|hop| hop.address == Some(route.target())));
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
            hops: target_hop.filter(|_| reached).map(|hop| usize::from(hop.ttl)),
            target_hop: target_hop.cloned(),
            reached,
        }
    }

    /// Draws the tab: toolbar, the hop table pane and the timeline pane. The panes keep their frames and the table
    /// its header even before a trace runs, so an empty tab shows where everything will appear. Returns the action
    /// asked for in the menu.
    pub fn ui(&mut self, ui: &mut Ui, context: Context<'_>, table_share: &mut f32) -> Option<Action> {
        self.save_settings(context.targets);
        let route = self.route();
        let toolbar_frame = Frame::new().fill(theme::CHASSIS).inner_margin(Margin::symmetric(10, 8));
        let action = egui::Panel::top("tab-toolbar")
            .frame(toolbar_frame)
            .show(ui, |ui| self.toolbar(ui, context.targets))
            .inner;
        // The clicked hop while it's still on the route, otherwise the target (last) hop.
        let ttl = route.as_ref().and_then(|route| {
            self.selected_ttl
                .filter(|ttl| route.hops().iter().any(|hop| hop.ttl == *ttl))
                .or_else(|| route.hops().last().map(|hop| hop.ttl))
        });
        let area = ui.available_rect_before_wrap();
        ui.allocate_rect(area, egui::Sense::hover());
        let (table_rect, splitter_rect, timeline_rect) = split(area, *table_share);
        splitter(ui, splitter_rect, area, table_share);

        pane(ui, table_rect, Margin::same(1), |ui| {
            let focus = route.as_ref().and_then(focus_label);
            if let Some(clicked) = hop_table::show(ui, route.as_ref(), context.names, ttl, focus.as_deref()) {
                self.selected_ttl = Some(clicked);
            }
        });
        pane(ui, timeline_rect, Margin::same(8), |ui| match (&self.state, &route, ttl) {
            (State::Running(session), Some(route), Some(ttl)) => {
                let title = hop_title(route, ttl, context.names);
                // The target's line is read by target, not hop number, so it keeps its history when the route
                // length changes.
                let is_target =
                    route.hops().last().is_some_and(|hop| hop.ttl == ttl && hop.address == Some(route.target()));
                let hop = if is_target { HopKey::Target } else { HopKey::Ttl(ttl) };
                self.timeline.show(ui, context.history_path, session.target_id, hop, &title, self.interval);
            }
            _ => {
                ui.weak("The history of the selected hop appears here once a trace runs.");
            }
        });
        action
    }

    /// Toolbar: the target field (or the traced target with pause and its menu), interval, focus, problems and the
    /// latency zone legend. Returns the action asked.
    fn toolbar(&mut self, ui: &mut Ui, targets: Option<&TargetList>) -> Option<Action> {
        let mut action = None;
        ui.horizontal(|ui| {
            if matches!(self.state, State::Running(_)) {
                action = self.trace_controls(ui);
            } else {
                self.target_entry(ui, targets);
            }
            ui.add_space(12.0);
            ui.weak("Interval");
            let mut interval = self.interval;
            if interval_choice(ui, &mut interval) {
                self.set_interval(interval);
            }
            ui.weak("Focus");
            focus_choice(ui, &mut self.focus);
            ui.with_layout(Layout::right_to_left(Align::Center), latency_bars::legend);
        });
        action
    }

    /// Target field and the Trace button (a spinner while resolving).
    fn target_entry(&mut self, ui: &mut Ui, targets: Option<&TargetList>) {
        let resolving = matches!(self.state, State::Resolving(_));
        let mut start = self.entry.show(ui, targets, !resolving);
        if resolving {
            ui.spinner();
            ui.weak(format!("Resolving {}…", self.host));
        } else if icons::button(ui, Icon::Play, Style::Primary, "Trace (Enter)").clicked() {
            start = true;
        }
        if start {
            let host = self.entry.text().to_owned();
            self.begin(host);
        }
    }

    /// Traced target, pause/resume and the menu of a running tab. Returns the menu action asked.
    fn trace_controls(&mut self, ui: &mut Ui) -> Option<Action> {
        let State::Running(session) = &self.state else {
            return None;
        };
        let mut action = None;
        let label = target_label(session.target, &self.host, self.name.as_deref(), self.reverse.as_deref());
        ui.label(RichText::new(label).font(theme::heading_font(17.0)));
        ui.add_space(8.0);
        let paused = session.tracer.is_paused();
        // One split button, as in PingPlotter: the face pauses or resumes, the arrow lists the other actions.
        let split = if paused {
            icons::split_button(ui, Icon::Play, theme::GOOD, "Resume probing")
        } else {
            icons::split_button(ui, Icon::Pause, theme::SIGNAL, "Pause probing (the numbers are kept)")
        };
        if split.main.clicked() {
            session.tracer.set_paused(!paused);
        }
        // Only actions on this trace; a new target is opened with "+".
        Popup::from_toggle_button_response(&split.menu).width(TRACE_MENU_WIDTH).show(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            let items = [
                ("Reset and restart", RESTART_SHORTCUT, Action::Restart),
                ("Close and stop", CLOSE_SHORTCUT, Action::Close),
            ];
            for (label, shortcut, item) in items {
                if icons::menu_row(ui, label, &ui.ctx().format_shortcut(&shortcut)).clicked() {
                    action = Some(item);
                    ui.close();
                }
            }
        });
        action
    }
}

/// Splits the pane area into the hop table (top, `table_share` of the height, kept by the app for all tabs), the splitter and the timeline
/// (the rest). Computed from the window size every frame, so the panes are in place from the first frame.
pub fn split(area: Rect, table_share: f32) -> (Rect, Rect, Rect) {
    let usable = area.height() - SPLITTER_HEIGHT;
    let splitter_top = area.top() + usable * table_share;
    let timeline_top = splitter_top + SPLITTER_HEIGHT;
    let table = Rect::from_min_max(area.min, pos2(area.right(), splitter_top));
    let splitter = Rect::from_min_max(pos2(area.left(), splitter_top), pos2(area.right(), timeline_top));
    let timeline = Rect::from_min_max(pos2(area.left(), timeline_top), area.max);
    (table, splitter, timeline)
}

/// The bar between the panes: dragging it moves the border; it lights up under the pointer.
pub fn splitter(ui: &mut Ui, rect: Rect, area: Rect, table_share: &mut f32) {
    let response = ui
        .interact(rect, ui.scope_id().with("pane-splitter"), egui::Sense::drag())
        .on_hover_cursor(egui::CursorIcon::ResizeVertical);
    let usable = (area.height() - SPLITTER_HEIGHT).max(1.0);
    if response.dragged() {
        let share = *table_share + response.drag_delta().y / usable;
        *table_share = share.clamp(*TABLE_SHARE_RANGE.start(), *TABLE_SHARE_RANGE.end());
    }
    let active = response.hovered() || response.dragged();
    let color = if active { theme::SIGNAL } else { theme::RULE };
    let center = rect.center();
    // A full-width hairline with a thicker grip in the middle shows where to grab.
    ui.painter().hline(rect.x_range(), center.y, Stroke::new(1.0, color));
    ui.painter().hline((center.x - 24.0)..=(center.x + 24.0), center.y, Stroke::new(4.0, color));
}

/// Draws a framed pane exactly into `rect`: the content gets its own child ui clipped to the rect, so whatever it
/// draws can never move or resize the pane.
pub fn pane(ui: &mut Ui, rect: Rect, margin: Margin, content: impl FnOnce(&mut Ui)) {
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(Layout::top_down(Align::Min)));
    child.set_clip_rect(rect.intersect(ui.clip_rect()));
    theme::pane_frame().inner_margin(margin).show(&mut child, |ui| {
        ui.set_min_size(ui.available_size());
        content(ui);
    });
}

/// "Focus: from – to" of a route, shown under its latency bars.
fn focus_label(route: &Route) -> Option<String> {
    let from_ms = route.focus_start_ms()?;
    let to_ms = route.focus_end_ms().unwrap_or_else(|| history::unix_ms(SystemTime::now()));
    Some(format!("Focus {} – {}", clock(from_ms), clock(to_ms)))
}

/// Combo box choosing a probe interval; returns whether it changed.
pub fn interval_choice(ui: &mut Ui, interval: &mut Duration) -> bool {
    let before = *interval;
    egui::ComboBox::from_id_salt("interval").selected_text(interval_label(*interval)).show_ui(ui, |ui| {
        for (choice, label) in INTERVALS {
            ui.selectable_value(interval, choice, label);
        }
    });
    *interval != before
}

/// Combo box choosing the focus.
pub fn focus_choice(ui: &mut Ui, focus: &mut Focus) {
    egui::ComboBox::from_id_salt("focus").selected_text(focus.label()).show_ui(ui, |ui| {
        for choice in Focus::ALL {
            ui.selectable_value(focus, choice, choice.label());
        }
    });
}

/// Menu label of a probe interval.
fn interval_label(interval: Duration) -> String {
    INTERVALS
        .iter()
        .find(|(known, _)| *known == interval)
        .map_or_else(|| format!("{:.1} seconds", interval.as_secs_f64()), |(_, label)| (*label).to_owned())
}

/// Local wall-clock time `HH:MM:SS` of Unix milliseconds.
pub fn clock(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|time| time.with_timezone(&chrono::Local).format("%H:%M:%S").to_string())
        .unwrap_or_default()
}

/// Label of a traced target, "address / description": the description is the `name` from the target list if it
/// has one, else the `host` as entered if that is a domain name, else (an IP was entered) the `reverse` DNS name of
/// the address, if any — "9.9.9.9 / Quad9 DNS", "140.82.121.4 / github.com", "1.1.1.1 / one.one.one.one".
fn target_label(target: IpAddr, host: &str, name: Option<&str>, reverse: Option<&str>) -> String {
    let entered_ip = host.trim().parse::<IpAddr>().is_ok();
    let description = name.or(if entered_ip { reverse } else { Some(host.trim()) });
    match description {
        Some(description) => format!("{target} / {description}"),
        None => target.to_string(),
    }
}

/// Name under which a host's history is stored.
fn history_key(host: &str) -> String {
    host.to_lowercase()
}

/// Timeline title of hop `ttl`, in the style of the target labels: "Hop 10: 208.67.222.222 / dns.sse.cisco.com"
/// (the reverse name only if there is one).
fn hop_title(route: &Route, ttl: u8, names: &NameCache) -> String {
    let address = route.hops().iter().find(|hop| hop.ttl == ttl).and_then(|hop| hop.address);
    match address {
        Some(ip) => match names.get(ip).filter(|name| *name != ip.to_string()) {
            Some(name) => format!("Hop {ttl}: {ip} / {name}"),
            None => format!("Hop {ttl}: {ip}"),
        },
        None => format!("Hop {ttl}: no response"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_label_is_address_and_one_description() {
        let yandex: IpAddr = "2a02:6b8::feed:ff".parse().expect("valid address");
        let cloudflare = IpAddr::from([1, 1, 1, 1]);
        let github = IpAddr::from([140, 82, 121, 4]);
        // A name wins over everything.
        assert_eq!(
            target_label(yandex, "2a02:6b8::feed:0ff", Some("Yandex DNS"), None),
            "2a02:6b8::feed:ff / Yandex DNS"
        );
        assert_eq!(target_label(github, "github.com", Some("GitHub"), Some("lb.github.net")), "140.82.121.4 / GitHub");
        // An entered domain name.
        assert_eq!(target_label(github, "github.com", None, Some("lb.github.net")), "140.82.121.4 / github.com");
        // An entered IP: its reverse name, or nothing.
        assert_eq!(target_label(cloudflare, "1.1.1.1", None, Some("one.one.one.one")), "1.1.1.1 / one.one.one.one");
        assert_eq!(target_label(cloudflare, "1.1.1.1", None, None), "1.1.1.1");
    }
}
