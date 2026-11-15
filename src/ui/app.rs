//! Main window: a tab bar ("All targets", one tab per target, "+"), the All targets page or the active target tab,
//! and the status bar. Keyboard shortcuts act on the active tab.

use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use eframe::egui::{self, Button, CornerRadius, Frame, Margin, Rect, RichText, Sense, Stroke, StrokeKind, Ui, Vec2};

use crate::alerts::{Condition, Engine, Event, Observe, Rule, WindowStats};
use crate::probe::names::NameCache;
use crate::store::alerts::AlertStore;
use crate::store::history::{self, Store};
use crate::store::settings::{AppSettings, SettingsStore};
use crate::store::targets::{TargetList, TraceSettings};
use crate::store::window_state::{SavedTab, WindowState, WindowStore};
use crate::ui::alerts_window::AlertsWindow;
use crate::ui::all_targets::{AllTargets, Request};
use crate::ui::hop_table::format_ms;
use crate::ui::icons;
use crate::ui::notify::Notifier;
use crate::ui::settings::SettingsWindow;
use crate::ui::tab::{self, Action, Tab};
use crate::ui::tab_strip;
use crate::ui::theme;
use crate::ui::theme::ThemeChoice;
use crate::ui::tray::{Tray, TrayCommand};

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
/// Width of the bell (alerts) button left of the gear.
const BELL_WIDTH: f32 = 28.0;
/// Room for the red dot of a tab with an alert on.
const ALERT_DOT_WIDTH: f32 = 14.0;
/// How often the alert rules are checked.
const ALERT_CHECK_INTERVAL: Duration = Duration::from_secs(1);
/// Width of the "+" (new target) button.
const PLUS_WIDTH: f32 = 24.0;
/// Width of the gear (app menu) button at the right end of the tab bar.
const GEAR_WIDTH: f32 = 28.0;

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
    /// The app's settings as changed in the Settings window.
    settings: AppSettings,
    /// The settings as last stored.
    settings_saved: AppSettings,
    /// Where the settings are kept, `None` if history is off.
    settings_store: Option<SettingsStore>,
    /// The Settings window.
    settings_window: SettingsWindow,
    /// Set when the app must really quit (tray Exit, Ctrl+C), even if the close button only hides the window.
    quit: Arc<AtomicBool>,
    /// The theme choice last applied to the context, `None` before the first frame.
    applied_theme: Option<ThemeChoice>,
    /// Alert rules and journal, `None` if history is off.
    alert_store: Option<AlertStore>,
    /// The alert rules as stored.
    alert_rules: Vec<Rule>,
    /// Checks the rules and keeps which alerts are on.
    alert_engine: Engine,
    /// When the rules were last checked.
    alerts_checked_at: Instant,
    /// Windows notifications.
    notifier: Notifier,
    /// The Alerts window.
    alerts_window: AlertsWindow,
}

impl PingWatchApp {
    /// Creates the app with one tab per host in `hosts` (all tracing right away), or one empty tab, the `tray`
    /// icon, and the window state as stored in `window_store` (`saved_window`, already applied to the window's
    /// geometry by the caller). `quit` is set by whoever wants the app to quit for good (Ctrl+C).
    pub fn new(
        hosts: Vec<String>,
        tray: Option<Tray>,
        window_store: Option<WindowStore>,
        saved_window: Option<WindowState>,
        quit: Arc<AtomicBool>,
    ) -> Self {
        let history_path = history::default_path().inspect_err(|error| log::warn!("{error}")).ok();
        let settings_store = history_path
            .as_deref()
            .and_then(|path| SettingsStore::open(path).inspect_err(|error| log::warn!("{error}")).ok());
        let settings = settings_store
            .as_ref()
            .and_then(|store| store.load().inspect_err(|error| log::warn!("{error}")).ok())
            .unwrap_or_default();
        let defaults = settings.defaults;
        let alert_store = history_path
            .as_deref()
            .and_then(|path| AlertStore::open(path).inspect_err(|error| log::warn!("{error}")).ok());
        let alert_rules = alert_store
            .as_ref()
            .and_then(|store| store.rules().inspect_err(|error| log::warn!("{error}")).ok())
            .unwrap_or_default();
        let notifier = Notifier::new(history_path.as_deref().and_then(std::path::Path::parent), crate::ICON_PNG);
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
        let mut tabs: Vec<Tab> = hosts.into_iter().map(|host| Tab::tracing(host, defaults)).collect();
        tabs.extend(saved_tabs.iter().map(|saved| Tab::restored(saved, defaults)));
        let page = match saved_tabs.iter().position(|saved| saved.shown) {
            Some(index) => Page::Target(index),
            None if !saved_tabs.is_empty() => Page::AllTargets,
            None => Page::Target(0),
        };
        if tabs.is_empty() {
            tabs.push(Tab::new(defaults));
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
            settings,
            settings_saved: settings,
            settings_store,
            settings_window: SettingsWindow::default(),
            quit,
            applied_theme: None,
            alert_store,
            alert_rules,
            alert_engine: Engine::default(),
            alerts_checked_at: Instant::now(),
            notifier,
            alerts_window: AlertsWindow::default(),
        }
    }

