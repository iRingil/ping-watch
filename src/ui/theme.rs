//! The look of the app: an instrument panel set in Barlow (a grotesk in the spirit of DIN road signs, SIL Open Font
//! License, embedded in the binary), in two palettes — dark (graphite slate with warm "paper" ink) and light (the
//! same panel in daylight: pale cool slate with graphite ink). Latency zones are the only saturated colors, so
//! jade / ochre / rose always mean "fine / slow / bad"; the signal blue marks what the user selected or focused.

use std::sync::Arc;

use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, Stroke, TextStyle, Ui,
};

/// Every color the app paints with, by role. One instance per theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    /// Window background: the instrument chassis; the open tab and toolbars.
    pub chassis: Color32,
    /// Fill of the framed panes (hop table, timeline) and dialogs.
    pub pane: Color32,
    /// Pane headers and table header rows, a step away from the pane.
    pub pane_header: Color32,
    /// Inactive tabs.
    pub tab_idle: Color32,
    /// The tab bar and the status bar.
    pub bar: Color32,
    /// Input fields and plot backgrounds.
    pub well: Color32,
    /// Hairlines: pane borders, separators, the tab outlines.
    pub rule: Color32,
    /// Primary text and the latency line.
    pub ink: Color32,
    /// Secondary text: column titles, hints, units.
    pub ink_muted: Color32,
    /// Selection, keyboard focus, links and the average markers.
    pub signal: Color32,
    /// Signal blue under the pointer.
    pub signal_hover: Color32,
    /// Text on a signal-blue fill (the primary button).
    pub on_signal: Color32,
    /// Background of the selected row.
    pub selection: Color32,
    /// Stripes of striped tables.
    pub stripe: Color32,
    /// Widget fills: idle, under the pointer, pressed.
    pub widget: [Color32; 3],
    /// Border of a widget under the pointer.
    pub widget_hover_border: Color32,
    /// Green zone background (latency below 100 ms): jade.
    pub good_zone: Color32,
    /// Amber zone background (100–200 ms): ochre.
    pub warn_zone: Color32,
    /// Red zone background (over 200 ms): rose.
    pub bad_zone: Color32,
    /// Green marks: hop badge of a fast hop, legend.
    pub good: Color32,
    /// Amber marks: slow hop, paused.
    pub warn: Color32,
    /// Red marks: very slow hop, loss, errors, the current-latency dot.
    pub bad: Color32,
    /// Background of a hop that doesn't answer, and the loss bar in the latency column.
    pub lost: Color32,
    /// The min–max range in the latency column.
    pub range: Color32,
    /// The jitter line.
    pub jitter: Color32,
    /// Packet loss bars of the timeline.
    pub loss: Color32,
    /// Grid lines of the charts (premultiplied, drawn over the zones).
    pub grid: Color32,
    /// Comment markers on the timeline.
    pub comment: Color32,
}

/// The dark palette: graphite slate with warm "paper" ink.
pub const DARK: Palette = Palette {
    chassis: Color32::from_rgb(0x1a, 0x1f, 0x29),
    pane: Color32::from_rgb(0x21, 0x28, 0x35),
    pane_header: Color32::from_rgb(0x29, 0x31, 0x40),
    tab_idle: Color32::from_rgb(0x1d, 0x23, 0x2f),
    bar: Color32::from_rgb(0x14, 0x18, 0x20),
    well: Color32::from_rgb(0x14, 0x18, 0x20),
    rule: Color32::from_rgb(0x36, 0x40, 0x54),
    ink: Color32::from_rgb(0xdc, 0xd7, 0xcb),
    ink_muted: Color32::from_rgb(0x8b, 0x93, 0xa5),
    signal: Color32::from_rgb(0x6f, 0xb7, 0xd9),
    signal_hover: Color32::from_rgb(0x8f, 0xc9, 0xe4),
    on_signal: Color32::from_rgb(0x1a, 0x1f, 0x29),
    selection: Color32::from_rgb(0x2b, 0x4a, 0x5e),
    stripe: Color32::from_rgb(0x25, 0x2d, 0x3b),
    widget: [
        Color32::from_rgb(0x2a, 0x33, 0x44),
        Color32::from_rgb(0x33, 0x3e, 0x52),
        Color32::from_rgb(0x3a, 0x47, 0x5e),
    ],
    widget_hover_border: Color32::from_rgb(0x4b, 0x58, 0x72),
    good_zone: Color32::from_rgb(0x26, 0x4a, 0x42),
    warn_zone: Color32::from_rgb(0x57, 0x42, 0x1f),
    bad_zone: Color32::from_rgb(0x55, 0x28, 0x36),
    good: Color32::from_rgb(0x52, 0xb0, 0x94),
    warn: Color32::from_rgb(0xe2, 0xa1, 0x3e),
    bad: Color32::from_rgb(0xe0, 0x5f, 0x74),
    lost: Color32::from_rgb(0x69, 0x2b, 0x3c),
    range: Color32::from_rgb(0xa9, 0xae, 0xb8),
    jitter: Color32::from_rgb(0xff, 0x8a, 0x65),
    loss: Color32::from_rgb(0xd6, 0x45, 0x5e),
    grid: Color32::from_rgba_premultiplied(0x15, 0x16, 0x19, 0x26),
    comment: Color32::from_rgb(0xff, 0xd5, 0x4f),
};

