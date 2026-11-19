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
use crate::store::window_state::WindowStore;
use crate::ui::columns::{self, ColumnWidths};
use crate::ui::latency_bars::{self, Zones};

/// Height of a table row.
const ROW_HEIGHT: f32 = 24.0;
/// Share of the table width given to the latency bars (pane 2) by default; the Name column takes what is left.
const LATENCY_SHARE: f32 = 0.3;
/// Narrowest the latency bars get.
const LATENCY_MIN_WIDTH: f32 = 200.0;
/// Inset of a header title from its cell's edges.
const HEADER_INSET: f32 = 4.0;
/// Height of the header row.
const HEADER_HEIGHT: f32 = 26.0;
/// Width of the Hop column: the chart checkbox and the hop number.
const HOP_WIDTH: f32 = 64.0;
/// Width of the IP column (fits an IPv4 address; IPv6 ones are clipped, the full address is in the timeline).
const IP_WIDTH: f32 = 130.0;
/// Width of a numeric column.
const NUMBER_WIDTH: f32 = 58.0;
/// Diameter of the hop number badge.
const BADGE_SIZE: f32 = 17.0;
/// Column titles, whether the column is numeric (right-aligned) and the hint on its title.
const HEADERS: [(&str, bool, &str); 11] = [
    (n!("Hop"), false, n!("Hop number: how many routers away (the probe's TTL)")),
    (n!("Count"), true, n!("Probes sent over the focus period")),
    (n!("IP"), false, n!("Address that answered")),
    (n!("Name"), false, n!("Name of the address by reverse DNS")),
    (n!("Avg"), true, n!("Average round trip, ms")),
    (n!("Min"), true, n!("Fastest round trip, ms")),
    (n!("Max"), true, n!("Slowest round trip, ms")),
    (n!("Cur"), true, n!("Round trip of the latest probe, ms; * if it was lost")),
    (n!("Jitter"), true, n!("Jitter: how much the round trip varies from probe to probe, ms")),
    (n!("PL%"), true, n!("Packet loss: share of probes without an answer, %")),
    (n!("PL pcs"), true, n!("Packet loss: number of probes without an answer")),
];
/// The hop table's columns as stored in `store`: all but the latency bars can be dragged, all but Hop, IP and
/// Name hidden.
pub fn column_set(store: Option<&WindowStore>) -> ColumnWidths {
    ColumnWidths::load("hop-table", &HEADERS.map(|(title, _, _)| title), &FIXED_COLUMNS, store)
}

/// Position of the Name column, which takes the room the others leave by default.
const NAME_COLUMN: usize = 3;
/// Columns that can't be hidden: Hop (its checkbox charts the hop; it also paints the header band), IP and Name
/// (they say which router a row is; Name also holds the round trip row's title).
const FIXED_COLUMNS: [usize; 3] = [0, 2, NAME_COLUMN];

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

/// What the user did in the hop table this frame, by TTL.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Clicks {
    /// The row clicked: it gets highlighted.
    pub selected: Option<u8>,
    /// The row whose chart was switched on or off (its checkbox, or a double click on the row).
    pub charted: Option<u8>,
}

