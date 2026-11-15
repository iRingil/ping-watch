//! Pane 1: the hop table — the route to the target with live per-hop statistics, in PingPlotter style.
//! Its last column holds pane 2, the latency bars ([`latency_bars`]). Without a route it still shows its header.

use std::time::Duration;

use eframe::egui::{Align, Align2, Color32, Layout, Pos2, RichText, Sense, Stroke, Ui, Vec2};

use crate::ui::theme;
use egui_extras::{Column, TableBuilder, TableRow};

use crate::probe::names::NameCache;
use crate::probe::route::{Hop, Route};
use crate::probe::stats::HopStats;
use crate::ui::latency_bars;

/// Height of a table row.
const ROW_HEIGHT: f32 = 22.0;
/// Share of the table width given to the latency bars (pane 2); the Name column takes what is left.
const LATENCY_SHARE: f32 = 0.3;
/// Inset of a header title from its cell's edges.
const HEADER_INSET: f32 = 4.0;
/// Height of the header row.
const HEADER_HEIGHT: f32 = 26.0;
/// Width of the Hop column.
const HOP_WIDTH: f32 = 40.0;
/// Width of the IP column (fits an IPv4 address; IPv6 ones are clipped, the full address is in the timeline).
const IP_WIDTH: f32 = 130.0;
/// Width of a numeric column.
const NUMBER_WIDTH: f32 = 58.0;
/// Diameter of the hop number badge.
const BADGE_SIZE: f32 = 17.0;
/// Column titles and whether the column is numeric (right-aligned).
const HEADERS: [(&str, bool); 10] = [
    ("Hop", false),
    ("Count", true),
    ("IP", false),
    ("Name", false),
    ("Avg", true),
    ("Min", true),
    ("Max", true),
    ("Cur", true),
    ("Jitter", true),
    ("PL%", true),
];

/// How bad a hop's packet loss is; picks the badge and PL% color.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LossLevel {
    /// No loss.
    None,
    /// Occasional loss, below 10 %.
    Some,
    /// 10 % or more.
    Heavy,
}

impl LossLevel {
    /// Classifies a loss percentage.
    fn of(percent: f64) -> Self {
        if percent <= 0.0 {
            Self::None
        } else if percent < 10.0 {
            Self::Some
        } else {
            Self::Heavy
        }
    }

    /// Color used for this level.
    fn color(self) -> Color32 {
        match self {
            Self::None => theme::GOOD,
            Self::Some => theme::WARN,
            Self::Heavy => theme::BAD,
        }
    }
}

/// Draws the hop table of `route` (just the header without one) with the latency bars; hop names come from
/// `names`, the `selected` hop is highlighted, `focus` labels the period under the bars. Returns the TTL of the
/// hop row clicked in this frame.
pub fn show(
    ui: &mut Ui,
    route: Option<&Route>,
    names: &NameCache,
    selected: Option<u8>,
    focus: Option<&str>,
) -> Option<u8> {
    let hops = route.map_or(&[][..], Route::hops);
    let target_hop = route.and_then(|route| hops.last().filter(|hop| hop.address == Some(route.target())));
    let scale_ms = latency_bars::scale_ms(hops.iter().map(|hop| &hop.stats));
    let mut averages: Vec<Pos2> = Vec::new();
    let mut clicked = None;
    // No gaps between columns: cells keep their own inset, and row bands (summary, header) can then be painted
    // cell by cell without holes — striped rows repaint each cell's background, so a band drawn once across the row
    // gets covered.
    ui.spacing_mut().item_spacing.x = 0.0;
    // Explicit widths: the latency bars get their share, Name gets whatever the fixed columns leave.
    let available = ui.available_width();
    let latency_width = (available * LATENCY_SHARE).max(200.0);
    let spacing = ui.spacing().item_spacing.x * 11.0;
    let fixed = HOP_WIDTH + NUMBER_WIDTH * 7.0 + IP_WIDTH + latency_width + spacing;
    let name_width = (available - fixed).max(80.0);
    TableBuilder::new(ui)
        .striped(true)
        .sense(Sense::click())
        .cell_layout(Layout::left_to_right(Align::Center))
        .column(Column::exact(HOP_WIDTH))
        .column(Column::exact(NUMBER_WIDTH))
        .column(Column::exact(IP_WIDTH).clip(true))
        .column(Column::exact(name_width).clip(true))
        .columns(Column::exact(NUMBER_WIDTH), 6)
        .column(Column::exact(latency_width).clip(true))
        .header(HEADER_HEIGHT, |mut header| {
            for (index, (title, numeric)) in HEADERS.into_iter().enumerate() {
                header.col(|ui| header_cell(ui, index == 0, numeric, title, available));
            }
            header.col(|ui| latency_bars::header(ui, scale_ms));
        })
        .body(|mut body| {
            for hop in hops {
                body.row(ROW_HEIGHT, |mut row| {
                    row.set_selected(selected == Some(hop.ttl));
                    averages.extend(hop_row(&mut row, hop, names, scale_ms));
                    if row.response().clicked() {
                        clicked = Some(hop.ttl);
                    }
                });
            }
            if let Some(hop) = target_hop {
                body.row(ROW_HEIGHT, |mut row| round_trip_row(&mut row, &hop.stats, focus));
            }
        });
    latency_bars::connect(ui, &averages);
    clicked
}

