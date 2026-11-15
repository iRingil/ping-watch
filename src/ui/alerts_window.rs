//! The Alerts window, opened from the bell: the rules (list, add, edit, delete, on/off) and the journal of what
//! fired. Rules are written to the store at once; the app reloads them when told so.

use std::time::Duration;

use eframe::egui::{self, Align, Layout, RichText, ScrollArea, Ui};
use egui_extras::{Column, TableBuilder};

use crate::alerts::{Condition, ConditionKind, Rule, Targets};
use crate::store::alerts::AlertStore;
use crate::ui::icons::{self, Icon, Style};
use crate::ui::settings::{close_mark, label_first_row, page_tabs, settings_grid};
use crate::ui::theme;

/// Width of the window's content.
const WIDTH: f32 = 720.0;
/// Least height of a page, so switching pages doesn't make the window jump.
const PAGE_HEIGHT: f32 = 340.0;
/// Height of a journal row.
const JOURNAL_ROW: f32 = 24.0;
/// Periods a loss or latency rule can look back over.
const WINDOWS: [Duration; 7] = [
    Duration::from_secs(30),
    Duration::from_secs(60),
    Duration::from_secs(120),
    Duration::from_secs(300),
    Duration::from_secs(600),
    Duration::from_secs(1_800),
    Duration::from_secs(3_600),
];
/// How long without an answer a "no answer" rule waits.
const SILENCES: [Duration; 5] = [
    Duration::from_secs(10),
    Duration::from_secs(30),
    Duration::from_secs(60),
    Duration::from_secs(120),
    Duration::from_secs(300),
];

/// A page of the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Page {
    /// The rules.
    #[default]
    Rules,
    /// What fired.
    Journal,
}

/// A rule being added or edited.
#[derive(Debug, Clone, PartialEq)]
struct Draft {
    /// Row id, 0 for a new rule.
    id: i64,
    /// Kind of condition.
    kind: ConditionKind,
    /// Loss threshold, percent.
    percent: f64,
    /// Latency threshold, ms.
    ms: f64,
    /// Period of a loss or latency rule.
    window: Duration,
    /// Silence of a "no answer" rule.
    after: Duration,
    /// All targets, or the chosen `hosts`.
    all_targets: bool,
    /// Chosen targets.
    hosts: Vec<String>,
    /// Windows notification when it fires.
    notify: bool,
    /// Notification when it's over.
    notify_recovery: bool,
    /// Whether the rule is on.
    enabled: bool,
}

impl Draft {
    /// A new rule: loss above 10 % over 2 minutes, all targets, with notifications.
    fn new() -> Self {
        Self::of(&Rule {
            id: 0,
            targets: Targets::All,
            condition: Condition::default_of(ConditionKind::Loss),
            notify: true,
            notify_recovery: true,
            enabled: true,
        })
    }

    /// The draft of `rule` as it is.
    fn of(rule: &Rule) -> Self {
        // Numbers of the other kinds start at their defaults, so switching the kind in the form shows sensible ones.
        let (mut percent, mut ms, mut window, mut after) =
            (10.0, 150.0, Duration::from_secs(120), Duration::from_secs(30));
        match rule.condition {
            Condition::Loss { percent: p, window: w } => (percent, window) = (p, w),
            Condition::Latency { ms: m, window: w } => (ms, window) = (m, w),
            Condition::Unreachable { after: a } => after = a,
            Condition::RouteChanged => {}
        }
        let (all_targets, hosts) = match &rule.targets {
            Targets::All => (true, Vec::new()),
            Targets::Only(hosts) => (false, hosts.clone()),
        };
        Self {
            id: rule.id,
            kind: rule.condition.kind(),
            percent,
            ms,
            window,
            after,
            all_targets,
            hosts,
            notify: rule.notify,
            notify_recovery: rule.notify_recovery,
            enabled: rule.enabled,
        }
    }

