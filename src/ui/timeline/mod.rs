//! Pane 3: timeline of one hop over a chosen period (60 seconds … 7 days), read from the history in the
//! background — a jitter strip, the latency chart with packet loss and comments, a right-click menu (periods,
//! Reset Focus to Current, Create Comment) and a slider to scroll back in time.

mod fetch;
mod series;

use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

use eframe::egui::{self, Align, Align2, Button, Color32, Layout, Response, RichText, Slider, Stroke, TextEdit, Ui};
use egui_plot::{
    Bar, BarChart, GridInput, GridMark, HLine, Line, LineStyle, MarkerShape, Plot, PlotBounds, PlotPoint, PlotPoints,
    Points, Polygon, Text, VLine,
};

use self::fetch::{Fetched, Fetcher, Request};
pub use self::series::Period;
use self::series::{Series, Trace, Window, clock, millis_of, seconds, time_marks, value_marks};
use crate::store::history::{self, Comment, HopKey};
use crate::ui::latency_bars::{GOOD_MS, WARN_MS};
use crate::ui::theme::{self, BAD_ZONE, GOOD_ZONE, WARN_ZONE};

/// Longest time between history queries of a live timeline, however coarse the buckets are.
const MAX_REFRESH: Duration = Duration::from_secs(60);
/// Vertical space between the jitter strip and the latency chart.
const PLOT_GAP: f32 = 8.0;
/// Height of the jitter strip.
const JITTER_HEIGHT: f32 = 44.0;
/// Width of the y axis, equal for both plots so their time axes line up.
const Y_AXIS_WIDTH: f32 = 40.0;
/// Color of the latency line.
const LATENCY_COLOR: Color32 = theme::INK;
/// Color of the jitter line.
const JITTER_COLOR: Color32 = Color32::from_rgb(0xff, 0x8a, 0x65);
/// Color of the packet loss bars.
const LOSS_COLOR: Color32 = Color32::from_rgb(0xd6, 0x45, 0x5e);
/// Grid lines, drawn over everything (zones included) so they look the same on zones and on the bare plot: a faint
/// muted ink. (egui_plot's own grid lies under the zones and glares on the bare plot.)
const GRID: Color32 = Color32::from_rgba_premultiplied(0x15, 0x16, 0x19, 0x26);
/// Color of comment markers.
const COMMENT_COLOR: Color32 = Color32::from_rgb(0xff, 0xd5, 0x4f);

/// Chart data on screen and what it was built from.
#[derive(Debug)]
struct Shown {
    /// The request answered.
    request: Request,
    /// Chart series.
    series: Series,
    /// Comments in the window.
    comments: Vec<Comment>,
    /// Oldest stored sample of the hop.
    oldest_ms: Option<i64>,
    /// When the answer arrived.
    received: Instant,
}

impl Shown {
    /// Builds the chart data of a fetched answer.
    fn new(fetched: Fetched) -> Self {
        let series = Series::from_buckets(&fetched.buckets, fetched.request.window.bucket_ms);
        Self {
            request: fetched.request,
            series,
            comments: fetched.comments,
            oldest_ms: fetched.oldest_ms,
            received: Instant::now(),
        }
    }
}

/// A comment being written.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Draft {
    /// Target id in the history.
    target_id: i64,
    /// The moment commented, Unix ms.
    ts_ms: i64,
    /// Text so far.
    text: String,
}

/// A choice from the chart's right-click menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MenuChoice {
    /// Show another period.
    Period(Period),
    /// Follow the current time again.
    ResetFocus,
    /// Comment the moment right-clicked.
    Comment(i64),
}

