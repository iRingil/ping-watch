//! The overview page ("All targets"): a toolbar (target field, interval, focus), one table row per target with its final-hop
//! numbers and latency bar, and below it the charts of the targets ticked in the Status column, all over the same
//! period. Its interval, focus and chart period are its own, remembered apart from the target tabs.

use std::path::Path;
use std::time::{Duration, SystemTime};

use eframe::egui::{self, Align, Frame, Layout, Margin, RichText, ScrollArea, Sense, Ui, vec2};
use egui_extras::TableBuilder;

use crate::i18n::{Lang, Language, n, tr, trf, trn};
use crate::probe::tracer::Probe;
use crate::snapshot::{HopRow, Snapshot, SummaryRow};
use crate::store::history;
use crate::store::history::Store;
use crate::store::targets::{TargetList, TraceSettings};
use crate::store::window_state::{AllTargetsSettings, WindowStore};
use crate::ui::columns::{self, ColumnWidths};
use crate::ui::focus::Focus;
use crate::ui::hop_table::{self, format_ms};
use crate::ui::icons::{self, Icon, Style};
use crate::ui::latency_bars;
use crate::ui::tab::{self, Overview, Tab};
use crate::ui::target_entry::TargetEntry;
use crate::ui::theme;
use crate::ui::timeline::Period;

/// Height of a table row.
const ROW_HEIGHT: f32 = 24.0;
/// Width of the Status column (checkbox and state).
const STATUS_WIDTH: f32 = 128.0;
/// Widest the Target column gets.
const TARGET_WIDTH: f32 = 208.0;
/// Widest the IP column gets (room for most IPv6 addresses).
const IP_WIDTH: f32 = 238.0;
/// Width of a numeric column.
const NUMBER_WIDTH: f32 = 70.0;
/// Column titles and whether the column is numeric (right-aligned): all but the latency bars, which come last.
const HEADERS: [(&str, bool); 10] = [
    (n!("Status"), false),
    (n!("Target"), false),
    (n!("IP"), false),
    (n!("Hops"), true),
    (n!("Avg"), true),
    (n!("Min"), true),
    (n!("Max"), true),
    (n!("Cur"), true),
    (n!("PL%"), true),
    (n!("PL pcs"), true),
];
/// Number of columns that can be dragged: all but the latency bars.
const RESIZABLE_COLUMNS: usize = HEADERS.len();
/// Columns that can't be hidden: Status (its checkbox shows the chart; it also paints the header band) and
/// Target (the link to its tab).
const FIXED_COLUMNS: [usize; 2] = [0, 1];
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

/// State of the overview page.
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
    /// Widths of the table's columns.
    columns: ColumnWidths,
}

impl AllTargets {
    /// The page with the settings and column widths stored last time in `store`, or the defaults.
    pub fn new(store: Option<&WindowStore>) -> Self {
        let saved = store
            .and_then(|store| store.load_all_targets().inspect_err(|error| log::warn!("{error}")).ok())
            .flatten();
        let defaults = AllTargetsSettings {
            trace: TraceSettings {
                interval: Duration::from_secs(1),
                focus: Focus::default(),
                period: Period::default(),
                probe: Probe::Icmp,
            },
            table_share: TABLE_SHARE,
        };
        Self {
            entry: TargetEntry::new(false),
            settings: saved.unwrap_or(defaults),
            saved,
            selected: None,
            columns: ColumnWidths::load("all-targets", RESIZABLE_COLUMNS, store),
        }
    }

    /// Host of the selected row, if any.
    pub fn selected(&self) -> Option<&str> {
        self.selected.as_deref()
    }

