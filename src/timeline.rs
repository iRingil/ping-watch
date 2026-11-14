//! Pane 3: timeline of one hop's latency and packet loss over a chosen period (1 minute … 7 days), read from the
//! history database.

use std::time::{Duration, Instant, SystemTime};

use eframe::egui::{self, Align, Color32, Layout, Ui};
use egui_plot::{Axis, Bar, BarChart, GridInput, GridMark, Line, Plot, PlotPoints, Span};

use crate::history::{self, Bucket, Store};
use crate::latency_bars::{BAD_ZONE, GOOD_MS, GOOD_ZONE, WARN_MS, WARN_ZONE, millis};

/// Roughly how many buckets a period is split into: about one per pixel column of the chart.
const TARGET_BUCKETS: i64 = 600;
/// Smallest bucket: the probe interval, so short periods show every sample.
const MIN_BUCKET_MS: i64 = 1_000;
/// Smallest latency scale of the chart.
const MIN_SCALE_MS: f64 = 50.0;
/// Latency scale is rounded up to a multiple of this.
const SCALE_STEP_MS: f64 = 25.0;
/// Head room above the highest average.
const SCALE_HEADROOM: f64 = 1.25;
/// Longest time between history queries, however coarse the buckets are.
const MAX_REFRESH: Duration = Duration::from_secs(60);
/// Candidate steps between time axis marks, in seconds.
const TIME_STEPS_S: [i64; 17] =
    [1, 5, 10, 15, 30, 60, 120, 300, 600, 900, 1_800, 3_600, 7_200, 10_800, 21_600, 43_200, 86_400];
/// At most this many time axis marks.
const MAX_TIME_MARKS: i64 = 8;
/// Color of the latency line.
const LATENCY_COLOR: Color32 = Color32::from_gray(0xf0);
/// Color of the packet loss bars.
const LOSS_COLOR: Color32 = Color32::from_rgb(0xe5, 0x39, 0x35);

/// Period shown by the timeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Period {
    /// Last minute.
    Minute,
    /// Last 5 minutes.
    Minutes5,
    /// Last 10 minutes.
    #[default]
    Minutes10,
    /// Last 30 minutes.
    Minutes30,
    /// Last hour.
    Hour,
    /// Last 3 hours.
    Hours3,
    /// Last 6 hours.
    Hours6,
    /// Last 12 hours.
    Hours12,
    /// Last 24 hours.
    Day,
    /// Last 48 hours.
    Days2,
    /// Last 7 days, the whole history.
    Days7,
}

impl Period {
    /// All periods in menu order.
    const ALL: [Self; 11] = [
        Self::Minute,
        Self::Minutes5,
        Self::Minutes10,
        Self::Minutes30,
        Self::Hour,
        Self::Hours3,
        Self::Hours6,
        Self::Hours12,
        Self::Day,
        Self::Days2,
        Self::Days7,
    ];

    /// Length of the period in seconds.
    fn seconds(self) -> u64 {
        match self {
            Self::Minute => 60,
            Self::Minutes5 => 5 * 60,
            Self::Minutes10 => 10 * 60,
            Self::Minutes30 => 30 * 60,
            Self::Hour => 60 * 60,
            Self::Hours3 => 3 * 60 * 60,
            Self::Hours6 => 6 * 60 * 60,
            Self::Hours12 => 12 * 60 * 60,
            Self::Day => 24 * 60 * 60,
            Self::Days2 => 2 * 24 * 60 * 60,
            Self::Days7 => 7 * 24 * 60 * 60,
        }
    }

    /// Menu label.
    fn label(self) -> &'static str {
        match self {
            Self::Minute => "1 minute",
            Self::Minutes5 => "5 minutes",
            Self::Minutes10 => "10 minutes",
            Self::Minutes30 => "30 minutes",
            Self::Hour => "1 hour",
            Self::Hours3 => "3 hours",
            Self::Hours6 => "6 hours",
            Self::Hours12 => "12 hours",
            Self::Day => "24 hours",
            Self::Days2 => "48 hours",
            Self::Days7 => "7 days",
        }
    }

    /// `strftime` format of time axis labels: seconds for short periods, the date for long ones.
    fn clock_format(self) -> &'static str {
        match self.seconds() {
            ..=600 => "%H:%M:%S",
            601..=86_400 => "%H:%M",
            _ => "%d.%m %H:%M",
        }
    }
}