/// State of the timeline pane.
#[derive(Debug, Default)]
pub struct Timeline {
    /// Period shown.
    period: Period,
    /// Right end of the window, Unix ms; `None` follows the current time.
    end_ms: Option<i64>,
    /// Background query thread, started on first use.
    fetcher: Option<Fetcher>,
    /// Why the thread couldn't start.
    fetcher_error: Option<String>,
    /// Data on screen.
    shown: Option<Shown>,
    /// A query is running.
    in_flight: bool,
    /// Query again on the next frame (after saving a comment).
    refresh_now: bool,
    /// Time under the pointer when the menu was opened, Unix ms.
    menu_at_ms: Option<i64>,
    /// Comment being written.
    draft: Option<Draft>,
    /// A small chart (All targets page): latency only, no jitter strip, no slider.
    compact: bool,
}

impl Timeline {
    /// A small chart for the All targets page: latency only, no jitter strip, no slider.
    pub fn compact() -> Self {
        Self { compact: true, ..Self::default() }
    }

    /// Period shown.
    pub fn period(&self) -> Period {
        self.period
    }

    /// Shows another period.
    pub fn set_period(&mut self, period: Period) {
        self.period = period;
    }

    /// Time range the chart shows, Unix ms.
    pub fn visible_range(&self, now_ms: i64) -> (i64, i64) {
        let end = self.end_ms.unwrap_or(now_ms);
        (end.saturating_sub(self.period.millis()), end)
    }

