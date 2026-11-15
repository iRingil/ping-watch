//! The look of the app: an instrument panel in graphite slate with warm "paper" ink, set in Barlow (a grotesk in
//! the spirit of DIN road signs, SIL Open Font License, embedded in the binary). Latency zones are the only
//! saturated colors, so green / amber / red always mean "fine / slow / bad"; the cool signal blue marks what the
//! user selected or focused.

use std::sync::Arc;

use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, Stroke, TextStyle,
};

/// Window background: the instrument chassis.
pub const CHASSIS: Color32 = Color32::from_rgb(0x1a, 0x1f, 0x29);
/// Fill of the framed panes (hop table, timeline).
pub const PANE: Color32 = Color32::from_rgb(0x21, 0x28, 0x35);
/// Pane headers and table header rows, a step lighter than the pane.
pub const PANE_HEADER: Color32 = Color32::from_rgb(0x29, 0x31, 0x40);
/// Inactive tabs: between the tab bar (WELL) and the open tab (CHASSIS).
pub const TAB_IDLE: Color32 = Color32::from_rgb(0x1d, 0x23, 0x2f);
/// Input fields and plot backgrounds, a step darker than the chassis.
pub const WELL: Color32 = Color32::from_rgb(0x14, 0x18, 0x20);
/// Hairlines: pane borders, grid, separators.
pub const RULE: Color32 = Color32::from_rgb(0x36, 0x40, 0x54);
/// Primary text: warm off-white, reads like ink on paper against the cool slate.
pub const INK: Color32 = Color32::from_rgb(0xdc, 0xd7, 0xcb);
/// Secondary text: column titles, hints, units.
pub const INK_MUTED: Color32 = Color32::from_rgb(0x8b, 0x93, 0xa5);
/// Selection, keyboard focus and the average markers.
pub const SIGNAL: Color32 = Color32::from_rgb(0x6f, 0xb7, 0xd9);
/// Signal blue under the pointer.
pub const SIGNAL_HOVER: Color32 = Color32::from_rgb(0x8f, 0xc9, 0xe4);

// The zone hues are turned a little off the usual traffic-light set (and PingPlotter's): green leans to jade, amber
// to ochre, red to rose. They still read as fine / slow / bad at a glance, but as this app's own.
/// Green zone background (latency below 100 ms): dark jade.
pub const GOOD_ZONE: Color32 = Color32::from_rgb(0x26, 0x4a, 0x42);
/// Amber zone background (100–200 ms): dark ochre.
pub const WARN_ZONE: Color32 = Color32::from_rgb(0x57, 0x42, 0x1f);
/// Red zone background (over 200 ms): dark rose.
pub const BAD_ZONE: Color32 = Color32::from_rgb(0x55, 0x28, 0x36);
/// Green marks: hop badge of a fast hop, legend.
pub const GOOD: Color32 = Color32::from_rgb(0x52, 0xb0, 0x94);
/// Amber marks: slow hop, paused.
pub const WARN: Color32 = Color32::from_rgb(0xe2, 0xa1, 0x3e);
/// Red marks: very slow hop, loss, errors, the current-latency dot.
pub const BAD: Color32 = Color32::from_rgb(0xe0, 0x5f, 0x74);

/// Text face, embedded so the app looks the same on any machine (license: `assets/fonts/OFL.txt`).
const BARLOW_REGULAR: &[u8] = include_bytes!("../assets/fonts/Barlow-Regular.ttf");
/// Heavier face for headings.
const BARLOW_MEDIUM: &[u8] = include_bytes!("../assets/fonts/Barlow-Medium.ttf");
/// Font family of headings and other emphasized labels.
const HEADING_FAMILY: &str = "heading";

/// Applies fonts, text sizes, colors and spacing to `ctx`.
pub fn apply(ctx: &egui::Context) {
    install_fonts(ctx);
    ctx.set_theme(egui::Theme::Dark);
    ctx.style_mut_of(egui::Theme::Dark, |style| {
        style.text_styles = [
            (TextStyle::Small, FontId::proportional(12.5)),
            (TextStyle::Body, FontId::proportional(15.0)),
            (TextStyle::Button, FontId::proportional(15.0)),
            (TextStyle::Heading, heading_font(17.0)),
            (TextStyle::Monospace, FontId::monospace(13.0)),
        ]
        .into();
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(10.0, 4.0);
        style.spacing.interact_size.y = 26.0;
        style.spacing.menu_margin = Margin::same(6);

        let visuals = &mut style.visuals;
        visuals.override_text_color = Some(INK);
        visuals.weak_text_color = Some(INK_MUTED);
        visuals.panel_fill = CHASSIS;
        visuals.window_fill = PANE;
        visuals.window_stroke = Stroke::new(1.0, RULE);
        visuals.window_corner_radius = CornerRadius::same(4);
        visuals.menu_corner_radius = CornerRadius::same(4);
        visuals.extreme_bg_color = WELL;
        visuals.text_edit_bg_color = Some(WELL);
        visuals.faint_bg_color = Color32::from_rgb(0x25, 0x2d, 0x3b);
        visuals.error_fg_color = BAD;
        visuals.warn_fg_color = WARN;
        visuals.hyperlink_color = SIGNAL;
        visuals.selection.bg_fill = Color32::from_rgb(0x2b, 0x4a, 0x5e);
        visuals.selection.stroke = Stroke::new(1.0, SIGNAL);

        let widgets = &mut visuals.widgets;
        widgets.noninteractive.bg_fill = PANE;
        widgets.noninteractive.weak_bg_fill = PANE;
        widgets.noninteractive.bg_stroke = Stroke::new(1.0, RULE);
        widgets.noninteractive.fg_stroke = Stroke::new(1.0, INK);
        for (state, fill, border) in [
            (&mut widgets.inactive, Color32::from_rgb(0x2a, 0x33, 0x44), RULE),
            (&mut widgets.hovered, Color32::from_rgb(0x33, 0x3e, 0x52), Color32::from_rgb(0x4b, 0x58, 0x72)),
            (&mut widgets.active, Color32::from_rgb(0x3a, 0x47, 0x5e), SIGNAL),
            (&mut widgets.open, Color32::from_rgb(0x33, 0x3e, 0x52), SIGNAL),
        ] {
            state.bg_fill = fill;
            state.weak_bg_fill = fill;
            state.bg_stroke = Stroke::new(1.0, border);
            state.fg_stroke = Stroke::new(1.0, INK);
            state.corner_radius = CornerRadius::same(3);
        }
    });
}

/// Puts Barlow first in the proportional family and makes Barlow Medium the heading family; egui's own fonts stay
/// as fallback for glyphs Barlow lacks (Cyrillic, symbols).
fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert("barlow".to_owned(), Arc::new(FontData::from_static(BARLOW_REGULAR)));
    fonts.font_data.insert("barlow-medium".to_owned(), Arc::new(FontData::from_static(BARLOW_MEDIUM)));
    let fallback = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    fonts.families.entry(FontFamily::Proportional).or_default().insert(0, "barlow".to_owned());
    let heading = std::iter::once("barlow-medium".to_owned()).chain(fallback).collect();
    fonts.families.insert(FontFamily::Name(HEADING_FAMILY.into()), heading);
    ctx.set_fonts(fonts);
}

/// Font of headings and emphasized labels at `size`.
pub fn heading_font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(HEADING_FAMILY.into()))
}

/// Frame of a pane: pane fill with a hairline border and square corners (the table header band runs edge to edge).
pub fn pane_frame() -> egui::Frame {
    egui::Frame::new().fill(PANE).stroke(Stroke::new(1.0, RULE))
}
