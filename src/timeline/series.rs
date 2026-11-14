//! Pure chart computations of the timeline: periods, the queried window, chart series with a smoothed scale, and
//! time axis marks.

use crate::history::Bucket;
use crate::latency_bars::millis;

/// Roughly how many buckets a period is split into: about one per pixel column of the chart.
const TARGET_BUCKETS: i64 = 600;
/// Smallest bucket: the probe interval, so short periods show every sample.
const MIN_BUCKET_MS: i64 = 1_000;
/// Smallest latency scale of the chart.
const MIN_SCALE_MS: f64 = 50.0;
/// Smallest jitter scale of the jitter strip.
const MIN_JITTER_SCALE_MS: f64 = 10.0;
/// Scales are rounded up to a multiple of this.
const SCALE_STEP_MS: f64 = 5.0;
/// Head room above the smoothed peak: the usual line sits in the lower part of the chart and spikes up to twice
/// the usual level stay fully visible.
const SCALE_HEADROOM: f64 = 2.0;
/// Number of consecutive buckets averaged to find the scale: a spike narrower than this can't stretch the scale
/// by itself — it is drawn clipped at the top instead — while a sustained rise moves the scale with it.
const SMOOTHING_BUCKETS: usize = 15;
/// Candidate steps between time axis marks, in seconds.
const TIME_STEPS_S: [i64; 17] =
    [1, 5, 10, 15, 30, 60, 120, 300, 600, 900, 1_800, 3_600, 7_200, 10_800, 21_600, 43_200, 86_400];
/// At most this many time axis marks.
const MAX_TIME_MARKS: i64 = 8;
/// Candidate steps between value (latency) marks, in ms: round numbers only.
const VALUE_STEPS_MS: [f64; 12] = [1.0, 2.0, 5.0, 10.0, 20.0, 25.0, 50.0, 100.0, 200.0, 250.0, 500.0, 1_000.0];
/// At most this many value marks above zero.
const MAX_VALUE_MARKS: f64 = 7.0;
/// Least vertical space per value mark, in points: the plot hides labels of marks set closer than ~20 points.
const MIN_VALUE_MARK_SPACING: f32 = 26.0;

/// Period shown by the timeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Period {
    /// 60 seconds.
    Minute,
    /// 5 minutes.
    Minutes5,
    /// 10 minutes.
    #[default]
    Minutes10,
    /// 30 minutes.
    Minutes30,
    /// 60 minutes.
    Hour,
    /// 3 hours.
    Hours3,
    /// 6 hours.
    Hours6,
    /// 12 hours.
    Hours12,
    /// 24 hours.
    Day,
    /// 48 hours.
    Days2,
    /// 7 days, the whole history.
    Days7,
}

impl Period {
    /// All periods in menu order.
    pub const ALL: [Self; 11] = [
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
    pub fn seconds(self) -> u64 {
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

    /// Length of the period in milliseconds.
    pub fn millis(self) -> i64 {
        i64::try_from(self.seconds()).unwrap_or(i64::MAX).saturating_mul(1_000)
    }

    /// Menu label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Minute => "60 seconds",
            Self::Minutes5 => "5 minutes",
            Self::Minutes10 => "10 minutes",
            Self::Minutes30 => "30 minutes",
            Self::Hour => "60 minutes",
            Self::Hours3 => "3 hours",
            Self::Hours6 => "6 hours",
            Self::Hours12 => "12 hours",
            Self::Day => "24 hours",
            Self::Days2 => "48 hours",
            Self::Days7 => "7 days",
        }
    }

    /// `strftime` format of a time axis label for marks `step_s` apart: seconds only when the marks fall between
    /// whole minutes (no ever-repeating ":00"), the date when the period spans days.
    pub fn axis_format(self, step_s: f64) -> &'static str {
        let whole_minutes = step_s >= 60.0 && step_s.rem_euclid(60.0) == 0.0;
        match (self.seconds() > 86_400, whole_minutes) {
            (true, _) => "%d.%m %H:%M",
            (false, true) => "%H:%M",
            (false, false) => "%H:%M:%S",
        }
    }
}

/// Time range of a history query and its bucket size, all in Unix milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    /// Start, aligned to `bucket_ms` so buckets don't shift between refreshes.
    pub from_ms: i64,
    /// End (exclusive).
    pub to_ms: i64,
    /// Bucket size.
    pub bucket_ms: i64,
}

impl Window {
    /// Window of `period` ending at `end_ms`.
    pub fn ending_at(end_ms: i64, period: Period) -> Self {
        let period_ms = period.millis();
        let bucket_ms = period_ms.checked_div(TARGET_BUCKETS).unwrap_or(0).max(MIN_BUCKET_MS);
        let start = end_ms.saturating_sub(period_ms);
        let from_ms = start.saturating_sub(start.checked_rem_euclid(bucket_ms).unwrap_or(0));
        Self { from_ms, to_ms: end_ms.saturating_add(1), bucket_ms }
    }
}

