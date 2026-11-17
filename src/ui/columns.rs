//! Columns of a table that the user can drag and hide: until the user moves a column border the table keeps its
//! default widths, which follow the window; after that the user's widths stay as they are, until the user asks for
//! the defaults again. A right click on the header shows or hides columns; the Columns page of the settings does the
//! same from the keyboard. Widths and hidden columns are kept between runs. The target tabs share one set (one hop
//! table look for all of them), the overview page has its own.

use eframe::egui::{Button, Grid, Id, Response, Stroke, Ui, UiBuilder};
use egui_extras::{Column, TableBuilder, TableRow};

use crate::i18n::{Lang, tr};
use crate::store::window_state::WindowStore;

/// Narrowest a column can be dragged.
const MIN_WIDTH: f32 = 24.0;
/// Header widths differing from the defaults by more than this were set by the user.
const TOLERANCE: f32 = 0.5;

/// Widths and visibility of a table's resizable columns (all but the last one, which takes what is left and is
/// always shown).
#[derive(Debug)]
pub struct ColumnWidths {
    /// Name of the table in the store.
    name: &'static str,
    /// Titles of the resizable columns (marked texts), by position.
    titles: Vec<&'static str>,
    /// Positions of the columns that can't be hidden.
    fixed: &'static [usize],
    /// Number of resizable columns.
    count: usize,
    /// The user's widths of all resizable columns, hidden ones included; `None` while the defaults are used.
    widths: Option<Vec<f32>>,
    /// Widths as last stored.
    saved: Option<Vec<f32>>,
    /// Whether each resizable column is shown, by position.
    shown: Vec<bool>,
    /// Visibility as last stored.
    saved_shown: Vec<bool>,
    /// Visibility the table was last built with: a change drops the widths egui keeps for the old set.
    built_shown: Vec<bool>,
    /// Width available to the table in the last frame.
    available: f32,
    /// Default widths of all resizable columns passed to the table in the last frame.
    defaults: Vec<f32>,
}

impl ColumnWidths {
    /// Columns of table `name` with resizable columns titled `titles` (marked texts), those at the `fixed`
    /// positions always shown, as stored in `store` (widths ignored unless they fit).
    pub fn load(
        name: &'static str,
        titles: &[&'static str],
        fixed: &'static [usize],
        store: Option<&WindowStore>,
    ) -> Self {
        let stored = store.and_then(|store| store.load_columns(name).inspect_err(|error| log::warn!("{error}")).ok());
        let hidden = store
            .and_then(|store| store.load_hidden_columns(name).inspect_err(|error| log::warn!("{error}")).ok())
            .unwrap_or_default();
        Self::new(name, titles, fixed, stored, &hidden)
    }

    /// Columns of table `name` with resizable columns titled `titles`, starting from `stored` widths if they fit,
    /// with the columns at the `hidden` positions hidden unless they are `fixed`.
    fn new(
        name: &'static str,
        titles: &[&'static str],
        fixed: &'static [usize],
        stored: Option<Vec<f32>>,
        hidden: &[usize],
    ) -> Self {
        let count = titles.len();
        let stored = stored.filter(|widths| {
            widths.len() == count && widths.iter().all(|width| width.is_finite() && *width >= MIN_WIDTH)
        });
        let shown: Vec<bool> =
            (0..count).map(|position| fixed.contains(&position) || !hidden.contains(&position)).collect();
        Self {
            name,
            titles: titles.to_vec(),
            fixed,
            count,
            widths: stored.clone(),
            saved: stored,
            saved_shown: shown.clone(),
            built_shown: shown.clone(),
            shown,
            available: f32::NAN,
            defaults: Vec::new(),
        }
    }

    /// Whether the resizable column at `position` is shown.
    pub fn is_shown(&self, position: usize) -> bool {
        self.shown.get(position).copied().unwrap_or(false)
    }

    /// Whether each resizable column is shown, by position.
    pub fn shown(&self) -> &[bool] {
        &self.shown
    }

    /// Adds the columns to `builder`: the shown resizable ones with the user's widths or `defaults` (one per
    /// resizable column), then the last one taking the rest, at least `last_min` wide. While the defaults are
    /// used, a change of `available` (the table's width) drops the widths egui keeps, so the defaults follow the
    /// window; so does showing or hiding a column.
    pub fn columns<'a>(
        &mut self,
        builder: TableBuilder<'a>,
        defaults: Vec<f32>,
        available: f32,
        last_min: f32,
    ) -> TableBuilder<'a> {
        let window_changed = (available - self.available).abs() > TOLERANCE || self.available.is_nan();
        if (self.widths.is_none() && window_changed) || self.built_shown != self.shown {
            builder.reset();
        }
        self.built_shown.clone_from(&self.shown);
        self.available = available;
        self.defaults = defaults;
        let widths = self.widths.as_ref().unwrap_or(&self.defaults);
        let builder =
            widths.iter().zip(&self.shown).filter(|(_, shown)| **shown).fold(builder, |builder, (width, _)| {
                builder.column(Column::initial(*width).at_least(MIN_WIDTH).resizable(true).clip(true))
            });
        builder.column(Column::remainder().at_least(last_min).clip(true))
    }