    /// The rule this draft describes.
    fn rule(&self) -> Rule {
        let condition = match self.kind {
            ConditionKind::Loss => Condition::Loss { percent: self.percent, window: self.window },
            ConditionKind::Latency => Condition::Latency { ms: self.ms, window: self.window },
            ConditionKind::Unreachable => Condition::Unreachable { after: self.after },
            ConditionKind::RouteChanged => Condition::RouteChanged,
        };
        let targets = if self.all_targets { Targets::All } else { Targets::Only(self.hosts.clone()) };
        Rule {
            id: self.id,
            targets,
            condition,
            notify: self.notify,
            notify_recovery: self.notify_recovery && self.kind != ConditionKind::RouteChanged,
            enabled: self.enabled,
        }
    }
}

/// State of the Alerts window.
#[derive(Debug, Default)]
pub struct AlertsWindow {
    /// Whether the window is shown.
    open: bool,
    /// Page shown.
    page: Page,
    /// Rule being added or edited.
    draft: Option<Draft>,
}

impl AlertsWindow {
    /// Shows the window.
    pub fn open(&mut self) {
        self.open = true;
    }

    /// Draws the window if it is open. `rules` are the current rules, `hosts` the open targets to choose from.
    /// Returns whether the rules changed in `store`.
    pub fn show(&mut self, ctx: &egui::Context, store: Option<&AlertStore>, rules: &[Rule], hosts: &[String]) -> bool {
        if !self.open {
            return false;
        }
        let mut changed = false;
        let modal = egui::Modal::new(egui::Id::unique("alerts")).show(ctx, |ui| {
            // Exactly this wide: content never widens the window (the journal cuts long texts instead).
            ui.set_width(WIDTH);
            ui.set_max_width(WIDTH);
            let close = ui
                .horizontal(|ui| {
                    ui.label(RichText::new("Alerts").font(theme::heading_font(18.0)));
                    ui.with_layout(Layout::right_to_left(Align::Center), close_mark).inner
                })
                .inner;
            ui.add_space(8.0);
            page_tabs(ui, &[(Page::Rules, "Rules"), (Page::Journal, "Journal")], &mut self.page);
            ui.add_space(12.0);
            ui.allocate_ui(egui::vec2(WIDTH, PAGE_HEIGHT), |ui| {
                ui.set_min_height(PAGE_HEIGHT);
                let Some(store) = store else {
                    ui.weak("Alerts need the history database, which couldn't be opened.");
                    return;
                };
                match self.page {
                    Page::Rules => changed = self.rules_page(ui, store, rules, hosts),
                    Page::Journal => journal_page(ui, store),
                }
            });
            close
        });
        if modal.inner || modal.should_close() {
            self.open = false;
            self.draft = None;
        }
        changed
    }

    /// The rules page: the list, or the form of the rule being edited. Returns whether a rule changed.
    fn rules_page(&mut self, ui: &mut Ui, store: &AlertStore, rules: &[Rule], hosts: &[String]) -> bool {
        if let Some(draft) = &mut self.draft {
            return match rule_form(ui, draft, hosts) {
                FormAnswer::Save => {
                    let rule = draft.rule();
                    if let Err(error) = store.save_rule(&rule) {
                        log::error!("cannot save the alert rule: {error}");
                    }
                    self.draft = None;
                    true
                }
                FormAnswer::Cancel => {
                    self.draft = None;
                    false
                }
                FormAnswer::None => false,
            };
        }
        let mut changed = false;
        ScrollArea::vertical().max_height(PAGE_HEIGHT - 44.0).show(ui, |ui| {
            for rule in rules {
                ui.horizontal(|ui| {
                    let mut enabled = rule.enabled;
                    if ui.checkbox(&mut enabled, "").on_hover_text("On or off").changed() {
                        let toggled = Rule { enabled, ..rule.clone() };
                        if let Err(error) = store.save_rule(&toggled) {
                            log::error!("cannot save the alert rule: {error}");
                        }
                        changed = true;
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if icons::button(ui, Icon::Cross, Style::Normal, "Delete").clicked() {
                            if let Err(error) = store.delete_rule(rule.id) {
                                log::error!("cannot delete the alert rule: {error}");
                            }
                            changed = true;
                        }
                        if icons::button(ui, Icon::Pencil, Style::Normal, "Edit").clicked() {
                            self.draft = Some(Draft::of(rule));
                        }
                        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                            let palette = theme::palette(ui);
                            let color = if rule.enabled { palette.ink } else { palette.ink_muted };
                            ui.label(RichText::new(rule.describe()).color(color));
                            if !rule.notify {
                                ui.weak("(journal and timeline only)");
                            }
                        });
                    });
                });
            }
            if rules.is_empty() {
                ui.weak("No rules. Add one to be told when a target slows down or drops packets.");
            }
        });
        ui.add_space(8.0);
        if ui.button("Add a rule").clicked() {
            self.draft = Some(Draft::new());
        }
        changed
    }
}