/// One chart line: segments of `[time s, value ms]` broken at gaps, the points clipped at the scale, and the
/// scale itself.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Trace {
    /// Line segments, values clamped to `scale_ms`.
    pub segments: Vec<Vec<[f64; 2]>>,
    /// Points whose real value lies above the scale, placed at the top: the marked anomalies.
    pub clipped: Vec<[f64; 2]>,
    /// Top of the scale, ms.
    pub scale_ms: f64,
}

impl Trace {
    /// Builds a trace from `(x, value)` points in time order, `None` meaning no value in that bucket; `gap_before`
    /// tells whether a point doesn't directly follow the previous one.
    fn build(points: &[(f64, Option<f64>, bool)], min_scale_ms: f64) -> Self {
        let values: Vec<f64> = points.iter().filter_map(|(_, value, _)| *value).collect();
        let scale_ms = scale(&values, min_scale_ms);
        let mut trace = Self { scale_ms, ..Self::default() };
        let mut current: Vec<[f64; 2]> = Vec::new();
        for &(x, value, gap_before) in points {
            if gap_before && !current.is_empty() {
                trace.segments.push(std::mem::take(&mut current));
            }
            match value {
                Some(value) => {
                    current.push([x, value.min(scale_ms)]);
                    if value > scale_ms {
                        trace.clipped.push([x, scale_ms]);
                    }
                }
                None if !current.is_empty() => trace.segments.push(std::mem::take(&mut current)),
                None => {}
            }
        }
        if !current.is_empty() {
            trace.segments.push(current);
        }
        trace
    }
}

/// Chart data built from history buckets.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Series {
    /// Average latency.
    pub latency: Trace,
    /// Jitter, for the strip above the chart.
    pub jitter: Trace,
    /// Packet loss `(time s, loss %)` of buckets that lost something.
    pub loss: Vec<(f64, f64)>,
    /// Start of the first bucket with data, s; `None` if the window holds no data.
    pub first_s: Option<f64>,
}

impl Series {
    /// Builds the chart data from `buckets` of `bucket_ms` each.
    pub fn from_buckets(buckets: &[Bucket], bucket_ms: i64) -> Self {
        let mut latency = Vec::with_capacity(buckets.len());
        let mut jitter = Vec::with_capacity(buckets.len());
        let mut loss = Vec::new();
        let mut previous_start: Option<i64> = None;
        for bucket in buckets {
            let x = seconds(bucket.start_ms) + seconds(bucket_ms) / 2.0;
            let gap_before = previous_start.is_some_and(|start| start.saturating_add(bucket_ms) != bucket.start_ms);
            latency.push((x, bucket.avg.map(millis), gap_before));
            jitter.push((x, bucket.jitter.map(millis), gap_before));
            if bucket.sent > bucket.received {
                let lost = f64::from(bucket.sent.saturating_sub(bucket.received));
                loss.push((x, lost * 100.0 / f64::from(bucket.sent)));
            }
            previous_start = Some(bucket.start_ms);
        }
        Self {
            latency: Trace::build(&latency, MIN_SCALE_MS),
            jitter: Trace::build(&jitter, MIN_JITTER_SCALE_MS),
            loss,
            first_s: buckets.first().map(|bucket| seconds(bucket.start_ms)),
        }
    }
}

/// Highest mean of [`SMOOTHING_BUCKETS`] consecutive values (of all values if there are fewer); 0 if none.
#[expect(clippy::cast_precision_loss, reason = "a window holds at most a few dozen values")]
fn smoothed_peak(values: &[f64]) -> f64 {
    let width = SMOOTHING_BUCKETS.min(values.len()).max(1);
    values.windows(width).map(|window| window.iter().sum::<f64>() / width as f64).fold(0.0, f64::max)
}

/// Chart scale for `values`: the smoothed peak plus head room, rounded up, at least `min_scale_ms`.
fn scale(values: &[f64], min_scale_ms: f64) -> f64 {
    ((smoothed_peak(values) * SCALE_HEADROOM / SCALE_STEP_MS).ceil() * SCALE_STEP_MS).max(min_scale_ms)
}

/// Unix milliseconds as fractional seconds, the chart's x unit.
#[expect(clippy::cast_precision_loss, reason = "Unix milliseconds fit into f64's 52-bit mantissa until year 287396")]
pub fn seconds(ms: i64) -> f64 {
    ms as f64 / 1_000.0
}

/// Fractional seconds (a chart x value) as Unix milliseconds.
#[expect(clippy::cast_possible_truncation, reason = "chart x values are Unix seconds, far inside i64")]
pub fn millis_of(seconds: f64) -> i64 {
    (seconds * 1_000.0).round() as i64
}