/// The light palette: the same panel in daylight — pale cool slate (not cream), graphite ink, a deeper signal blue,
/// zones as pale washes with darker marks so they keep their contrast on light ground.
pub const LIGHT: Palette = Palette {
    chassis: Color32::from_rgb(0xe4, 0xe7, 0xeb),
    pane: Color32::from_rgb(0xf5, 0xf6, 0xf8),
    pane_header: Color32::from_rgb(0xe8, 0xeb, 0xef),
    tab_idle: Color32::from_rgb(0xd9, 0xdd, 0xe3),
    bar: Color32::from_rgb(0xcf, 0xd4, 0xdb),
    well: Color32::from_rgb(0xff, 0xff, 0xff),
    rule: Color32::from_rgb(0xbd, 0xc4, 0xcf),
    ink: Color32::from_rgb(0x1f, 0x25, 0x30),
    ink_muted: Color32::from_rgb(0x5b, 0x65, 0x75),
    signal: Color32::from_rgb(0x1f, 0x7d, 0xab),
    signal_hover: Color32::from_rgb(0x2b, 0x91, 0xc4),
    on_signal: Color32::from_rgb(0xff, 0xff, 0xff),
    selection: Color32::from_rgb(0xcd, 0xe2, 0xee),
    stripe: Color32::from_rgb(0xec, 0xee, 0xf2),
    widget: [
        Color32::from_rgb(0xe6, 0xe9, 0xee),
        Color32::from_rgb(0xd9, 0xde, 0xe6),
        Color32::from_rgb(0xcb, 0xd3, 0xde),
    ],
    widget_hover_border: Color32::from_rgb(0x94, 0xa1, 0xb3),
    good_zone: Color32::from_rgb(0xcd, 0xe8, 0xde),
    warn_zone: Color32::from_rgb(0xf3, 0xe1, 0xbd),
    bad_zone: Color32::from_rgb(0xf3, 0xd1, 0xda),
    good: Color32::from_rgb(0x1f, 0x86, 0x6a),
    warn: Color32::from_rgb(0xb3, 0x74, 0x15),
    bad: Color32::from_rgb(0xc4, 0x35, 0x4d),
    lost: Color32::from_rgb(0xec, 0xc3, 0xcd),
    range: Color32::from_rgb(0x5f, 0x69, 0x78),
    jitter: Color32::from_rgb(0xd2, 0x5e, 0x26),
    loss: Color32::from_rgb(0xd0, 0x45, 0x5e),
    grid: Color32::from_rgba_premultiplied(0x00, 0x00, 0x00, 0x16),
    comment: Color32::from_rgb(0xa8, 0x7a, 0x00),
};

/// The palette of the theme `ui` is drawn in.
pub fn palette(ui: &Ui) -> &'static Palette {
    of(ui.ctx())
}

/// The palette of the theme `ctx` is drawn in.
pub fn of(ctx: &egui::Context) -> &'static Palette {
    match ctx.theme() {
        egui::Theme::Dark => &DARK,
        egui::Theme::Light => &LIGHT,
    }
}

