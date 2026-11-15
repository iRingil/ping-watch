//! The All targets page: a toolbar (target field, interval, focus), one table row per target with its final-hop
//! numbers and latency bar, and below it the charts of the targets ticked in the Status column, all over the same
//! period. Its interval, focus and chart period are its own, remembered apart from the target tabs.

use std::path::Path;
use std::time::{Duration, SystemTime};

use eframe::egui::{self, Align, Frame, Layout, Margin, RichText, ScrollArea, Sense, Ui, vec2};
use egui_extras::{Column, TableBuilder};

use crate::focus::Focus;
use crate::history;
use crate::hop_table::{self, format_ms};
use crate::icons::{self, Icon, Style};
use crate::latency_bars;
use crate::tab::{self, Overview, Tab};
use crate::target_entry::TargetEntry;
use crate::targets::{TargetList, TraceSettings};
use crate::theme;
use crate::timeline::Period;
use crate::window_state::{AllTargetsSettings, WindowStore};

/// Height of a table row.
const ROW_HEIGHT: f32 = 24.0;
/// Width of the Status column (check box and state).
const STATUS_WIDTH: f32 = 120.0;
/// Width of the Target column.
const TARGET_WIDTH: f32 = 200.0;
/// Width of the IP column (room for most IPv6 addresses).
const IP_WIDTH: f32 = 230.0;
/// Width of a numeric column.
const NUMBER_WIDTH: f32 = 62.0;
/// Number of numeric columns (Hops, Avg, Min, Max, Cur, PL%).
const NUMBER_COLUMNS: usize = 6;
/// Narrowest the latency column gets.
const LATENCY_MIN_WIDTH: f32 = 160.0;
/// Least height of a chart: the charts share the pane equally, and below this the pane scrolls instead.
const CHART_MIN_HEIGHT: f32 = 130.0;
/// Share of the pane area the table gets when charts are first shown.
const TABLE_SHARE: f32 = 0.4;

/// What the page asks the app to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// Show target tab `index`.
    Open(usize),
    /// Open a tab tracing this host (the page stays shown).
    Add(String),
}

/// State of the All targets page.
#[derive(Debug)]
pub struct AllTargets {
    /// Target field of the toolbar.
    entry: TargetEntry,
    /// Current settings.
    settings: AllTargetsSettings,
    /// Settings as last stored.
    saved: Option<AllTargetsSettings>,
    /// Host of the selected row.
    selected: Option<String>,
}

impl AllTargets {
    /// The page with the settings stored last time, or the defaults.
    pub fn new(saved: Option<AllTargetsSettings>) -> Self {
        let defaults = AllTargetsSettings {
            trace: TraceSettings {
                interval: Duration::from_secs(1),
                focus: Focus::default(),
                period: Period::default(),
            },
            table_share: TABLE_SHARE,
        };
        Self { entry: TargetEntry::new(false), settings: saved.unwrap_or(defaults), saved, selected: None }
    }

    /// Host of the selected row, if any.
    pub fn selected(&self) -> Option<&str> {
        self.selected.as_deref()
    }

    /// Interval for targets added on this page.
    pub fn interval(&self) -> Duration {
        self.settings.trace.interval
    }

    /// Stores the settings in `store` whenever they changed.
    pub fn save(&mut self, store: Option<&WindowStore>) {
        if self.saved == Some(self.settings) {
            return;
        }
        if let Some(store) = store
            && let Err(error) = store.save_all_targets(self.settings)
        {
            log::warn!("cannot save the All targets settings: {error}");
        }
        // One attempt per change, not one per frame.
        self.saved = Some(self.settings);
    }

