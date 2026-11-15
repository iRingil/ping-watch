//! Main window: a tab bar ("All targets", one tab per target, "+"), the All targets page or the active target tab,
//! and the status bar. Keyboard shortcuts act on the active tab.

use std::net::IpAddr;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use eframe::egui::{self, Button, CornerRadius, Frame, Margin, Rect, RichText, Sense, Stroke, StrokeKind, Ui, Vec2};

use crate::all_targets::{AllTargets, Request};
use crate::history::{self, Store};
use crate::hop_table::format_ms;
use crate::names::NameCache;
use crate::tab::{self, Action, Tab};
use crate::tab_strip;
use crate::targets::TargetList;
use crate::theme;
use crate::tray::{Tray, TrayCommand};
use crate::window_state::{SavedTab, WindowState, WindowStore};

/// Least time between two stores of the window state while it changes (moving, resizing).
const WINDOW_SAVE_INTERVAL: Duration = Duration::from_secs(1);
/// How often the window redraws to show fresh numbers.
const REFRESH: Duration = Duration::from_millis(250);
/// Height of a tab in the tab bar.
const TAB_HEIGHT: f32 = 30.0;
/// Space between a tab's edge and its title.
const TAB_PADDING: f32 = 12.0;
/// Width of a tab's close mark.
const TAB_CLOSE_WIDTH: f32 = 16.0;
/// Space between tabs.
const TAB_GAP: f32 = 4.0;
/// Width of a tab bar scroll arrow.
const ARROW_WIDTH: f32 = 22.0;
/// Least room for the next tab to peek into the end of a scrolled tab strip.
const PEEK_MIN_WIDTH: f32 = 16.0;
/// Width of the fade at the end of a scrolled tab strip.
const FADE_WIDTH: f32 = 48.0;
/// Width of the "+" (new target) button.
const PLUS_WIDTH: f32 = 24.0;

/// Which page the window shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    /// Overview of all targets.
    AllTargets,
    /// The target tab at this index.
    Target(usize),
}

/// Application state.
#[derive(Debug)]
pub struct PingWatchApp {
    /// Target tabs in display order.
    tabs: Vec<Tab>,
    /// Page shown.
    page: Page,
    /// First target tab shown in the tab bar when they don't all fit.
    tab_first: usize,
    /// Page shown when the tab bar was last drawn; a change scrolls the newly shown tab into view.
    tab_bar_page: Page,
    /// Reverse-DNS cache for hop names, shared by all tabs.
    names: NameCache,
    /// History database file, `None` if there is no place for it.
    history_path: Option<PathBuf>,
    /// Read connection to the history for the timelines, `None` if history is off.
    reader: Option<Store>,
    /// The user's target list, `None` if history is off.
    targets: Option<TargetList>,
    /// Icon in the notification area, `None` if it couldn't be created.
    tray: Option<Tray>,
    /// Where the window state is kept between runs, `None` if history is off.
    window_store: Option<WindowStore>,
    /// The window state as of the last frame.
    window: WindowState,
    /// The window state as last stored.
    window_saved: Option<WindowState>,
    /// When the window state was last stored.
    window_saved_at: Instant,
    /// Minimize the window on the first frame (it was left minimized).
    start_minimized: bool,
    /// The open tabs as last stored.
    tabs_saved: Vec<SavedTab>,
    /// The All targets page.
    all_targets: AllTargets,
}