    /// Takes the widths the header shows (`header`, every shown column's), as egui keeps them; once they differ
    /// from the defaults the user has dragged a border.
    pub fn take_header(&mut self, header: &[f32]) {
        let positions: Vec<usize> = (0..self.count).filter(|position| self.is_shown(*position)).collect();
        let (Some(header), true) = (header.get(..positions.len()), self.defaults.len() == self.count) else {
            return;
        };
        let differs = positions.iter().zip(header).any(|(position, shown)| {
            self.defaults.get(*position).is_none_or(|default| (shown - default).abs() > TOLERANCE)
        });
        if self.widths.is_none() && !differs {
            return;
        }
        let mut widths = self.widths.clone().unwrap_or_else(|| self.defaults.clone());
        for (position, shown) in positions.iter().zip(header) {
            if let Some(width) = widths.get_mut(*position) {
                *width = *shown;
            }
        }
        self.widths = Some(widths);
    }

    /// The header's right-click menu on `response` (a header cell): the [`choices`](Self::choices) in one column.
    pub fn menu(&mut self, response: &Response) {
        response.context_menu(|ui| self.choices(ui, 1));
    }

    /// A checkbox per column that can be hidden, laid out in `across` columns, and a button bringing back the
    /// default widths (enabled once the user has dragged a border).
    pub fn choices(&mut self, ui: &mut Ui, across: usize) {
        let positions: Vec<usize> = (0..self.count).filter(|position| !self.fixed.contains(position)).collect();
        Grid::new(self.name).num_columns(across).show(ui, |ui| {
            for row in positions.chunks(across.max(1)) {
                for position in row {
                    if let (Some(title), Some(shown)) = (self.titles.get(*position), self.shown.get_mut(*position)) {
                        ui.checkbox(shown, ui.language().translate(title));
                    }
                }
                ui.end_row();
            }
        });
        ui.separator();
        ui.horizontal(|ui| {
            let reset = ui.add_enabled(self.widths.is_some(), Button::new(tr!(ui, "Default widths")));
            if reset.on_hover_text(tr!(ui, "Column widths that follow the window again")).clicked() {
                self.reset_widths();
            }
        });
    }

    /// Drops the user's widths: the defaults are used again, following the window, from the next frame.
    fn reset_widths(&mut self) {
        self.widths = None;
        // A changed width makes `columns` reset the widths egui keeps.
        self.available = f32::NAN;
    }

    /// Stores the user's widths and the hidden columns in `store` when they changed, the widths once `dragging`
    /// (the mouse button held) ends.
    pub fn save(&mut self, mut store: Option<&mut WindowStore>, dragging: bool) {
        if self.shown != self.saved_shown {
            let hidden: Vec<usize> = (0..self.count).filter(|position| !self.is_shown(*position)).collect();
            if let Some(store) = store.as_deref_mut()
                && let Err(error) = store.save_hidden_columns(self.name, &hidden)
            {
                log::warn!("cannot save the hidden columns: {error}");
            }
            self.saved_shown.clone_from(&self.shown);
        }
        if dragging || self.widths == self.saved {
            return;
        }
        if let Some(store) = store {
            let saved = match &self.widths {
                Some(widths) => store.save_columns(self.name, widths),
                None => store.save_columns(self.name, &[]),
            };
            if let Err(error) = saved {
                log::warn!("cannot save the column widths: {error}");
            }
        }
        // One attempt per change, not one per frame.
        self.saved.clone_from(&self.widths);
    }
}

