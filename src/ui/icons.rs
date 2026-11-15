//! Painted icons and the icon buttons built from them. Icons are drawn with shapes, not glyphs: the emoji glyphs
//! of egui's fallback font render as colored boxes.

use eframe::egui::{
    self, Color32, CornerRadius, Pos2, Rect, Response, Sense, Stroke, StrokeKind, Ui, Vec2, pos2, vec2,
};

use crate::ui::theme;

/// What an icon shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    /// Triangle pointing right: start, resume.
    Play,
    /// Two bars: pause.
    Pause,
    /// Triangle pointing down: open a list.
    ArrowDown,
    /// Pencil: edit.
    Pencil,
    /// Diagonal cross: delete, close.
    Cross,
}

/// Look of an icon button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    /// The main action of a group: signal-blue fill, dark icon.
    Primary,
    /// An ordinary button: widget fill and border.
    Normal,
}

/// Size of a toolbar icon button: square, as tall as other widgets.
pub fn button_size(ui: &Ui) -> Vec2 {
    Vec2::splat(ui.spacing().interact_size.y)
}

/// Paints `icon` centered on `center` in `color`.
pub fn paint(ui: &Ui, center: Pos2, icon: Icon, color: Color32) {
    let painter = ui.painter();
    let c = center;
    match icon {
        Icon::Play => {
            let points = vec![pos2(c.x - 4.0, c.y - 6.0), pos2(c.x + 6.0, c.y), pos2(c.x - 4.0, c.y + 6.0)];
            painter.add(egui::Shape::convex_polygon(points, color, Stroke::NONE));
        }
        Icon::Pause => {
            for x in [c.x - 3.5, c.x + 3.5] {
                let bar = Rect::from_center_size(pos2(x, c.y), vec2(3.0, 11.0));
                painter.rect_filled(bar, CornerRadius::same(1), color);
            }
        }
        Icon::ArrowDown => {
            let points = vec![pos2(c.x - 5.0, c.y - 2.5), pos2(c.x + 5.0, c.y - 2.5), pos2(c.x, c.y + 3.5)];
            painter.add(egui::Shape::convex_polygon(points, color, Stroke::NONE));
        }
        Icon::Pencil => {
            // Body from the tip (bottom left) to the end (top right), the tip as a small wedge.
            painter.line_segment([pos2(c.x - 3.0, c.y + 3.0), pos2(c.x + 4.5, c.y - 4.5)], Stroke::new(3.0, color));
            let tip = vec![pos2(c.x - 6.0, c.y + 6.0), pos2(c.x - 4.6, c.y + 1.8), pos2(c.x - 1.8, c.y + 4.6)];
            painter.add(egui::Shape::convex_polygon(tip, color, Stroke::NONE));
        }
        Icon::Cross => {
            let stroke = Stroke::new(1.8, color);
            painter.line_segment([pos2(c.x - 4.5, c.y - 4.5), pos2(c.x + 4.5, c.y + 4.5)], stroke);
            painter.line_segment([pos2(c.x - 4.5, c.y + 4.5), pos2(c.x + 4.5, c.y - 4.5)], stroke);
        }
    }
}

/// Fill and icon color of a button face in `style`, lit when hovered.
fn colors(ui: &Ui, style: Style, hovered: bool) -> (Color32, Color32) {
    let widgets = &ui.visuals().widgets;
    match style {
        Style::Primary => (if hovered { theme::SIGNAL_HOVER } else { theme::SIGNAL }, theme::CHASSIS),
        Style::Normal => {
            let fill = if hovered { widgets.hovered.weak_bg_fill } else { widgets.inactive.weak_bg_fill };
            (fill, theme::INK)
        }
    }
}

/// A square button showing `icon`, with `tooltip` on hover.
pub fn button(ui: &mut Ui, icon: Icon, style: Style, tooltip: &str) -> Response {
    let (rect, response) = ui.allocate_exact_size(button_size(ui), Sense::click());
    let (fill, icon_color) = colors(ui, style, response.hovered());
    ui.painter()
        .rect(rect, CornerRadius::same(3), fill, Stroke::new(1.0, theme::RULE), StrokeKind::Inside);
    paint(ui, rect.center(), icon, icon_color);
    response.on_hover_text(tooltip)
}

/// Responses of the two halves of a split button.
#[derive(Debug, Clone)]
pub struct Split {
    /// The main action (left half).
    pub main: Response,
    /// The arrow opening the list of other actions (right half).
    pub menu: Response,
}

/// A split button: the left half does the main action (`icon`, `tooltip`), the narrow right half opens a list of
/// other actions; one frame around both, a divider between.
pub fn split_button(ui: &mut Ui, icon: Icon, icon_color: Color32, tooltip: &str) -> Split {
    let height = ui.spacing().interact_size.y;
    let main_width = height + 6.0;
    let menu_width = height * 0.75;
    let (rect, _) = ui.allocate_exact_size(vec2(main_width + menu_width, height), Sense::hover());
    let main_rect = Rect::from_min_size(rect.min, vec2(main_width, height));
    let menu_rect = Rect::from_min_max(pos2(main_rect.right(), rect.top()), rect.max);
    let main = ui.interact(main_rect, ui.next_auto_id().with("split-main"), Sense::click()).on_hover_text(tooltip);
    let menu = ui.interact(menu_rect, main.id.with("split-menu"), Sense::click()).on_hover_text("More actions");

    let painter = ui.painter();
    let (main_fill, _) = colors(ui, Style::Normal, main.hovered());
    let (menu_fill, _) = colors(ui, Style::Normal, menu.hovered());
    painter.rect_filled(main_rect, CornerRadius { nw: 3, sw: 3, ne: 0, se: 0 }, main_fill);
    painter.rect_filled(menu_rect, CornerRadius { nw: 0, sw: 0, ne: 3, se: 3 }, menu_fill);
    painter.rect_stroke(rect, CornerRadius::same(3), Stroke::new(1.0, theme::RULE), StrokeKind::Inside);
    painter.vline(main_rect.right(), rect.y_range().shrink(4.0), Stroke::new(1.0, theme::RULE));
    paint(ui, main_rect.center(), icon, icon_color);
    paint(ui, menu_rect.center(), Icon::ArrowDown, if menu.hovered() { theme::INK } else { theme::INK_MUTED });
    Split { main, menu }
}

/// A menu entry drawn as a row: the whole row lights up under the pointer, the label on the left, the keyboard
/// shortcut dimmed on the right. Takes the menu's full width.
pub fn menu_row(ui: &mut Ui, label: &str, shortcut: &str) -> Response {
    let size = vec2(ui.available_width(), ui.spacing().interact_size.y + 2.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let painter = ui.painter();
    if response.hovered() {
        painter.rect_filled(rect, CornerRadius::same(3), theme::PANE_HEADER);
    }
    let font = egui::FontId::proportional(15.0);
    painter.text(pos2(rect.left() + 8.0, rect.center().y), egui::Align2::LEFT_CENTER, label, font.clone(), theme::INK);
    painter.text(
        pos2(rect.right() - 8.0, rect.center().y),
        egui::Align2::RIGHT_CENTER,
        shortcut,
        font,
        theme::INK_MUTED,
    );
    response
}
