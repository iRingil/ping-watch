//! Snapshot: the shown target (or All targets) as one self-contained HTML page — the hop table with latency bars,
//! the latency and loss chart of the period as inline SVG, and the alerts of that period. Always in the light
//! palette, laid out for printing too. Pure: the window gathers the data, this module only writes the page.

use std::fmt::Write as _;

use eframe::egui::Color32;

use crate::i18n::{Language, n, tr, trf};
use crate::ui::theme::LIGHT;
use crate::ui::timeline::{Period, Series, Trace, clock, seconds, time_marks, value_marks};
use crate::{APP_NAME, SITE_URL};

/// Latency below which a value is fine (green zone), ms.
const GOOD_MS: f64 = 100.0;
/// Latency below which a value is acceptable (amber zone), ms; above it is red.
const WARN_MS: f64 = 200.0;
/// Size of a latency bar in the table.
const BAR: (f64, f64) = (220.0, 14.0);
/// Size of a chart, and its left and bottom margins for the axis labels.
const CHART: (f64, f64, f64, f64) = (1000.0, 160.0, 44.0, 20.0);
/// Room a time label needs from the chart's right edge to stay centred on its mark.
const EDGE_LABEL_ROOM: f64 = 32.0;
/// Height of a chart's jitter strip.
const JITTER_HEIGHT: f64 = 26.0;
/// Space between the jitter strip and the latency plot.
const PLOT_GAP: f64 = 6.0;

/// One hop of the table.
#[derive(Debug, Clone, PartialEq)]
pub struct HopRow {
    /// Hop number.
    pub ttl: u8,
    /// Address that answered, `None` for no answer.
    pub address: Option<String>,
    /// Reverse name, if it differs from the address.
    pub name: Option<String>,
    /// Probes sent over the focus period.
    pub sent: u64,
    /// Average, best, worst and current latency, ms.
    pub avg: Option<f64>,
    /// Best latency, ms.
    pub min: Option<f64>,
    /// Worst latency, ms.
    pub max: Option<f64>,
    /// Latest probe, ms; `None` if it was lost.
    pub cur: Option<f64>,
    /// Jitter, ms.
    pub jitter: Option<f64>,
    /// Packet loss, percent.
    pub loss: f64,
}

/// A latency and loss chart: the same series the window's timeline draws.
#[derive(Debug, Clone, PartialEq)]
pub struct Chart {
    /// Period shown, Unix ms.
    pub from_ms: i64,
    /// End of the period, Unix ms.
    pub to_ms: i64,
    /// Length of a bucket, ms.
    pub bucket_ms: i64,
    /// The period's length, for the time labels' format.
    pub period: Period,
    /// Latency, jitter and loss.
    pub series: Series,
    /// Comments and alert marks: time and text.
    pub marks: Vec<(i64, String)>,
}

/// An alert of the period.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlertRow {
    /// When it started, as shown.
    pub started: String,
    /// What happened.
    pub what: String,
    /// How long it lasted, as shown (empty for a moment).
    pub lasted: String,
    /// The worst value, as shown.
    pub worst: String,
}

/// A target tab.
#[derive(Debug, Clone, PartialEq)]
pub struct TargetReport {
    /// The target's label, "208.67.222.222 / OpenDNS".
    pub label: String,
    /// Period of the table, as shown ("12:43:25 – 12:53:25").
    pub focus: String,
    /// The hops.
    pub hops: Vec<HopRow>,
    /// The target's chart over the timeline's period.
    pub chart: Option<Chart>,
    /// Alerts of the chart's period.
    pub alerts: Vec<AlertRow>,
}

/// A row of All targets.
#[derive(Debug, Clone, PartialEq)]
pub struct SummaryRow {
    /// Host as entered.
    pub title: String,
    /// Traced address.
    pub address: String,
    /// State: running, paused, alert…
    pub status: String,
    /// An alert is on for the target (the status is drawn in red).
    pub alert: bool,
    /// Hops to the target.
    pub hops: Option<usize>,
    /// The target hop's numbers (avg, min, max, cur, jitter unused, loss).
    pub numbers: Option<HopRow>,
}