/// Adds the cell of the resizable column at `position` to `row` with `add`, if the column is shown (`shown`, by
/// position); returns the cell's response then.
pub fn cell(
    row: &mut TableRow<'_, '_>,
    shown: &[bool],
    position: usize,
    add: impl FnOnce(&mut Ui),
) -> Option<Response> {
    shown.get(position).copied().unwrap_or(false).then(|| row.col(add).1)
}

/// Runs `add_table` in a `Ui` whose id is `id` wherever it is drawn, so the tabs share one table state. Columns
/// have no gaps between them: a column's drag handle then lies on the divider the header draws, cells keep their
/// own inset, and row bands (summary, header) can be painted cell by cell without holes — striped rows repaint
/// each cell's background, so a band drawn once across the row gets covered. Column borders show only under the
/// mouse.
pub fn scope<R>(ui: &mut Ui, id: &str, add_table: impl FnOnce(&mut Ui) -> R) -> R {
    ui.scope_builder(UiBuilder::new().scope_id(Id::unique(id)), |ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        // The tables are read, not copied from (a snapshot is for that): a click selects a row, not text, and a
        // right click on a header title opens the column menu like one beside it.
        ui.style_mut().interaction.selectable_labels = false;
        ui.visuals_mut().widgets.noninteractive.bg_stroke = Stroke::NONE;
        add_table(ui)
    })
    .inner
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Titles of the test columns.
    const TITLES: [&str; 3] = ["A", "B", "C"];

    #[test]
    fn stored_widths_are_used_only_if_they_fit() {
        assert_eq!(
            ColumnWidths::new("t", &TITLES[..2], &[], Some(vec![50.0, 60.0]), &[]).widths,
            Some(vec![50.0, 60.0])
        );
        assert_eq!(ColumnWidths::new("t", &TITLES[..3], &[], Some(vec![50.0, 60.0]), &[]).widths, None);
        assert_eq!(ColumnWidths::new("t", &TITLES[..2], &[], Some(vec![50.0, f32::NAN]), &[]).widths, None);
        assert_eq!(ColumnWidths::new("t", &TITLES[..2], &[], Some(vec![50.0, 1.0]), &[]).widths, None);
    }

    #[test]
    fn defaults_stay_until_the_header_differs() {
        let mut columns = ColumnWidths::new("t", &TITLES[..2], &[], None, &[]);
        columns.defaults = vec![50.0, 60.0];
        columns.take_header(&[50.0, 60.2, 300.0]);
        assert_eq!(columns.widths, None);
        columns.take_header(&[50.0, 80.0, 280.0]);
        assert_eq!(columns.widths, Some(vec![50.0, 80.0]));
        // Once the user's, the widths follow the header even back to the defaults.
        columns.take_header(&[50.0, 60.0, 300.0]);
        assert_eq!(columns.widths, Some(vec![50.0, 60.0]));
    }

    #[test]
    fn saved_once_dragging_ends() {
        let mut columns = ColumnWidths::new("t", &TITLES[..1], &[], None, &[]);
        columns.defaults = vec![50.0];
        columns.take_header(&[70.0, 300.0]);
        columns.save(None, true);
        assert_eq!(columns.saved, None);
        columns.save(None, false);
        assert_eq!(columns.saved, Some(vec![70.0]));
    }

    #[test]
    fn hidden_columns_keep_their_widths() {
        let mut columns = ColumnWidths::new("t", &TITLES[..3], &[], None, &[1]);
        assert_eq!(columns.shown(), &[true, false, true]);
        columns.defaults = vec![50.0, 60.0, 70.0];
        // The header has the shown columns only, then the last one.
        columns.take_header(&[50.0, 70.0, 300.0]);
        assert_eq!(columns.widths, None);
        columns.take_header(&[55.0, 70.0, 300.0]);
        assert_eq!(columns.widths, Some(vec![55.0, 60.0, 70.0]));
    }

    #[test]
    fn fixed_columns_are_always_shown() {
        let columns = ColumnWidths::new("t", &TITLES, &[0], None, &[0, 2]);
        assert_eq!(columns.shown(), &[true, true, false]);
    }

    #[test]
    fn default_widths_come_back_and_are_stored_as_none() {
        let mut columns = ColumnWidths::new("t", &TITLES[..1], &[], Some(vec![70.0]), &[]);
        columns.reset_widths();
        assert_eq!(columns.widths, None);
        columns.save(None, false);
        assert_eq!(columns.saved, None);
    }
}