/// Which theme the user chose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThemeChoice {
    /// Follow the Windows app theme.
    #[default]
    Auto,
    /// Always dark.
    Dark,
    /// Always light.
    Light,
}

impl ThemeChoice {
    /// All choices in menu order.
    pub const ALL: [Self; 3] = [Self::Auto, Self::Dark, Self::Light];

    /// Stable key for storing the choice.
    pub fn key(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Dark => "dark",
            Self::Light => "light",
        }
    }

    /// The choice stored under `key`, if it is one.
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|choice| choice.key() == key)
    }

    /// Makes `ctx` draw in this theme (Auto follows Windows as it changes).
    pub fn apply(self, ctx: &egui::Context) {
        ctx.set_theme(match self {
            Self::Auto => egui::ThemePreference::System,
            Self::Dark => egui::ThemePreference::Dark,
            Self::Light => egui::ThemePreference::Light,
        });
    }
}

/// Text face, embedded so the app looks the same on any machine (license: `assets/fonts/OFL.txt`).
const BARLOW_REGULAR: &[u8] = include_bytes!("../../assets/fonts/Barlow-Regular.ttf");
/// Heavier face for headings.
const BARLOW_MEDIUM: &[u8] = include_bytes!("../../assets/fonts/Barlow-Medium.ttf");
/// Font family of headings and other emphasized labels.
const HEADING_FAMILY: &str = "heading";

/// Applies fonts, text sizes, spacing and both palettes to `ctx`; [`ThemeChoice::apply`] picks the one shown.
pub fn apply(ctx: &egui::Context) {
    install_fonts(ctx);
    for (theme, palette) in [(egui::Theme::Dark, &DARK), (egui::Theme::Light, &LIGHT)] {
        ctx.style_mut_of(theme, |style| style_with(style, palette));
    }
}

/// Sets text sizes, spacing and the colors of `palette` on `style`.
fn style_with(style: &mut egui::Style, palette: &Palette) {
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
    visuals.override_text_color = Some(palette.ink);
    visuals.weak_text_color = Some(palette.ink_muted);
    visuals.panel_fill = palette.chassis;
    visuals.window_fill = palette.pane;
    visuals.window_stroke = Stroke::new(1.0, palette.rule);
    visuals.window_corner_radius = CornerRadius::same(4);
    visuals.menu_corner_radius = CornerRadius::same(4);
    visuals.extreme_bg_color = palette.well;
    visuals.text_edit_bg_color = Some(palette.well);
    visuals.faint_bg_color = palette.stripe;
    visuals.error_fg_color = palette.bad;
    visuals.warn_fg_color = palette.warn;
    visuals.hyperlink_color = palette.signal;
    visuals.selection.bg_fill = palette.selection;
    visuals.selection.stroke = Stroke::new(1.0, palette.signal);

    let widgets = &mut visuals.widgets;
    widgets.noninteractive.bg_fill = palette.pane;
    widgets.noninteractive.weak_bg_fill = palette.pane;
    widgets.noninteractive.bg_stroke = Stroke::new(1.0, palette.rule);
    widgets.noninteractive.fg_stroke = Stroke::new(1.0, palette.ink);
    let [idle, hovered, pressed] = palette.widget;
    for (state, fill, border) in [
        (&mut widgets.inactive, idle, palette.rule),
        (&mut widgets.hovered, hovered, palette.widget_hover_border),
        (&mut widgets.active, pressed, palette.signal),
        (&mut widgets.open, hovered, palette.signal),
    ] {
        state.bg_fill = fill;
        state.weak_bg_fill = fill;
        state.bg_stroke = Stroke::new(1.0, border);
        state.fg_stroke = Stroke::new(1.0, palette.ink);
        state.corner_radius = CornerRadius::same(3);
    }
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

/// Frame of a pane in `ui`'s theme: pane fill with a hairline border and square corners (the table header band
/// runs edge to edge).
pub fn pane_frame(ui: &Ui) -> egui::Frame {
    let palette = palette(ui);
    egui::Frame::new().fill(palette.pane).stroke(Stroke::new(1.0, palette.rule))
}