/// Time axis marks in `[min_s, max_s]`: at most [`MAX_TIME_MARKS`] on a round step, aligned to local time
/// (`utc_offset_s` east of UTC). Returns `(value, step)` pairs.
#[expect(clippy::cast_possible_truncation, clippy::cast_precision_loss, reason = "axis values are Unix seconds")]
pub fn time_marks(min_s: f64, max_s: f64, utc_offset_s: i64) -> Vec<(f64, f64)> {
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

/// Value axis marks from 0 to `max` for a plot `height` points tall, on the roundest step that keeps the marks at
/// most [`MAX_VALUE_MARKS`] and at least [`MIN_VALUE_MARK_SPACING`] apart (20 ms rather than 10 ms for a 140 ms
/// scale). Returns `(value, step)` pairs.
pub fn value_marks(max: f64, height: f32) -> Vec<(f64, f64)> {
    let fitting = f64::from((height / MIN_VALUE_MARK_SPACING).floor()).clamp(1.0, MAX_VALUE_MARKS);
    let step = VALUE_STEPS_MS.into_iter().find(|step| max / step <= fitting).unwrap_or(1_000.0);
    (0_u32..)
        .map(|index| f64::from(index) * step)
        .take_while(|value| *value <= max + f64::EPSILON)
        .map(|value| (value, step))
        .collect()
}

/// Local time of Unix milliseconds in `strftime` `format`.
pub fn clock(ms: i64, format: &str) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|time| time.with_timezone(&chrono::Local).format(format).to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// Bucket at `start_ms` with `sent`/`received` probes, an average of `avg_ms` and no jitter.
    fn bucket(start_ms: i64, sent: u32, received: u32, avg_ms: Option<u64>) -> Bucket {
        let avg = avg_ms.map(Duration::from_millis);
        Bucket { start_ms, sent, received, min: avg, avg, max: avg, jitter: None }
    }

    /// One-second buckets with the given averages, all answered.
    fn steady(averages: &[u64]) -> Vec<Bucket> {
        averages
            .iter()
            .zip(0_i64..)
            .map(|(&avg, index)| bucket(index.saturating_mul(1_000), 1, 1, Some(avg)))
            .collect()
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
        assert_eq!(series.latency.segments, vec![vec![[0.5, 10.0], [1.5, 20.0]], vec![[3.5, 30.0]], vec![[5.5, 40.0]]]);
        assert_eq!(series.loss, vec![(2.5, 100.0)]);
    }

    #[test]
    fn a_lone_spike_is_clipped_instead_of_stretching_the_scale() {
        let mut averages = vec![50; 100];
        if let Some(spike) = averages.get_mut(60) {
            *spike = 600;
        }
        let series = Series::from_buckets(&steady(&averages), 1_000);
        let scale = series.latency.scale_ms;
        // Smoothed peak (14 × 50 + 600) / 15 ≈ 86.7, × 2 head room → 175.
        assert_eq!(scale, 175.0);
        assert_eq!(series.latency.clipped, vec![[60.5, scale]]);
        assert!(series.latency.segments.iter().flatten().all(|point| point[1] <= scale));
    }

    #[test]
    fn a_sustained_rise_moves_the_scale() {
        let series = Series::from_buckets(&steady(&[200; 30]), 1_000);
        assert_eq!(series.latency.scale_ms, 400.0);
        assert!(series.latency.clipped.is_empty());
    }

    #[test]
    fn first_data_time_is_known() {
        assert_eq!(Series::from_buckets(&steady(&[50, 60]), 1_000).first_s, Some(0.0));
        assert_eq!(Series::from_buckets(&[], 1_000).first_s, None);
    }

    #[test]
    fn scale_has_a_minimum() {
        assert_eq!(Series::from_buckets(&steady(&[5; 10]), 1_000).latency.scale_ms, MIN_SCALE_MS);
        assert_eq!(Series::from_buckets(&[], 1_000).latency.scale_ms, MIN_SCALE_MS);
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

    #[test]
    fn value_marks_use_round_steps() {
        let values = |max, height| value_marks(max, height).into_iter().map(|(value, _)| value).collect::<Vec<_>>();
        assert_eq!(values(140.0, 300.0), [0.0, 20.0, 40.0, 60.0, 80.0, 100.0, 120.0, 140.0]);
        assert_eq!(values(50.0, 300.0), [0.0, 10.0, 20.0, 30.0, 40.0, 50.0]);
        assert_eq!(values(400.0, 300.0).get(1), Some(&100.0));
        // A short plot gets fewer, wider-spaced marks so their labels still show.
        assert_eq!(values(140.0, 80.0), [0.0, 50.0, 100.0]);
    }

    #[test]
    fn axis_labels_skip_seconds_on_whole_minutes() {
        assert_eq!(Period::Minutes10.axis_format(120.0), "%H:%M");
        assert_eq!(Period::Minute.axis_format(15.0), "%H:%M:%S");
        assert_eq!(Period::Days7.axis_format(86_400.0), "%d.%m %H:%M");
    }

    #[test]
    fn seconds_and_millis_round_trip() {
        assert_eq!(millis_of(seconds(1_794_681_852_797)), 1_794_681_852_797);
    }
}