/// What the rule form's buttons asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FormAnswer {
    /// Nothing yet.
    None,
    /// Store the rule.
    Save,
    /// Drop the changes.
    Cancel,
}

/// The form of a rule: condition, its numbers, targets, notifications.
fn rule_form(ui: &mut Ui, draft: &mut Draft, hosts: &[String]) -> FormAnswer {
    ui.label(RichText::new(if draft.id == 0 { "New rule" } else { "Edit rule" }).font(theme::heading_font(15.0)));
    ui.add_space(6.0);
    settings_grid(ui, "alert-rule", |ui| {
        ui.weak("When");
        egui::ComboBox::from_id_salt("alert-kind").selected_text(draft.kind.label()).show_ui(ui, |ui| {
            for kind in ConditionKind::ALL {
                ui.selectable_value(&mut draft.kind, kind, kind.label());
            }
        });
        ui.end_row();
        match draft.kind {
            ConditionKind::Loss => {
                ui.weak("is above");
                ui.add(egui::DragValue::new(&mut draft.percent).range(1.0..=100.0).speed(0.5).suffix(" %"));
                ui.end_row();
                ui.weak("over the last");
                period_choice(ui, "alert-window", &mut draft.window, &WINDOWS);
                ui.end_row();
            }
            ConditionKind::Latency => {
                ui.weak("is above");
                ui.add(egui::DragValue::new(&mut draft.ms).range(10.0..=5_000.0).speed(1.0).suffix(" ms"));
                ui.end_row();
                ui.weak("over the last");
                period_choice(ui, "alert-window", &mut draft.window, &WINDOWS);
                ui.end_row();
            }
            ConditionKind::Unreachable => {
                ui.weak("for");
                period_choice(ui, "alert-after", &mut draft.after, &SILENCES);
                ui.end_row();
            }
            ConditionKind::RouteChanged => {}
        }
        let lines = u8::try_from(hosts.len().saturating_add(1)).unwrap_or(u8::MAX);
        label_first_row(ui, "Targets", lines, ui.spacing().interact_size.y);
        ui.vertical(|ui| {
            ui.checkbox(&mut draft.all_targets, "All targets");
            // The open targets, plus any the rule names that aren't open now.
            let mut choices: Vec<String> = hosts.to_vec();
            for host in &draft.hosts {
                if !choices.iter().any(|choice| choice.eq_ignore_ascii_case(host)) {
                    choices.push(host.clone());
                }
            }
            ui.add_enabled_ui(!draft.all_targets, |ui| {
                for host in choices {
                    let mut chosen = draft.hosts.iter().any(|listed| listed.eq_ignore_ascii_case(&host));
                    if ui.checkbox(&mut chosen, &host).changed() {
                        if chosen {
                            draft.hosts.push(host.clone());
                        } else {
                            draft.hosts.retain(|listed| !listed.eq_ignore_ascii_case(&host));
                        }
                    }
                }
            });
        });
        ui.end_row();
        let route = draft.kind == ConditionKind::RouteChanged;
        label_first_row(ui, "Tell me", if route { 1 } else { 2 }, ui.spacing().interact_size.y);
        ui.vertical(|ui| {
            ui.checkbox(&mut draft.notify, "With a Windows notification");
            if !route {
                ui.checkbox(&mut draft.notify_recovery, "Also when it's over");
            }
        });
        ui.end_row();
    });
    ui.add_space(4.0);
    ui.weak("Every alert is written to the journal and marked on the target's timeline.");
    ui.add_space(10.0);
    let can_save = draft.all_targets || !draft.hosts.is_empty();
    let mut answer = FormAnswer::None;
    ui.horizontal(|ui| {
        let palette = theme::palette(ui);
        let save = egui::Button::new(RichText::new("Save").color(palette.on_signal)).fill(palette.signal);
        if ui.add_enabled(can_save, save).on_disabled_hover_text("Choose at least one target").clicked() {
            answer = FormAnswer::Save;
        }
        if ui.button("Cancel").clicked() {
            answer = FormAnswer::Cancel;
        }
    });
    answer
}