    /// The page as a snapshot: the table over the page's focus and the ticked charts over its period.
    pub fn snapshot(&self, tabs: &[Tab], reader: Option<&Store>, alerting: &[String], language: Language) -> Snapshot {
        let now_ms = history::unix_ms(SystemTime::now());
        let visible = (now_ms.saturating_sub(self.settings.trace.period.millis()), now_ms);
        let focus = match self.settings.trace.focus.range(visible, now_ms) {
            Some((from, to)) => format!("{} – {}", tab::clock(from), tab::clock(to)),
            None => tr!(language, "since the traces started").to_owned(),
        };
        let rows = tabs
            .iter()
            .map(|tab| {
                let overview = tab.overview_with(self.settings.trace.focus, visible);
                let alert = alerting.iter().any(|host| host.eq_ignore_ascii_case(&overview.title));
                let numbers = overview.target_hop.as_ref().map(|hop| HopRow {
                    ttl: hop.ttl,
                    address: hop.address.map(|ip| ip.to_string()),
                    name: None,
                    sent: hop.stats.sent(),
                    avg: hop.stats.average().map(latency_bars::millis),
                    min: hop.stats.best().map(latency_bars::millis),
                    max: hop.stats.worst().map(latency_bars::millis),
                    cur: hop.stats.last().map(latency_bars::millis),
                    jitter: hop.stats.jitter().map(latency_bars::millis),
                    loss: hop.stats.loss_percent(),
                    lost: hop.stats.lost(),
                });
                SummaryRow {
                    title: overview.title.clone(),
                    address: overview.target.map(|ip| ip.to_string()).unwrap_or_default(),
                    status: if alert { tr!(language, "alert").to_owned() } else { overview.status.clone() },
                    alert,
                    hops: overview.hops,
                    numbers,
                }
            })
            .collect();
        let charts = tabs
            .iter()
            .filter(|tab| tab.is_charted())
            .filter_map(|tab| Some((tab.label()?, tab.chart_over(reader, self.settings.trace.period, now_ms)?)))
            .collect();
        Snapshot::AllTargets { focus, rows, charts }
    }

    /// Interval for targets added on this page.
    pub fn interval(&self) -> Duration {
        self.settings.trace.interval
    }

