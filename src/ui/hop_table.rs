//! Pane 1: the hop table — the route to the target with live per-hop statistics, in PingPlotter style.
//! Its last column holds pane 2, the latency bars ([`latency_bars`]). Without a route it still shows its header.

use std::time::Duration;

use eframe::egui::{Align, Align2, Color32, Layout, Pos2, Rect, RichText, Sense, Stroke, TextStyle, Ui, Vec2};

use crate::ui::theme::{self, Palette};
use egui_extras::{TableBuilder, TableRow};

use crate::i18n::{self, Lang, Language, n, tr, trn};
use crate::probe::names::NameCache;
use crate::probe::route::{Hop, Route};
use crate::probe::stats::HopStats;
use crate::ui::columns::{self, ColumnWidths};
use crate::ui::latency_bars::{self, Zones};

/// Height of a table row.
const ROW_HEIGHT: f32 = 22.0;
/// Share of the table width given to the latency bars (pane 2) by default; the Name column takes what is left.
const LATENCY_SHARE: f32 = 0.3;
/// Narrowest the latency bars get.
const LATENCY_MIN_WIDTH: f32 = 200.0;
/// Number of columns that can be dragged: all but the latency bars.
pub const RESIZABLE_COLUMNS: usize = HEADERS.len();
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
    (n!("Hop"), false),
    (n!("Count"), true),
    (n!("IP"), false),
    (n!("Name"), false),
    (n!("Avg"), true),
    (n!("Min"), true),
    (n!("Max"), true),
    (n!("Cur"), true),
    (n!("Jitter"), true),
    (n!("PL%"), true),
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

    /// Color used for this level in `palette`.
    fn color(self, palette: &Palette) -> Color32 {
        match self {
            Self::None => palette.good,
            Self::Some => palette.warn,
            Self::Heavy => palette.bad,
        }
    }
}

/// Draws the hop table of `route` (just the header without one) with the latency bars; at most `limit` hops are
/// shown, the first ones beyond it are folded into one row. Hop names come from `names`, the `selected` hop is
/// highlighted, `focus` labels the period under the bars. Returns the TTL of the hop row clicked in this frame. The
/// columns can be dragged wider or narrower; `columns` keeps their widths.
pub fn show(
    ui: &mut Ui,
    route: Option<&Route>,
    limit: u8,
    names: &NameCache,
    selected: Option<u8>,
    focus: Option<&str>,
    columns: &mut ColumnWidths,
) -> Option<u8> {
    let hops = route.map_or(&[][..], |route| route.shown(limit));
    let hidden = route.map_or(0, |route| route.hidden(limit));
    let target_hop = route.and_then(|route| hops.last().filter(|hop| hop.address == Some(route.target())));
    let scale_ms = latency_bars::scale_ms(hops.iter().map(|hop| &hop.stats), Zones::of(ui.ctx()));
    let mut averages: Vec<Pos2> = Vec::new();
    let mut clicked = None;
    let mut hidden_cell = None;
    let mut body_top = f32::NEG_INFINITY;
    // One table state for all tabs: a column dragged in one tab is that wide in the others.
    columns::scope(ui, "hop-table", |ui| {
        // Default widths: the latency bars get their share, Name gets whatever the fixed columns leave.
        let available = ui.available_width();
        let latency_width = (available * LATENCY_SHARE).max(LATENCY_MIN_WIDTH);
        let fixed = HOP_WIDTH + NUMBER_WIDTH * 7.0 + IP_WIDTH + latency_width;
        let name_width = (available - fixed).max(80.0);
        let mut defaults = vec![HOP_WIDTH, NUMBER_WIDTH, IP_WIDTH, name_width];
        defaults.extend([NUMBER_WIDTH; 6]);
        let builder = TableBuilder::new(ui)
            .striped(true)
            .sense(Sense::click())
            .cell_layout(Layout::left_to_right(Align::Center));
        let mut header_widths = Vec::new();
        columns
            .columns(builder, defaults, available, LATENCY_MIN_WIDTH)
            .header(HEADER_HEIGHT, |mut header| {
                for (index, (title, numeric)) in HEADERS.into_iter().enumerate() {
                    header.col(|ui| {
                        header_widths.push(ui.max_rect().width());
                        let title = ui.language().translate(title);
                        header_cell(ui, index == 0, numeric, title, available);
                    });
                }
                header.col(|ui| {
                    body_top = ui.max_rect().bottom();
                    latency_bars::header(ui, scale_ms);
                });
            })
            .body(|mut body| {
                if hidden > 0 {
                    body.row(ROW_HEIGHT, |mut row| hidden_cell = hidden_row(&mut row));
                }
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
        columns.take_header(&header_widths);
        if let Some(cell) = hidden_cell {
            paint_hidden(ui, cell, available, body_top, hidden, limit);
        }
        latency_bars::connect(ui, &averages);
    });
    clicked
}

/// Fills in the row of one hop; returns the position of its average latency marker, if drawn.
fn hop_row(row: &mut TableRow<'_, '_>, hop: &Hop, names: &NameCache, scale_ms: f64) -> Option<Pos2> {
    row.col(|ui| badge(ui, hop));
    row.col(|ui| cell(ui, true, hop.stats.sent().to_string()));
    match hop.address {
        Some(ip) => {
            // The column fits an IPv4 address; a longer IPv6 one is clipped, so the whole address is on hover.
            let (_, address) = row.col(|ui| cell(ui, false, ip.to_string()));
            address.on_hover_text(ip.to_string());
            let name = names.get(ip).unwrap_or_else(|| ip.to_string());
            let (_, name_cell) = row.col(|ui| cell(ui, false, name.clone()));
            name_cell.on_hover_text(name);
        }
        None => {
            row.col(|ui| cell(ui, false, "*"));
            row.col(|ui| cell(ui, false, tr!(ui, "No response")));
        }
    }
    stat_cols(row, &hop.stats, false);
    let mut average = None;
    row.col(|ui| average = latency_bars::cell(ui, &hop.stats, hop.address.is_some(), scale_ms));
    average
}

/// Takes the row standing for the hidden first hops of a route: empty cells, painted over by [`paint_hidden`]
/// once the table is drawn. Returns where the row starts (its left top) and its height.
fn hidden_row(row: &mut TableRow<'_, '_>) -> Option<Rect> {
    let mut first = None;
    row.col(|ui| first = Some(ui.max_rect()));
    for _ in 0..RESIZABLE_COLUMNS {
        row.col(|_| {});
    }
    first
}

/// Paints the row of the `hidden` first hops of a route longer than `limit` over `cell` (its first cell): one
/// muted band across the whole row of width `row_width`, without column divisions, with the explanation in it.
/// Painted after the table, as cells clip their own painting to themselves; nothing above `body_top` (the header's
/// bottom), where the row goes when the table is scrolled.
fn paint_hidden(ui: &Ui, cell: Rect, row_width: f32, body_top: f32, hidden: usize, limit: u8) {
    let band = Rect::from_min_size(cell.min, Vec2::new(row_width, cell.height()));
    let palette = theme::palette(ui);
    let mut visible = band;
    visible.min.y = visible.min.y.max(body_top);
    let painter = ui.painter_at(visible);
    painter.rect_filled(band, 0.0, palette.pane_header);
    let count = u64::try_from(hidden).unwrap_or(u64::MAX);
    let text = i18n::fill(
        &trn!(ui, count, "{n} hop hidden by the limit of {limit}|{n} hops hidden by the limit of {limit}"),
        &[("limit", limit.to_string())],
    );
    painter.text(
        Pos2::new(band.left() + HEADER_INSET, band.center().y),
        Align2::LEFT_CENTER,
        text,
        TextStyle::Body.resolve(ui.style()),
        palette.ink_muted,
    );
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
        cell(ui, true, emphasis(tr!(ui, "Round trip (ms)").to_owned()));
    });
    stat_cols(row, stats, true);
    row.col(|ui| {
        summary_band(ui);
        if let Some(focus) = focus {
            cell(ui, true, RichText::new(focus).small().color(theme::palette(ui).ink_muted));
        }
    });
}