impl PingWatchApp {
    /// Creates the app with one tab per host in `hosts` (all tracing right away), or one empty tab, the `tray`
    /// icon, and the window state as stored in `window_store` (`saved_window`, already applied to the window's
    /// geometry by the caller).
    pub fn new(
        hosts: Vec<String>,
        tray: Option<Tray>,
        window_store: Option<WindowStore>,
        saved_window: Option<WindowState>,
    ) -> Self {
        let history_path = history::default_path().inspect_err(|error| log::warn!("{error}")).ok();
        let reader = history_path
            .as_deref()
            .and_then(|path| Store::open(path).inspect_err(|error| log::warn!("{error}")).ok());
        // Hosts given on the command line win; otherwise the tabs of the last run come back.
        let saved_tabs = if hosts.is_empty() {
            window_store
                .as_ref()
                .and_then(|store| store.load_tabs().inspect_err(|error| log::warn!("{error}")).ok())
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let mut tabs: Vec<Tab> = hosts.into_iter().map(Tab::tracing).collect();
        tabs.extend(saved_tabs.iter().map(Tab::restored));
        let page = match saved_tabs.iter().position(|saved| saved.shown) {
            Some(index) => Page::Target(index),
            None if !saved_tabs.is_empty() => Page::AllTargets,
            None => Page::Target(0),
        };
        if tabs.is_empty() {
            tabs.push(Tab::new());
        }
        let targets = history_path
            .as_deref()
            .and_then(|path| TargetList::open(path).inspect_err(|error| log::warn!("{error}")).ok());
        // The current state starts as "not minimized": the saved flag only says what to do on start, and the
        // window reports the truth from then on.
        let window = saved_window.map(|state| WindowState { minimized: false, ..state }).unwrap_or(WindowState {
            x: 0.0,
            y: 0.0,
            width: 0.0,
            height: 0.0,
            maximized: false,
            minimized: false,
            table_share: tab::TABLE_SHARE,
        });
        let all_targets = AllTargets::new(
            window_store
                .as_ref()
                .and_then(|store| store.load_all_targets().inspect_err(|error| log::warn!("{error}")).ok())
                .flatten(),
        );
        Self {
            all_targets,
            tabs,
            page,
            tabs_saved: saved_tabs,
            tab_first: 0,
            tab_bar_page: Page::AllTargets,
            names: NameCache::default(),
            history_path,
            reader,
            targets,
            tray,
            window_store,
            window,
            window_saved: saved_window,
            window_saved_at: Instant::now(),
            start_minimized: saved_window.is_some_and(|state| state.minimized),
        }
    }

    /// Carries out `action` on the active tab.
    fn apply(&mut self, action: Action) {
        match action {
            Action::NewTarget => {
                self.tabs.push(Tab::new());
                self.page = Page::Target(self.tabs.len().saturating_sub(1));
            }
            Action::Restart => {
                if let Some(tab) = self.active_tab() {
                    tab.restart();
                }
            }
            Action::Close => {
                if let Page::Target(index) = self.page {
                    self.close(index);
                }
            }
        }
    }

    /// The target tab shown, if any.
    fn active_tab(&mut self) -> Option<&mut Tab> {
        match self.page {
            Page::Target(index) => self.tabs.get_mut(index),
            Page::AllTargets => None,
        }
    }

    /// Closes (and stops) tab `index`; if it was shown, the overview is shown instead.
    fn close(&mut self, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        self.tabs.remove(index);
        self.page = match self.page {
            // Closing the shown tab leads to the overview, not to some neighbouring trace.
            Page::Target(shown) if shown == index => Page::AllTargets,
            Page::Target(shown) if shown > index => Page::Target(shown.saturating_sub(1)),
            page => page,
        };
    }

    /// Keeps one tab per traced address: when a tab starts tracing an address another tab already traces, it is
    /// closed and the earlier tab is shown instead.
    fn merge_duplicate_traces(&mut self) {
        let targets: Vec<Option<IpAddr>> = self.tabs.iter().map(Tab::target).collect();
        let duplicate = targets.iter().enumerate().find_map(|(later, target)| {
            let target = (*target)?;
            let earlier = targets.iter().take(later).position(|other| *other == Some(target))?;
            Some((earlier, later))
        });
        if let Some((earlier, later)) = duplicate {
            log::info!("{} is already traced in another tab", self.tabs.get(later).map(Tab::title).unwrap_or_default());
            self.tabs.remove(later);
            // A target added on the All targets page leaves that page shown.
            self.page = match self.page {
                Page::Target(shown) if shown == later => Page::Target(earlier),
                Page::Target(shown) if shown > later => Page::Target(shown.saturating_sub(1)),
                page => page,
            };
        }
    }

    /// Tab bar: All targets (always in place), the target tabs with close buttons, and "+". When the target tabs
    /// don't fit, they scroll: an arrow after All targets shows earlier tabs, an arrow pinned to the right (before
    /// "+") shows later ones; each appears only while there is something to scroll to, and the mouse wheel over the
    /// bar scrolls too. A newly shown tab is scrolled into view.
    fn tab_bar(&mut self, ui: &mut Ui) {
        let mut close = None;
        let mut new_target = false;
        // The baseline the tabs stand on; the open tab covers it, so it reads as joined to the toolbar below.
        let bar = ui.max_rect();
        ui.painter().hline(bar.x_range(), bar.bottom() - 0.5, Stroke::new(1.0, theme::RULE));
        let reveal = match self.page {
            Page::Target(index) if self.page != self.tab_bar_page => Some(index),
            Page::Target(_) | Page::AllTargets => None,
        };
        self.tab_bar_page = self.page;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = TAB_GAP;
            if tab_widget(ui, self.page == Page::AllTargets, "All targets", false).0.clicked() {
                self.page = Page::AllTargets;
            }
            // A tracing tab is labelled like its headings ("address / description"), others by the host entered.
            let titles: Vec<String> = self.tabs.iter().map(|tab| tab.label().unwrap_or_else(|| tab.title())).collect();
            let widths: Vec<f32> = titles.iter().map(|title| tab_width(ui, title, true)).collect();
            let all_width: f32 = widths.iter().map(|width| width + TAB_GAP).sum();
            let available = ui.available_width();
            let (first, count, strip) = if all_width + PLUS_WIDTH <= available {
                (0, titles.len(), None)
            } else {
                let strip = available - 2.0 * (ARROW_WIDTH + TAB_GAP) - PLUS_WIDTH - TAB_GAP;
                let mut first = self.tab_first;
                let wheel = ui.input(|input| {
                    input
                        .raw
                        .events
                        .iter()
                        .filter_map(|event| match event {
                            egui::Event::MouseWheel { delta, .. } => Some(*delta),
                            _ => None,
                        })
                        .fold(Vec2::ZERO, |sum, delta| Vec2::new(sum.x + delta.x, sum.y + delta.y))
                });
                if ui.rect_contains_pointer(bar) {
                    let delta = if wheel.y == 0.0 { wheel.x } else { wheel.y };
                    if delta > 0.0 {
                        first = first.saturating_sub(1);
                    } else if delta < 0.0 {
                        first = first.saturating_add(1);
                    }
                }
                let first = tab_strip::first_visible(&widths, TAB_GAP, strip, first, reveal);
                (first, tab_strip::fitting(&widths, TAB_GAP, strip, first), Some(strip))
            };
            self.tab_first = first;
            let last = first.saturating_add(count);
            if strip.is_some() && scroll_arrow(ui, true, first > 0) {
                self.tab_first = first.saturating_sub(1);
            }
            let mut used = 0.0;
            for (index, title) in titles.iter().enumerate().skip(first).take(count) {
                let (response, close_clicked) = tab_widget(ui, self.page == Page::Target(index), title, true);
                used += response.rect.width() + TAB_GAP;
                if close_clicked {
                    close = Some(index);
                } else if response.clicked() {
                    self.page = Page::Target(index);
                }
            }
            if let Some(strip) = strip {
                // The right arrow and "+" stay pinned to the right edge however wide the shown tabs are.
                let room = (strip + TAB_GAP - used).max(0.0);
                // The next tab peeks into the room left, fading out before the arrow.
                if let Some(title) = titles.get(last)
                    && room > PEEK_MIN_WIDTH
                {
                    let peek = Rect::from_min_size(ui.cursor().min, Vec2::new(room - TAB_GAP, TAB_HEIGHT));
                    let mut child = ui.new_child(
                        egui::UiBuilder::new().max_rect(peek).layout(egui::Layout::left_to_right(egui::Align::Min)),
                    );
                    child.set_clip_rect(peek.intersect(ui.clip_rect()));
                    if tab_widget(&mut child, self.page == Page::Target(last), title, false).0.clicked() {
                        self.page = Page::Target(last);
                    }
                    let fade_width = peek.width().min(FADE_WIDTH);
                    let fade = Rect::from_min_max(
                        egui::pos2(peek.right() - fade_width, peek.top()),
                        egui::pos2(peek.right(), peek.bottom() - 1.0),
                    );
                    fade_out(ui, fade, theme::WELL);
                }
                ui.add_space(room);
                if scroll_arrow(ui, false, last < titles.len()) {
                    self.tab_first = first.saturating_add(1);
                }
            }
            let plus = Button::new(RichText::new("+").font(theme::heading_font(16.0))).frame(false);
            if ui.add_sized([PLUS_WIDTH, TAB_HEIGHT], plus).on_hover_text("New target (Ctrl+N)").clicked() {
                new_target = true;
            }
        });
        if new_target {
            self.apply(Action::NewTarget);
        }
        if let Some(index) = close {
            self.close(index);
        }
    }
}

