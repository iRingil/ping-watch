//! Pane 1: the hop table — the route to the target with live per-hop statistics, in PingPlotter style.
//! Its last column holds pane 2, the latency bars ([`latency_bars`]). Without a route it still shows its header.

use std::time::Duration;

use eframe::egui::{Align, Align2, Color32, Layout, Pos2, RichText, Sense, Ui, Vec2};

use crate::theme;
use egui_extras::{Column, TableBuilder, TableRow};

use crate::latency_bars;
use crate::names::NameCache;
use crate::route::{Hop, Route};
use crate::stats::HopStats;

/// Height of a table row.
const ROW_HEIGHT: f32 = 22.0;
/// Share of the table width given to the latency bars (pane 2); the Name column takes what is left.
const LATENCY_SHARE: f32 = 0.3;
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
            for (title, numeric) in HEADERS {
                header.col(|ui| {
                    ui.painter().rect_filled(ui.max_rect().expand2(Vec2::new(4.0, 0.0)), 0.0, theme::PANE_HEADER);
                    cell(ui, numeric, RichText::new(title).color(theme::INK_MUTED));
                });
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
    stat_cols(row, &hop.stats);
    let mut average = None;
    row.col(|ui| average = latency_bars::cell(ui, &hop.stats, hop.address.is_some(), scale_ms));
    average
}

/// Fills the summary row of the whole round trip (the target hop), with the focus period under the bars.
fn round_trip_row(row: &mut TableRow<'_, '_>, stats: &HopStats, focus: Option<&str>) {
    let emphasis = |text: String| RichText::new(text).font(theme::heading_font(14.5));
    row.col(|_| {});
    row.col(|ui| cell(ui, true, emphasis(stats.sent().to_string())));
    row.col(|_| {});
    row.col(|ui| cell(ui, true, emphasis("Round trip (ms)".to_owned())));
    stat_cols(row, stats);
    row.col(|ui| {
        if let Some(focus) = focus {
            cell(ui, true, RichText::new(focus).small().color(theme::INK_MUTED));
        }
    });
}

/// Fills the Avg, Min, Max, Cur, Jitter and PL% columns.
fn stat_cols(row: &mut TableRow<'_, '_>, stats: &HopStats) {
    for value in [stats.average(), stats.best(), stats.worst()] {
        row.col(|ui| cell(ui, true, format_ms(value)));
    }
    let current = match stats.last() {
        Some(rtt) => format_ms(Some(rtt)),
        None if stats.sent() > 0 => "*".to_owned(),
        None => format_ms(None),
    };
    row.col(|ui| cell(ui, true, current));
    row.col(|ui| cell(ui, true, format_ms(stats.jitter())));
    let loss = stats.loss_percent();
    let loss_text = RichText::new(format!("{loss:.1}"));
    let loss_text = if loss > 0.0 { loss_text.color(LossLevel::of(loss).color()) } else { loss_text };
    row.col(|ui| cell(ui, true, loss_text));
}

/// Hop number in a circle colored by loss; plain number for a hop that never answered.
fn badge(ui: &mut Ui, hop: &Hop) {
    let text = hop.ttl.to_string();
    if hop.address.is_none() {
        cell(ui, false, text);
        return;
    }
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(BADGE_SIZE), Sense::hover());
    let color = LossLevel::of(hop.stats.loss_percent()).color();
    ui.painter().circle_filled(rect.center(), BADGE_SIZE / 2.0, color);
    ui.painter()
        .text(rect.center(), Align2::CENTER_CENTER, text, theme::heading_font(12.0), theme::CHASSIS);
}

/// Adds a label, right-aligned if `numeric`.
fn cell(ui: &mut Ui, numeric: bool, text: impl Into<RichText>) {
    if numeric {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| ui.label(text.into()));
    } else {
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
    fn milliseconds_with_one_decimal() {
        assert_eq!(format_ms(Some(Duration::from_micros(51_449))), "51.4");
        assert_eq!(format_ms(None), "-");
    }
}
