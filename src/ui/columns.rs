//! Column widths of a table that the user can drag: until the user moves a column border the table keeps its
//! default widths, which follow the window; after that the user's widths stay as they are and are kept between
//! runs. The target tabs share one set (one hop table look for all of them), the overview page has its own.

use eframe::egui::{Id, Stroke, Ui, UiBuilder};
use egui_extras::{Column, TableBuilder};

use crate::store::window_state::WindowStore;

/// Narrowest a column can be dragged.
const MIN_WIDTH: f32 = 24.0;
/// Header widths differing from the defaults by more than this were set by the user.
const TOLERANCE: f32 = 0.5;

/// Widths of a table's resizable columns (all but the last one, which takes what is left).
#[derive(Debug)]
pub struct ColumnWidths {
    /// Name of the table in the store.
    name: &'static str,
    /// Number of resizable columns.
    count: usize,
    /// The user's widths, `None` while the defaults are used.
    widths: Option<Vec<f32>>,
    /// Widths as last stored.
    saved: Option<Vec<f32>>,
    /// Width available to the table in the last frame.
    available: f32,
    /// Default widths passed to the table in the last frame.
    defaults: Vec<f32>,
}

impl ColumnWidths {
    /// Widths of table `name` with `count` resizable columns, as stored in `store` (ignored unless they fit).
    pub fn load(name: &'static str, count: usize, store: Option<&WindowStore>) -> Self {
        let stored = store.and_then(|store| store.load_columns(name).inspect_err(|error| log::warn!("{error}")).ok());
        Self::new(name, count, stored)
    }

    /// Widths of table `name` with `count` resizable columns, starting from `stored` if they fit.
    fn new(name: &'static str, count: usize, stored: Option<Vec<f32>>) -> Self {
        let stored = stored.filter(|widths| {
            widths.len() == count && widths.iter().all(|width| width.is_finite() && *width >= MIN_WIDTH)
        });
        Self { name, count, widths: stored.clone(), saved: stored, available: f32::NAN, defaults: Vec::new() }
    }

    /// Adds the columns to `builder`: the resizable ones with the user's widths or `defaults`, then the last one
    /// taking the rest, at least `last_min` wide. While the defaults are used, a change of `available` (the
    /// table's width) drops the widths egui keeps, so the defaults follow the window.
    pub fn columns<'a>(
        &mut self,
        builder: TableBuilder<'a>,
        defaults: Vec<f32>,
        available: f32,
        last_min: f32,
    ) -> TableBuilder<'a> {
        let window_changed = (available - self.available).abs() > TOLERANCE || self.available.is_nan();
        if self.widths.is_none() && window_changed {
            builder.reset();
        }
        self.available = available;
        self.defaults = defaults;
        let widths = self.widths.as_ref().unwrap_or(&self.defaults);
        let builder = widths.iter().fold(builder, |builder, width| {
            builder.column(Column::initial(*width).at_least(MIN_WIDTH).resizable(true).clip(true))
        });
        builder.column(Column::remainder().at_least(last_min).clip(true))
    }

    /// Takes the widths the header shows (`header`, every column's), as egui keeps them; once they differ from
    /// the defaults the user has dragged a border.
    pub fn take_header(&mut self, header: &[f32]) {
        let Some(header) = header.get(..self.count) else {
            return;
        };
        let differs = header.len() != self.defaults.len()
            || header.iter().zip(&self.defaults).any(|(shown, default)| (shown - default).abs() > TOLERANCE);
        if self.widths.is_some() || differs {
            self.widths = Some(header.to_vec());
        }
    }

    /// Stores the user's widths in `store` when they changed, once `dragging` (the mouse button held) ends.
    pub fn save(&mut self, store: Option<&mut WindowStore>, dragging: bool) {
        if dragging || self.widths == self.saved {
            return;
        }
        if let (Some(store), Some(widths)) = (store, &self.widths)
            && let Err(error) = store.save_columns(self.name, widths)
        {
            log::warn!("cannot save the column widths: {error}");
        }
        // One attempt per change, not one per frame.
        self.saved.clone_from(&self.widths);
    }
}

/// Runs `add_table` in a `Ui` whose id is `id` wherever it is drawn, so the tabs share one table state. Columns
/// have no gaps between them: a column's drag handle then lies on the divider the header draws, cells keep their
/// own inset, and row bands (summary, header) can be painted cell by cell without holes — striped rows repaint
/// each cell's background, so a band drawn once across the row gets covered. Column borders show only under the
/// mouse.
pub fn scope<R>(ui: &mut Ui, id: &str, add_table: impl FnOnce(&mut Ui) -> R) -> R {
    ui.scope_builder(UiBuilder::new().scope_id(Id::unique(id)), |ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        ui.visuals_mut().widgets.noninteractive.bg_stroke = Stroke::NONE;
        add_table(ui)
    })
    .inner
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_widths_are_used_only_if_they_fit() {
        assert_eq!(ColumnWidths::new("t", 2, Some(vec![50.0, 60.0])).widths, Some(vec![50.0, 60.0]));
        assert_eq!(ColumnWidths::new("t", 3, Some(vec![50.0, 60.0])).widths, None);
        assert_eq!(ColumnWidths::new("t", 2, Some(vec![50.0, f32::NAN])).widths, None);
        assert_eq!(ColumnWidths::new("t", 2, Some(vec![50.0, 1.0])).widths, None);
    }

    #[test]
    fn defaults_stay_until_the_header_differs() {
        let mut columns = ColumnWidths::new("t", 2, None);
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
        let mut columns = ColumnWidths::new("t", 1, None);
        columns.defaults = vec![50.0];
        columns.take_header(&[70.0, 300.0]);
        columns.save(None, true);
        assert_eq!(columns.saved, None);
        columns.save(None, false);
        assert_eq!(columns.saved, Some(vec![70.0]));
    }
}