    /// Draws the pane for hop `hop` (described by `title`) of target `target_id`, from the history at
    /// `history_path`.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        history_path: Option<&Path>,
        target_id: Option<i64>,
        hop: HopKey,
        title: &str,
        interval: Duration,
    ) {
        let now_ms = history::unix_ms(SystemTime::now());
        self.header(ui, title, now_ms);
        let (Some(path), Some(target_id)) = (history_path, target_id) else {
            ui.label("History is off, so there is nothing to chart.");
            return;
        };
        let interval_ms = i64::try_from(interval.as_millis()).unwrap_or(i64::MAX);
        if let Some(error) = self.sync(path, target_id, hop, now_ms, interval_ms) {
            ui.colored_label(theme::BAD, error);
            return;
        }
        let Some(shown) = &self.shown else {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Loading history…");
            });
            return;
        };
        let (response, pointer) = draw_charts(ui, shown, self.period, self.compact);
        let oldest_ms = shown.oldest_ms;
        let bucket_ms = shown.request.window.bucket_ms;
        if response.secondary_clicked() {
            self.menu_at_ms = pointer.map(|point| millis_of(point.x));
        }
        if let Some(choice) = self.menu(&response) {
            self.choose(choice, target_id);
        }
        if !self.compact {
            self.slider(ui, oldest_ms, bucket_ms, now_ms);
        }
        self.comment_dialog(ui.ctx());
    }

    /// Title, period and the shown range.
    fn header(&mut self, ui: &mut Ui, title: &str, now_ms: i64) {
        ui.horizontal(|ui| {
            ui.label(RichText::new(title).font(theme::heading_font(15.0)));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                egui::ComboBox::from_id_salt("timeline-period")
                    .selected_text(self.period.label())
                    .show_ui(ui, |ui| {
                        for period in Period::ALL {
                            ui.selectable_value(&mut self.period, period, period.label());
                        }
                    });
                let (from, to) = self.visible_range(now_ms);
                ui.label(format!("({} - {})", clock(from, "%H:%M:%S"), clock(to, "%H:%M:%S")));
                if self.end_ms.is_some() && ui.small_button("Reset Focus to Current").clicked() {
                    self.end_ms = None;
                }
            });
        });
    }

    /// Takes arrived answers and queries again when the data on screen no longer fits. Returns an error to show
    /// instead of the chart.
    fn sync(&mut self, path: &Path, target_id: i64, hop: HopKey, now_ms: i64, interval_ms: i64) -> Option<String> {
        if self.fetcher.is_none() && self.fetcher_error.is_none() {
            match Fetcher::start(path) {
                Ok(fetcher) => self.fetcher = Some(fetcher),
                Err(error) => self.fetcher_error = Some(format!("cannot read history: {error}")),
            }
        }
        let Some(fetcher) = &self.fetcher else {
            return self.fetcher_error.clone();
        };
        while let Some(result) = fetcher.try_result() {
            self.in_flight = false;
            match result {
                Ok(fetched) => self.shown = Some(Shown::new(fetched)),
                Err(error) => log::error!("cannot read history: {error}"),
            }
        }
        let window = Window::ending_at(self.end_ms.unwrap_or(now_ms), self.period, interval_ms);
        let request = Request { target_id, hop, period: self.period, window };
        let stale = self.refresh_now
            || self.shown.as_ref().is_none_or(|shown| {
                let old = shown.request;
                let moved = old.target_id != target_id
                    || old.hop != hop
                    || old.period != self.period
                    || old.window.bucket_ms != window.bucket_ms;
                let live_due = self.end_ms.is_none() && {
                    let bucket = Duration::from_millis(u64::try_from(window.bucket_ms).unwrap_or(u64::MAX));
                    shown.received.elapsed() >= bucket.min(MAX_REFRESH)
                };
                let scrolled = self.end_ms.is_some() && old.window != window;
                moved || live_due || scrolled
            });
        if stale && !self.in_flight {
            fetcher.fetch(request);
            self.in_flight = true;
            self.refresh_now = false;
        }
        None
    }

    /// The chart's right-click menu; returns the choice made.
    fn menu(&self, response: &Response) -> Option<MenuChoice> {
        let mut choice = None;
        response.context_menu(|ui| {
            if let Some(at_ms) = self.menu_at_ms {
                ui.weak(clock(at_ms, "%H:%M:%S"));
                ui.separator();
            }
            for period in Period::ALL {
                if ui.selectable_label(self.period == period, period.label()).clicked() {
                    choice = Some(MenuChoice::Period(period));
                }
            }
            ui.separator();
            if ui.add_enabled(self.end_ms.is_some(), Button::new("Reset Focus to Current")).clicked() {
                choice = Some(MenuChoice::ResetFocus);
            }
            if let Some(at_ms) = self.menu_at_ms
                && ui.button("Create Comment").clicked()
            {
                choice = Some(MenuChoice::Comment(at_ms));
            }
            if choice.is_some() {
                ui.close();
            }
        });
        choice
    }

    /// Carries out a menu choice.
    fn choose(&mut self, choice: MenuChoice, target_id: i64) {
        match choice {
            MenuChoice::Period(period) => self.period = period,
            MenuChoice::ResetFocus => self.end_ms = None,
            MenuChoice::Comment(ts_ms) => self.draft = Some(Draft { target_id, ts_ms, text: String::new() }),
        }
    }

    /// Slider moving the window back in time, from the oldest sample to now; the right end follows the current
    /// time. Always shown (as in PingPlotter), disabled while the whole history fits into the window.
    fn slider(&mut self, ui: &mut Ui, oldest_ms: Option<i64>, bucket_ms: i64, now_ms: i64) {
        let earliest_end = oldest_ms.map_or(now_ms, |oldest| oldest.saturating_add(self.period.millis()).min(now_ms));
        let scrollable = earliest_end < now_ms || self.end_ms.is_some();
        // A disabled slider still needs a non-empty range to draw its handle at the right end.
        let range_start = if scrollable { earliest_end } else { now_ms.saturating_sub(1) };
        let mut end_s = seconds(self.end_ms.unwrap_or(now_ms));
        ui.spacing_mut().slider_width = ui.available_width();
        let slider = Slider::new(&mut end_s, seconds(range_start)..=seconds(now_ms)).show_value(false);
        let response = ui.add_enabled(scrollable, slider).on_disabled_hover_text("The whole history fits on the chart");
        if response.changed() {
            let end_ms = millis_of(end_s);
            // Dropped close to the right end: follow the current time again.
            self.end_ms = (now_ms.saturating_sub(end_ms) > bucket_ms.saturating_mul(2)).then_some(end_ms);
        }
    }

    /// Window for writing a comment; saves it in the background.
    fn comment_dialog(&mut self, ctx: &egui::Context) {
        let Some(draft) = &mut self.draft else {
            return;
        };
        let mut done = false;
        let mut save = false;
        egui::Window::new("Create Comment")
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(format!("Comment at {}", clock(draft.ts_ms, "%d.%m %H:%M:%S")));
                ui.add(TextEdit::multiline(&mut draft.text).desired_rows(3).hint_text("What happened?"));
                ui.horizontal(|ui| {
                    if ui.add_enabled(!draft.text.trim().is_empty(), Button::new("Save")).clicked() {
                        save = true;
                        done = true;
                    }
                    if ui.button("Cancel").clicked() {
                        done = true;
                    }
                });
            });
        if save && let (Some(fetcher), Some(draft)) = (&self.fetcher, &self.draft) {
            fetcher.add_comment(draft.target_id, draft.ts_ms, draft.text.trim().to_owned());
            self.refresh_now = true;
        }
        if done {
            self.draft = None;
        }
    }
}