/// What a snapshot shows.
#[derive(Debug, Clone, PartialEq)]
pub enum Snapshot {
    /// One target.
    Target(Box<TargetReport>),
    /// All targets: the table and the charts that are ticked.
    AllTargets {
        /// Period of the table, as shown.
        focus: String,
        /// The rows.
        rows: Vec<SummaryRow>,
        /// Charts with their target's label.
        charts: Vec<(String, Chart)>,
    },
}

impl Snapshot {
    /// The suggested file name: "Ping Watch 2026-11-15 15-30 OpenDNS.html" (`stamp` is the date and time part).
    pub fn file_name(&self, stamp: &str, language: Language) -> String {
        let what = match self {
            Self::Target(report) => report.label.rsplit(" / ").next().unwrap_or(&report.label).to_owned(),
            Self::AllTargets { .. } => tr!(language, "All targets").to_owned(),
        };
        let safe: String = what.chars().map(|c| if "\\/:*?\"<>|".contains(c) { '-' } else { c }).collect();
        format!("{APP_NAME} {stamp} {safe}.html")
    }
}

/// The page of `snapshot`, taken at `taken` (local time as shown), in `language`.
pub fn html(snapshot: &Snapshot, taken: &str, version: &str, language: Language) -> String {
    let title = match snapshot {
        Snapshot::Target(report) => report.label.clone(),
        Snapshot::AllTargets { .. } => tr!(language, "All targets").to_owned(),
    };
    let mut page = String::new();
    write_head(&mut page, &title, language);
    // The first section's heading on the left, the title and when the snapshot was taken on the right, in one row.
    // The app and version link to the website; the rest of the line is the translated text, escaped.
    let app = format!("<a href=\"{SITE_URL}\">{APP_NAME} {}</a>", escape(version));
    let meta = escape(tr!(language, "{app} · snapshot of {taken}"));
    let meta = crate::i18n::fill(&meta, &[("app", app), ("taken", escape(taken))]);
    let header = format!("<header><h1>{}</h1><p class=\"meta\">{meta}</p></header>", escape(&title));
    match snapshot {
        Snapshot::Target(report) => write_target(&mut page, &header, report, language),
        Snapshot::AllTargets { focus, rows, charts } => write_all(&mut page, &header, focus, rows, charts, language),
    }
    page.push_str("</body></html>\n");
    page
}

