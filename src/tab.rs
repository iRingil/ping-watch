//! One target tab: the host is resolved in the background, then traced; the tab draws its toolbar (pause with its
//! menu, interval, focus) and the three panes.

use std::net::IpAddr;
use std::path::Path;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, SystemTime};

use eframe::egui::{
    self, Align, Button, CornerRadius, Frame, Key, KeyboardShortcut, Layout, Margin, Modifiers, Popup,
    PopupCloseBehavior, Rect, RichText, ScrollArea, Sense, Stroke, TextEdit, Ui, pos2,
};

use crate::focus::Focus;
use crate::history::{self, Recorder, Store};
use crate::hop_table;
use crate::icons::{self, Icon, Style};
use crate::latency_bars;
use crate::names::{NameCache, resolve};
use crate::route::{Hop, Route};
use crate::targets::{Family, SavedTarget, TargetList};
use crate::theme;
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
/// Width of the target field.
const ENTRY_WIDTH: f32 = 360.0;
/// Width of the arrow area at the right end of the target field.
const ARROW_WIDTH: f32 = 24.0;
/// Width of the target list: wider than the field, so a full IPv6 address and the description fit side by side.
const TARGET_LIST_WIDTH: f32 = 600.0;
/// Height of a target list row (also the size of its icon buttons).
const TARGET_ROW_HEIGHT: f32 = 28.0;
/// Tallest the target list grows before it scrolls.
const TARGET_LIST_HEIGHT: f32 = 380.0;
/// Share of the pane area the hop table gets at first; the timeline gets the rest.
const TABLE_SHARE: f32 = 0.66;
/// Limits of the table's share when the splitter is dragged, so neither pane disappears.
const TABLE_SHARE_RANGE: std::ops::RangeInclusive<f32> = 0.2..=0.85;
/// Height of the draggable bar between the panes.
const SPLITTER_HEIGHT: f32 = 10.0;
/// Gap between the panes and the window edge.
const PANE_GAP: f32 = 10.0;
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
    /// Share of the pane area taken by the hop table, changed by dragging the splitter.
    table_share: f32,
    /// Saved target being edited.
    editing: Option<Draft>,
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
            table_share: TABLE_SHARE,
            editing: None,
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
        if let Some(targets) = context.targets
            && let Err(error) = targets.remember(&self.host, target)
        {
            log::warn!("cannot add {} to the target list: {error}", self.host);
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
        let now_ms = history::unix_ms(SystemTime::now());
        Some(session.tracer.snapshot(self.focus.range(self.timeline.visible_range(now_ms), now_ms)))
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

    /// Draws the tab: toolbar, the hop table pane and the timeline pane. The panes keep their frames and the table
    /// its header even before a trace runs, so an empty tab shows where everything will appear. Returns the action
    /// asked for in the menu.
    pub fn ui(&mut self, ui: &mut Ui, context: Context<'_>) -> Option<Action> {
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
        let area = ui.available_rect_before_wrap().shrink2(egui::vec2(PANE_GAP, 0.0));
        ui.allocate_rect(area, egui::Sense::hover());
        let (table_rect, splitter_rect, timeline_rect) = self.split(area);
        self.splitter(ui, splitter_rect, area);

        pane(ui, table_rect, Margin::same(1), |ui| {
            let focus = route.as_ref().and_then(focus_label);
            if let Some(clicked) = hop_table::show(ui, route.as_ref(), context.names, ttl, focus.as_deref()) {
                self.selected_ttl = Some(clicked);
            }
        });
        pane(ui, timeline_rect, Margin::same(8), |ui| match (&self.state, &route, ttl) {
            (State::Running(session), Some(route), Some(ttl)) => {
                let title = hop_title(route, ttl, context.names);
                self.timeline.show(ui, context.history_path, session.target_id, ttl, &title, self.interval);
            }
            _ => {
                ui.weak("The history of the selected hop appears here once a trace runs.");
            }
        });
        action
    }

    /// Splits the pane area into the hop table (top, `table_share` of the height), the splitter and the timeline
    /// (the rest). Computed from the window size every frame, so the panes are in place from the first frame.
    fn split(&self, area: Rect) -> (Rect, Rect, Rect) {
        let usable = area.height() - SPLITTER_HEIGHT - PANE_GAP;
        let splitter_top = area.top() + usable * self.table_share;
        let timeline_top = splitter_top + SPLITTER_HEIGHT;
        let table = Rect::from_min_max(area.min, pos2(area.right(), splitter_top));
        let splitter = Rect::from_min_max(pos2(area.left(), splitter_top), pos2(area.right(), timeline_top));
        let timeline =
            Rect::from_min_max(pos2(area.left(), timeline_top), pos2(area.right(), area.bottom() - PANE_GAP));
        (table, splitter, timeline)
    }

    /// The bar between the panes: dragging it moves the border; it lights up under the pointer.
    fn splitter(&mut self, ui: &mut Ui, rect: Rect, area: Rect) {
        let response = ui
            .interact(rect, ui.scope_id().with("pane-splitter"), egui::Sense::drag())
            .on_hover_cursor(egui::CursorIcon::ResizeVertical);
        let usable = (area.height() - SPLITTER_HEIGHT - PANE_GAP).max(1.0);
        if response.dragged() {
            let share = self.table_share + response.drag_delta().y / usable;
            self.table_share = share.clamp(*TABLE_SHARE_RANGE.start(), *TABLE_SHARE_RANGE.end());
        }
        let active = response.hovered() || response.dragged();
        let color = if active { theme::SIGNAL } else { theme::RULE };
        let center = rect.center();
        // A full-width hairline with a thicker grip in the middle shows where to grab.
        ui.painter().hline(rect.x_range(), center.y, Stroke::new(1.0, color));
        ui.painter().hline((center.x - 24.0)..=(center.x + 24.0), center.y, Stroke::new(4.0, color));
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
            let before = self.interval;
            egui::ComboBox::from_id_salt("interval")
                .selected_text(interval_label(self.interval))
                .show_ui(ui, |ui| {
                    for (interval, label) in INTERVALS {
                        ui.selectable_value(&mut self.interval, interval, label);
                    }
                });
            if self.interval != before
                && let State::Running(session) = &self.state
            {
                session.tracer.set_interval(self.interval);
            }
            ui.weak("Focus");
            egui::ComboBox::from_id_salt("focus").selected_text(self.focus.label()).show_ui(ui, |ui| {
                for focus in Focus::ALL {
                    ui.selectable_value(&mut self.focus, focus, focus.label());
                }
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                latency_bars::legend(ui);
                let problem = match &self.state {
                    State::Editing(Some(error)) => Some(error.as_str()),
                    _ => self.warning.as_deref(),
                };
                if let Some(problem) = problem {
                    ui.colored_label(theme::BAD, problem);
                }
            });
        });
        action
    }

    /// Target field with the list of well-known targets and the Trace button (a spinner while resolving).
    fn target_entry(&mut self, ui: &mut Ui, targets: Option<&TargetList>) {
        let resolving = matches!(self.state, State::Resolving(_));
        // Room on the right of the field for the arrow that opens the target list.
        let field = TextEdit::singleline(&mut self.host_input)
            .hint_text("Enter target name or IP")
            .desired_width(ENTRY_WIDTH)
            .margin(Margin { left: 6, right: ARROW_WIDTH as i8, top: 3, bottom: 3 });
        let input = ui.add_enabled(!resolving, field);
        if std::mem::take(&mut self.focus_input) {
            input.request_focus();
        }
        let mut start = input.lost_focus() && ui.input(|input| input.key_pressed(Key::Enter));

        // The arrow is a button set into the field's right end: own face, a divider, a hover state.
        let arrow_rect =
            Rect::from_min_max(pos2(input.rect.right() - ARROW_WIDTH, input.rect.top()), input.rect.max).shrink(2.0);
        let arrow =
            ui.interact(arrow_rect, input.id.with("target-list"), Sense::click()).on_hover_text("Saved targets");
        let widgets = &ui.visuals().widgets;
        let arrow_fill = if arrow.hovered() { widgets.hovered.weak_bg_fill } else { widgets.inactive.weak_bg_fill };
        ui.painter().rect_filled(arrow_rect, CornerRadius::same(2), arrow_fill);
        icons::paint(
            ui,
            arrow_rect.center(),
            Icon::ArrowDown,
            if arrow.hovered() { theme::INK } else { theme::INK_MUTED },
        );
        if !resolving && let Some(targets) = targets {
            Popup::from_toggle_button_response(&arrow)
                .anchor(input.rect)
                .width(input.rect.width().max(TARGET_LIST_WIDTH))
                .close_behavior(PopupCloseBehavior::CloseOnClickOutside)
                .show(|ui| {
                    // Picking a target traces it right away.
                    if let Some(address) = self.target_list(ui, targets) {
                        self.host_input = address;
                        start = true;
                        ui.close();
                    }
                });
        }
        self.edit_dialog(ui.ctx(), targets);

        if resolving {
            ui.spinner();
            ui.weak(format!("Resolving {}…", self.host));
        } else if icons::button(ui, Icon::Play, Style::Primary, "Trace (Enter)").clicked() {
            start = true;
        }
        if start && !resolving {
            let host = self.host_input.clone();
            self.begin(host);
        }
    }

    /// Rows of the saved target list: address on the left, name and IP version on the right, Edit and Delete.
    /// Returns the address picked.
    fn target_list(&mut self, ui: &mut Ui, targets: &TargetList) -> Option<String> {
        let all = match targets.all() {
            Ok(all) => all,
            Err(error) => {
                ui.colored_label(theme::BAD, format!("Cannot read the target list: {error}"));
                return None;
            }
        };
        if all.is_empty() {
            ui.weak("No saved targets. Every target you trace is added here.");
            return None;
        }
        let mut picked = None;
        ScrollArea::vertical().max_height(TARGET_LIST_HEIGHT).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for target in all {
                match target_row(ui, &target) {
                    RowAction::Pick => picked = Some(target.address.clone()),
                    RowAction::Edit => self.editing = Some(Draft::of(&target)),
                    RowAction::Delete => {
                        if let Err(error) = targets.delete(target.id) {
                            log::error!("cannot delete {}: {error}", target.address);
                        }
                    }
                    RowAction::None => {}
                }
            }
        });
        picked
    }

    /// Dialog editing a saved target's address and name.
    fn edit_dialog(&mut self, ctx: &egui::Context, targets: Option<&TargetList>) {
        let (Some(draft), Some(targets)) = (&mut self.editing, targets) else {
            return;
        };
        let mut close = false;
        egui::Modal::new(egui::Id::unique("edit-target")).show(ctx, |ui| {
            ui.set_width(380.0);
            ui.label(RichText::new("Edit target").font(theme::heading_font(17.0)));
            ui.add_space(6.0);
            egui::Grid::new("edit-target-fields").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
                ui.weak("Address");
                ui.add(TextEdit::singleline(&mut draft.address).desired_width(f32::INFINITY));
                ui.end_row();
                ui.weak("Name");
                ui.add(TextEdit::singleline(&mut draft.name).hint_text("optional").desired_width(f32::INFINITY));
                ui.end_row();
            });
            if let Some(error) = &draft.error {
                ui.colored_label(theme::BAD, error);
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let can_save = !draft.address.trim().is_empty();
                if ui.add_enabled(can_save, primary_button("Save")).clicked() {
                    match targets.update(draft.id, &draft.address, &draft.name) {
                        Ok(()) => close = true,
                        Err(error) => draft.error = Some(format!("Not saved: {error}")),
                    }
                }
                if ui.button("Cancel").clicked() {
                    close = true;
                }
            });
        });
        if close {
            self.editing = None;
        }
    }

    /// Traced target, pause/resume and the menu of a running tab. Returns the menu action asked.
    fn trace_controls(&mut self, ui: &mut Ui) -> Option<Action> {
        let State::Running(session) = &self.state else {
            return None;
        };
        let mut action = None;
        ui.label(RichText::new(format!("{} / {}", session.target, self.host)).font(theme::heading_font(17.0)));
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
        Popup::from_toggle_button_response(&split.menu).show(|ui| {
            let items = [
                ("Reset and restart", RESTART_SHORTCUT, Action::Restart),
                ("New target", NEW_TARGET_SHORTCUT, Action::NewTarget),
                ("Close and stop", CLOSE_SHORTCUT, Action::Close),
            ];
            for (label, shortcut, item) in items {
                let shortcut = ui.ctx().format_shortcut(&shortcut);
                if ui.add(Button::new(label).shortcut_text(shortcut)).clicked() {
                    action = Some(item);
                    ui.close();
                }
            }
        });
        action
    }
}