    /// Draws the page for `tabs`; returns what the user asked the app to do.
    pub fn ui(
        &mut self,
        ui: &mut Ui,
        tabs: &mut [Tab],
        targets: Option<&TargetList>,
        history_path: Option<&Path>,
    ) -> Option<Request> {
        let toolbar_frame = Frame::new().fill(theme::CHASSIS).inner_margin(Margin::symmetric(10, 8));
        let mut request = egui::Panel::top("all-targets-toolbar")
            .frame(toolbar_frame)
            .show(ui, |ui| self.toolbar(ui, tabs, targets))
            .inner;

        let area = ui.available_rect_before_wrap();
        ui.allocate_rect(area, Sense::hover());
        let now_ms = history::unix_ms(SystemTime::now());
        let visible = (now_ms.saturating_sub(self.settings.trace.period.millis()), now_ms);
        if tabs.iter().any(Tab::is_charted) {
            let (table_rect, splitter_rect, charts_rect) = tab::split(area, self.settings.table_share);
            tab::splitter(ui, splitter_rect, area, &mut self.settings.table_share);
            tab::pane(ui, table_rect, Margin::same(1), |ui| {
                request = request.take().or(self.table(ui, tabs, visible, now_ms));
            });
            tab::pane(ui, charts_rect, Margin::same(8), |ui| self.charts(ui, tabs, history_path));
        } else {
            tab::pane(ui, area, Margin::same(1), |ui| {
                request = request.take().or(self.table(ui, tabs, visible, now_ms));
            });
        }
        request
    }

    /// Toolbar: target field with the Trace button, interval (applied to every trace when changed), focus and the
    /// latency zone legend. Returns a target to add.
    fn toolbar(&mut self, ui: &mut Ui, tabs: &mut [Tab], targets: Option<&TargetList>) -> Option<Request> {
        let mut request = None;
        ui.horizontal(|ui| {
            let mut start = self.entry.show(ui, targets, true);
            start |= icons::button(ui, Icon::Play, Style::Primary, "Trace in a new tab (Enter)").clicked();
            let host = self.entry.text().trim().to_owned();
            if start && !host.is_empty() {
                self.entry.clear();
                request = Some(Request::Add(host));
            }
            ui.add_space(12.0);
            ui.weak("Interval");
            if tab::interval_choice(ui, &mut self.settings.trace.interval) {
                for tab in tabs.iter_mut() {
                    tab.set_interval(self.settings.trace.interval);
                }
            }
            ui.weak("Focus");
            tab::focus_choice(ui, &mut self.settings.trace.focus);
            ui.with_layout(Layout::right_to_left(Align::Center), latency_bars::legend);
        });
        request
    }