/// Covers `rect` with `color` fading in from transparent on the left to opaque on the right.
fn fade_out(ui: &Ui, rect: Rect, color: egui::Color32) {
    let clear = egui::Color32::TRANSPARENT;
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(rect.left_top(), clear);
    mesh.colored_vertex(rect.right_top(), color);
    mesh.colored_vertex(rect.right_bottom(), color);
    mesh.colored_vertex(rect.left_bottom(), clear);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    ui.painter().add(mesh);
}

/// Width of a tab titled `title`, with a close mark if `closable`.
fn tab_width(ui: &Ui, title: &str, closable: bool) -> f32 {
    let galley = ui.painter().layout_no_wrap(title.to_owned(), theme::heading_font(14.5), theme::INK);
    galley.size().x + TAB_PADDING * 2.0 + if closable { TAB_CLOSE_WIDTH } else { 0.0 }
}

/// A scroll arrow of the tab bar pointing `left` or right. Its place is always kept; it is drawn and clickable only
/// while `active` (there are tabs to scroll to). Returns whether it was clicked.
fn scroll_arrow(ui: &mut Ui, left: bool, active: bool) -> bool {
    let sense = if active { Sense::click() } else { Sense::hover() };
    let (rect, response) = ui.allocate_exact_size(Vec2::new(ARROW_WIDTH, TAB_HEIGHT), sense);
    if !active {
        return false;
    }
    let painter = ui.painter();
    if response.hovered() {
        painter.rect_filled(rect.shrink2(Vec2::new(0.0, 3.0)), CornerRadius::same(3), theme::PANE_HEADER);
    }
    let color = if response.hovered() { theme::INK } else { theme::INK_MUTED };
    let center = rect.center();
    let (tip, back) = if left { (-4.0, 3.0) } else { (4.0, -3.0) };
    let points = vec![
        egui::pos2(center.x + tip, center.y),
        egui::pos2(center.x + back, center.y - 6.0),
        egui::pos2(center.x + back, center.y + 6.0),
    ];
    painter.add(egui::Shape::convex_polygon(points, color, Stroke::NONE));
    let tooltip = if left { "Earlier tabs" } else { "Later tabs" };
    response.on_hover_text(tooltip).clicked()
}

