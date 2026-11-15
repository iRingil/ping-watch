//! The target field: a text field for a host name or IP address with an arrow that opens the saved target list
//! (pick, edit, delete). Used by target tabs and by the All targets page.

use eframe::egui::{
    self, Button, CornerRadius, Key, Margin, Popup, PopupCloseBehavior, Rect, RichText, ScrollArea, Sense, TextEdit,
    Ui, pos2,
};

use crate::i18n::{tr, trf};
use crate::store::history::HistoryError;
use crate::store::targets::{Family, SavedTarget, TargetList};
use crate::ui::icons::{self, Icon};
use crate::ui::theme;

/// Width of the target field.
const ENTRY_WIDTH: f32 = 360.0;
/// Width of the arrow area at the right end of the target field.
const ARROW_WIDTH: f32 = 28.0;
/// Width of the target list: wider than the field, so a full IPv6 address and the description fit side by side.
const TARGET_LIST_WIDTH: f32 = 600.0;
/// Height of a target list row (also the size of its icon buttons).
const TARGET_ROW_HEIGHT: f32 = 28.0;
/// Tallest the target list grows before it scrolls.
const TARGET_LIST_HEIGHT: f32 = 380.0;

/// State of a target field.
#[derive(Debug, Default)]
pub struct TargetEntry {
    /// Text in the field.
    text: String,
    /// Move keyboard focus to the field on the next frame.
    focus: bool,
    /// Saved target being edited.
    editing: Option<Draft>,
    /// Saved target whose delete cross was clicked once: the list asks before deleting.
    confirm_delete: Option<i64>,
    /// Why the last delete failed.
    problem: Option<String>,
}

impl TargetEntry {
    /// An empty field; `focus` puts the keyboard focus into it on the first frame.
    pub fn new(focus: bool) -> Self {
        Self { focus, ..Self::default() }
    }

    /// Text in the field.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Replaces the text in the field.
    pub fn set_text(&mut self, text: &str) {
        text.clone_into(&mut self.text);
    }

    /// Empties the field.
    pub fn clear(&mut self) {
        self.text.clear();
    }

    /// Draws the field (disabled unless `enabled`) with its list arrow; with a `problem` (the host couldn't be
    /// resolved) the field is outlined in red and tells it on hover, next to where the host was typed. Returns
    /// whether the user asked to trace the text: pressed Enter in the field or picked a list entry (which replaces
    /// the text).
    pub fn show(&mut self, ui: &mut Ui, targets: Option<&TargetList>, enabled: bool, problem: Option<&str>) -> bool {
        // Room on the right of the field for the arrow that opens the target list.
        let field = TextEdit::singleline(&mut self.text)
            .hint_text(tr!(ui, "Enter target name or IP"))
            .desired_width(ENTRY_WIDTH)
            .margin(Margin { left: 6, right: ARROW_WIDTH as i8, top: 3, bottom: 3 });
        let mut input = ui.add_enabled(enabled, field);
        if let Some(problem) = problem {
            ui.painter().rect_stroke(
                input.rect,
                CornerRadius::same(3),
                egui::Stroke::new(1.5, theme::palette(ui).bad),
                egui::StrokeKind::Inside,
            );
            input = input.on_hover_text(RichText::new(problem).color(theme::palette(ui).bad));
        }
        if std::mem::take(&mut self.focus) {
            input.request_focus();
        }
        let mut start = input.lost_focus() && ui.input(|input| input.key_pressed(Key::Enter));

        // The arrow is a button set into the field's right end: own face, a divider, a hover state.
        let arrow_rect =
            Rect::from_min_max(pos2(input.rect.right() - ARROW_WIDTH, input.rect.top()), input.rect.max).shrink(2.0);
        let arrow = ui
            .interact(arrow_rect, input.id.with("target-list"), Sense::click())
            .on_hover_text(tr!(ui, "Saved targets"));
        let widgets = &ui.visuals().widgets;
        let arrow_fill = if arrow.hovered() { widgets.hovered.weak_bg_fill } else { widgets.inactive.weak_bg_fill };
        ui.painter().rect_filled(arrow_rect, CornerRadius::same(2), arrow_fill);
        icons::paint(
            ui,
            arrow_rect.center(),
            Icon::ArrowDown,
            if arrow.hovered() { theme::palette(ui).ink } else { theme::palette(ui).ink_muted },
        );
        icons::focus_ring(ui, &arrow, arrow_rect);
        if enabled && let Some(targets) = targets {
            Popup::from_toggle_button_response(&arrow)
                .anchor(input.rect)
                .width(input.rect.width().max(TARGET_LIST_WIDTH))
                .close_behavior(PopupCloseBehavior::CloseOnClickOutside)
                .show(|ui| {
                    // Picking a target traces it right away.
                    if let Some(address) = self.target_list(ui, targets) {
                        self.text = address;
                        start = true;
                        ui.close();
                    }
                });
        }
        self.edit_dialog(ui.ctx(), targets);
        start && enabled
    }