    /// Checks the alert rules once a second (also while the window is hidden) and acts on what changed.
    fn check_alerts(&mut self) {
        if self.alerts_checked_at.elapsed() < ALERT_CHECK_INTERVAL {
            return;
        }
        self.alerts_checked_at = Instant::now();
        let now_ms = history::unix_ms(std::time::SystemTime::now());
        let events = self.alert_engine.check(&self.alert_rules, &Watched(&self.tabs), now_ms);
        for event in events {
            self.on_alert(event, now_ms);
        }
    }

    /// Acts on one alert event: the journal, a mark on the target's timeline, a notification if the rule asks.
    fn on_alert(&self, event: Event, now_ms: i64) {
        let rule_of = |rule_id: i64| self.alert_rules.iter().find(|rule| rule.id == rule_id);
        let mark = |host: &str, text: &str| {
            let target_id =
                self.tabs.iter().find(|tab| tab.host().eq_ignore_ascii_case(host)).and_then(Tab::history_id);
            if let (Some(reader), Some(target_id)) = (&self.reader, target_id)
                && let Err(error) = reader.add_comment(target_id, now_ms, text)
            {
                log::warn!("cannot mark the alert on the timeline: {error}");
            }
        };
        let journal = self.alert_store.as_ref();
        match event {
            Event::Started { rule_id, host, value } => {
                let Some(rule) = rule_of(rule_id) else {
                    return;
                };
                let (measured, unit) = measured(&rule.condition, value);
                if let Some(journal) = journal
                    && let Err(error) = journal.start(rule_id, &host, &rule.condition.describe(), now_ms, value, unit)
                {
                    log::warn!("cannot write the alert journal: {error}");
                }
                mark(&host, &format!("Alert: {measured} ({})", rule.condition.describe()));
                if rule.notify {
                    self.notifier.show(&format!("{host}: {measured}"), &capitalized(&rule.condition.describe()));
                }
            }
            Event::Ended { rule_id, host, started_ms, worst, recovered } => {
                if let Some(journal) = journal
                    && let Err(error) = journal.end(rule_id, &host, now_ms, worst)
                {
                    log::warn!("cannot write the alert journal: {error}");
                }
                let Some(rule) = rule_of(rule_id).filter(|_| recovered) else {
                    return;
                };
                let seconds = u64::try_from(now_ms.saturating_sub(started_ms) / 1_000).unwrap_or(0);
                let lasted = crate::alerts::span(Duration::from_secs(seconds.max(1)));
                let (worst_text, _) = measured(&rule.condition, worst);
                mark(&host, &format!("Back to normal after {lasted} (worst: {worst_text})"));
                if rule.notify_recovery {
                    self.notifier
                        .show(&format!("{host} is back to normal"), &format!("Lasted {lasted}, worst: {worst_text}"));
                }
            }
            Event::RouteChanged { rule_id, host, change } => {
                let what = change.describe();
                if let Some(journal) = journal
                    && let Err(error) = journal.note(rule_id, &host, &format!("Route changed: {what}"), now_ms)
                {
                    log::warn!("cannot write the alert journal: {error}");
                }
                mark(&host, &format!("Route changed: {what}"));
                if rule_of(rule_id).is_some_and(|rule| rule.notify) {
                    self.notifier.show(&format!("{host}: route changed"), &what);
                }
            }
        }
    }

    /// Reloads the alert rules after the Alerts window changed them.
    fn reload_alert_rules(&mut self) {
        if let Some(store) = &self.alert_store {
            match store.rules() {
                Ok(rules) => self.alert_rules = rules,
                Err(error) => log::warn!("cannot read the alert rules: {error}"),
            }
        }
    }

    /// Hosts with an alert on now.
    fn alerting_hosts(&self) -> Vec<String> {
        self.tabs
            .iter()
            .map(Tab::host)
            .filter(|host| !host.is_empty() && self.alert_engine.is_alerting(host))
            .map(str::to_owned)
            .collect()
    }

