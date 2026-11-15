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

use crate::alerts::WindowStats;
use crate::i18n::{Lang, Language, n, tr, trf};
use crate::probe::names::{NameCache, ResolveError, resolve};
use crate::probe::route::{Hop, Route};
use crate::probe::tracer::{TraceConfig, Tracer};
use crate::snapshot::{AlertRow, Chart, HopRow, TargetReport};
use crate::store::alerts::JournalEntry;
use crate::store::history::{self, HopKey, Recorder, Store};
use crate::store::settings::{AppSettings, INTERVALS};
use crate::store::targets::{TargetList, TraceSettings};
use crate::store::window_state::SavedTab;
use crate::ui::focus::Focus;
use crate::ui::hop_table;
use crate::ui::icons::{self, Icon, Style};
use crate::ui::latency_bars;
use crate::ui::target_entry::TargetEntry;
use crate::ui::theme;
use crate::ui::timeline::{Period, Series, Timeline, Window};

/// Names of the probe intervals offered ([`INTERVALS`], in the same order).
const INTERVAL_LABELS: [&str; 7] = [
    n!("0.5 seconds"),
    n!("1 second"),
    n!("2.5 seconds"),
    n!("5 seconds"),
    n!("10 seconds"),
    n!("30 seconds"),
    n!("60 seconds"),
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
/// Stops the trace and closes its tab (Ctrl+W, as tabs close everywhere on Windows; Ctrl+Q would mean "quit").
pub const CLOSE_SHORTCUT: KeyboardShortcut = KeyboardShortcut::new(Modifiers::CTRL, Key::W);

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
    /// The app's settings (probe timeout, hop limit, history retention).
    pub settings: &'a AppSettings,
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
    Resolving(Receiver<Result<IpAddr, ResolveError>>),
    /// The trace runs.
    Running(Session),
}