    /// Rows of the saved target list: address on the left, name and IP version on the right, Edit and Delete.
    /// Returns the address picked.
    fn target_list(&mut self, ui: &mut Ui, targets: &TargetList) -> Option<String> {
        let all = match targets.all() {
            Ok(all) => all,
            Err(error) => {
                ui.colored_label(
                    theme::palette(ui).bad,
                    trf!(ui, "Cannot read the target list: {error}", error = error),
                );
                return None;
            }
        };
        if all.is_empty() {
            ui.weak(tr!(ui, "No saved targets. Every target you trace is added here."));
            return None;
        }
        let mut picked = None;
        if let Some(problem) = &self.problem {
            ui.colored_label(theme::palette(ui).bad, problem);
        }
        ScrollArea::vertical().max_height(TARGET_LIST_HEIGHT).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for target in all {
                let confirming = self.confirm_delete == Some(target.id);
                match target_row(ui, &target, confirming) {
                    RowAction::Pick => picked = Some(target.address.clone()),
                    RowAction::Edit => self.editing = Some(Draft::of(&target)),
                    // The first click on the cross asks; the second deletes.
                    RowAction::Delete if !confirming => self.confirm_delete = Some(target.id),
                    RowAction::Delete => {
                        self.confirm_delete = None;
                        self.problem = targets
                            .delete(target.id)
                            .inspect_err(|error| log::error!("cannot delete {}: {error}", target.address))
                            .err()
                            .map(|error| trf!(ui, "Not deleted: {error}", error = error));
                    }
                    RowAction::None => {}
                }
            }
        });
        picked
    }

    /// Dialog editing a saved target's address and name.
    fn edit_dialog(&mut self, ctx: &egui::Context, targets: Option<&TargetList>) {
        let (Some(draft), Some(targets)) = (&mut self.editing, targets) else {
            return;
        };
        let mut close = false;
        egui::Modal::new(egui::Id::unique("edit-target")).show(ctx, |ui| {
            ui.set_width(380.0);
            ui.label(RichText::new(tr!(ui, "Edit target")).font(theme::heading_font(17.0)));
            ui.add_space(6.0);
            egui::Grid::new("edit-target-fields").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
                ui.weak(tr!(ui, "Address"));
                ui.add(TextEdit::singleline(&mut draft.address).desired_width(f32::INFINITY));
                ui.end_row();
                ui.weak(tr!(ui, "Name"));
                ui.add(
                    TextEdit::singleline(&mut draft.name).hint_text(tr!(ui, "optional")).desired_width(f32::INFINITY),
                );
                ui.end_row();
            });
            if let Some(error) = &draft.error {
                ui.colored_label(theme::palette(ui).bad, error);
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let can_save = !draft.address.trim().is_empty();
                if ui.add_enabled(can_save, primary_button(ui, tr!(ui, "Save"))).clicked() {
                    match targets.update(draft.id, &draft.address, &draft.name) {
                        Ok(()) => close = true,
                        Err(error) if is_duplicate(&error) => {
                            draft.error = Some(tr!(ui, "This address is already in the list").to_owned());
                        }
                        Err(error) => draft.error = Some(trf!(ui, "Not saved: {error}", error = error)),
                    }
                }
                if ui.button(tr!(ui, "Cancel")).clicked() {
                    close = true;
                }
            });
        });
        if close {
            self.editing = None;
        }
    }
}

/// A saved target being edited.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Draft {
    /// Row id in the list.
    id: i64,
    /// Address as edited.
    address: String,
    /// Name as edited.
    name: String,
    /// Why the last save failed.
    error: Option<String>,
}