/// The document head with the light palette's styles.
fn write_head(page: &mut String, title: &str, language: Language) {
    let p = &LIGHT;
    let _ = write!(
        page,
        "<!doctype html><html lang=\"{lang}\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
<title>{title} · {APP_NAME}</title><style>\
:root{{color-scheme:light}}\
body{{margin:24px;background:{bg};color:{ink};font:14px/1.45 \"Segoe UI\",system-ui,sans-serif}}\
h1{{margin:0;font-size:22px;font-weight:600}}h2{{margin:28px 0 8px;font-size:16px;font-weight:600}}h3{{margin:16px 0 4px;font-size:14px;font-weight:600}}\
.meta,.muted{{color:{muted}}}.meta{{margin:2px 0 0}}.meta a{{color:{link};text-decoration:none}}.meta a:hover{{text-decoration:underline}}\
.top{{display:flex;justify-content:space-between;align-items:flex-end;gap:24px;margin-bottom:8px}}\
.top h2{{margin:0}}.top p{{margin:2px 0 0}}header{{text-align:right}}\
table{{border-collapse:collapse;width:100%;background:{pane};border:1px solid {rule}}}\
th{{background:{header};color:{muted};font-weight:500;text-align:left;padding:5px 8px;border-bottom:1px solid {rule}}}\
td{{padding:4px 8px;border-top:1px solid {stripe};white-space:nowrap}}\
.num{{text-align:right;font-variant-numeric:tabular-nums}}.bad{{color:{bad}}}.lost td{{color:{muted}}}\
svg{{display:block}}.chart{{background:{pane};border:1px solid {rule};margin-top:6px}}\
@media print{{body{{margin:0}}table,.chart{{break-inside:avoid}}}}\
</style></head><body>",
        title = escape(title),
        lang = language.key(),
        bg = hex(p.chassis),
        link = hex(p.signal),
        ink = hex(p.ink),
        muted = hex(p.ink_muted),
        pane = hex(p.pane),
        rule = hex(p.rule),
        header = hex(p.pane_header),
        stripe = hex(p.stripe),
        bad = hex(p.bad),
    );
}

/// The page of a target: table, chart, alerts.
fn write_target(page: &mut String, header: &str, report: &TargetReport, language: Language) {
    let _ = write!(
        page,
        "<div class=\"top\"><div><h2>{}</h2><p class=\"muted\">{}</p></div>{header}</div>",
        tr!(language, "Route"),
        escape(&trf!(language, "Focus {range}", range = report.focus))
    );
    let scale = bar_scale(report.hops.iter().filter_map(|hop| hop.avg));
    let _ = write!(
        page,
        "<table><tr><th class=\"num\">{}</th><th class=\"num\">{}</th><th>IP</th><th>{}</th>",
        tr!(language, "Hop"),
        tr!(language, "Count"),
        tr!(language, "Name")
    );
    write_number_titles(page, scale, language);
    for hop in &report.hops {
        let lost = hop.address.is_none();
        let _ = write!(
            page,
            "<tr{}><td class=\"num\">{}</td><td class=\"num\">{}</td><td>{}</td><td>{}</td>",
            if lost { " class=\"lost\"" } else { "" },
            hop.ttl,
            hop.sent,
            escape(hop.address.as_deref().unwrap_or("*")),
            escape(hop.name.as_deref().unwrap_or(if lost { tr!(language, "No response") } else { "" })),
        );
        write_numbers(page, hop);
        let _ = write!(page, "<td>{}</td></tr>", latency_bar(hop, scale));
    }
    page.push_str("</table>");
    if let Some(chart) = &report.chart {
        let _ =
            write!(page, "<h3>{}</h3>{}", tr!(language, "Latency and loss of the target"), chart_svg(chart, language));
    }
    let _ = write!(page, "<h2>{}</h2>", tr!(language, "Alerts in this period"));
    if report.alerts.is_empty() {
        let _ = write!(page, "<p class=\"muted\">{}</p>", tr!(language, "None."));
    } else {
        let _ = write!(
            page,
            "<table><tr><th>{}</th><th>{}</th><th>{}</th><th>{}</th></tr>",
            tr!(language, "Started"),
            tr!(language, "What"),
            tr!(language, "Lasted"),
            tr!(language, "Worst")
        );
        for alert in &report.alerts {
            let _ = write!(
                page,
                "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
                escape(&alert.started),
                escape(&alert.what),
                escape(&alert.lasted),
                escape(&alert.worst)
            );
        }
        page.push_str("</table>");
    }
}

/// The page of All targets: table and the ticked charts.
fn write_all(
    page: &mut String,
    header: &str,
    focus: &str,
    rows: &[SummaryRow],
    charts: &[(String, Chart)],
    language: Language,
) {
    let _ = write!(
        page,
        "<div class=\"top\"><div><h2>{}</h2><p class=\"muted\">{}</p></div>{header}</div>",
        tr!(language, "Targets"),
        escape(&trf!(language, "Focus {range}", range = focus))
    );
    let scale = bar_scale(rows.iter().filter_map(|row| row.numbers.as_ref().and_then(|hop| hop.avg)));
    let _ = write!(
        page,
        "<table><tr><th>{}</th><th>{}</th><th>IP</th><th class=\"num\">{}</th>",
        tr!(language, "Status"),
        tr!(language, "Target"),
        tr!(language, "Hops")
    );
    write_number_titles(page, scale, language);
    for row in rows {
        let status_class = if row.alert { " class=\"bad\"" } else { "" };
        let _ = write!(
            page,
            "<tr><td{status_class}>{}</td><td>{}</td><td>{}</td><td class=\"num\">{}</td>",
            escape(&row.status),
            escape(&row.title),
            escape(&row.address),
            row.hops.map(|hops| hops.to_string()).unwrap_or_default()
        );
        match &row.numbers {
            Some(hop) => {
                write_numbers(page, hop);
                let _ = write!(page, "<td>{}</td>", latency_bar(hop, scale));
            }
            None => page.push_str(&"<td></td>".repeat(7)),
        }
        page.push_str("</tr>");
    }
    page.push_str("</table>");
    for (label, chart) in charts {
        let _ = write!(page, "<h2>{}</h2>{}", escape(label), chart_svg(chart, language));
    }
}

/// The titles of the number columns and the latency bar column (its scale `0–scale ms`), closing the header row.
fn write_number_titles(page: &mut String, scale: f64, language: Language) {
    for title in [n!("Avg"), n!("Min"), n!("Max"), n!("Cur"), n!("Jitter"), n!("PL%")] {
        let _ = write!(page, "<th class=\"num\">{}</th>", language.translate(title));
    }
    let latency = trf!(language, "Latency (0\u{2013}{scale} ms)", scale = format!("{scale:.0}"));
    let _ = write!(page, "<th>{}</th></tr>", escape(&latency));
}

/// The Avg, Min, Max, Cur, Jitter and PL% cells of a hop.
fn write_numbers(page: &mut String, hop: &HopRow) {
    for value in [hop.avg, hop.min, hop.max] {
        let _ = write!(page, "<td class=\"num\">{}</td>", ms(value));
    }
    let current = match hop.cur {
        Some(value) => format!("{value:.1}"),
        None if hop.sent > 0 => "*".to_owned(),
        None => "-".to_owned(),
    };
    let _ = write!(page, "<td class=\"num\">{current}</td><td class=\"num\">{}</td>", ms(hop.jitter));
    let loss_class = if hop.loss > 0.0 { "num bad" } else { "num" };
    let _ = write!(page, "<td class=\"{loss_class}\">{:.1}</td>", hop.loss);
}

/// A latency as shown in the table: one decimal, "-" for none.
fn ms(value: Option<f64>) -> String {
    value.map_or_else(|| "-".to_owned(), |value| format!("{value:.1}"))
}

/// The scale of the latency bars: twice the highest average, rounded up to 50 ms, at least 250 ms (as the app).
fn bar_scale(averages: impl Iterator<Item = f64>) -> f64 {
    let highest = averages.fold(0.0_f64, f64::max);
    ((highest * 2.0 / 50.0).ceil() * 50.0).max(250.0)
}

/// A hop's latency bar: zones, the loss share, the min–max range, the average and the current value.
fn latency_bar(hop: &HopRow, scale: f64) -> String {
    let p = &LIGHT;
    let (width, height) = BAR;
    let x = |value: f64| (value / scale).clamp(0.0, 1.0) * width;
    let mut svg = format!("<svg width=\"{width}\" height=\"{height}\" viewBox=\"0 0 {width} {height}\">");
    if hop.address.is_none() {
        let _ = write!(svg, "<rect width=\"{width}\" height=\"{height}\" fill=\"{}\"/>", hex(p.lost));
        svg.push_str("</svg>");
        return svg;
    }
    for (from, to, color) in
        [(0.0, GOOD_MS, p.good_zone), (GOOD_MS, WARN_MS, p.warn_zone), (WARN_MS, scale, p.bad_zone)]
    {
        let _ = write!(
            svg,
            "<rect x=\"{:.1}\" width=\"{:.1}\" height=\"{height}\" fill=\"{}\"/>",
            x(from),
            (x(to) - x(from)).max(0.0),
            hex(color)
        );
    }
    if hop.loss > 0.0 {
        let loss_width = width * (hop.loss / 100.0).clamp(0.0, 1.0);
        let _ = write!(
            svg,
            "<rect y=\"{:.1}\" width=\"{loss_width:.1}\" height=\"{:.1}\" fill=\"{}\" stroke=\"{}\"/>",
            height * 0.2,
            height * 0.6,
            hex(p.lost),
            hex(p.bad)
        );
    }
    let middle = height / 2.0;
    if let (Some(min), Some(max)) = (hop.min, hop.max) {
        let _ = write!(
            svg,
            "<line x1=\"{:.1}\" x2=\"{:.1}\" y1=\"{middle}\" y2=\"{middle}\" stroke=\"{}\"/>",
            x(min),
            x(max),
            hex(p.range)
        );
    }
    if let Some(avg) = hop.avg {
        let _ = write!(
            svg,
            "<line x1=\"{0:.1}\" x2=\"{0:.1}\" y1=\"2\" y2=\"{1}\" stroke=\"{2}\" stroke-width=\"2\"/>",
            x(avg),
            height - 2.0,
            hex(p.signal)
        );
    }
    if let Some(cur) = hop.cur {
        let _ = write!(svg, "<circle cx=\"{:.1}\" cy=\"{middle}\" r=\"2.5\" fill=\"{}\"/>", x(cur), hex(p.bad));
    }
    svg.push_str("</svg>");
    svg
}

/// A chart as the window draws it: the jitter strip, then the latency steps over the zones (from the first sample
/// on), loss hanging from the top, a grid over everything on round values and local times, marks as dashed lines
/// with their text on hover.
fn chart_svg(chart: &Chart, language: Language) -> String {
    let p = &LIGHT;
    let (width, left) = (CHART.0, CHART.2);
    let plot_w = width - left;
    let (from_s, to_s) = (seconds(chart.from_ms), seconds(chart.to_ms));
    let span = (to_s - from_s).max(1.0);
    let x = |s: f64| left + ((s - from_s) / span).clamp(0.0, 1.0) * plot_w;
    let utc_offset_s = i64::from(chrono::Local::now().offset().local_minus_utc());
    let jitter_top = 6.0;
    let latency_top = jitter_top + JITTER_HEIGHT + PLOT_GAP;
    let latency_h = CHART.1 - latency_top - CHART.3;
    let height = CHART.1;
    let mut svg = format!("<svg class=\"chart\" viewBox=\"0 0 {width} {height}\" width=\"100%\" role=\"img\">");
    let series = &chart.series;
    plot(&mut svg, &series.jitter, jitter_top, JITTER_HEIGHT, false, &x, chart, utc_offset_s);
    let _ = write!(
        svg,
        "<text x=\"{:.1}\" y=\"{:.1}\" font-size=\"11\" fill=\"{}\">{}</text>",
        left + 4.0,
        jitter_top + 11.0,
        hex(p.jitter),
        tr!(language, "Jitter (ms)")
    );
    plot(&mut svg, &series.latency, latency_top, latency_h, true, &x, chart, utc_offset_s);
    let _ = write!(
        svg,
        "<text x=\"{:.1}\" y=\"{:.1}\" font-size=\"11\" fill=\"{}\">{}</text>",
        left + 4.0,
        latency_top + 11.0,
        hex(p.ink_muted),
        tr!(language, "Latency (ms)")
    );
    for (value, step) in time_marks(from_s, to_s, utc_offset_s) {
        let label = clock(timeline_millis(value), chart.period.axis_format(step));
        // Labels near the right edge end there instead of being cut off.
        let at = x(value);
        let anchor = if at > width - EDGE_LABEL_ROOM { "end" } else { "middle" };
        let _ = write!(
            svg,
            "<text x=\"{:.1}\" y=\"{:.1}\" font-size=\"11\" text-anchor=\"{anchor}\" fill=\"{}\">{}</text>",
            at.min(width - 2.0),
            height - 6.0,
            hex(p.ink_muted),
            escape(&label)
        );
    }
    svg.push_str("</svg>");
    svg
}

/// One plot of a chart, `height` tall from `top`: the latency plot (`latency`) gets zones, loss bars and marks,
/// the jitter strip only its line. Both get the grid and their value labels.
#[expect(clippy::too_many_arguments, reason = "the plot's place and what to draw in it")]
fn plot(
    svg: &mut String,
    trace: &Trace,
    top: f64,
    height: f64,
    latency: bool,
    x: &dyn Fn(f64) -> f64,
    chart: &Chart,
    utc_offset_s: i64,
) {
    let p = &LIGHT;
    let (width, left) = (CHART.0, CHART.2);
    let scale = trace.scale_ms.max(f64::EPSILON);
    let y = |value: f64| top + height - (value / scale).clamp(0.0, 1.0) * height;
    let _ = write!(
        svg,
        "<rect x=\"{left}\" y=\"{top}\" width=\"{:.1}\" height=\"{height}\" fill=\"{}\"/>",
        width - left,
        hex(p.well)
    );
    let series = &chart.series;
    if latency && let Some(first_s) = series.first_s {
        let data_x = x(first_s);
        for (low, high, color) in
            [(0.0, GOOD_MS, p.good_zone), (GOOD_MS, WARN_MS, p.warn_zone), (WARN_MS, scale, p.bad_zone)]
        {
            if low < scale {
                let band_top = y(high.min(scale));
                let _ = write!(
                    svg,
                    "<rect x=\"{data_x:.1}\" y=\"{band_top:.1}\" width=\"{:.1}\" height=\"{:.1}\" fill=\"{}\"/>",
                    (width - data_x).max(0.0),
                    y(low) - band_top,
                    hex(color)
                );
            }
        }
    }
    if latency {
        let bucket_w = (seconds(chart.bucket_ms) / (seconds(chart.to_ms) - seconds(chart.from_ms)).max(1.0)
            * (width - left))
            .max(1.0);
        for &(at_s, percent) in &series.loss {
            let _ = write!(
                svg,
                "<rect x=\"{:.1}\" y=\"{top:.1}\" width=\"{bucket_w:.1}\" height=\"{:.1}\" fill=\"{}\"/>",
                x(at_s) - bucket_w / 2.0,
                percent / 100.0 * height,
                hex(p.loss)
            );
        }
    }
    // The grid over everything, on round values and local times, as in the window.
    #[expect(clippy::cast_possible_truncation, reason = "a plot is a few hundred points tall")]
    let marks = value_marks(scale, height as f32);
    for (value, _) in &marks {
        if *value > 0.0 {
            let _ = write!(
                svg,
                "<line x1=\"{left}\" x2=\"{width}\" y1=\"{0:.1}\" y2=\"{0:.1}\" stroke=\"#0000001c\"/>",
                y(*value)
            );
        }
        let _ = write!(
            svg,
            "<text x=\"{:.1}\" y=\"{:.1}\" font-size=\"11\" text-anchor=\"end\" fill=\"{}\">{value:.0}</text>",
            left - 6.0,
            (y(*value) + 4.0).max(top + 9.0),
            hex(p.ink_muted)
        );
    }
    for (value, _) in time_marks(seconds(chart.from_ms), seconds(chart.to_ms), utc_offset_s) {
        let _ = write!(
            svg,
            "<line x1=\"{0:.1}\" x2=\"{0:.1}\" y1=\"{top:.1}\" y2=\"{1:.1}\" stroke=\"#0000001c\"/>",
            x(value),
            top + height
        );
    }
    let color = if latency { p.ink } else { p.jitter };
    for segment in &trace.segments {
        let points: Vec<String> =
            segment.iter().map(|[at_s, value]| format!("{:.1},{:.1}", x(*at_s), y(*value))).collect();
        let _ = write!(
            svg,
            "<polyline fill=\"none\" stroke=\"{}\" stroke-width=\"1.1\" points=\"{}\"/>",
            hex(color),
            points.join(" ")
        );
    }
    for [at_s, _] in &trace.clipped {
        let (cx, cy) = (x(*at_s), top + 1.0);
        let _ = write!(
            svg,
            "<polygon points=\"{:.1},{:.1} {:.1},{:.1} {:.1},{:.1}\" fill=\"{}\"/>",
            cx - 4.0,
            cy + 7.0,
            cx + 4.0,
            cy + 7.0,
            cx,
            cy,
            hex(color)
        );
    }
    if latency {
        for (at_ms, text) in &chart.marks {
            let _ = write!(
                svg,
                "<line x1=\"{0:.1}\" x2=\"{0:.1}\" y1=\"{top:.1}\" y2=\"{1:.1}\" stroke=\"{2}\" stroke-dasharray=\"4 3\"><title>{3}</title></line>",
                x(seconds(*at_ms)),
                top + height,
                hex(p.comment),
                escape(text)
            );
        }
    }
}

/// A chart x value (seconds) as Unix ms.
#[expect(clippy::cast_possible_truncation, reason = "chart x values are Unix seconds, far inside i64")]
fn timeline_millis(seconds: f64) -> i64 {
    (seconds * 1_000.0).round() as i64
}

/// `#rrggbb` of a color.
fn hex(color: Color32) -> String {
    format!("#{:02x}{:02x}{:02x}", color.r(), color.g(), color.b())
}

/// Text made safe for HTML.
fn escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            c => escaped.push(c),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::store::history::Bucket;

    /// A hop answering from 10.0.0.`last` in `avg` ms.
    fn hop(ttl: u8, avg: f64, loss: f64) -> HopRow {
        HopRow {
            ttl,
            address: Some(format!("10.0.0.{ttl}")),
            name: None,
            sent: 10,
            avg: Some(avg),
            min: Some(avg - 1.0),
            max: Some(avg + 1.0),
            cur: Some(avg),
            jitter: Some(0.5),
            loss,
        }
    }

    #[test]
    fn text_is_escaped() {
        assert_eq!(escape("<b>\"A&B\"</b>"), "&lt;b&gt;&quot;A&amp;B&quot;&lt;/b&gt;");
    }

    #[test]
    fn file_names_are_safe() {
        let report = TargetReport {
            label: "1.1.1.1 / a/b:c".to_owned(),
            focus: String::new(),
            hops: Vec::new(),
            chart: None,
            alerts: Vec::new(),
        };
        assert_eq!(
            Snapshot::Target(Box::new(report)).file_name("2026-11-15 15-30", Language::English),
            "Ping Watch 2026-11-15 15-30 a-b-c.html"
        );
        let all = Snapshot::AllTargets { focus: String::new(), rows: Vec::new(), charts: Vec::new() };
        assert_eq!(
            all.file_name("2026-11-15 15-30", Language::English),
            "Ping Watch 2026-11-15 15-30 All targets.html"
        );
    }

    #[test]
    fn a_target_page_has_its_parts() {
        let buckets: Vec<Bucket> = (0..60_i64)
            .map(|second| Bucket {
                start_ms: second.saturating_mul(1_000),
                sent: 1,
                received: u32::from(second != 30),
                min: None,
                avg: (second != 30).then(|| Duration::from_millis(20)),
                max: None,
                jitter: Some(Duration::from_millis(1)),
            })
            .collect();
        let chart = Chart {
            from_ms: 0,
            to_ms: 60_000,
            bucket_ms: 1_000,
            period: Period::Minute,
            series: Series::from_buckets(&buckets, 1_000),
            marks: vec![(30_000, "Route changed <here>".to_owned())],
        };
        let report = TargetReport {
            label: "1.1.1.1 / Cloudflare".to_owned(),
            focus: "15:00 – 15:10".to_owned(),
            hops: vec![hop(1, 2.0, 0.0), hop(2, 30.0, 5.0)],
            chart: Some(chart),
            alerts: vec![AlertRow {
                started: "15.11 15:05".to_owned(),
                what: "loss above 10 %".to_owned(),
                lasted: "2 minutes".to_owned(),
                worst: "23 %".to_owned(),
            }],
        };
        let page = html(&Snapshot::Target(Box::new(report)), "15.11.2026 15:10", "1.0.0", Language::English);
        assert!(page.starts_with("<!doctype html>"));
        assert!(page.contains("1.1.1.1 / Cloudflare"));
        assert!(page.contains("10.0.0.2"));
        assert!(page.contains("<polyline"), "the latency steps");
        assert!(page.contains("Jitter (ms)"), "the jitter strip");
        assert!(page.contains("Route changed &lt;here&gt;"), "marks are escaped");
        assert!(page.contains("loss above 10 %"));
        assert!(
            page.contains("<a href=\"https://iringil.github.io/ping-watch/\">Ping Watch 1.0.0</a>"),
            "a link to the site"
        );
        assert!(!page.contains("src=") && !page.contains("<link"), "nothing is loaded from outside");
    }
}