/// A tab of the tab bar: a box with rounded top corners. Every tab keeps its outline; the open one is filled with
/// the toolbar color (so it joins the toolbar below) and has a signal-blue line along its bottom. Returns the tab's response
/// and whether its close mark was clicked.
fn tab_widget(ui: &mut Ui, open: bool, title: &str, closable: bool) -> (egui::Response, bool) {
    let font = theme::heading_font(14.5);
    let text_color = if open { theme::INK } else { theme::INK_MUTED };
    let galley = ui.painter().layout_no_wrap(title.to_owned(), font, text_color);
    let close_width = if closable { TAB_CLOSE_WIDTH } else { 0.0 };
    let size = Vec2::new(galley.size().x + TAB_PADDING * 2.0 + close_width, TAB_HEIGHT);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());

    let fill = if open {
        theme::CHASSIS
    } else if response.hovered() {
        theme::PANE_HEADER
    } else {
        theme::TAB_IDLE
    };
    let corners = CornerRadius { nw: 5, ne: 5, sw: 0, se: 0 };
    let painter = ui.painter();
    painter.rect(rect, corners, fill, Stroke::new(1.0, theme::RULE), StrokeKind::Inside);
    if open {
        // Open the bottom edge into the toolbar and mark it with a signal line just above (a line along the top
        // would cut across the rounded corners).
        let inner = (rect.left() + 1.0)..=(rect.right() - 1.0);
        painter.hline(inner.clone(), rect.bottom() - 0.5, Stroke::new(2.0, fill));
        painter.hline(inner, rect.bottom() - 3.0, Stroke::new(2.0, theme::SIGNAL));
    }
    let text_pos = egui::pos2(rect.left() + TAB_PADDING, rect.center().y - galley.size().y / 2.0);
    painter.galley(text_pos, galley, text_color);

    let mut close_clicked = false;
    if closable {
        let close_rect = Rect::from_center_size(
            egui::pos2(rect.right() - TAB_PADDING / 2.0 - TAB_CLOSE_WIDTH / 2.0, rect.center().y),
            Vec2::splat(TAB_CLOSE_WIDTH),
        );
        let close = ui.interact(close_rect, response.id.with("close"), Sense::click()).on_hover_text("Close and stop");
        let color = if close.hovered() { theme::BAD } else { theme::INK_MUTED };
        ui.painter()
            .text(close_rect.center(), egui::Align2::CENTER_CENTER, "\u{d7}", theme::heading_font(16.0), color);
        close_clicked = close.clicked();
    }
    (response, close_clicked)
}