    /// Stores the settings and column widths in `store` whenever they changed (the widths once `dragging`, the
    /// mouse button held, ends).
    pub fn save(&mut self, mut store: Option<&mut WindowStore>, dragging: bool) {
        self.columns.save(store.as_deref_mut(), dragging);
        if self.saved == Some(self.settings) {
            return;
        }
        if let Some(store) = store
            && let Err(error) = store.save_all_targets(self.settings)
        {
            log::warn!("cannot save the overview page settings: {error}");
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
        alerting: &[String],
    ) -> Option<Request> {
        let toolbar_frame = Frame::new().fill(theme::palette(ui).chassis).inner_margin(Margin::symmetric(10, 8));
        let mut request = egui::Panel::top("all-targets-toolbar")
            .frame(toolbar_frame)
            .show(ui, |ui| self.toolbar(ui, tabs, targets))
            .inner;

        let area = ui.available_rect_before_wrap();
        ui.allocate_rect(area, Sense::hover());
        let now_ms = history::unix_ms(SystemTime::now());
        let visible = (now_ms.saturating_sub(self.settings.trace.period.millis()), now_ms);
        // With charts shown the table shares the area with them, above a splitter.
        let (table_rect, charts_rect) = if tabs.iter().any(Tab::is_charted) {
            let (table_rect, splitter_rect, charts_rect) = tab::split(area, self.settings.table_share);
            tab::splitter(ui, splitter_rect, area, &mut self.settings.table_share);
            (table_rect, Some(charts_rect))
        } else {
            (area, None)
        };
        tab::pane(ui, table_rect, Margin::same(1), |ui| {
            let table = columns::scope(ui, "all-targets-table", |ui| self.table(ui, tabs, visible, now_ms, alerting));
            request = request.take().or(table);
        });
        if let Some(charts_rect) = charts_rect {
            tab::pane(ui, charts_rect, Margin::same(8), |ui| self.charts(ui, tabs, history_path));
        }
        request
    }

    /// Toolbar: target field with the Trace button, interval (applied to every trace when changed), focus and the
    /// latency zone legend. Returns a target to add.
    fn toolbar(&mut self, ui: &mut Ui, tabs: &mut [Tab], targets: Option<&TargetList>) -> Option<Request> {
        let mut request = None;
        ui.horizontal(|ui| {
            let mut start = self.entry.show(ui, targets, true, None);
            start |= icons::button(ui, Icon::Play, Style::Primary, tr!(ui, "Trace in a new tab (Enter)")).clicked();
            let host = self.entry.text().trim().to_owned();
            if start && !host.is_empty() {
                self.entry.clear();
                request = Some(Request::Add(host));
            }
            ui.add_space(12.0);
            // Shows what the traces really use ("different" when they differ); a choice sets it on all of them and
            // on targets added here.
            ui.weak(tr!(ui, "Interval of all"));
            let intervals: Vec<Duration> =
                tabs.iter().filter(|tab| !tab.host().is_empty()).map(Tab::interval).collect();
            if let Some(interval) = tab::interval_for_all(ui, &intervals, self.settings.trace.interval) {
                self.settings.trace.interval = interval;
                for tab in tabs.iter_mut() {
                    tab.set_interval(interval);
                }
            }
            ui.weak(tr!(ui, "Focus"));
            tab::focus_choice(ui, &mut self.settings.trace.focus);
            ui.with_layout(Layout::right_to_left(Align::Center), latency_bars::legend);
        });
        request
    }

    /// The table: one row per target. A click selects a row, a double click shows or hides its chart, the checkbox
    /// does the same, and the target's link opens its tab. Returns the tab to open.
    fn table(
        &mut self,
        ui: &mut Ui,
        tabs: &mut [Tab],
        visible: (i64, i64),
        now_ms: i64,
        alerting: &[String],
    ) -> Option<Request> {
        let focus = self.settings.trace.focus;
        let overviews: Vec<Overview> = tabs.iter().map(|tab| tab.overview_with(focus, visible)).collect();
        let scale_ms = latency_bars::scale_ms(
            overviews.iter().filter_map(|overview| overview.target_hop.as_ref()).map(|hop| &hop.stats),
            latency_bars::Zones::of(ui.ctx()),
        );
        let row_width = ui.available_width();
        // Default widths: in a narrow window Target and IP give up room (clipped), so the numbers and the bars stay
        // in view.
        let shown = self.columns.shown().to_vec();
        let numbers: f32 = (3..RESIZABLE_COLUMNS)
            .filter(|position| self.columns.is_shown(*position))
            .map(|_| NUMBER_WIDTH)
            .sum();
        let ip = if self.columns.is_shown(2) { IP_WIDTH } else { 0.0 };
        let fixed = STATUS_WIDTH + numbers + LATENCY_MIN_WIDTH;
        let squeeze = ((row_width - fixed) / (TARGET_WIDTH + ip)).clamp(0.3, 1.0);
        let mut defaults = vec![STATUS_WIDTH, TARGET_WIDTH * squeeze, IP_WIDTH * squeeze];
        defaults.extend([NUMBER_WIDTH; RESIZABLE_COLUMNS - 3]);
        let mut request = None;
        let mut toggle = None;
        let mut header_widths = Vec::new();
        let builder = TableBuilder::new(ui)
            .striped(true)
            .sense(Sense::click())
            .cell_layout(Layout::left_to_right(Align::Center));
        self.columns
            .columns(builder, defaults, row_width, LATENCY_MIN_WIDTH)
            .header(ROW_HEIGHT, |mut header| {
                let titles = HEADERS.map(|(title, _)| title);
                for (index, (title, numeric)) in HEADERS.into_iter().enumerate() {
                    let cell = columns::cell(&mut header, &shown, index, |ui| {
                        header_widths.push(ui.max_rect().width());
                        let title = ui.language().translate(title);
                        hop_table::header_cell(ui, index == 0, numeric, title, row_width);
                    });
                    if let Some(response) = cell {
                        self.columns.menu(&response, &titles, &FIXED_COLUMNS);
                    }
                }
                let (_, latency) = header.col(|ui| latency_bars::header(ui, scale_ms));
                self.columns.menu(&latency, &titles, &FIXED_COLUMNS);
            })
            .body(|mut body| {
                for (index, overview) in overviews.iter().enumerate() {
                    let charted = tabs.get(index).is_some_and(Tab::is_charted);
                    body.row(ROW_HEIGHT, |mut row| {
                        row.set_selected(self.selected.as_deref() == Some(overview.title.as_str()));
                        columns::cell(&mut row, &shown, 0, |ui| {
                            ui.add_space(4.0);
                            let mut ticked = charted;
                            if ui.checkbox(&mut ticked, "").on_hover_text(tr!(ui, "Show the chart below")).changed() {
                                toggle = Some(index);
                            }
                            ui.add_space(4.0);
                            if alerting.iter().any(|host| host.eq_ignore_ascii_case(&overview.title)) {
                                ui.label(RichText::new(tr!(ui, "alert")).color(theme::palette(ui).bad));
                            } else {
                                ui.label(RichText::new(&overview.status).color(theme::palette(ui).ink_muted));
                            }
                        });
                        columns::cell(&mut row, &shown, 1, |ui| {
                            ui.add_space(4.0);
                            let link = egui::Link::new(
                                RichText::new(&overview.title)
                                    .font(theme::heading_font(15.0))
                                    .color(theme::palette(ui).signal),
                            );
                            if ui.add(link).on_hover_text(tr!(ui, "Open its tab")).clicked() {
                                request = Some(Request::Open(index));
                            }
                        });
                        columns::cell(&mut row, &shown, 2, |ui| {
                            hop_table::cell(ui, false, overview.target.map(|ip| ip.to_string()).unwrap_or_default());
                        });
                        columns::cell(&mut row, &shown, 3, |ui| {
                            hop_table::cell(ui, true, overview.hops.map(|hops| hops.to_string()).unwrap_or_default());
                        });
                        let stats = overview.target_hop.as_ref().map(|hop| &hop.stats);
                        for (position, value) in (4..).zip([
                            stats.and_then(|stats| stats.average()),
                            stats.and_then(|stats| stats.best()),
                            stats.and_then(|stats| stats.worst()),
                            stats.and_then(|stats| stats.last()),
                        ]) {
                            columns::cell(&mut row, &shown, position, |ui| {
                                hop_table::cell(ui, true, format_ms(value, ui.language()));
                            });
                        }
                        columns::cell(&mut row, &shown, 8, |ui| {
                            let loss = stats.map(|stats| ui.language().decimal(stats.loss_percent()));
                            hop_table::cell(ui, true, loss.unwrap_or_default());
                        });
                        columns::cell(&mut row, &shown, 9, |ui| {
                            hop_table::cell(ui, true, stats.map(|stats| stats.lost().to_string()).unwrap_or_default());
                        });
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
        self.columns.take_header(&header_widths);
        if let Some(tab) = toggle.and_then(|index| tabs.get_mut(index)) {
            tab.set_charted(!tab.is_charted());
        }
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            if tabs.is_empty() {
                ui.weak(tr!(ui, "No targets yet. Enter one above."));
            } else {
                let count = tabs.len();
                ui.weak(trn!(ui, u64::try_from(count).unwrap_or(u64::MAX), "{n} target|{n} targets"));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(8.0);
                let text = match focus.range(visible, now_ms) {
                    Some((from, to)) => {
                        let range = format!("{} – {}", tab::clock(from), tab::clock(to));
                        trf!(ui, "Focus {range}", range = range)
                    }
                    None => tr!(ui, "Focus: all").to_owned(),
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