/// Fills the row of one hop; returns the position of its average latency marker, if drawn.
fn hop_row(row: &mut TableRow<'_, '_>, hop: &Hop, names: &NameCache, scale_ms: f64) -> Option<Pos2> {
    row.col(|ui| badge(ui, hop));
    row.col(|ui| cell(ui, true, hop.stats.sent().to_string()));
    match hop.address {
        Some(ip) => {
            row.col(|ui| cell(ui, false, ip.to_string()));
            let name = names.get(ip).unwrap_or_else(|| ip.to_string());
            row.col(|ui| cell(ui, false, name));
        }
        None => {
            row.col(|ui| cell(ui, false, "*"));
            row.col(|ui| cell(ui, false, "No response"));
        }
    }
    stat_cols(row, &hop.stats, false);
    let mut average = None;
    row.col(|ui| average = latency_bars::cell(ui, &hop.stats, hop.address.is_some(), scale_ms));
    average
}

/// Fills the summary row of the whole round trip (the target hop), with the focus period under the bars.
/// The row reads as a summary, not another hop: a header-colored band with a rule above it.
fn round_trip_row(row: &mut TableRow<'_, '_>, stats: &HopStats, focus: Option<&str>) {
    let emphasis = |text: String| RichText::new(text).font(theme::heading_font(14.5));
    row.col(summary_band);
    row.col(|ui| {
        summary_band(ui);
        cell(ui, true, emphasis(stats.sent().to_string()));
    });
    row.col(summary_band);
    row.col(|ui| {
        summary_band(ui);
        cell(ui, true, emphasis("Round trip (ms)".to_owned()));
    });
    stat_cols(row, stats, true);
    row.col(|ui| {
        summary_band(ui);
        if let Some(focus) = focus {
            cell(ui, true, RichText::new(focus).small().color(theme::INK_MUTED));
        }
    });
}

/// One header cell: the first paints the header band across the whole row of width `row_width` (cells clip
/// their own painting, so per-cell bands left gaps); every other cell starts with a thin divider. The title keeps a
/// small inset from the cell edges, right-aligned if `numeric`.
pub fn header_cell(ui: &mut Ui, first: bool, numeric: bool, title: &str, row_width: f32) {
    let cell_rect = ui.max_rect();
    if first {
        let row = eframe::egui::Rect::from_min_size(cell_rect.min, Vec2::new(row_width, cell_rect.height()));
        let mut painter = ui.painter().clone();
        painter.set_clip_rect(row.expand(1.0));
        painter.rect_filled(row, 0.0, theme::PANE_HEADER);
    } else {
        ui.painter()
            .vline(cell_rect.left() + 0.5, cell_rect.y_range().shrink(4.0), Stroke::new(1.0, theme::RULE));
    }
    let text = RichText::new(title).color(theme::INK_MUTED);
    if numeric {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add_space(HEADER_INSET);
            ui.label(text);
        });
    } else {
        ui.add_space(HEADER_INSET);
        ui.label(text);
    }
}