/// Draws the hop table of `route` (just the header without one) with the latency bars; at most `limit` hops are
/// shown, those after the first hop beyond it are folded into one row. Hop names come from `names`, the `selected` hop is
/// highlighted, the `charted` hops have their checkbox ticked, `focus` labels the period under the bars. The
/// columns can be dragged wider or narrower and hidden from the header's right-click menu; `columns` keeps them.
/// With `blocked` (a firewall on this computer may keep the answers to TCP probes away) the silent hops say so.
#[expect(clippy::too_many_arguments, reason = "the table's inputs, all needed every frame")]
pub fn show(
    ui: &mut Ui,
    route: Option<&Route>,
    limit: u8,
    names: &NameCache,
    selected: Option<u8>,
    charted: &[u8],
    focus: Option<&str>,
    blocked: bool,
    columns: &mut ColumnWidths,
) -> Clicks {
    let (head, tail) = route.map_or((&[][..], &[][..]), |route| route.shown(limit));
    let hidden = route.map_or(0, |route| route.hidden(limit));
    let target_hop = route.and_then(|route| route.hops().last().filter(|hop| hop.address == Some(route.target())));
    let scale_ms = latency_bars::scale_ms(head.iter().chain(tail).map(|hop| &hop.stats), Zones::of(ui.ctx()));
    let mut averages: Vec<Pos2> = Vec::new();
    // Where the averages before the hidden row end: the line connecting them breaks there.
    let mut break_at = None;
    let mut clicks = Clicks::default();
    let mut hidden_cell = None;
    let mut body_top = f32::NEG_INFINITY;
    // One table state for all tabs: a column dragged in one tab is that wide in the others.
    columns::scope(ui, "hop-table", |ui| {
        // Default widths: the latency bars get their share, Name gets whatever the other shown columns leave.
        let available = ui.available_width();
        let latency_width = (available * LATENCY_SHARE).max(LATENCY_MIN_WIDTH);
        let language = ui.language();
        let number = |index: usize| {
            HEADERS
                .get(index)
                .map_or(NUMBER_WIDTH, |(title, ..)| number_width(ui, language.translate(title), NUMBER_WIDTH))
        };
        let mut defaults = vec![HOP_WIDTH, number(1), IP_WIDTH, 0.0];
        defaults.extend((4..HEADERS.len()).map(number));
        let others: f32 = defaults
            .iter()
            .enumerate()
            .filter(|(index, _)| columns.is_shown(*index))
            .map(|(_, width)| *width)
            .sum();
        if let Some(name) = defaults.get_mut(NAME_COLUMN) {
            *name = (available - others - latency_width).max(80.0);
        }
        let shown = columns.shown().to_vec();
        let language = ui.language();
        let builder = TableBuilder::new(ui)
            .striped(true)
            .sense(Sense::CLICK)
            .cell_layout(Layout::left_to_right(Align::Center));
        let mut header_widths = Vec::new();
        columns
            .columns(builder, defaults, available, LATENCY_MIN_WIDTH)
            .header(HEADER_HEIGHT, |mut header| {
                for (index, (title, numeric, hint)) in HEADERS.into_iter().enumerate() {
                    let cell = columns::cell(&mut header, &shown, index, |ui| {
                        header_widths.push(ui.max_rect().width());
                        let title = ui.language().translate(title);
                        header_cell(ui, index == 0, numeric, title, available);
                    });
                    if let Some(response) = cell {
                        let response = response.on_hover_text(language.translate(hint));
                        columns.menu(&response);
                    }
                }
                let (_, latency) = header.col(|ui| {
                    body_top = ui.max_rect().bottom();
                    latency_bars::header(ui, scale_ms);
                });
                columns.menu(&latency);
            })
            .body(|mut body| {
                for (index, hop) in head.iter().chain(tail).enumerate() {
                    if index == head.len() && hidden > 0 {
                        let text = hidden_text(language, hidden, limit);
                        body.row(ROW_HEIGHT, |mut row| hidden_cell = hidden_row(&mut row, &shown, &text));
                        break_at = Some(averages.len());
                    }
                    body.row(ROW_HEIGHT, |mut row| {
                        row.set_selected(selected == Some(hop.ttl));
                        let mut ticked = charted.contains(&hop.ttl);
                        averages.extend(hop_row(&mut row, &shown, hop, names, scale_ms, blocked, &mut ticked));
                        let response = row.response();
                        if ticked != charted.contains(&hop.ttl) || response.double_clicked() {
                            clicks.charted = Some(hop.ttl);
                        } else if response.clicked() {
                            clicks.selected = Some(hop.ttl);
                        }
                    });
                }
                if let Some(hop) = target_hop {
                    body.row(ROW_HEIGHT, |mut row| round_trip_row(&mut row, &shown, &hop.stats, focus));
                }
            });
        columns.take_header(&header_widths);
        if let Some(cell) = hidden_cell {
            paint_hidden(ui, cell, available, body_top, hidden, limit);
        }
        let (before, after) = averages.split_at(break_at.unwrap_or(0));
        latency_bars::connect(ui, before);
        latency_bars::connect(ui, after);
    });
    clicks
}