/// Time range of a history query and its bucket size, all in Unix milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Window {
    /// Start, aligned to `bucket_ms` so buckets don't shift between refreshes.
    from_ms: i64,
    /// End (exclusive).
    to_ms: i64,
    /// Bucket size.
    bucket_ms: i64,
}

impl Window {
    /// Window of `period` ending at `now_ms`.
    fn ending_at(now_ms: i64, period: Period) -> Self {
        let period_ms = i64::try_from(period.seconds()).unwrap_or(i64::MAX).saturating_mul(1_000);
        let bucket_ms = period_ms.checked_div(TARGET_BUCKETS).unwrap_or(0).max(MIN_BUCKET_MS);
        let start = now_ms.saturating_sub(period_ms);
        let from_ms = start.saturating_sub(start.checked_rem_euclid(bucket_ms).unwrap_or(0));
        Self { from_ms, to_ms: now_ms.saturating_add(1), bucket_ms }
    }
}

/// Chart data built from history buckets.
#[derive(Debug, Clone, Default, PartialEq)]
struct Series {
    /// Average latency lines `[time s, latency ms]`, broken where no probe was answered.
    segments: Vec<Vec<[f64; 2]>>,
    /// Packet loss `(time s, loss %)` of buckets that lost something.
    loss: Vec<(f64, f64)>,
    /// Top of the latency scale, ms.
    scale_ms: f64,
}

/// Unix milliseconds as fractional seconds, the chart's x unit.
#[expect(clippy::cast_precision_loss, reason = "Unix milliseconds fit into f64's 52-bit mantissa until year 287396")]
fn seconds(ms: i64) -> f64 {
    ms as f64 / 1_000.0
}

impl Series {
    /// Builds the chart data from `buckets` of `bucket_ms` each.
    fn from_buckets(buckets: &[Bucket], bucket_ms: i64) -> Self {
        let mut series = Self::default();
        let mut current: Vec<[f64; 2]> = Vec::new();
        let mut previous_start: Option<i64> = None;
        let mut highest_ms = 0.0_f64;
        for bucket in buckets {
            let x = seconds(bucket.start_ms) + seconds(bucket_ms) / 2.0;
            let follows = previous_start.is_some_and(|start| start.saturating_add(bucket_ms) == bucket.start_ms);
            if !follows && !current.is_empty() {
                series.segments.push(std::mem::take(&mut current));
            }
            match bucket.avg.map(millis) {
                Some(avg_ms) => {
                    current.push([x, avg_ms]);
                    highest_ms = highest_ms.max(avg_ms);
                }
                None if !current.is_empty() => series.segments.push(std::mem::take(&mut current)),
                None => {}
            }
            if bucket.sent > bucket.received {
                let lost = f64::from(bucket.sent.saturating_sub(bucket.received));
                series.loss.push((x, lost * 100.0 / f64::from(bucket.sent)));
            }
            previous_start = Some(bucket.start_ms);
        }
        if !current.is_empty() {
            series.segments.push(current);
        }
        series.scale_ms = ((highest_ms * SCALE_HEADROOM / SCALE_STEP_MS).ceil() * SCALE_STEP_MS).max(MIN_SCALE_MS);
        series
    }
}

/// Time axis marks in `[min_s, max_s]`: at most [`MAX_TIME_MARKS`] on a round step, aligned to local time
/// (`utc_offset_s` east of UTC). Returns `(value, step)` pairs.
#[expect(clippy::cast_possible_truncation, clippy::cast_precision_loss, reason = "axis values are Unix seconds")]
fn time_marks(min_s: f64, max_s: f64, utc_offset_s: i64) -> Vec<(f64, f64)> {
    let (min, max) = (min_s.floor() as i64, max_s.ceil() as i64);
    let span = max.saturating_sub(min);
    let step = TIME_STEPS_S
        .into_iter()
        .find(|step| span.checked_div(*step).unwrap_or(0) < MAX_TIME_MARKS)
        .unwrap_or(86_400);
    let local_min = min.saturating_add(utc_offset_s);
    // Round `local_min` up to a multiple of `step`, then shift back to UTC.
    let past_mark = local_min.checked_rem_euclid(step).unwrap_or(0);
    let to_next = if past_mark == 0 { 0 } else { step.saturating_sub(past_mark) };
    let first = local_min.saturating_add(to_next).saturating_sub(utc_offset_s);
    (0..)
        .map_while(|index: i64| {
            let value = first.saturating_add(index.saturating_mul(step));
            (value <= max).then_some((value as f64, step as f64))
        })
        .collect()
}