/// Paints the summary row's band and the rule above it over the cell of `ui`; with no gaps between columns the
/// cells join into one unbroken band.
fn summary_band(ui: &mut Ui) {
    let rect = ui.max_rect();
    ui.painter().rect_filled(rect, 0.0, theme::PANE_HEADER);
    ui.painter().hline(rect.x_range(), rect.top() + 0.5, Stroke::new(1.5, theme::INK_MUTED));
}

/// Fills the Avg, Min, Max, Cur, Jitter and PL% columns; `summary` gives them the summary row's band.
fn stat_cols(row: &mut TableRow<'_, '_>, stats: &HopStats, summary: bool) {
    let band = |ui: &mut Ui| {
        if summary {
            summary_band(ui);
        }
    };
    for value in [stats.average(), stats.best(), stats.worst()] {
        row.col(|ui| {
            band(ui);
            cell(ui, true, format_ms(value));
        });
    }
    let current = match stats.last() {
        Some(rtt) => format_ms(Some(rtt)),
        None if stats.sent() > 0 => "*".to_owned(),
        None => format_ms(None),
    };
    row.col(|ui| {
        band(ui);
        cell(ui, true, current);
    });
    row.col(|ui| {
        band(ui);
        cell(ui, true, format_ms(stats.jitter()));
    });
    let loss = stats.loss_percent();
    let loss_text = RichText::new(format!("{loss:.1}"));
    let loss_text = if loss > 0.0 { loss_text.color(LossLevel::of(loss).color()) } else { loss_text };
    row.col(|ui| {
        band(ui);
        cell(ui, true, loss_text);
    });
}

/// Hop number in a circle colored by the hop's average latency zone, as in PingPlotter (loss shows in the PL%
/// column and as a red bar in the latency column); plain number for a hop that never answered.
fn badge(ui: &mut Ui, hop: &Hop) {
    let text = hop.ttl.to_string();
    if hop.address.is_none() {
        cell(ui, false, text);
        return;
    }
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(BADGE_SIZE), Sense::hover());
    let color = hop.stats.average().map_or(theme::INK_MUTED, latency_color);
    ui.painter().circle_filled(rect.center(), BADGE_SIZE / 2.0, color);
    ui.painter()
        .text(rect.center(), Align2::CENTER_CENTER, text, theme::heading_font(12.0), theme::CHASSIS);
}

/// Color of a latency's zone: green below 100 ms, amber below 200 ms, red above.
fn latency_color(latency: Duration) -> Color32 {
    let ms = latency_bars::millis(latency);
    if ms < latency_bars::GOOD_MS {
        theme::GOOD
    } else if ms < latency_bars::WARN_MS {
        theme::WARN
    } else {
        theme::BAD
    }
}

/// Adds a label with the same inset as the header titles, right-aligned if `numeric`.
pub fn cell(ui: &mut Ui, numeric: bool, text: impl Into<RichText>) {
    if numeric {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add_space(HEADER_INSET);
            ui.label(text.into());
        });
    } else {
        ui.add_space(HEADER_INSET);
        ui.label(text.into());
    }
}

/// Formats a duration as milliseconds with one decimal, `-` for none.
pub fn format_ms(value: Option<Duration>) -> String {
    value.map_or_else(|| "-".to_owned(), |duration| format!("{:.1}", duration.as_secs_f64() * 1000.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loss_levels() {
        assert_eq!(LossLevel::of(0.0), LossLevel::None);
        assert_eq!(LossLevel::of(5.0), LossLevel::Some);
        assert_eq!(LossLevel::of(10.0), LossLevel::Heavy);
        assert_eq!(LossLevel::of(100.0), LossLevel::Heavy);
    }

    #[test]
    fn badge_color_follows_latency_zones() {
        assert_eq!(latency_color(Duration::from_millis(52)), theme::GOOD);
        assert_eq!(latency_color(Duration::from_millis(177)), theme::WARN);
        assert_eq!(latency_color(Duration::from_millis(260)), theme::BAD);
    }

    #[test]
    fn milliseconds_with_one_decimal() {
        assert_eq!(format_ms(Some(Duration::from_micros(51_449))), "51.4");
        assert_eq!(format_ms(None), "-");
    }
}