    /// The table: one row per target. A click selects a row, a double click shows or hides its chart, the check box
    /// does the same, and the target's link opens its tab. Returns the tab to open.
    fn table(&mut self, ui: &mut Ui, tabs: &mut [Tab], visible: (i64, i64), now_ms: i64) -> Option<Request> {
        let focus = self.settings.trace.focus;
        let overviews: Vec<Overview> = tabs.iter().map(|tab| tab.overview_with(focus, visible)).collect();
        let scale_ms = latency_bars::scale_ms(
            overviews.iter().filter_map(|overview| overview.target_hop.as_ref()).map(|hop| &hop.stats),
        );
        let row_width = ui.available_width();
        let mut request = None;
        let mut toggle = None;
        TableBuilder::new(ui)
            .striped(true)
            .sense(Sense::click())
            .cell_layout(Layout::left_to_right(Align::Center))
            .column(Column::exact(STATUS_WIDTH).clip(true))
            .column(Column::exact(TARGET_WIDTH).clip(true))
            .column(Column::exact(IP_WIDTH).clip(true))
            .columns(Column::exact(NUMBER_WIDTH), NUMBER_COLUMNS)
            .column(Column::remainder().at_least(LATENCY_MIN_WIDTH).clip(true))
            .header(ROW_HEIGHT, |mut header| {
                // Title and whether the column is numeric (right-aligned).
                let columns = [
                    ("Status", false),
                    ("Target", false),
                    ("IP", false),
                    ("Hops", true),
                    ("Avg", true),
                    ("Min", true),
                    ("Max", true),
                    ("Cur", true),
                    ("PL%", true),
                ];
                for (index, (title, numeric)) in columns.into_iter().enumerate() {
                    header.col(|ui| hop_table::header_cell(ui, index == 0, numeric, title, row_width));
                }
                header.col(|ui| latency_bars::header(ui, scale_ms));
            })
            .body(|mut body| {
                for (index, overview) in overviews.iter().enumerate() {
                    let charted = tabs.get(index).is_some_and(Tab::is_charted);
                    body.row(ROW_HEIGHT, |mut row| {
                        row.set_selected(self.selected.as_deref() == Some(overview.title.as_str()));
                        row.col(|ui| {
                            ui.add_space(4.0);
                            let mut ticked = charted;
                            if ui.checkbox(&mut ticked, "").on_hover_text("Show the chart below").changed() {
                                toggle = Some(index);
                            }
                            ui.label(RichText::new(&overview.status).color(theme::INK_MUTED));
                        });
                        row.col(|ui| {
                            ui.add_space(4.0);
                            let link = egui::Link::new(
                                RichText::new(&overview.title).font(theme::heading_font(15.0)).color(theme::SIGNAL),
                            );
                            if ui.add(link).on_hover_text("Open its tab").clicked() {
                                request = Some(Request::Open(index));
                            }
                        });
                        row.col(|ui| {
                            hop_table::cell(ui, false, overview.target.map(|ip| ip.to_string()).unwrap_or_default());
                        });
                        row.col(|ui| {
                            hop_table::cell(ui, true, overview.hops.map(|hops| hops.to_string()).unwrap_or_default());
                        });
                        let stats = overview.target_hop.as_ref().map(|hop| &hop.stats);
                        for value in [
                            stats.and_then(|stats| stats.average()),
                            stats.and_then(|stats| stats.best()),
                            stats.and_then(|stats| stats.worst()),
                            stats.and_then(|stats| stats.last()),
                        ] {
                            row.col(|ui| hop_table::cell(ui, true, format_ms(value)));
                        }
                        let loss = stats.map(|stats| format!("{:.1}", stats.loss_percent())).unwrap_or_default();
                        row.col(|ui| hop_table::cell(ui, true, loss));
                        row.col(|ui| {
                            if let Some(stats) = stats {
                                latency_bars::cell(ui, stats, overview.reached, scale_ms);
                            }
                        });
                        let response = row.response();
                        if response.double_clicked() {
                            toggle = Some(index);
                        } else if response.clicked() {
                            // A click on the selected row clears the selection.
                            let title = Some(overview.title.clone());
                            self.selected = if self.selected == title { None } else { title };
                        }
                    });
                }
            });
        if let Some(tab) = toggle.and_then(|index| tabs.get_mut(index)) {
            tab.set_charted(!tab.is_charted());
        }
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            if tabs.is_empty() {
                ui.weak("No targets yet. Enter one above.");
            } else {
                let count = tabs.len();
                ui.weak(if count == 1 { "1 target".to_owned() } else { format!("{count} targets") });
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(8.0);
                let text = match focus.range(visible, now_ms) {
                    Some((from, to)) => format!("Focus {} – {}", tab::clock(from), tab::clock(to)),
                    None => "Focus: all".to_owned(),
                };
                ui.weak(text);
            });
        });
        request
    }

    /// The charts of the ticked targets, one under another in table order, all over the page's period. They share
    /// the pane's height equally; once that would make them shorter than [`CHART_MIN_HEIGHT`], the pane scrolls.
    fn charts(&mut self, ui: &mut Ui, tabs: &mut [Tab], history_path: Option<&Path>) {
        let count = tabs.iter().filter(|tab| tab.is_charted()).count();
        let gap = ui.spacing().item_spacing.y;
        #[expect(clippy::cast_precision_loss, reason = "a handful of charts")]
        let height = {
            let count = count.max(1) as f32;
            ((ui.available_height() - gap * (count - 1.0)) / count).max(CHART_MIN_HEIGHT)
        };
        ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            for (index, tab) in tabs.iter_mut().enumerate().filter(|(_, tab)| tab.is_charted()) {
                let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
                let mut child = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(rect)
                        .layout(Layout::top_down(Align::Min))
                        .id_salt(("chart", index)),
                );
                child.set_clip_rect(rect.intersect(ui.clip_rect()));
                tab.chart(&mut child, history_path, &mut self.settings.trace.period);
            }
        });
    }
}