/// Draws the jitter strip and the latency chart; returns the chart's response and the time/latency under the
/// pointer.
/// `compact` leaves out the jitter strip and the slider's row.
fn draw_charts(ui: &mut Ui, shown: &Shown, period: Period, compact: bool) -> (Response, Option<PlotPoint>) {
    let window = shown.request.window;
    let (from_s, to_s, bucket_s) = (seconds(window.from_ms), seconds(window.to_ms), seconds(window.bucket_ms));
    let utc_offset_s = i64::from(chrono::Local::now().offset().local_minus_utc());
    let base = |id: &'static str, scale: f64, height: f32| {
        Plot::new(id)
            .allow_drag(false)
            .allow_zoom(false)
            .allow_scroll(false)
            .allow_boxed_zoom(false)
            .allow_double_click_reset(false)
            // Exact bounds, no margins: 0 sits on the bottom edge (nothing below it to show) and the period
            // spans the full width.
            .default_x_bounds(from_s, to_s)
            .default_y_bounds(0.0, scale)
            .set_margin_fraction(egui::Vec2::ZERO)
            .y_axis_min_width(Y_AXIS_WIDTH)
            .x_grid_spacer(move |input: GridInput| {
                time_marks(input.bounds.0, input.bounds.1, utc_offset_s)
                    .into_iter()
                    .map(|(value, step_size)| GridMark { value, step_size })
                    .collect()
            })
            .height(height)
            .y_grid_spacer(move |input: GridInput| {
                value_marks(input.bounds.1, height)
                    .into_iter()
                    .map(|(value, step_size)| GridMark { value, step_size })
                    .collect()
            })
            .y_axis_formatter(|mark, _| format!("{:.0}", mark.value))
            .show_grid(false)
    };
    let grid = |plot: &mut egui_plot::PlotUi<'_>, scale: f64, height: f32| {
        for (value, _) in value_marks(scale, height).into_iter().skip(1) {
            plot.hline(HLine::new("", value).color(GRID).width(1.0).allow_hover(false));
        }
        for (value, _) in time_marks(from_s, to_s, utc_offset_s) {
            plot.vline(VLine::new("", value).color(GRID).width(1.0).allow_hover(false));
        }
    };

    let jitter = &shown.series.jitter;
    if !compact {
        base("timeline-jitter", jitter.scale_ms, JITTER_HEIGHT).show_x(false).show_axes([false, true]).show(
            ui,
            |plot| {
                plot.set_plot_bounds(PlotBounds::from_min_max([from_s, 0.0], [to_s, jitter.scale_ms]));
                grid(plot, jitter.scale_ms, JITTER_HEIGHT);
                plot.add(
                    Text::new("", PlotPoint::new(from_s, jitter.scale_ms), "Jitter (ms)")
                        .anchor(Align2::LEFT_TOP)
                        .color(JITTER_COLOR),
                );
                add_trace(plot, "Jitter", jitter, JITTER_COLOR);
            },
        );
        // Room between the plots, so the jitter strip's "0" and the latency scale's top label don't overlap.
        ui.add_space(PLOT_GAP);
    }
    let latency = &shown.series.latency;
    let scale = latency.scale_ms;
    // Leave exactly the slider's row: taking more than the panel has would grow the panel every frame.
    let slider_row = if compact { 0.0 } else { ui.spacing().interact_size.y + ui.spacing().item_spacing.y * 2.0 };
    let height = (ui.available_height() - slider_row).max(60.0);
    let response = base("timeline", scale, height)
        .x_axis_formatter(move |mark, _| clock(millis_of(mark.value), period.axis_format(mark.step_size)))
        .show(ui, |plot| {
            plot.set_plot_bounds(PlotBounds::from_min_max([from_s, 0.0], [to_s, scale]));
            // Zones are painted only where there is data, as in PingPlotter; time before the first sample stays plain.
            if let Some(data_from_s) = shown.series.first_s {
                for (low, high, color) in
                    [(0.0, GOOD_MS, GOOD_ZONE), (GOOD_MS, WARN_MS, WARN_ZONE), (WARN_MS, scale, BAD_ZONE)]
                {
                    if low < scale {
                        let top = high.min(scale);
                        let corners = vec![[data_from_s, low], [to_s, low], [to_s, top], [data_from_s, top]];
                        plot.polygon(
                            Polygon::new("", PlotPoints::from(corners)).fill_color(color).stroke(Stroke::NONE),
                        );
                    }
                }
            }
            // Grid lines on top of the zones, as in PingPlotter.
            grid(plot, scale, height);
            // Titled inside, like the jitter strip, so both charts share one y-axis width and line up.
            plot.add(
                Text::new("", PlotPoint::new(from_s, scale), "Latency (ms)")
                    .anchor(Align2::LEFT_TOP)
                    .color(theme::INK_MUTED),
            );
            add_trace(plot, "Latency", latency, LATENCY_COLOR);
            // Loss hangs from the top of the chart: a fully lost bucket is a red bar of full height.
            let bars = shown
                .series
                .loss
                .iter()
                .map(|&(x, percent)| {
                    Bar::new(x, -percent / 100.0 * scale).base_offset(scale).width(bucket_s).fill(LOSS_COLOR)
                })
                .collect();
            plot.bar_chart(BarChart::new("Packet loss", bars).color(LOSS_COLOR));
            for comment in &shown.comments {
                let x = seconds(comment.ts_ms);
                plot.vline(VLine::new(&comment.text, x).color(COMMENT_COLOR).style(LineStyle::dashed_dense()));
                plot.add(
                    Text::new("", PlotPoint::new(x, scale), &comment.text)
                        .anchor(Align2::LEFT_TOP)
                        .color(COMMENT_COLOR),
                );
            }
            plot.pointer_coordinate()
        });
    (response.response, response.inner)
}

/// Adds a trace's line segments and its clipped-spike markers to `plot`.
fn add_trace(plot: &mut egui_plot::PlotUi<'_>, name: &str, trace: &Trace, color: Color32) {
    for segment in &trace.segments {
        // Steps repeat x at every riser; without `allow_band(false)` egui_plot fills those as a min–max band.
        plot.line(Line::new(name, PlotPoints::from(segment.clone())).color(color).width(1.0).allow_band(false));
    }
    if !trace.clipped.is_empty() {
        let points = Points::new(format!("{name} above scale"), PlotPoints::from(trace.clipped.clone()));
        plot.points(points.shape(MarkerShape::Up).radius(4.0).filled(true).color(color));
    }
}