    /// Stores the settings whenever they changed.
    fn save_settings(&mut self) {
        if self.settings == self.settings_saved {
            return;
        }
        if let Some(store) = &mut self.settings_store
            && let Err(error) = store.save(&self.settings)
        {
            log::warn!("cannot save the settings: {error}");
        }
        // One attempt per change, not one per frame.
        self.settings_saved = self.settings;
    }

    /// With "hide to the tray" set, the close button hides the window instead of quitting, unless the app was told
    /// to quit for good (tray Exit, Ctrl+C).
    fn hide_instead_of_closing(&self, ctx: &egui::Context) {
        let closing = ctx.input(|input| input.viewport().close_requested());
        if closing && self.settings.close_to_tray && !self.quit.load(Ordering::Relaxed) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }
    }

    /// Carries out `action` on the active tab.
    fn apply(&mut self, action: Action) {
        match action {
            Action::NewTarget => {
                self.tabs.push(Tab::new(self.settings.defaults));
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
        ui.painter().hline(bar.x_range(), bar.bottom() - 0.5, Stroke::new(1.0, theme::palette(ui).rule));
        let reveal = match self.page {
            Page::Target(index) if self.page != self.tab_bar_page => Some(index),
            Page::Target(_) | Page::AllTargets => None,
        };
        self.tab_bar_page = self.page;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = TAB_GAP;
            if tab_widget(ui, self.page == Page::AllTargets, "All targets", false, false).0.clicked() {
                self.page = Page::AllTargets;
            }
            // A tracing tab is labelled like its headings ("address / description"), others by the host entered.
            let titles: Vec<String> = self.tabs.iter().map(|tab| tab.label().unwrap_or_else(|| tab.title())).collect();
            let alerting: Vec<bool> = self
                .tabs
                .iter()
                .map(|tab| !tab.host().is_empty() && self.alert_engine.is_alerting(tab.host()))
                .collect();
            let widths: Vec<f32> =
                titles.iter().zip(&alerting).map(|(title, alert)| tab_width(ui, title, true, *alert)).collect();
            let all_width: f32 = widths.iter().map(|width| width + TAB_GAP).sum();
            let available = ui.available_width();
            // The gear keeps its place at the right end whatever the tabs do.
            let available = available - BELL_WIDTH - GEAR_WIDTH - TAB_GAP * 2.0;
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
                let alert = alerting.get(index).copied().unwrap_or(false);
                let (response, close_clicked) = tab_widget(ui, self.page == Page::Target(index), title, true, alert);
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
                    let alert = alerting.get(last).copied().unwrap_or(false);
                    if tab_widget(&mut child, self.page == Page::Target(last), title, false, alert).0.clicked() {
                        self.page = Page::Target(last);
                    }
                    let fade_width = peek.width().min(FADE_WIDTH);
                    let fade = Rect::from_min_max(
                        egui::pos2(peek.right() - fade_width, peek.top()),
                        egui::pos2(peek.right(), peek.bottom() - 1.0),
                    );
                    fade_out(ui, fade, theme::palette(ui).bar);
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
            ui.add_space((ui.available_width() - BELL_WIDTH - GEAR_WIDTH - TAB_GAP * 2.0).max(0.0));
            if bell_button(ui, self.alert_engine.active_count()).clicked() {
                self.alerts_window.open();
            }
            if gear_button(ui).clicked() {
                self.settings_window.open();
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

/// The gear at the right end of the tab bar: will open the app menu (settings, alerts, copy, about).
fn gear_button(ui: &mut Ui) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(GEAR_WIDTH, TAB_HEIGHT), Sense::click());
    if response.hovered() {
        ui.painter().rect_filled(
            rect.shrink2(Vec2::new(0.0, 3.0)),
            CornerRadius::same(3),
            theme::palette(ui).pane_header,
        );
    }
    let color = if response.hovered() { theme::palette(ui).ink } else { theme::palette(ui).ink_muted };
    icons::paint(ui, rect.center(), icons::Icon::Gear, color);
    response.on_hover_text("Settings and more")
}

/// Width of a tab titled `title`, with a close mark if `closable` and the alert dot if `alert`.
fn tab_width(ui: &Ui, title: &str, closable: bool, alert: bool) -> f32 {
    let galley = ui.painter().layout_no_wrap(title.to_owned(), theme::heading_font(14.5), theme::palette(ui).ink);
    galley.size().x
        + TAB_PADDING * 2.0
        + if closable { TAB_CLOSE_WIDTH } else { 0.0 }
        + if alert { ALERT_DOT_WIDTH } else { 0.0 }
}

/// The bell left of the gear: opens the Alerts window; a red badge counts the alerts that are on.
fn bell_button(ui: &mut Ui, active: usize) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(BELL_WIDTH, TAB_HEIGHT), Sense::click());
    let palette = theme::palette(ui);
    if response.hovered() {
        ui.painter()
            .rect_filled(rect.shrink2(Vec2::new(0.0, 3.0)), CornerRadius::same(3), palette.pane_header);
    }
    let color = if response.hovered() { palette.ink } else { palette.ink_muted };
    icons::paint(ui, rect.center(), icons::Icon::Bell, color);
    if active > 0 {
        let badge = egui::pos2(rect.right() - 7.0, rect.top() + 9.0);
        ui.painter().circle_filled(badge, 7.0, palette.bad);
        ui.painter().text(
            badge,
            egui::Align2::CENTER_CENTER,
            active.min(9).to_string(),
            theme::heading_font(11.0),
            palette.on_signal,
        );
    }
    let tooltip = match active {
        0 => "Alerts".to_owned(),
        1 => "Alerts: 1 is on now".to_owned(),
        count => format!("Alerts: {count} are on now"),
    };
    response.on_hover_text(tooltip)
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
        painter.rect_filled(rect.shrink2(Vec2::new(0.0, 3.0)), CornerRadius::same(3), theme::palette(ui).pane_header);
    }
    let color = if response.hovered() { theme::palette(ui).ink } else { theme::palette(ui).ink_muted };
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
fn tab_widget(ui: &mut Ui, open: bool, title: &str, closable: bool, alert: bool) -> (egui::Response, bool) {
    let font = theme::heading_font(14.5);
    let text_color = if open { theme::palette(ui).ink } else { theme::palette(ui).ink_muted };
    let galley = ui.painter().layout_no_wrap(title.to_owned(), font, text_color);
    let close_width = if closable { TAB_CLOSE_WIDTH } else { 0.0 };
    let dot_width = if alert { ALERT_DOT_WIDTH } else { 0.0 };
    let size = Vec2::new(galley.size().x + TAB_PADDING * 2.0 + close_width + dot_width, TAB_HEIGHT);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());

    let fill = if open {
        theme::palette(ui).chassis
    } else if response.hovered() {
        theme::palette(ui).pane_header
    } else {
        theme::palette(ui).tab_idle
    };
    let corners = CornerRadius { nw: 5, ne: 5, sw: 0, se: 0 };
    let painter = ui.painter();
    painter.rect(rect, corners, fill, Stroke::new(1.0, theme::palette(ui).rule), StrokeKind::Inside);
    if open {
        // Open the bottom edge into the toolbar and mark it with a signal line just above (a line along the top
        // would cut across the rounded corners).
        let inner = (rect.left() + 1.0)..=(rect.right() - 1.0);
        painter.hline(inner.clone(), rect.bottom() - 0.5, Stroke::new(2.0, fill));
        painter.hline(inner, rect.bottom() - 3.0, Stroke::new(2.0, theme::palette(ui).signal));
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
        let color = if close.hovered() { theme::palette(ui).bad } else { theme::palette(ui).ink_muted };
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
            let active = self.alert_engine.active_count();
            if active > 0 {
                status_problem(ui, &if active == 1 { "1 alert on".to_owned() } else { format!("{active} alerts on") });
                ui.separator();
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
    let average = stats.and_then(|stats| stats.average());
    status_segment(
        ui,
        "Avg",
        average.map_or_else(|| "-".to_owned(), |average| format!("{} ms", format_ms(Some(average)))),
    );
    ui.separator();
    status_segment(ui, "Loss", stats.map_or_else(|| "-".to_owned(), |stats| format!("{:.1} %", stats.loss_percent())));
    ui.separator();
    status_segment(ui, "Hops", overview.hops.map_or_else(|| "-".to_owned(), |hops| hops.to_string()));
    if tab.is_paused() {
        ui.separator();
        ui.colored_label(theme::palette(ui).warn, "Paused");
    }
}

/// The open page under the tab bar: a box continuing the open tab down to the status bar (its sides and bottom
/// are outlined, its top is the tab bar's baseline), inset from the window edges like the tabs.
fn page<R>(ui: &mut Ui, content: impl FnOnce(&mut Ui) -> R) -> R {
    let frame =
        Frame::new()
            .fill(theme::palette(ui).bar)
            .inner_margin(Margin { left: 10, right: 10, top: 0, bottom: 8 });
    egui::CentralPanel::default()
        .frame(frame)
        .show(ui, |ui| {
            let rect = ui.max_rect();
            ui.painter().rect_filled(rect, 0.0, theme::palette(ui).chassis);
            let inner = content(ui);
            // Drawn after the content, whose toolbar fill would cover them.
            let stroke = Stroke::new(1.0, theme::palette(ui).rule);
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
    ui.painter().circle_filled(dot.center(), 4.0, theme::palette(ui).bad);
    ui.colored_label(theme::palette(ui).bad, message);
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
        if self.applied_theme != Some(self.settings.theme) {
            self.settings.theme.apply(ctx);
            self.applied_theme = Some(self.settings.theme);
        }
        self.hide_instead_of_closing(ctx);
        self.track_window(ctx);
        self.track_tabs();
        self.all_targets.save(self.window_store.as_ref());
        self.save_settings();
        self.check_alerts();
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
                TrayCommand::Exit => {
                    self.quit.store(true, Ordering::Relaxed);
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
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
        let bar_frame =
            Frame::new()
                .fill(theme::palette(ui).bar)
                .inner_margin(Margin { left: 10, right: 10, top: 6, bottom: 0 });
        egui::Panel::top("tab-bar")
            .frame(bar_frame)
            .show_separator_line(false)
            .show(ui, |ui| self.tab_bar(ui));
        let status_frame = Frame::new().fill(theme::palette(ui).bar).inner_margin(Margin::symmetric(12, 5));
        egui::Panel::bottom("status-bar")
            .frame(status_frame)
            .resizable(false)
            .show(ui, |ui| self.status_bar(ui));
        let context = tab::Context {
            names: &self.names,
            reader: self.reader.as_ref(),
            history_path: self.history_path.as_deref(),
            targets: self.targets.as_ref(),
            settings: &self.settings,
        };
        for tab in &mut self.tabs {
            tab.poll(context);
        }
        let action = match self.page {
            Page::AllTargets => {
                let request = page(ui, |ui| {
                    let alerting = self.alerting_hosts();
                    self.all_targets.ui(
                        ui,
                        &mut self.tabs,
                        self.targets.as_ref(),
                        self.history_path.as_deref(),
                        &alerting,
                    )
                });
                match request {
                    Some(Request::Open(index)) => self.page = Page::Target(index),
                    Some(Request::Add(host)) => {
                        let defaults =
                            TraceSettings { interval: self.all_targets.interval(), ..self.settings.defaults };
                        self.tabs.push(Tab::tracing(host, defaults));
                    }
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
        let data_folder = self.history_path.as_deref().and_then(std::path::Path::parent);
        self.settings_window.show(ui.ctx(), &mut self.settings, data_folder);
        let hosts: Vec<String> =
            self.tabs.iter().map(Tab::host).filter(|host| !host.is_empty()).map(str::to_owned).collect();
        if self.alerts_window.show(ui.ctx(), self.alert_store.as_ref(), &self.alert_rules, &hosts) {
            self.reload_alert_rules();
        }
        ui.ctx().request_repaint_after(REFRESH);
    }
}

/// The tabs as the alert engine sees them: the targets that trace and aren't paused.
struct Watched<'a>(&'a [Tab]);

impl Observe for Watched<'_> {
    fn hosts(&self) -> Vec<String> {
        self.0.iter().filter(|tab| tab.is_watched()).map(|tab| tab.host().to_owned()).collect()
    }

    fn window(&self, host: &str, window: Duration) -> Option<WindowStats> {
        self.0.iter().find(|tab| tab.host() == host).and_then(|tab| tab.window_stats(window))
    }

    fn path(&self, host: &str) -> Vec<(u8, IpAddr)> {
        self.0.iter().find(|tab| tab.host() == host).map(Tab::answering_path).unwrap_or_default()
    }
}

/// A measured value of `condition` for people, with its unit: "loss 23 %", "average latency 180 ms",
/// "no answer for 30 seconds".
fn measured(condition: &Condition, value: f64) -> (String, &'static str) {
    match condition {
        Condition::Loss { .. } => (format!("loss {value:.0} %"), "%"),
        Condition::Latency { .. } => (format!("average latency {value:.0} ms"), "ms"),
        Condition::Unreachable { .. } => (format!("no answer for {value:.0} s"), "s"),
        Condition::RouteChanged => (String::new(), ""),
    }
}

/// `text` with a capital first letter.
fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map(|first| first.to_uppercase().chain(chars).collect()).unwrap_or_default()
}