/// Draws a framed pane exactly into `rect`: the content gets its own child ui clipped to the rect, so whatever it
/// draws can never move or resize the pane.
fn pane(ui: &mut Ui, rect: Rect, margin: Margin, content: impl FnOnce(&mut Ui)) {
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(Layout::top_down(Align::Min)));
    child.set_clip_rect(rect.intersect(ui.clip_rect()));
    theme::pane_frame().inner_margin(margin).show(&mut child, |ui| {
        ui.set_min_size(ui.available_size());
        content(ui);
    });
}

/// A saved target being edited.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Draft {
    /// Row id in the list.
    id: i64,
    /// Address as edited.
    address: String,
    /// Name as edited.
    name: String,
    /// Why the last save failed.
    error: Option<String>,
}

impl Draft {
    /// Draft of `target` as it is now.
    fn of(target: &SavedTarget) -> Self {
        Self { id: target.id, address: target.address.clone(), name: target.name.clone(), error: None }
    }
}

/// What a click on a target list row asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowAction {
    /// Nothing clicked.
    None,
    /// Put the address into the field.
    Pick,
    /// Open the edit dialog.
    Edit,
    /// Remove the entry.
    Delete,
}

/// One row of the target list: the whole row highlights under the pointer and picks the address when clicked;
/// the pencil and cross icons on the right sit on top of it, so clicking them edits or deletes instead.
fn target_row(ui: &mut Ui, target: &SavedTarget) -> RowAction {
    let size = egui::vec2(ui.available_width(), TARGET_ROW_HEIGHT);
    let (rect, row) = ui.allocate_exact_size(size, Sense::click());
    let icon = egui::Vec2::splat(TARGET_ROW_HEIGHT);
    let delete_rect = Rect::from_min_size(pos2(rect.right() - icon.x, rect.top()), icon);
    let edit_rect = delete_rect.translate(egui::vec2(-icon.x, 0.0));
    let edit = ui.interact(edit_rect, row.id.with("edit"), Sense::click()).on_hover_text("Edit");
    let delete = ui.interact(delete_rect, row.id.with("delete"), Sense::click()).on_hover_text("Delete");

    let painter = ui.painter();
    if row.hovered() || edit.hovered() || delete.hovered() {
        painter.rect_filled(rect, CornerRadius::same(3), theme::PANE_HEADER);
    }
    let font = egui::FontId::proportional(15.0);
    let text_y = rect.center().y;
    painter.text(pos2(rect.left() + 8.0, text_y), egui::Align2::LEFT_CENTER, &target.address, font.clone(), theme::INK);
    let description_right = edit_rect.left() - 8.0;
    painter.text(
        pos2(description_right, text_y),
        egui::Align2::RIGHT_CENTER,
        target_description(target),
        font,
        theme::INK_MUTED,
    );
    icons::paint(ui, edit_rect.center(), Icon::Pencil, if edit.hovered() { theme::SIGNAL } else { theme::INK_MUTED });
    icons::paint(ui, delete_rect.center(), Icon::Cross, if delete.hovered() { theme::BAD } else { theme::INK_MUTED });

    if delete.clicked() {
        RowAction::Delete
    } else if edit.clicked() {
        RowAction::Edit
    } else if row.clicked() {
        RowAction::Pick
    } else {
        RowAction::None
    }
}

/// Right-hand text of a target list row: its name and IP version, whichever are known ("Office router, ipv4").
fn target_description(target: &SavedTarget) -> String {
    let family = target.family.map(Family::label);
    [Some(target.name.as_str()).filter(|name| !name.is_empty()), family]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(", ")
}

/// The main button of a group: signal-colored, so it reads as "the thing to press".
fn primary_button(text: &str) -> Button<'_> {
    Button::new(RichText::new(text).color(theme::CHASSIS)).fill(theme::SIGNAL)
}

/// "Focus: from – to" of a route, shown under its latency bars.
fn focus_label(route: &Route) -> Option<String> {
    let from_ms = route.focus_start_ms()?;
    let to_ms = route.focus_end_ms().unwrap_or_else(|| history::unix_ms(SystemTime::now()));
    Some(format!("Focus {} – {}", clock(from_ms), clock(to_ms)))
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