impl PingWatchApp {
    /// Follows the window's geometry and state, and stores it when it changed: at most once a second, and at once
    /// when the window is being closed. Position and size are taken only from the normal (not maximized or
    /// minimized) window, so restoring a maximized window brings back its normal size too.
    fn track_window(&mut self, ctx: &egui::Context) {
        let (outer, inner, minimized, maximized, closing) = ctx.input(|input| {
            let viewport = input.viewport();
            (
                viewport.outer_rect,
                viewport.inner_rect,
                viewport.minimized,
                viewport.maximized,
                viewport.close_requested(),
            )
        });
        let window = &mut self.window;
        // Unknown means it hasn't been minimized: winit reports the flag once it changes.
        window.minimized = minimized.unwrap_or(false);
        window.maximized = maximized.unwrap_or(window.maximized);
        if !window.minimized
            && !window.maximized
            && let (Some(outer), Some(inner)) = (outer, inner)
        {
            window.x = outer.min.x;
            window.y = outer.min.y;
            window.width = inner.width();
            window.height = inner.height();
        }
        let due = closing || self.window_saved_at.elapsed() >= WINDOW_SAVE_INTERVAL;
        if self.window_saved == Some(self.window) || !due || !self.window.is_plausible() {
            return;
        }
        if let Some(store) = &mut self.window_store
            && let Err(error) = store.save(self.window)
        {
            log::warn!("cannot save the window state: {error}");
        }
        self.window_saved = Some(self.window);
        self.window_saved_at = Instant::now();
    }

    /// Stores the open tabs (targets, the shown one, tracing or not) whenever they changed.
    fn track_tabs(&mut self) {
        let shown = match self.page {
            Page::Target(index) => Some(index),
            Page::AllTargets => None,
        };
        let tabs: Vec<SavedTab> = self
            .tabs
            .iter()
            .enumerate()
            .filter_map(|(index, tab)| tab.remembered().map(|saved| SavedTab { shown: shown == Some(index), ..saved }))
            .collect();
        if tabs == self.tabs_saved {
            return;
        }
        if let Some(store) = &mut self.window_store
            && let Err(error) = store.save_tabs(&tabs)
        {
            log::warn!("cannot save the open tabs: {error}");
        }
        self.tabs_saved = tabs;
    }

    /// Status bar. On a target tab: its problem in red (network down, history off, unknown host) or its key
    /// numbers. On All targets: the same for the selected row; with no row selected, the number of targets and the
    /// problems — the message if there is one, the count if there are more.
    fn status_bar(&self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            let tab = match self.page {
                Page::Target(index) => self.tabs.get(index),
                Page::AllTargets => self
                    .all_targets
                    .selected()
                    .and_then(|selected| self.tabs.iter().find(|tab| tab.title() == selected)),
            };
            if let Some(tab) = tab {
                tab_status(ui, tab);
                return;
            }
            status_segment(ui, "Targets", self.tabs.len().to_string());
            let problems: Vec<(String, String)> =
                self.tabs.iter().filter_map(|tab| tab.problem().map(|problem| (tab.title(), problem))).collect();
            match problems.as_slice() {
                [] => {}
                [(title, problem)] => {
                    ui.separator();
                    status_problem(ui, &format!("{title}: {problem}"));
                }
                more => {
                    ui.separator();
                    status_problem(ui, &format!("Errors {}", more.len()));
                }
            }
        });
    }
}

/// Status of one target: its problem in red, or target, average, loss, hops and whether it's paused.
fn tab_status(ui: &mut Ui, tab: &Tab) {
    if let Some(problem) = tab.problem() {
        status_problem(ui, &problem);
        return;
    }
    let overview = tab.overview();
    let Some(label) = tab.label() else {
        ui.weak(&overview.status);
        return;
    };
    status_segment(ui, "Target", label);
    let stats = overview.target_hop.as_ref().map(|hop| &hop.stats);
    ui.separator();
    status_segment(ui, "Avg", format!("{} ms", format_ms(stats.and_then(|stats| stats.average()))));
    ui.separator();
    status_segment(ui, "Loss", stats.map_or_else(|| "-".to_owned(), |stats| format!("{:.1} %", stats.loss_percent())));
    ui.separator();
    status_segment(ui, "Hops", overview.hops.map_or_else(|| "-".to_owned(), |hops| hops.to_string()));
    if tab.is_paused() {
        ui.separator();
        ui.colored_label(theme::WARN, "Paused");
    }
}