/// Combo box choosing a period from `options`.
fn period_choice(ui: &mut Ui, id: &str, value: &mut Duration, options: &[Duration]) {
    egui::ComboBox::from_id_salt(id).selected_text(crate::alerts::span(*value)).show_ui(ui, |ui| {
        for option in options {
            ui.selectable_value(value, *option, crate::alerts::span(*option));
        }
    });
}

/// The journal page: newest first, with a button to clear it. Fixed columns that fit the window; a long "What" is
/// cut short, with the whole text on hover.
fn journal_page(ui: &mut Ui, store: &AlertStore) {
    let journal = store
        .journal()
        .inspect_err(|error| log::error!("cannot read the alert journal: {error}"))
        .unwrap_or_default();
    if journal.is_empty() {
        ui.weak("Nothing has fired yet.");
        return;
    }
    let palette = theme::palette(ui);
    ui.push_id("alert-journal", |ui| {
        TableBuilder::new(ui)
            .striped(true)
            .max_scroll_height(PAGE_HEIGHT - 76.0)
            .cell_layout(Layout::left_to_right(Align::Center))
            .column(Column::exact(118.0))
            .column(Column::exact(140.0).clip(true))
            .column(Column::remainder().clip(true))
            .column(Column::exact(74.0))
            .column(Column::exact(58.0))
            .header(JOURNAL_ROW, |mut header| {
                for title in ["Started", "Target", "What", "Lasted", "Worst"] {
                    header.col(|ui| {
                        ui.weak(title);
                    });
                }
            })
            .body(|body| {
                body.rows(JOURNAL_ROW, journal.len(), |mut row| {
                    let Some(entry) = journal.get(row.index()) else {
                        return;
                    };
                    row.col(|ui| {
                        ui.label(time_of(entry.started_ms));
                    });
                    row.col(|ui| {
                        ui.add(egui::Label::new(&entry.host).truncate());
                    });
                    row.col(|ui| {
                        ui.add(egui::Label::new(&entry.what).truncate());
                    });
                    row.col(|ui| match entry.ended_ms {
                        None => {
                            ui.colored_label(palette.bad, "on now");
                        }
                        Some(ended) if ended == entry.started_ms => {}
                        Some(ended) => {
                            let seconds = u64::try_from(ended.saturating_sub(entry.started_ms) / 1_000).unwrap_or(0);
                            ui.label(crate::alerts::span(Duration::from_secs(seconds.max(1))));
                        }
                    });
                    row.col(|ui| {
                        if let (Some(worst), Some(unit)) = (entry.worst, entry.unit.as_deref()) {
                            ui.label(format!("{worst:.0} {unit}"));
                        }
                    });
                });
            });
    });
    ui.add_space(8.0);
    if ui.button("Clear the journal").clicked()
        && let Err(error) = store.clear_journal()
    {
        log::error!("cannot clear the alert journal: {error}");
    }
}

/// Local "15.11 13:05:12" of Unix ms.
fn time_of(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|time| time.with_timezone(&chrono::Local).format("%d.%m %H:%M:%S").to_string())
        .unwrap_or_default()
}