/// The final-hop numbers of a tab for the overview page.
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
    /// Interface language, taken from the settings every frame (for texts made outside drawing).
    language: Language,
    /// Hop chosen for the timeline by clicking its row; `None` means the target hop.
    selected_ttl: Option<u8>,
    /// Timeline pane state.
    timeline: Timeline,
    /// Probe interval.
    interval: Duration,
    /// Period the hop table summarizes.
    focus: Focus,
    /// The target's chart on the overview page, `None` while it isn't charted there.
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
    /// An empty tab asking for a host, with the interval, focus and timeline period of `defaults`.
    pub fn new(defaults: TraceSettings) -> Self {
        let mut timeline = Timeline::default();
        timeline.set_period(defaults.period);
        Self {
            entry: TargetEntry::new(true),
            host: String::new(),
            state: State::Editing(None),
            warning: None,
            selected_ttl: None,
            timeline,
            interval: defaults.interval,
            focus: defaults.focus,
            chart: None,
            saved_settings: None,
            start_paused: false,
            name: None,
            reverse: None,
            language: Language::English,
        }
    }

    /// A tab that starts tracing `host` right away.
    pub fn tracing(host: String, defaults: TraceSettings) -> Self {
        let mut tab = Self::new(defaults);
        tab.entry.set_text(&host);
        tab.begin(host);
        tab
    }

    /// A tab restored from the last run: traces `saved.host` again, starting paused if it wasn't tracing then,
    /// with its chart on the overview page if it had one.
    pub fn restored(saved: &SavedTab, defaults: TraceSettings) -> Self {
        let mut tab = Self::tracing(saved.host.clone(), defaults);
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

    /// Probe interval.
    pub fn interval(&self) -> Duration {
        self.interval
    }

    /// Whether the target is charted on the overview page.
    pub fn is_charted(&self) -> bool {
        self.chart.is_some()
    }

    /// Shows or hides the target's chart on the overview page.
    pub fn set_charted(&mut self, charted: bool) {
        if charted != self.is_charted() {
            self.chart = charted.then(Timeline::compact);
        }
    }

    /// Draws the target's chart for the overview page (the target hop over `period`, which the chart's own
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
                ui.weak(tr!(ui, "The chart appears once the trace runs."));
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
        if self.host.is_empty() { tr!(self.language, "New target").to_owned() } else { self.host.clone() }
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
        // A fixed thread name: the host may hold anything, and a NUL in a thread name panics.
        let spawned = thread::Builder::new().name("resolve".to_owned()).spawn(move || {
            // The tab may be gone by now; then nobody needs the answer.
            sender.send(resolve(&name)).ok();
        });
        self.state = match spawned {
            Ok(_) => State::Resolving(receiver),
            Err(error) => State::Editing(Some(trf!(
                self.language,
                "Cannot resolve {host}: {error}",
                host = self.host,
                error = error
            ))),
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
        self.language = context.settings.language;
        self.refresh_names(context);
        let State::Resolving(receiver) = &self.state else {
            return;
        };
        let target = match receiver.try_recv() {
            Ok(Ok(target)) => target,
            Ok(Err(ResolveError::Lookup { host, source })) => {
                return self.fail(lookup_message(&host, &source, self.language));
            }
            Ok(Err(ResolveError::NoAddress(host))) => {
                return self.fail(trf!(self.language, "{host} has no address", host = host));
            }
            Ok(Err(ResolveError::Invalid(_))) => {
                return self.fail(tr!(self.language, "This is not a host name or an IP address").to_owned());
            }
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => {
                return self.fail(trf!(self.language, "Cannot resolve {host}", host = self.host));
            }
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
            None => Err(tr!(self.language, "History is off: no data folder").to_owned()),
            Some(path) => Recorder::start(path, &key)
                .map_err(|error| trf!(self.language, "History is off: {error}", error = error)),
        };
        if let Err(warning) = &recorder {
            log::warn!("{warning}");
        }
        let recorder = recorder.map_err(|warning| self.warning = Some(warning)).ok();
        let config = TraceConfig {
            interval: self.interval,
            timeout: context.settings.timeout,
            max_hops: context.settings.max_hops,
        };
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

    /// Host as entered, empty for a tab without a target.
    pub fn host(&self) -> &str {
        &self.host
    }

    /// Id of the target in the history (for timeline marks), once the trace runs with history on.
    pub fn history_id(&self) -> Option<i64> {
        match &self.state {
            State::Running(session) => session.target_id,
            State::Editing(_) | State::Resolving(_) => None,
        }
    }

    /// Whether the alerts watch this tab: it traces and isn't paused.
    pub fn is_watched(&self) -> bool {
        matches!(&self.state, State::Running(session) if !session.tracer.is_paused())
    }

    /// The target's own probes (its last hop) over the last `window`, for the alerts; `None` unless watched.
    pub fn window_stats(&self, window: Duration) -> Option<WindowStats> {
        let State::Running(session) = &self.state else {
            return None;
        };
        if session.tracer.is_paused() {
            return None;
        }
        let now_ms = history::unix_ms(SystemTime::now());
        let window_ms = i64::try_from(window.as_millis()).unwrap_or(i64::MAX);
        let route = session.tracer.snapshot(Some((now_ms.saturating_sub(window_ms), now_ms)));
        let hop = route.hops().last()?;
        let covered_ms = now_ms.saturating_sub(route.focus_start_ms()?);
        Some(WindowStats {
            sent: hop.stats.sent(),
            received: hop.stats.received(),
            average_ms: hop.stats.average().map(latency_bars::millis),
            covered: Duration::from_millis(u64::try_from(covered_ms).unwrap_or(0)),
        })
    }

    /// The answering hops of the route (TTL and the address that answered last), for route change detection.
    pub fn answering_path(&self) -> Vec<(u8, IpAddr)> {
        self.route()
            .map(|route| route.hops().iter().filter_map(|hop| hop.address.map(|address| (hop.ttl, address))).collect())
            .unwrap_or_default()
    }

    /// The tab as a snapshot: the route over the focus, the target's chart over the timeline's period and the
    /// alerts of that period from `journal`; `None` unless the trace runs.
    pub fn snapshot(
        &self,
        names: &NameCache,
        reader: Option<&Store>,
        journal: &[JournalEntry],
    ) -> Option<TargetReport> {
        let route = self.route()?;
        let label = self.label()?;
        let hops = route.hops().iter().map(|hop| hop_row(hop, names)).collect();
        let focus = focus_range(&route).unwrap_or_else(|| tr!(self.language, "since the trace started").to_owned());
        let now_ms = history::unix_ms(SystemTime::now());
        let (from_ms, to_ms) = self.timeline.visible_range(now_ms);
        let alerts = journal
            .iter()
            .filter(|entry| entry.host.eq_ignore_ascii_case(&self.host))
            .filter(|entry| entry.started_ms <= to_ms && entry.ended_ms.is_none_or(|ended| ended >= from_ms))
            .map(|entry| alert_row(entry, self.language))
            .collect();
        let chart = self.chart_over(reader, self.timeline.period(), to_ms);
        Some(TargetReport { label, focus, hops, chart, alerts })
    }

    /// The target's chart over `period` ending at `end_ms`, built as the window's timeline builds it (same buckets,
    /// steps, bridged gaps, scale and jitter), with the comments of that period.
    pub fn chart_over(&self, reader: Option<&Store>, period: Period, end_ms: i64) -> Option<Chart> {
        let (target_id, reader) = (self.history_id()?, reader?);
        let interval_ms = i64::try_from(self.interval.as_millis()).unwrap_or(i64::MAX);
        let window = Window::ending_at(end_ms, period, interval_ms);
        let buckets = reader
            .buckets(target_id, HopKey::Target, window.from_ms, window.to_ms, window.bucket_ms)
            .inspect_err(|error| log::warn!("cannot read the history for the snapshot: {error}"))
            .ok()?;
        // The user's comments and the alerts' marks, as the window's timeline draws them.
        let alerts = reader.alerts(target_id, window.from_ms, window.to_ms).unwrap_or_default();
        let mut marks: Vec<(i64, String)> = reader
            .comments(target_id, window.from_ms, window.to_ms)
            .unwrap_or_default()
            .into_iter()
            .map(|comment| (comment.ts_ms, comment.text))
            .chain(alerts.iter().flat_map(|entry| entry.marks(self.language)))
            .collect();
        marks.sort_by_key(|(at_ms, _)| *at_ms);
        Some(Chart {
            from_ms: window.from_ms,
            to_ms: window.to_ms,
            bucket_ms: window.bucket_ms,
            period,
            series: Series::from_buckets(&buckets, window.bucket_ms),
            marks,
        })
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

    /// Final-hop numbers summarized over `focus`, where Auto means `visible` (Unix ms) — for the overview page,
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
            State::Editing(None) => tr!(self.language, "no target").to_owned(),
            State::Resolving(_) => tr!(self.language, "resolving…").to_owned(),
            State::Running(session) if session.tracer.is_paused() => tr!(self.language, "paused").to_owned(),
            State::Running(_) => tr!(self.language, "running").to_owned(),
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
        let toolbar_frame = Frame::new().fill(theme::palette(ui).chassis).inner_margin(Margin::symmetric(10, 8));
        let action = egui::Panel::top("tab-toolbar")
            .frame(toolbar_frame)
            .show(ui, |ui| self.toolbar(ui, context.targets))
            .inner;
        // The clicked hop while it's still on the route, otherwise the target (last) hop.
        let mut ttl = route.as_ref().and_then(|route| {
            self.selected_ttl
                .filter(|ttl| route.hops().iter().any(|hop| hop.ttl == *ttl))
                .or_else(|| route.hops().last().map(|hop| hop.ttl))
        });
        // Up and down arrows choose the hop too, unless a field or list has the keyboard.
        let typing = ui.ctx().memory(|memory| memory.focused().is_some());
        if let (Some(route), Some(current), false) = (&route, ttl, typing) {
            let (up, down) = ui.input(|input| (input.key_pressed(Key::ArrowUp), input.key_pressed(Key::ArrowDown)));
            let hops: Vec<u8> = route.hops().iter().map(|hop| hop.ttl).collect();
            if let Some(position) = hops.iter().position(|hop| *hop == current) {
                let moved = if up {
                    position.checked_sub(1)
                } else if down {
                    position.checked_add(1)
                } else {
                    None
                };
                if let Some(next) = moved.and_then(|index| hops.get(index)) {
                    self.selected_ttl = Some(*next);
                    ttl = Some(*next);
                }
            }
        }
        let area = ui.available_rect_before_wrap();
        ui.allocate_rect(area, egui::Sense::hover());
        let (table_rect, splitter_rect, timeline_rect) = split(area, *table_share);
        splitter(ui, splitter_rect, area, table_share);

        pane(ui, table_rect, Margin::same(1), |ui| {
            let focus = route.as_ref().and_then(focus_range).map(|range| trf!(ui, "Focus {range}", range = range));
            if let Some(clicked) = hop_table::show(ui, route.as_ref(), context.names, ttl, focus.as_deref()) {
                self.selected_ttl = Some(clicked);
            }
        });
        pane(ui, timeline_rect, Margin::same(8), |ui| match (&self.state, &route, ttl) {
            (State::Running(session), Some(route), Some(ttl)) => {
                let title = hop_title(route, ttl, context.names, self.language);
                // The target's line is read by target, not hop number, so it keeps its history when the route
                // length changes.
                let is_target =
                    route.hops().last().is_some_and(|hop| hop.ttl == ttl && hop.address == Some(route.target()));
                let hop = if is_target { HopKey::Target } else { HopKey::Ttl(ttl) };
                self.timeline.show(ui, context.history_path, session.target_id, hop, &title, self.interval);
            }
            _ => {
                ui.weak(tr!(ui, "The history of the selected hop appears here once a trace runs."));
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
            ui.weak(tr!(ui, "Interval"));
            let mut interval = self.interval;
            if interval_choice(ui, &mut interval) {
                self.set_interval(interval);
            }
            ui.weak(tr!(ui, "Focus"));
            focus_choice(ui, &mut self.focus);
            ui.with_layout(Layout::right_to_left(Align::Center), latency_bars::legend);
        });
        action
    }

    /// Target field and the Trace button (a spinner while resolving).
    fn target_entry(&mut self, ui: &mut Ui, targets: Option<&TargetList>) {
        let resolving = matches!(self.state, State::Resolving(_));
        let problem = match &self.state {
            State::Editing(Some(error)) => Some(error.as_str()),
            State::Editing(None) | State::Resolving(_) | State::Running(_) => None,
        };
        let mut start = self.entry.show(ui, targets, !resolving, problem);
        if resolving {
            ui.spinner();
            ui.weak(trf!(ui, "Resolving {host}…", host = self.host));
        } else if icons::button(ui, Icon::Play, Style::Primary, tr!(ui, "Trace (Enter)")).clicked() {
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
            icons::split_button(ui, Icon::Play, theme::palette(ui).good, tr!(ui, "Resume probing"))
        } else {
            icons::split_button(
                ui,
                Icon::Pause,
                theme::palette(ui).signal,
                tr!(ui, "Pause probing (the numbers are kept)"),
            )
        };
        if split.main.clicked() {
            session.tracer.set_paused(!paused);
        }
        // Only actions on this trace; a new target is opened with "+".
        let items = [
            (tr!(ui, "Reset and restart"), ui.ctx().format_shortcut(&RESTART_SHORTCUT), Action::Restart),
            (tr!(ui, "Close and stop"), ui.ctx().format_shortcut(&CLOSE_SHORTCUT), Action::Close),
        ];
        // As wide as the longest row needs in the current language, never narrower than TRACE_MENU_WIDTH.
        let width = items
            .iter()
            .map(|(label, shortcut, _)| icons::menu_row_width(ui, label, shortcut))
            .fold(TRACE_MENU_WIDTH, f32::max);
        Popup::from_toggle_button_response(&split.menu).width(width).show(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for (label, shortcut, item) in &items {
                if icons::menu_row(ui, label, shortcut).clicked() {
                    action = Some(*item);
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
    // The share comes from the database too: kept within the splitter's range, whatever is stored.
    let table_share = if table_share.is_finite() {
        table_share.clamp(*TABLE_SHARE_RANGE.start(), *TABLE_SHARE_RANGE.end())
    } else {
        TABLE_SHARE
    };
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
    let color = if active { theme::palette(ui).signal } else { theme::palette(ui).rule };
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
    theme::pane_frame(ui).inner_margin(margin).show(&mut child, |ui| {
        ui.set_min_size(ui.available_size());
        content(ui);
    });
}

/// "from – to" of a route's focus.
fn focus_range(route: &Route) -> Option<String> {
    let from_ms = route.focus_start_ms()?;
    let to_ms = route.focus_end_ms().unwrap_or_else(|| history::unix_ms(SystemTime::now()));
    Some(format!("{} \u{2013} {}", clock(from_ms), clock(to_ms)))
}

/// Combo box choosing a probe interval; returns whether it changed.
pub fn interval_choice(ui: &mut Ui, interval: &mut Duration) -> bool {
    let before = *interval;
    let language = ui.language();
    egui::ComboBox::from_id_salt("interval")
        .selected_text(interval_label(*interval, language))
        .show_ui(ui, |ui| {
            for (choice, label) in INTERVALS.into_iter().zip(INTERVAL_LABELS) {
                ui.selectable_value(interval, choice, language.translate(label));
            }
        });
    *interval != before
}

/// Combo box setting one interval on all traces: it shows the interval they share, or "different" when they
/// don't (`fallback` with no traces at all); returns the interval chosen.
pub fn interval_for_all(ui: &mut Ui, intervals: &[Duration], fallback: Duration) -> Option<Duration> {
    let language = ui.language();
    let shared = match intervals.split_first() {
        None => Some(fallback),
        Some((first, rest)) => rest.iter().all(|interval| interval == first).then_some(*first),
    };
    let shown =
        shared.map_or_else(|| tr!(language, "different").to_owned(), |interval| interval_label(interval, language));
    let mut chosen = None;
    egui::ComboBox::from_id_salt("interval-of-all").selected_text(shown).show_ui(ui, |ui| {
        for (choice, label) in INTERVALS.into_iter().zip(INTERVAL_LABELS) {
            if ui.selectable_label(shared == Some(choice), language.translate(label)).clicked() {
                chosen = Some(choice);
            }
        }
    });
    chosen
}

/// Combo box choosing the focus.
pub fn focus_choice(ui: &mut Ui, focus: &mut Focus) {
    let language = ui.language();
    egui::ComboBox::from_id_salt("focus").selected_text(focus.label(language)).show_ui(ui, |ui| {
        for choice in Focus::ALL {
            ui.selectable_value(focus, choice, choice.label(language));
        }
    });
}

/// Menu label of a probe interval.
fn interval_label(interval: Duration, language: Language) -> String {
    INTERVALS.iter().zip(INTERVAL_LABELS).find(|(known, _)| **known == interval).map_or_else(
        || trf!(language, "{seconds} seconds", seconds = language.decimal(interval.as_secs_f64())),
        |(_, label)| language.translate(label).to_owned(),
    )
}

/// Local wall-clock time `HH:MM:SS` of Unix milliseconds.
pub fn clock(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|time| time.with_timezone(&chrono::Local).format("%H:%M:%S").to_string())
        .unwrap_or_default()
}

/// Label of a traced target, "address / description": the description is the `name` from the target list if it
/// has one, else the `host` as entered if that is a domain name, else (an IP was entered) the `reverse` DNS name of
/// the address, if any — `9.9.9.9 / Quad9 DNS`, `140.82.121.4 / github.com`, `1.1.1.1 / one.one.one.one`.
fn target_label(target: IpAddr, host: &str, name: Option<&str>, reverse: Option<&str>) -> String {
    let entered_ip = host.trim().parse::<IpAddr>().is_ok();
    let description = name.or(if entered_ip { reverse } else { Some(host.trim()) });
    match description {
        Some(description) => format!("{target} / {description}"),
        None => target.to_string(),
    }
}

/// A failed lookup of `host` in words: the usual Windows resolver errors get their own message, anything else
/// keeps the system's text.
fn lookup_message(host: &str, error: &std::io::Error, language: Language) -> String {
    /// WSAHOST_NOT_FOUND: no such name.
    const HOST_NOT_FOUND: i32 = 11_001;
    /// WSATRY_AGAIN: the DNS server didn't answer.
    const TRY_AGAIN: i32 = 11_002;
    /// WSANO_DATA: the name exists without an address.
    const NO_DATA: i32 = 11_004;
    match error.raw_os_error() {
        Some(HOST_NOT_FOUND) => trf!(language, "{host} is not known to DNS: check the spelling", host = host),
        Some(TRY_AGAIN) => trf!(language, "DNS didn't answer for {host}: is the network up?", host = host),
        Some(NO_DATA) => trf!(language, "{host} has no address", host = host),
        _ => trf!(language, "Cannot resolve {host}: {error}", host = host, error = error),
    }
}

/// Name under which a host's history is stored.
fn history_key(host: &str) -> String {
    host.to_lowercase()
}

/// A hop of the route as a snapshot table row.
fn hop_row(hop: &Hop, names: &NameCache) -> HopRow {
    let stats = &hop.stats;
    HopRow {
        ttl: hop.ttl,
        address: hop.address.map(|ip| ip.to_string()),
        name: hop.address.and_then(|ip| names.get(ip).filter(|name| *name != ip.to_string())),
        sent: stats.sent(),
        avg: stats.average().map(latency_bars::millis),
        min: stats.best().map(latency_bars::millis),
        max: stats.worst().map(latency_bars::millis),
        cur: stats.last().map(latency_bars::millis),
        jitter: stats.jitter().map(latency_bars::millis),
        loss: stats.loss_percent(),
    }
}

/// A journal entry as a snapshot alert row.
fn alert_row(entry: &JournalEntry, language: Language) -> AlertRow {
    let lasted = match entry.ended_ms {
        None => tr!(language, "on now").to_owned(),
        Some(_) => entry.lasted(language).unwrap_or_default(),
    };
    let worst = entry.worst(language).unwrap_or_default();
    AlertRow { started: date_time(entry.started_ms), what: entry.what(language), lasted, worst }
}

/// Local "15.11 13:05:12" of Unix ms.
fn date_time(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|time| time.with_timezone(&chrono::Local).format("%d.%m %H:%M:%S").to_string())
        .unwrap_or_default()
}

/// Timeline title of hop `ttl`, in the style of the target labels: "Hop 10: 208.67.222.222 / dns.sse.cisco.com"
/// (the reverse name only if there is one).
fn hop_title(route: &Route, ttl: u8, names: &NameCache, language: Language) -> String {
    let address = route.hops().iter().find(|hop| hop.ttl == ttl).and_then(|hop| hop.address);
    let what = match address {
        Some(ip) => match names.get(ip).filter(|name| *name != ip.to_string()) {
            Some(name) => format!("{ip} / {name}"),
            None => ip.to_string(),
        },
        None => tr!(language, "no response").to_owned(),
    };
    trf!(language, "Hop {ttl}: {what}", ttl = ttl, what = what)
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
