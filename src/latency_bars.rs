//! Pane 2: per-hop latency bars drawn in the last column of the hop table — min–max range, average and current
//! latency over colored latency zones, with a line connecting the averages of consecutive hops. The scale follows
//! the averages, so one slow answer doesn't squash every bar; a range reaching past it ends in an arrow.

use std::time::Duration;

use eframe::egui::{self, Align2, Color32, CornerRadius, Pos2, Rect, Sense, Stroke, Ui, Vec2, pos2, vec2};

use crate::stats::HopStats;
use crate::theme::{self, BAD, GOOD, INK_MUTED, WARN};
pub use crate::theme::{BAD_ZONE, GOOD_ZONE, WARN_ZONE};

/// Latency below which a hop is fine (green zone).
pub const GOOD_MS: f64 = 100.0;
/// Latency below which a hop is acceptable (yellow zone); above it is the red zone.
pub const WARN_MS: f64 = 200.0;
/// Smallest scale, so that the yellow zone and the start of the red one are always visible.
const MIN_SCALE_MS: f64 = 250.0;
/// Scale is rounded up to a multiple of this.
const SCALE_STEP_MS: f64 = 50.0;
/// The scale is this many times the highest hop average.
const SCALE_PER_AVERAGE: f64 = 2.0;

/// Background of a row whose hop doesn't answer.
const LOST_ROW: Color32 = Color32::from_rgb(0x69, 0x2b, 0x3c);
/// Color of the min–max range.
const RANGE_COLOR: Color32 = Color32::from_rgb(0xa9, 0xae, 0xb8);
/// Color of the average marker.
const AVG_COLOR: Color32 = theme::SIGNAL;
/// Color of the current-latency dot and of the line connecting averages.
const CURRENT_COLOR: Color32 = BAD;
/// Size of the legend in the toolbar.
const LEGEND_SIZE: Vec2 = vec2(150.0, 26.0);

/// Scale of the bars in milliseconds: [`SCALE_PER_AVERAGE`] times the highest hop average, rounded up to
/// [`SCALE_STEP_MS`], at least [`MIN_SCALE_MS`]. Rare slow answers (the "max" end) may lie beyond it.
pub fn scale_ms<'a>(stats: impl IntoIterator<Item = &'a HopStats>) -> f64 {
    let highest = stats.into_iter().filter_map(HopStats::average).max().map_or(0.0, millis);
    ((highest * SCALE_PER_AVERAGE / SCALE_STEP_MS).ceil() * SCALE_STEP_MS).max(MIN_SCALE_MS)
}

/// Duration as fractional milliseconds.
pub fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

/// Horizontal position of `ms` in `rect` for a scale of `scale_ms` (clamped to the rect).
fn x_of(rect: Rect, ms: f64, scale_ms: f64) -> f32 {
    let fraction = (ms / scale_ms).clamp(0.0, 1.0) as f32;
    rect.left() + fraction * rect.width()
}

/// Claims the whole table cell of `ui` and returns its rectangle. (`available_size` may be wider than the
/// column, which would make the drawing spill past it.)
fn cell_rect(ui: &mut Ui) -> Rect {
    let rect = ui.max_rect();
    ui.allocate_rect(rect, Sense::hover());
    rect
}

/// Header of the latency column: scale start, title and scale end.
pub fn header(ui: &mut Ui, scale_ms: f64) {
    let rect = cell_rect(ui);
    let painter = ui.painter();
    painter.rect_filled(rect, 0.0, theme::PANE_HEADER);
    painter.vline(rect.left() + 0.5, rect.y_range().shrink(4.0), Stroke::new(1.0, theme::RULE));
    let font = theme::heading_font(12.5);
    // The scale ends sit on tabs in their zone's color, so the header doubles as the axis of the bars below.
    let tab = |anchor: Align2, text: String, zone: Color32| {
        let text_size = painter.layout_no_wrap(text.clone(), font.clone(), theme::INK).size();
        let size = vec2(text_size.x + 12.0, text_size.y + 4.0);
        let tab = anchor.align_size_within_rect(size, rect.shrink2(vec2(0.0, 3.0)));
        painter.rect_filled(tab, CornerRadius::same(2), zone);
        painter.text(tab.center(), Align2::CENTER_CENTER, text, font.clone(), theme::INK);
    };
    tab(Align2::LEFT_CENTER, "0 ms".to_owned(), GOOD_ZONE);
    let end_zone = if scale_ms > WARN_MS { BAD_ZONE } else { WARN_ZONE };
    tab(Align2::RIGHT_CENTER, format!("{scale_ms:.0} ms"), end_zone);
    painter.text(rect.center(), Align2::CENTER_CENTER, "Latency", font, INK_MUTED);
}

