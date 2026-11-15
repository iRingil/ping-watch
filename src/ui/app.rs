//! Main window: a tab bar ("All targets", one tab per target, "+"), the All targets page or the active target tab,
//! and the status bar. Keyboard shortcuts act on the active tab.

use std::collections::HashMap;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use eframe::egui::{self, Button, CornerRadius, Frame, Margin, Rect, RichText, Sense, Stroke, StrokeKind, Ui, Vec2};

use crate::alerts::{Engine, Event, Observe, Rule, WindowStats};
use crate::i18n::{self, Language, tr, trf, trn};
use crate::probe::names::NameCache;
use crate::snapshot::{self, Snapshot};
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
use crate::update::{Check, Outcome, REPOSITORY};

/// Least time between two stores of the window state while it changes (moving, resizing).
const WINDOW_SAVE_INTERVAL: Duration = Duration::from_secs(1);
/// How often the window redraws to show fresh numbers.
const REFRESH: Duration = Duration::from_millis(250);
/// Height of a tab in the tab bar.
const TAB_HEIGHT: f32 = 30.0;
/// Space between a tab's edge and its title.
const TAB_PADDING: f32 = 12.0;
/// Width of a tab's close mark (its click target is a square this wide: WCAG 2.5.8 asks for 24).
const TAB_CLOSE_WIDTH: f32 = 24.0;
/// Space between tabs.
const TAB_GAP: f32 = 4.0;
/// Width of a tab bar scroll arrow.
const ARROW_WIDTH: f32 = 24.0;
/// Least room for the next tab to peek into the end of a scrolled tab strip.
const PEEK_MIN_WIDTH: f32 = 16.0;
/// Width of the fade at the end of a scrolled tab strip.
const FADE_WIDTH: f32 = 48.0;
/// Width of the snapshot button left of the bell.
const SNAPSHOT_WIDTH: f32 = 28.0;
/// Width of the bell (alerts) button left of the gear.
const BELL_WIDTH: f32 = 28.0;
/// Room for the red dot of a tab with an alert on.
const ALERT_DOT_WIDTH: f32 = 14.0;
/// Wait after the start before the first daily update check, so it doesn't compete with the start itself.
const UPDATE_CHECK_DELAY: Duration = Duration::from_secs(10);
/// How often the daily update check runs.
const UPDATE_PERIOD: Duration = Duration::from_secs(24 * 60 * 60);
/// Wait before trying a failed update check again.
const UPDATE_RETRY: Duration = Duration::from_secs(60 * 60);
/// How often the alert rules are checked.
const ALERT_CHECK_INTERVAL: Duration = Duration::from_secs(1);
/// Least time between two notifications of the same rule and target (the journal still gets every event).
const NOTIFY_INTERVAL: Duration = Duration::from_secs(10 * 60);
/// How often expired history is deleted.
const PRUNE_INTERVAL: Duration = Duration::from_secs(60 * 60);
/// How long a notice stays in the status bar.
const NOTICE_SHOWN: Duration = Duration::from_secs(8);
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

/// A message in the status bar for a while: how something the user did turned out (saved, or failed and why).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Notice {
    /// The message.
    text: String,
    /// Whether it reports a failure (shown in red).
    problem: bool,
    /// When it appeared.
    at: Instant,
}

impl Notice {
    /// A failure.
    fn problem(text: String) -> Self {
        Self { text, problem: true, at: Instant::now() }
    }

    /// A success.
    fn done(text: String) -> Self {
        Self { text, problem: false, at: Instant::now() }
    }
}