impl Draft {
    /// Draft of `target` as it is now.
    fn of(target: &SavedTarget) -> Self {
        Self { id: target.id, address: target.address.clone(), name: target.name.clone(), error: None }
    }
}

/// What a click on a target list row asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowAction {
    /// Nothing clicked.
    None,
    /// Put the address into the field.
    Pick,
    /// Open the edit dialog.
    Edit,
    /// Remove the entry.
    Delete,
}

/// Whether `error` is the list refusing a second entry with the same address.
fn is_duplicate(error: &HistoryError) -> bool {
    matches!(
        error,
        HistoryError::Db(rusqlite::Error::SqliteFailure(failure, _))
            if failure.code == rusqlite::ErrorCode::ConstraintViolation
    )
}

/// One row of the target list: the whole row highlights under the pointer and picks the address when clicked;
/// the pencil and cross icons on the right sit on top of it, so clicking them edits or deletes instead. While
/// `confirming`, the row asks whether to delete, and the cross deletes.
fn target_row(ui: &mut Ui, target: &SavedTarget, confirming: bool) -> RowAction {
    let size = egui::vec2(ui.available_width(), TARGET_ROW_HEIGHT);
    let (rect, row) = ui.allocate_exact_size(size, Sense::click());
    let icon = egui::Vec2::splat(TARGET_ROW_HEIGHT);
    let delete_rect = Rect::from_min_size(pos2(rect.right() - icon.x, rect.top()), icon);
    let edit_rect = delete_rect.translate(egui::vec2(-icon.x, 0.0));
    let edit = ui.interact(edit_rect, row.id.with("edit"), Sense::click()).on_hover_text(tr!(ui, "Edit"));
    let delete_hint = if confirming { tr!(ui, "Click again to delete") } else { tr!(ui, "Delete") };
    let delete = ui.interact(delete_rect, row.id.with("delete"), Sense::click()).on_hover_text(delete_hint);

    let painter = ui.painter();
    if row.hovered() || edit.hovered() || delete.hovered() {
        painter.rect_filled(rect, CornerRadius::same(3), theme::palette(ui).pane_header);
    }
    let font = egui::FontId::proportional(15.0);
    let text_y = rect.center().y;
    painter.text(
        pos2(rect.left() + 8.0, text_y),
        egui::Align2::LEFT_CENTER,
        &target.address,
        font.clone(),
        theme::palette(ui).ink,
    );
    let description_right = edit_rect.left() - 8.0;
    let (description, description_color) = if confirming {
        (tr!(ui, "Delete it? Click the cross again").to_owned(), theme::palette(ui).bad)
    } else {
        (target_description(target), theme::palette(ui).ink_muted)
    };
    painter.text(pos2(description_right, text_y), egui::Align2::RIGHT_CENTER, description, font, description_color);
    icons::paint(
        ui,
        edit_rect.center(),
        Icon::Pencil,
        if edit.hovered() { theme::palette(ui).signal } else { theme::palette(ui).ink_muted },
    );
    icons::paint(
        ui,
        delete_rect.center(),
        Icon::Cross,
        if delete.hovered() || confirming { theme::palette(ui).bad } else { theme::palette(ui).ink_muted },
    );
    icons::focus_ring(ui, &row, rect);
    icons::focus_ring(ui, &edit, edit_rect.shrink(2.0));
    icons::focus_ring(ui, &delete, delete_rect.shrink(2.0));

    if delete.clicked() {
        RowAction::Delete
    } else if edit.clicked() {
        RowAction::Edit
    } else if row.clicked() {
        RowAction::Pick
    } else {
        RowAction::None
    }
}

/// Right-hand text of a target list row: its name and IP version, whichever are known ("Office router, ipv4").
fn target_description(target: &SavedTarget) -> String {
    let family = target.family.map(Family::label);
    [Some(target.name.as_str()).filter(|name| !name.is_empty()), family]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(", ")
}

/// The main button of a group: signal-colored, so it reads as "the thing to press".
fn primary_button<'a>(ui: &Ui, text: &'a str) -> Button<'a> {
    let palette = theme::palette(ui);
    Button::new(RichText::new(text).color(palette.on_signal)).fill(palette.signal)
}