/// Local time label of an axis value in Unix seconds.
#[expect(clippy::cast_possible_truncation, reason = "axis values are Unix seconds")]
fn clock_label(value_s: f64, format: &str) -> String {
    chrono::DateTime::from_timestamp(value_s.round() as i64, 0)
        .map(|time| time.with_timezone(&chrono::Local).format(format).to_string())
        .unwrap_or_default()
}

/// Last query result, reused until it gets stale.
#[derive(Debug)]
struct Cached {
    /// What was queried: target id, hop and period.
    key: (i64, u8, Period),
    /// When it was queried.
    fetched: Instant,
    /// Queried time range.
    window: Window,
    /// Chart data.
    series: Series,
}

/// State of the timeline pane.
#[derive(Debug, Default)]
pub struct Timeline {
    /// Chosen period.
    period: Period,
    /// Last query result.
    cached: Option<Cached>,
}

impl Timeline {
    /// Length of the period the timeline shows.
    pub fn period_length(&self) -> Duration {
        Duration::from_secs(self.period.seconds())
    }

    /// Draws the pane for hop `ttl` (described by `title`) of target `target_id`, from `store`.
    pub fn show(&mut self, ui: &mut Ui, store: Option<&Store>, target_id: Option<i64>, ttl: u8, title: &str) {
        ui.horizontal(|ui| {
            ui.strong(title);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                egui::ComboBox::from_id_salt("timeline-period")
                    .selected_text(self.period.label())
                    .show_ui(ui, |ui| {
                        for period in Period::ALL {
                            ui.selectable_value(&mut self.period, period, period.label());
                        }
                    });
            });
        });
        let (Some(store), Some(target_id)) = (store, target_id) else {
            ui.label("History is off, so there is nothing to chart.");
            return;
        };
        let period = self.period;
        let Some(cached) = self.refresh(store, (target_id, ttl, period)) else {
            ui.label("Cannot read the history (see the log).");
            return;
        };
        draw_chart(ui, cached, period);
    }

    /// Returns fresh chart data for `key`, querying the history only when the cached data is stale.
    fn refresh(&mut self, store: &Store, key: (i64, u8, Period)) -> Option<&Cached> {
        let stale = self.cached.as_ref().is_none_or(|cached| {
            let bucket = Duration::from_millis(u64::try_from(cached.window.bucket_ms).unwrap_or(u64::MAX));
            cached.key != key || cached.fetched.elapsed() >= bucket.min(MAX_REFRESH)
        });
        if stale {
            let (target_id, ttl, period) = key;
            let window = Window::ending_at(history::unix_ms(SystemTime::now()), period);
            match store.buckets(target_id, ttl, window.from_ms, window.to_ms, window.bucket_ms) {
                Ok(buckets) => {
                    let series = Series::from_buckets(&buckets, window.bucket_ms);
                    self.cached = Some(Cached { key, fetched: Instant::now(), window, series });
                }
                Err(error) => {
                    log::error!("cannot read history: {error}");
                    self.cached = None;
                }
            }
        }
        self.cached.as_ref()
    }
}