/// The file a snapshot was saved to, or why it wasn't.
type SnapshotResult = Result<PathBuf, String>;

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
    /// The language last applied to the context and the tray, `None` before the first frame.
    applied_language: Option<Language>,
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
    /// When the app started (the first daily update check waits a little after it).
    started_at: Instant,
    /// The daily update check while it runs.
    update_check: Option<Check>,
    /// When to try the daily check again after it failed (no internet), so it doesn't retry every frame.
    update_retry_at: Option<Instant>,
    /// A newer version found by the daily check: marks the gear.
    update_available: bool,
    /// The status bar's notice, if one is shown.
    notice: Option<Notice>,
    /// When the last notification of each rule and target was shown.
    notified: HashMap<(i64, String), Instant>,
    /// The retention the history was last pruned with, and when.
    pruned: Option<(Duration, Instant)>,
    /// Where the snapshot thread reports the file it saved (or why it couldn't).
    snapshot_results: (Sender<SnapshotResult>, Receiver<SnapshotResult>),
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
            applied_language: None,
            alert_store,
            alert_rules,
            alert_engine: Engine::default(),
            alerts_checked_at: Instant::now(),
            notifier,
            alerts_window: AlertsWindow::default(),
            started_at: Instant::now(),
            update_check: None,
            update_retry_at: None,
            update_available: false,
            notice: None,
            notified: HashMap::new(),
            pruned: None,
            snapshot_results: mpsc::channel(),
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

    /// Acts on one alert event: the journal (which the timelines draw their alert marks from) and a notification
    /// if the rule asks — at most one per rule and target every [`NOTIFY_INTERVAL`].
    fn on_alert(&mut self, event: Event, now_ms: i64) {
        let language = self.settings.language;
        let journal = self.alert_store.as_ref();
        let rule_of = |rule_id: i64| self.alert_rules.iter().find(|rule| rule.id == rule_id);
        let mut toast = None;
        let mut failure = None;
        match &event {
            Event::Started { rule_id, host, value } => {
                let Some(rule) = rule_of(*rule_id) else {
                    return;
                };
                if let Some(journal) = journal
                    && let Err(error) = journal.start(*rule_id, host, &rule.condition, now_ms, *value)
                {
                    failure = Some(error);
                }
                if rule.notify {
                    let measured = rule.condition.measured(*value, language);
                    toast = Some((format!("{host}: {measured}"), rule.condition.describe(language)));
                }
            }
            Event::Ended { rule_id, host, started_ms, worst, recovered } => {
                if let Some(journal) = journal
                    && let Err(error) = journal.end(*rule_id, host, now_ms, *worst, *recovered)
                {
                    failure = Some(error);
                }
                if let Some(rule) = rule_of(*rule_id).filter(|rule| *recovered && rule.notify_recovery) {
                    let seconds = u64::try_from(now_ms.saturating_sub(*started_ms) / 1_000).unwrap_or(0);
                    let lasted = crate::alerts::span(Duration::from_secs(seconds.max(1)), language);
                    let worst = rule.condition.measured(*worst, language);
                    toast = Some((
                        trf!(language, "{host} is back to normal", host = host),
                        trf!(language, "Lasted {lasted}, worst: {worst}", lasted = lasted, worst = worst),
                    ));
                }
            }
            Event::RouteChanged { rule_id, host, note } => {
                if let Some(journal) = journal
                    && let Err(error) = journal.note(*rule_id, host, *note, now_ms)
                {
                    failure = Some(error);
                }
                if rule_of(*rule_id).is_some_and(|rule| rule.notify) {
                    toast = Some((trf!(language, "{host}: route changed", host = host), note.describe(language)));
                }
            }
        }
        if let Some(error) = failure {
            log::warn!("cannot write the alert journal: {error}");
            self.notice =
                Some(Notice::problem(trf!(language, "The alert journal couldn't be written: {error}", error = error)));
        }
        let (rule_id, host) = match &event {
            Event::Started { rule_id, host, .. }
            | Event::Ended { rule_id, host, .. }
            | Event::RouteChanged { rule_id, host, .. } => (*rule_id, host.clone()),
        };
        if let Some((title, text)) = toast {
            let key = (rule_id, host);
            if self.notified.get(&key).is_none_or(|at| at.elapsed() >= NOTIFY_INTERVAL) {
                self.notifier.show(&title, &text);
                self.notified.insert(key, Instant::now());
            } else {
                log::info!("notification held back, one came lately: {title}");
            }
        }
    }

    /// Deletes what is older than the retention period: an hour after the last time, and at once when the
    /// setting changes (also on the first frame).
    fn prune_history(&mut self) {
        let Some(path) = &self.history_path else {
            return;
        };
        let due = self
            .pruned
            .is_none_or(|(retention, at)| retention != self.settings.retention || at.elapsed() >= PRUNE_INTERVAL);
        if due {
            history::prune_in_background(path, self.settings.retention);
            self.pruned = Some((self.settings.retention, Instant::now()));
        }
    }

    /// The daily update check: UPDATE_CHECK_DELAY after the start, then once a day (the time of the last success is
    /// stored, so restarts don't ask again). It runs on its own thread, so a missing network never holds up the
    /// window; a failure is retried an hour later. A newer version is announced once per check: a notification,
    /// a mark on the gear and the result in About.
    fn check_updates_daily(&mut self) {
        if let Some(check) = &self.update_check {
            let Some(result) = check.result() else {
                return;
            };
            self.update_check = None;
            match &result {
                Ok(outcome) => {
                    let now_ms = history::unix_ms(std::time::SystemTime::now());
                    if let Some(store) = &self.settings_store
                        && let Err(error) = store.set_last_update_check(now_ms)
                    {
                        log::warn!("cannot remember the update check: {error}");
                    }
                    if let Outcome::Newer { version, .. } = outcome {
                        self.update_available = true;
                        let language = self.settings.language;
                        self.notifier.show(
                            &trf!(language, "Ping Watch {version} is available", version = version),
                            tr!(language, "Settings → About has the link to download it"),
                        );
                        self.settings_window.set_update_result(result.clone());
                    }
                }
                Err(error) => {
                    log::info!("update check failed, retrying in an hour: {error}");
                    self.update_retry_at = Instant::now().checked_add(UPDATE_RETRY);
                }
            }
            return;
        }
        if !self.settings.check_updates
            || self.started_at.elapsed() < UPDATE_CHECK_DELAY
            || self.update_retry_at.is_some_and(|at| Instant::now() < at)
        {
            return;
        }
        let now_ms = history::unix_ms(std::time::SystemTime::now());
        let last = self.settings_store.as_ref().and_then(|store| store.last_update_check().ok().flatten());
        let day_ms = i64::try_from(UPDATE_PERIOD.as_millis()).unwrap_or(i64::MAX);
        if last.is_some_and(|last| now_ms.saturating_sub(last) < day_ms) {
            return;
        }
        self.update_retry_at = None;
        self.update_check = Some(Check::start(REPOSITORY, env!("CARGO_PKG_VERSION")));
    }

    /// Whether the shown page has something to snapshot: All targets always, a target tab once it traces.
    fn can_snapshot(&self) -> bool {
        match self.page {
            Page::Target(index) => self.tabs.get(index).is_some_and(|tab| tab.target().is_some()),
            Page::AllTargets => true,
        }
    }

    /// Saves the shown page as an HTML snapshot: the page is written here, then a "Save as" dialog (on the desktop
    /// at first) runs on its own thread, so the window keeps drawing while it is open; the file saved (or the
    /// failure) comes back to the status bar.
    fn save_snapshot(&mut self) {
        let journal = self.alert_store.as_ref().and_then(|store| store.journal().ok()).unwrap_or_default();
        let language = self.settings.language;
        let snapshot = match self.page {
            Page::Target(index) => {
                let Some(report) =
                    self.tabs.get(index).and_then(|tab| tab.snapshot(&self.names, self.reader.as_ref(), &journal))
                else {
                    self.notice = Some(Notice::problem(
                        tr!(language, "Nothing to save yet: the snapshot needs a running trace").to_owned(),
                    ));
                    return;
                };
                Snapshot::Target(Box::new(report))
            }
            Page::AllTargets => {
                self.all_targets.snapshot(&self.tabs, self.reader.as_ref(), &self.alerting_hosts(), language)
            }
        };
        let now = chrono::Local::now();
        let html = snapshot::html(
            &snapshot,
            &now.format("%d.%m.%Y %H:%M:%S").to_string(),
            env!("CARGO_PKG_VERSION"),
            language,
        );
        let file_name = snapshot.file_name(&now.format("%Y-%m-%d %H-%M").to_string(), language);
        let (title, filter) = (tr!(language, "Save snapshot"), tr!(language, "Web page"));
        let results = self.snapshot_results.0.clone();
        let spawned = std::thread::Builder::new().name("snapshot".to_owned()).spawn(move || {
            let mut dialog =
                rfd::FileDialog::new().set_title(title).set_file_name(&file_name).add_filter(filter, &["html"]);
            if let Some(desktop) = dirs::desktop_dir() {
                dialog = dialog.set_directory(desktop);
            }
            // Cancelled: nothing to report.
            let Some(path) = dialog.save_file() else {
                return;
            };
            let result = std::fs::write(&path, html).map(|()| path.clone()).map_err(|error| {
                log::error!("cannot save the snapshot to {}: {error}", path.display());
                format!("{}: {error}", path.display())
            });
            results.send(result).ok();
        });
        if let Err(error) = spawned {
            log::error!("cannot open the save dialog: {error}");
            self.notice = Some(Notice::problem(trf!(language, "Cannot open the save dialog: {error}", error = error)));
        }
    }

    /// Takes the outcome of a snapshot saved in the background into the status bar.
    fn take_snapshot_results(&mut self) {
        let language = self.settings.language;
        while let Ok(result) = self.snapshot_results.1.try_recv() {
            self.notice = Some(match result {
                Ok(path) => Notice::done(trf!(language, "Snapshot saved: {path}", path = path.display())),
                Err(error) => Notice::problem(trf!(language, "The snapshot wasn't saved: {error}", error = error)),
            });
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
            self.notice = Some(Notice::problem(trf!(
                self.settings.language,
                "The settings weren't saved: {error}",
                error = error
            )));
        }
        // One attempt per change, not one per frame.
        self.settings_saved = self.settings;
    }

    /// With "hide to the tray" set, the close button hides the window instead of quitting, unless the app was told
    /// to quit for good (tray Exit, Ctrl+C) or there is no tray icon to bring the window back with.
    fn hide_instead_of_closing(&self, ctx: &egui::Context) {
        let closing = ctx.input(|input| input.viewport().close_requested());
        if closing && self.settings.close_to_tray && self.tray.is_some() && !self.quit.load(Ordering::Relaxed) {
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
            if tab_widget(ui, self.page == Page::AllTargets, tr!(ui, "All targets"), false, false).0.clicked() {
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
            let available = available - SNAPSHOT_WIDTH - BELL_WIDTH - GEAR_WIDTH - TAB_GAP * 3.0;
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
            if ui.add_sized([PLUS_WIDTH, TAB_HEIGHT], plus).on_hover_text(tr!(ui, "New target (Ctrl+N)")).clicked() {
                new_target = true;
            }
            ui.add_space((ui.available_width() - SNAPSHOT_WIDTH - BELL_WIDTH - GEAR_WIDTH - TAB_GAP * 3.0).max(0.0));
            let snapshot_tooltip = if self.can_snapshot() {
                tr!(ui, "Save a snapshot of this page (HTML)")
            } else {
                tr!(ui, "A snapshot needs a running trace")
            };
            if bar_button(ui, icons::Icon::Camera, SNAPSHOT_WIDTH, snapshot_tooltip, self.can_snapshot()).clicked() {
                self.save_snapshot();
            }
            if bell_button(ui, self.alert_engine.active_count()).clicked() {
                self.alerts_window.open();
            }
            if gear_button(ui, self.update_available).clicked() {
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
fn gear_button(ui: &mut Ui, update_available: bool) -> egui::Response {
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
    if update_available {
        ui.painter()
            .circle_filled(egui::pos2(rect.right() - 6.0, rect.top() + 8.0), 4.0, theme::palette(ui).signal);
    }
    response.on_hover_text(if update_available {
        tr!(ui, "A new version is available: see About")
    } else {
        tr!(ui, "Settings and more")
    })
}

/// Width of a tab titled `title`, with a close mark if `closable` and the alert dot if `alert`.
fn tab_width(ui: &Ui, title: &str, closable: bool, alert: bool) -> f32 {
    let galley = ui.painter().layout_no_wrap(title.to_owned(), theme::heading_font(14.5), theme::palette(ui).ink);
    galley.size().x
        + TAB_PADDING * 2.0
        + if closable { TAB_CLOSE_WIDTH } else { 0.0 }
        + if alert { ALERT_DOT_WIDTH } else { 0.0 }
}

/// A plain icon button of the tab bar (`width` wide), lit under the pointer; dimmed and not clickable unless
/// `enabled` (the tooltip then says why).
fn bar_button(ui: &mut Ui, icon: icons::Icon, width: f32, tooltip: &str, enabled: bool) -> egui::Response {
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, TAB_HEIGHT), sense);
    let palette = theme::palette(ui);
    let lit = enabled && (response.hovered() || response.has_focus());
    if lit {
        ui.painter()
            .rect_filled(rect.shrink2(Vec2::new(0.0, 3.0)), CornerRadius::same(3), palette.pane_header);
    }
    let color = if !enabled {
        palette.rule
    } else if lit {
        palette.ink
    } else {
        palette.ink_muted
    };
    icons::paint(ui, rect.center(), icon, color);
    icons::focus_ring(ui, &response, rect.shrink2(Vec2::new(0.0, 3.0)));
    response.on_hover_text(tooltip)
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
            if active > 9 { "9+".to_owned() } else { active.to_string() },
            theme::heading_font(if active > 9 { 9.5 } else { 11.0 }),
            palette.on_signal,
        );
    }
    let tooltip = match active {
        0 => tr!(ui, "Alerts").to_owned(),
        count => trn!(ui, u64::try_from(count).unwrap_or(u64::MAX), "Alerts: {n} is on now|Alerts: {n} are on now"),
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
    let tooltip = if left { tr!(ui, "Earlier tabs") } else { tr!(ui, "Later tabs") };
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
        let close = ui
            .interact(close_rect, response.id.with("close"), Sense::click())
            .on_hover_text(tr!(ui, "Close and stop"));
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
            // The outcome of what the user just did comes first, for a few seconds.
            if let Some(notice) = self.notice.as_ref().filter(|notice| notice.at.elapsed() < NOTICE_SHOWN) {
                if notice.problem {
                    status_problem(ui, &notice.text);
                } else {
                    ui.colored_label(theme::palette(ui).ink, &notice.text);
                }
                return;
            }
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
                status_problem(ui, &trn!(ui, u64::try_from(active).unwrap_or(u64::MAX), "{n} alert on|{n} alerts on"));
                ui.separator();
            }
            status_segment(ui, tr!(ui, "Targets"), self.tabs.len().to_string());
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
                    status_problem(ui, &trf!(ui, "Errors {count}", count = more.len()));
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
    status_segment(ui, tr!(ui, "Target"), label);
    let stats = overview.target_hop.as_ref().map(|hop| &hop.stats);
    ui.separator();
    let average = stats.and_then(|stats| stats.average());
    status_segment(
        ui,
        tr!(ui, "Avg"),
        average.map_or_else(|| "-".to_owned(), |average| trf!(ui, "{ms} ms", ms = format_ms(Some(average)))),
    );
    ui.separator();
    status_segment(
        ui,
        tr!(ui, "Loss"),
        stats.map_or_else(|| "-".to_owned(), |stats| format!("{:.1} %", stats.loss_percent())),
    );
    ui.separator();
    status_segment(ui, tr!(ui, "Hops"), overview.hops.map_or_else(|| "-".to_owned(), |hops| hops.to_string()));
    if tab.is_paused() {
        ui.separator();
        ui.colored_label(theme::palette(ui).warn, tr!(ui, "Paused"));
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
        if self.applied_language != Some(self.settings.language) {
            let language = self.settings.language;
            i18n::set(ctx, language);
            if let Some(tray) = &self.tray {
                tray.set_labels(tr!(language, "Show Ping Watch"), tr!(language, "Exit"));
            }
            self.applied_language = Some(language);
        }
        self.hide_instead_of_closing(ctx);
        self.track_window(ctx);
        self.track_tabs();
        self.all_targets.save(self.window_store.as_ref());
        self.save_settings();
        self.check_alerts();
        self.check_updates_daily();
        self.prune_history();
        self.take_snapshot_results();
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