/// Fills in the row of one hop (the `shown` columns), with its chart checkbox `ticked`; a silent hop says a
/// firewall may be why if `blocked`. Returns the position of its average latency marker, if drawn.
fn hop_row(
    row: &mut TableRow<'_, '_>,
    shown: &[bool],
    hop: &Hop,
    names: &NameCache,
    scale_ms: f64,
    blocked: bool,
    ticked: &mut bool,
) -> Option<Pos2> {
    columns::cell(row, shown, 0, |ui| {
        ui.add_space(HEADER_INSET);
        ui.checkbox(ticked, "").on_hover_text(tr!(ui, "Show the chart below"));
        badge(ui, hop);
    });
    columns::cell(row, shown, 1, |ui| cell(ui, true, hop.stats.sent().to_string()));
    match hop.address {
        Some(ip) => {
            // The column fits an IPv4 address; a longer IPv6 one is clipped, so the whole address is on hover.
            if let Some(address) = columns::cell(row, shown, 2, |ui| cell(ui, false, ip.to_string())) {
                address.on_hover_text(ip.to_string());
            }
            // Empty while the lookup runs, with lookups off, and without a PTR record (the lookup gives back the
            // address then): the IP is in its own column.
            let name = names.get(ip).filter(|name| *name != ip.to_string());
            let name_cell =
                columns::cell(row, shown, NAME_COLUMN, |ui| cell(ui, false, name.clone().unwrap_or_default()));
            if let (Some(name_cell), Some(name)) = (name_cell, name) {
                name_cell.on_hover_text(name);
            }
        }
        None => {
            columns::cell(row, shown, 2, |ui| cell(ui, false, "*"));
            columns::cell(row, shown, NAME_COLUMN, |ui| {
                let silent = if blocked { tr!(ui, "Blocked by a firewall?") } else { tr!(ui, "No response") };
                cell(ui, false, silent)
            });
        }
    }
    stat_cols(row, shown, &hop.stats, false);
    let mut average = None;
    row.col(|ui| average = latency_bars::cell(ui, &hop.stats, hop.address.is_some(), scale_ms));
    average
}

/// The text of the row standing for the `hidden` hops of a route longer than `limit`.
pub fn hidden_text(language: Language, hidden: usize, limit: u8) -> String {
    let count = u64::try_from(hidden).unwrap_or(u64::MAX);
    i18n::fill(
        &trn!(language, count, "{n} hop hidden by the limit of {limit}|{n} hops hidden by the limit of {limit}"),
        &[("limit", limit.to_string())],
    )
}

/// Takes the row standing for the hidden hops of a route: empty cells in the `shown` columns, painted over by
/// [`paint_hidden`] once the table is drawn; its first cell carries `text` for screen readers, as the painted text
/// is only pixels. Returns the rect of its first cell.
fn hidden_row(row: &mut TableRow<'_, '_>, shown: &[bool], text: &str) -> Option<Rect> {
    let mut first = None;
    let (_, response) = row.col(|ui| first = Some(ui.max_rect()));
    response.widget_info(|| eframe::egui::WidgetInfo::labeled(eframe::egui::Role::Label, true, text));
    // The first (Hop) column is always shown; the latency column too.
    for _ in shown.iter().skip(1).filter(|shown| **shown) {
        row.col(|_| {});
    }
    row.col(|_| {});
    first
}