/// One header cell: the first paints the header band across the whole row of width `row_width` (cells clip
/// their own painting, so per-cell bands left gaps); every other cell starts with a thin divider. The title keeps a
/// small inset from the cell edges, right-aligned if `numeric`.
pub fn header_cell(ui: &mut Ui, first: bool, numeric: bool, title: &str, row_width: f32) {
    let cell_rect = ui.max_rect();
    if first {
        let row = Rect::from_min_size(cell_rect.min, Vec2::new(row_width, cell_rect.height()));
        let mut painter = ui.painter().clone();
        painter.set_clip_rect(row.expand(1.0));
        painter.rect_filled(row, 0.0, theme::palette(ui).pane_header);
    } else {
        ui.painter().vline(
            cell_rect.left() + 0.5,
            cell_rect.y_range().shrink(4.0),
            Stroke::new(1.0, theme::palette(ui).rule),
        );
    }
    let text = RichText::new(title).color(theme::palette(ui).ink_muted);
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
    ui.painter().rect_filled(rect, 0.0, theme::palette(ui).pane_header);
    ui.painter().hline(rect.x_range(), rect.top() + 0.5, Stroke::new(1.5, theme::palette(ui).ink_muted));
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
            cell(ui, true, format_ms(value, ui.language()));
        });
    }
    row.col(|ui| {
        band(ui);
        let current = match stats.last() {
            Some(rtt) => format_ms(Some(rtt), ui.language()),
            None if stats.sent() > 0 => "*".to_owned(),
            None => format_ms(None, ui.language()),
        };
        cell(ui, true, current);
    });
    row.col(|ui| {
        band(ui);
        cell(ui, true, format_ms(stats.jitter(), ui.language()));
    });
    let loss = stats.loss_percent();
    row.col(|ui| {
        band(ui);
        let loss_text = RichText::new(ui.language().decimal(loss));
        let loss_text =
            if loss > 0.0 { loss_text.color(LossLevel::of(loss).color(theme::palette(ui))) } else { loss_text };
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
    let palette = theme::palette(ui);
    let zones = Zones::of(ui.ctx());
    let color = hop
        .stats
        .average()
        .map_or(palette.ink_muted, |average| zones.color(latency_bars::millis(average), palette));
    ui.painter().circle_filled(rect.center(), BADGE_SIZE / 2.0, color);
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        text,
        theme::heading_font(12.0),
        theme::palette(ui).on_signal,
    );
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

/// Formats a duration as milliseconds with one decimal in `language`'s way, `-` for none.
pub fn format_ms(value: Option<Duration>, language: Language) -> String {
    value.map_or_else(|| "-".to_owned(), |duration| language.decimal(duration.as_secs_f64() * 1000.0))
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
        assert_eq!(format_ms(Some(Duration::from_micros(51_449)), Language::English), "51.4");
        assert_eq!(format_ms(Some(Duration::from_micros(51_449)), Language::German), "51,4");
        assert_eq!(format_ms(None, Language::English), "-");
    }
}