/// Draws the latency/loss chart of `cached` data.
fn draw_chart(ui: &mut Ui, cached: &Cached, period: Period) {
    let Cached { window, series, .. } = cached;
    let (from_s, to_s, bucket_s) = (seconds(window.from_ms), seconds(window.to_ms), seconds(window.bucket_ms));
    let scale = series.scale_ms;
    let utc_offset_s = i64::from(chrono::Local::now().offset().local_minus_utc());
    let format = period.clock_format();
    Plot::new("timeline")
        .allow_drag(false)
        .allow_zoom(false)
        .allow_scroll(false)
        .allow_boxed_zoom(false)
        .allow_double_click_reset(false)
        .include_x(from_s)
        .include_x(to_s)
        .include_y(0.0)
        .include_y(scale)
        .y_axis_label("Latency (ms)")
        .x_grid_spacer(move |input: GridInput| {
            time_marks(input.bounds.0, input.bounds.1, utc_offset_s)
                .into_iter()
                .map(|(value, step_size)| GridMark { value, step_size })
                .collect()
        })
        .x_axis_formatter(move |mark, _| clock_label(mark.value, format))
        .y_axis_formatter(|mark, _| format!("{:.0}", mark.value))
        .show(ui, |plot| {
            for (low, high, color) in
                [(0.0, GOOD_MS, GOOD_ZONE), (GOOD_MS, WARN_MS, WARN_ZONE), (WARN_MS, scale, BAD_ZONE)]
            {
                if low < scale {
                    plot.add(Span::new("", low..=high.min(scale)).axis(Axis::Y).fill(color).border_width(0.0));
                }
            }
            for segment in &series.segments {
                plot.line(Line::new("Latency", PlotPoints::from(segment.clone())).color(LATENCY_COLOR).width(1.0));
            }
            // Loss hangs from the top of the chart: a fully lost bucket is a red bar of full height.
            let bars = series
                .loss
                .iter()
                .map(|&(x, percent)| {
                    Bar::new(x, -percent / 100.0 * scale).base_offset(scale).width(bucket_s).fill(LOSS_COLOR)
                })
                .collect();
            plot.bar_chart(BarChart::new("Packet loss", bars).color(LOSS_COLOR));
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bucket at `start_ms` with `sent`/`received` probes and an average of `avg_ms`.
    fn bucket(start_ms: i64, sent: u32, received: u32, avg_ms: Option<u64>) -> Bucket {
        let avg = avg_ms.map(Duration::from_millis);
        Bucket { start_ms, sent, received, min: avg, avg, max: avg }
    }

    #[test]
    fn short_periods_use_one_second_buckets() {
        let window = Window::ending_at(1_000_500, Period::Minute);
        assert_eq!(window.bucket_ms, 1_000);
        assert_eq!(window.from_ms, 940_000);
        assert_eq!(window.to_ms, 1_000_501);
    }

    #[test]
    fn long_periods_use_coarser_aligned_buckets() {
        let window = Window::ending_at(1_000_000_000, Period::Days7);
        assert_eq!(window.bucket_ms, 7 * 24 * 3_600 * 1_000 / 600);
        assert_eq!(window.from_ms.rem_euclid(window.bucket_ms), 0);
        assert!(window.from_ms <= 1_000_000_000 - 7 * 24 * 3_600 * 1_000);
    }

    #[test]
    fn series_breaks_on_gaps_and_unanswered_buckets() {
        let buckets = [
            bucket(0, 1, 1, Some(10)),
            bucket(1_000, 1, 1, Some(20)),
            bucket(2_000, 1, 0, None), // lost
            bucket(3_000, 1, 1, Some(30)),
            bucket(5_000, 1, 1, Some(40)), // gap before
        ];
        let series = Series::from_buckets(&buckets, 1_000);
        assert_eq!(series.segments, vec![vec![[0.5, 10.0], [1.5, 20.0]], vec![[3.5, 30.0]], vec![[5.5, 40.0]]]);
        assert_eq!(series.loss, vec![(2.5, 100.0)]);
    }

    #[test]
    fn series_scale_leaves_headroom() {
        let series = Series::from_buckets(&[bucket(0, 4, 3, Some(90))], 1_000);
        assert_eq!(series.scale_ms, 125.0);
        assert_eq!(series.loss, vec![(0.5, 25.0)]);
        assert_eq!(Series::from_buckets(&[], 1_000).scale_ms, MIN_SCALE_MS);
    }

    #[test]
    fn time_marks_are_round_and_local() {
        // 10 minutes starting at 12:00:30 UTC → marks every 2 minutes from 12:02:00.
        let start = 1_700_000_000 - 1_700_000_000 % 3_600 + 30;
        let marks = time_marks(seconds(start * 1_000), seconds((start + 600) * 1_000), 0);
        assert_eq!(marks.first().map(|mark| mark.1), Some(120.0));
        assert!(marks.iter().all(|(value, _)| value.rem_euclid(120.0) == 0.0));
        assert_eq!(marks.len(), 5);
        // With a +3:30 offset, hourly marks fall on local hours, i.e. at :30 UTC.
        let marks = time_marks(0.0, 6.0 * 3_600.0, 3 * 3_600 + 1_800);
        assert!(marks.iter().all(|(value, _)| value.rem_euclid(3_600.0) == 1_800.0));
    }
}