/// Legend of the latency zones for the toolbar: a bar in the three zone colors with the 100 / 200 ms boundaries.
pub fn legend(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(LEGEND_SIZE, Sense::hover());
    let painter = ui.painter();
    let bar = Rect::from_min_max(pos2(rect.left(), rect.bottom() - 6.0), rect.right_bottom());
    let third = bar.width() / 3.0;
    for (index, color) in [GOOD, WARN, BAD].into_iter().enumerate() {
        let left = bar.left() + third * index as f32;
        painter.rect_filled(Rect::from_x_y_ranges(left..=left + third, bar.y_range()), 0.0, color);
    }
    let font = eframe::egui::FontId::proportional(12.0);
    for (index, label) in [(1.0, "100 ms"), (2.0, "200 ms")] {
        let x = bar.left() + third * index;
        painter.text(pos2(x, bar.top() - 2.0), Align2::CENTER_BOTTOM, label, font.clone(), INK_MUTED);
    }
}

/// Draws the bar of one hop in its table cell; returns the position of the average marker, if any.
pub fn cell(ui: &mut Ui, stats: &HopStats, answering: bool, scale_ms: f64) -> Option<Pos2> {
    let rect = cell_rect(ui);
    let painter = ui.painter();
    if !answering {
        painter.rect_filled(rect, 0.0, LOST_ROW);
        return None;
    }
    let zones = [(0.0, GOOD_MS, GOOD_ZONE), (GOOD_MS, WARN_MS, WARN_ZONE), (WARN_MS, scale_ms, BAD_ZONE)];
    for (from, to, color) in zones {
        let zone = Rect::from_x_y_ranges(x_of(rect, from, scale_ms)..=x_of(rect, to, scale_ms), rect.y_range());
        painter.rect_filled(zone, 0.0, color);
    }
    // Packet loss as in PingPlotter: a dark red bar from the left, as long as the loss share of the row.
    let loss = stats.loss_percent();
    if loss > 0.0 {
        let width = rect.width() * (loss / 100.0).clamp(0.0, 1.0) as f32;
        let bar = Rect::from_min_size(
            pos2(rect.left(), rect.center().y - rect.height() * 0.3),
            vec2(width, rect.height() * 0.6),
        );
        painter.rect_filled(bar, 0.0, LOST_ROW);
        painter.rect_stroke(bar, 0.0, Stroke::new(1.0, BAD), egui::StrokeKind::Inside);
    }
    let y = rect.center().y;
    if let (Some(best), Some(worst)) = (stats.best(), stats.worst()) {
        let (left, right) = (x_of(rect, millis(best), scale_ms), x_of(rect, millis(worst), scale_ms));
        let stroke = Stroke::new(1.0, RANGE_COLOR);
        painter.line_segment([pos2(left, y), pos2(right, y)], stroke);
        painter.line_segment([pos2(left, y - 3.0), pos2(left, y + 3.0)], stroke);
        if millis(worst) > scale_ms {
            // The slowest answer lies past the scale: an arrow head at the edge instead of an end tick.
            painter.line_segment([pos2(right - 5.0, y - 4.0), pos2(right, y)], stroke);
            painter.line_segment([pos2(right - 5.0, y + 4.0), pos2(right, y)], stroke);
        } else {
            painter.line_segment([pos2(right, y - 3.0), pos2(right, y + 3.0)], stroke);
        }
    }
    if let Some(current) = stats.last() {
        painter.circle_filled(pos2(x_of(rect, millis(current), scale_ms), y), 2.5, CURRENT_COLOR);
    }
    let average = stats.average().map(|avg| pos2(x_of(rect, millis(avg), scale_ms), y));
    if let Some(point) = average {
        painter.line_segment([pos2(point.x, y - 5.0), pos2(point.x, y + 5.0)], Stroke::new(2.0, AVG_COLOR));
    }
    average
}

/// Connects the average markers of consecutive answering hops with a line.
pub fn connect(ui: &Ui, points: &[Pos2]) {
    let stroke = Stroke::new(1.0, CURRENT_COLOR);
    for pair in points.windows(2) {
        if let [from, to] = pair {
            ui.painter().line_segment([*from, *to], stroke);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Stats with one answer of `ms` milliseconds.
    fn answered(ms: u64) -> HopStats {
        let mut stats = HopStats::default();
        stats.record_reply(Duration::from_millis(ms));
        stats
    }

    #[test]
    fn scale_has_a_minimum() {
        assert_eq!(scale_ms(&[answered(30), HopStats::default()]), MIN_SCALE_MS);
        assert_eq!(scale_ms(&[]), MIN_SCALE_MS);
    }

    #[test]
    fn scale_follows_the_highest_average() {
        assert_eq!(scale_ms(&[answered(30), answered(176)]), 400.0);
        assert_eq!(scale_ms(&[answered(150)]), 300.0);
    }

    #[test]
    fn one_slow_answer_does_not_stretch_the_scale() {
        let mut stats = HopStats::default();
        for rtt in [50, 50, 50, 50, 50, 50, 50, 50, 50, 900] {
            stats.record_reply(Duration::from_millis(rtt));
        }
        // Average 135 ms → 270 → 300, while the worst answer is 900 ms.
        assert_eq!(scale_ms(&[stats]), 300.0);
    }
}