/// Paints the row of the `hidden` hops (after the first) of a route longer than `limit` over `cell` (its first cell): one
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
    let text = hidden_text(ui.language(), hidden, limit);
    painter.text(
        Pos2::new(band.left() + HEADER_INSET, band.center().y),
        Align2::LEFT_CENTER,
        text,
        TextStyle::Body.resolve(ui.style()),
        palette.ink_muted,
    );
}

/// Fills the summary row of the whole round trip (the target hop), with the focus period under the bars.
/// The row reads as a summary, not another hop: a header-colored band with a rule above it. Its title sits in the
/// Name column, which can't be hidden.
fn round_trip_row(row: &mut TableRow<'_, '_>, shown: &[bool], stats: &HopStats, focus: Option<&str>) {
    let emphasis = |text: String| RichText::new(text).font(theme::heading_font(14.5));
    columns::cell(row, shown, 0, summary_band);
    columns::cell(row, shown, 1, |ui| {
        summary_band(ui);
        cell(ui, true, emphasis(stats.sent().to_string()));
    });
    columns::cell(row, shown, 2, summary_band);
    columns::cell(row, shown, NAME_COLUMN, |ui| {
        summary_band(ui);
        cell(ui, true, emphasis(tr!(ui, "Round trip (ms)").to_owned()));
    });
    stat_cols(row, shown, stats, true);
    row.col(|ui| {
        summary_band(ui);
        if let Some(focus) = focus {
            cell(ui, true, RichText::new(focus).small().color(theme::palette(ui).ink_muted));
        }
    });
}

/// Default width of a numeric column titled `title`: `least`, or more where the title (a translation may
/// be longer) wouldn't fit.
pub fn number_width(ui: &Ui, title: &str, least: f32) -> f32 {
    let font = TextStyle::Body.resolve(ui.style());
    let text = ui.painter().layout_no_wrap(title.to_owned(), font, theme::palette(ui).ink_muted);
    least.max((text.size().x + HEADER_INSET * 2.0 + 2.0).ceil())
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

/// Fills the Avg, Min, Max, Cur, Jitter, PL% and PL pcs columns that are `shown`; `summary` gives them the summary
/// row's band.
fn stat_cols(row: &mut TableRow<'_, '_>, shown: &[bool], stats: &HopStats, summary: bool) {
    let band = |ui: &mut Ui| {
        if summary {
            summary_band(ui);
        }
    };
    for (position, value) in (4..).zip([stats.average(), stats.best(), stats.worst()]) {
        columns::cell(row, shown, position, |ui| {
            band(ui);
            cell(ui, true, format_ms(value, ui.language()));
        });
    }
    columns::cell(row, shown, 7, |ui| {
        band(ui);
        let current = match stats.last() {
            Some(rtt) => format_ms(Some(rtt), ui.language()),
            None if stats.sent() > 0 => "*".to_owned(),
            None => format_ms(None, ui.language()),
        };
        cell(ui, true, current);
    });
    columns::cell(row, shown, 8, |ui| {
        band(ui);
        cell(ui, true, format_ms(stats.jitter(), ui.language()));
    });
    let loss = stats.loss_percent();
    let colored = |ui: &Ui, text: String| loss_colored(ui, loss, text);
    columns::cell(row, shown, 9, |ui| {
        band(ui);
        let text = loss_text(ui, loss);
        cell(ui, true, text);
    });
    columns::cell(row, shown, 10, |ui| {
        band(ui);
        let text = colored(ui, stats.lost().to_string());
        cell(ui, true, text);
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

/// `loss` (percent) as a table shows it, colored by how bad it is.
pub fn loss_text(ui: &Ui, loss: f64) -> RichText {
    loss_colored(ui, loss, ui.language().decimal(loss))
}

/// `text` about a `loss` (percent), colored by how bad that loss is; plain without loss.
fn loss_colored(ui: &Ui, loss: f64, text: String) -> RichText {
    let text = RichText::new(text);
    if loss > 0.0 { text.color(LossLevel::of(loss).color(theme::palette(ui))) } else { text }
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