/// The open page under the tab bar: a box continuing the open tab down to the status bar (its sides and bottom
/// are outlined, its top is the tab bar's baseline), inset from the window edges like the tabs.
fn page<R>(ui: &mut Ui, content: impl FnOnce(&mut Ui) -> R) -> R {
    let frame = Frame::new().fill(theme::WELL).inner_margin(Margin { left: 10, right: 10, top: 0, bottom: 8 });
    egui::CentralPanel::default()
        .frame(frame)
        .show(ui, |ui| {
            let rect = ui.max_rect();
            ui.painter().rect_filled(rect, 0.0, theme::CHASSIS);
            let inner = content(ui);
            // Drawn after the content, whose toolbar fill would cover them.
            let stroke = Stroke::new(1.0, theme::RULE);
            let painter = ui.painter();
            painter.vline(rect.left() + 0.5, rect.y_range(), stroke);
            painter.vline(rect.right() - 0.5, rect.y_range(), stroke);
            painter.hline(rect.x_range(), rect.bottom() - 0.5, stroke);
            inner
        })
        .inner
}

/// A "label value" pair of the status bar.
fn status_segment(ui: &mut Ui, label: &str, value: String) {
    ui.weak(label);
    ui.label(value);
}

/// A problem in the status bar: a red dot and the message.
fn status_problem(ui: &mut Ui, message: &str) {
    let (dot, _) = ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
    ui.painter().circle_filled(dot.center(), 4.0, theme::BAD);
    ui.colored_label(theme::BAD, message);
}

/// The action of a pressed keyboard shortcut, if any (consumed so widgets don't see it).
fn shortcut_action(ctx: &egui::Context) -> Option<Action> {
    let shortcuts = [
        (tab::RESTART_SHORTCUT, Action::Restart),
        (tab::NEW_TARGET_SHORTCUT, Action::NewTarget),
        (tab::CLOSE_SHORTCUT, Action::Close),
    ];
    ctx.input_mut(|input| {
        shortcuts
            .into_iter()
            .find(|(shortcut, _)| input.consume_shortcut(shortcut))
            .map(|(_, action)| action)
    })
}

impl eframe::App for PingWatchApp {
    /// Handles the tray icon and keeps the window state; runs even while the window is minimized or hidden.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.track_window(ctx);
        self.track_tabs();
        self.all_targets.save(self.window_store.as_ref());
        let Some(tray) = &self.tray else {
            return;
        };
        while let Some(command) = tray.poll() {
            match command {
                TrayCommand::Show => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
                TrayCommand::Exit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            }
        }
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        // Minimizing is sent once the window has drawn a frame; sent before it exists, Windows ignores it.
        if std::mem::take(&mut self.start_minimized) {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Minimized(true));
        }
        if let Some(action) = shortcut_action(ui.ctx()) {
            self.apply(action);
        }
        // Tabs that resolved last frame are checked before anything borrows them for drawing.
        self.merge_duplicate_traces();
        let bar_frame = Frame::new().fill(theme::WELL).inner_margin(Margin { left: 10, right: 10, top: 6, bottom: 0 });
        egui::Panel::top("tab-bar")
            .frame(bar_frame)
            .show_separator_line(false)
            .show(ui, |ui| self.tab_bar(ui));
        let status_frame = Frame::new().fill(theme::WELL).inner_margin(Margin::symmetric(12, 5));
        egui::Panel::bottom("status-bar")
            .frame(status_frame)
            .resizable(false)
            .show(ui, |ui| self.status_bar(ui));
        let context = tab::Context {
            names: &self.names,
            reader: self.reader.as_ref(),
            history_path: self.history_path.as_deref(),
            targets: self.targets.as_ref(),
        };
        for tab in &mut self.tabs {
            tab.poll(context);
        }
        let action = match self.page {
            Page::AllTargets => {
                let request = page(ui, |ui| {
                    self.all_targets.ui(ui, &mut self.tabs, self.targets.as_ref(), self.history_path.as_deref())
                });
                match request {
                    Some(Request::Open(index)) => self.page = Page::Target(index),
                    Some(Request::Add(host)) => self.tabs.push(Tab::tracing_every(host, self.all_targets.interval())),
                    None => {}
                }
                None
            }
            Page::Target(index) => self
                .tabs
                .get_mut(index)
                .and_then(|tab| page(ui, |ui| tab.ui(ui, context, &mut self.window.table_share))),
        };
        if let Some(action) = action {
            self.apply(action);
        }
        ui.ctx().request_repaint_after(REFRESH);
    }
}
