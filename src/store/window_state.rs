//! The window's state kept between runs: position and size (of the normal, not maximized window), whether it was
//! maximized or minimized, the split between the hop table and the timeline, the open tabs (their targets, which
//! one was shown, whether each was tracing or charted on the overview page), the overview page's own settings and
//! the column widths the user set. Stored in the history database.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};

use crate::store::history::{self, HistoryError};
use crate::store::settings::offered_interval;
use crate::store::targets::TraceSettings;
use crate::ui::focus::Focus;
use crate::ui::timeline::Period;

/// Schema: window geometry as key/value rows, the open tabs in order, the overview page's settings (one row) and
/// the column widths of each table.
const SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS window_state (
        key   TEXT PRIMARY KEY,
        value REAL NOT NULL
    ) WITHOUT ROWID;
    CREATE TABLE IF NOT EXISTS open_tabs (
        position INTEGER PRIMARY KEY,
        host     TEXT NOT NULL,
        tracing  INTEGER NOT NULL, -- 1 tracing, 0 paused or not started
        charted  INTEGER NOT NULL, -- 1 if charted on the overview page
        shown    INTEGER NOT NULL  -- 1 for the tab that was shown
    );
    CREATE TABLE IF NOT EXISTS all_targets (
        id          INTEGER PRIMARY KEY CHECK (id = 1),
        interval_ms INTEGER NOT NULL,
        focus       TEXT    NOT NULL,
        period_s    INTEGER NOT NULL,
        table_share REAL    NOT NULL
    );
    CREATE TABLE IF NOT EXISTS column_widths (
        table_name TEXT    NOT NULL,
        position   INTEGER NOT NULL,
        width      REAL    NOT NULL,
        PRIMARY KEY (table_name, position)
    ) WITHOUT ROWID;
";
/// Stored keys, in the order of [`WindowState::values`].
const KEYS: [&str; 7] = ["x", "y", "width", "height", "maximized", "minimized", "table_share"];
/// Coordinates beyond this (logical points) are treated as garbage, e.g. from a monitor no longer attached.
const MAX_COORDINATE: f32 = 20_000.0;

/// Everything remembered about the window.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowState {
    /// Left edge of the normal window, logical points.
    pub x: f32,
    /// Top edge of the normal window, logical points.
    pub y: f32,
    /// Inner width of the normal window, logical points.
    pub width: f32,
    /// Inner height of the normal window, logical points.
    pub height: f32,
    /// Whether the window was maximized.
    pub maximized: bool,
    /// Whether the window was minimized.
    pub minimized: bool,
    /// Share of the pane area taken by the hop table.
    pub table_share: f32,
}

impl WindowState {
    /// The values in the order of [`KEYS`].
    fn values(self) -> [f32; 7] {
        [
            self.x,
            self.y,
            self.width,
            self.height,
            f32::from(u8::from(self.maximized)),
            f32::from(u8::from(self.minimized)),
            self.table_share,
        ]
    }

    /// Whether the stored geometry is usable: a sane size and position.
    pub fn is_plausible(&self) -> bool {
        let finite = self.values().iter().all(|value| value.is_finite());
        finite
            && self.width >= 200.0
            && self.height >= 150.0
            && self.x.abs() < MAX_COORDINATE
            && self.y.abs() < MAX_COORDINATE
    }
}

/// An open tab as remembered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedTab {
    /// Target host as entered.
    pub host: String,
    /// Whether it was tracing (not paused).
    pub tracing: bool,
    /// Whether it was charted on the overview page.
    pub charted: bool,
    /// Whether it was the shown tab.
    pub shown: bool,
}

/// Settings of the overview page, kept apart from those of the target tabs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AllTargetsSettings {
    /// Interval (set on every trace when changed), focus of the table and period of the charts.
    pub trace: TraceSettings,
    /// Share of the pane area taken by the table while charts are shown.
    pub table_share: f32,
}

/// Connection to the stored window state.
#[derive(Debug)]
pub struct WindowStore {
    /// Open SQLite connection (the history database).
    connection: Connection,
}

impl WindowStore {
    /// Opens the store in the database at `path`, creating the table if needed.
    ///
    /// # Errors
    ///
    /// [`HistoryError`] if the database can't be opened or prepared.
    pub fn open(path: &Path) -> Result<Self, HistoryError> {
        Self::init(history::connect(path)?)
    }

    /// Opens a private store in memory.
    #[cfg(test)]
    fn open_in_memory() -> Result<Self, HistoryError> {
        Self::init(Connection::open_in_memory()?)
    }

    /// Creates the table.
    fn init(connection: Connection) -> Result<Self, HistoryError> {
        connection.execute_batch(SCHEMA)?;
        Ok(Self { connection })
    }

    /// The stored state, `None` before the first save or if anything is missing.
    ///
    /// # Errors
    ///
    /// [`HistoryError::Db`] if SQLite fails.
    pub fn load(&self) -> Result<Option<WindowState>, HistoryError> {
        let mut values = [0.0_f32; 7];
        for (value, key) in values.iter_mut().zip(KEYS) {
            let stored: Option<f64> = self
                .connection
                .query_row("SELECT value FROM window_state WHERE key = ?1", [key], |row| row.get(0))
                .optional()?;
            let Some(stored) = stored else {
                return Ok(None);
            };
            #[expect(clippy::cast_possible_truncation, reason = "stored from f32 values")]
            let narrowed = stored as f32;
            *value = narrowed;
        }
        let [x, y, width, height, maximized, minimized, table_share] = values;
        Ok(Some(WindowState {
            x,
            y,
            width,
            height,
            maximized: maximized > 0.5,
            minimized: minimized > 0.5,
            table_share,
        }))
    }

    /// The remembered tabs in order; empty if none were open.
    ///
    /// # Errors
    ///
    /// [`HistoryError::Db`] if SQLite fails.
    pub fn load_tabs(&self) -> Result<Vec<SavedTab>, HistoryError> {
        let mut statement =
            self.connection.prepare("SELECT host, tracing, charted, shown FROM open_tabs ORDER BY position")?;
        let rows = statement.query_map([], |row| {
            Ok(SavedTab {
                host: row.get(0)?,
                tracing: row.get::<_, i64>(1)? != 0,
                charted: row.get::<_, i64>(2)? != 0,
                shown: row.get::<_, i64>(3)? != 0,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Replaces the remembered tabs with `tabs`.
    ///
    /// # Errors
    ///
    /// [`HistoryError::Db`] if SQLite fails; then nothing is changed.
    pub fn save_tabs(&mut self, tabs: &[SavedTab]) -> Result<(), HistoryError> {
        let transaction = self.connection.transaction()?;
        transaction.execute("DELETE FROM open_tabs", [])?;
        for (position, tab) in (0_i64..).zip(tabs) {
            transaction.execute(
                "INSERT INTO open_tabs (position, host, tracing, charted, shown) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![position, tab.host, tab.tracing, tab.charted, tab.shown],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// The overview page's settings, `None` before the first save or if a stored value is unknown.
    ///
    /// # Errors
    ///
    /// [`HistoryError::Db`] if SQLite fails.
    pub fn load_all_targets(&self) -> Result<Option<AllTargetsSettings>, HistoryError> {
        let row: Option<(i64, String, i64, f64)> = self
            .connection
            .query_row("SELECT interval_ms, focus, period_s, table_share FROM all_targets WHERE id = 1", [], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .optional()?;
        let Some((interval_ms, focus, period_s, table_share)) = row else {
            return Ok(None);
        };
        let interval = u64::try_from(interval_ms).ok().and_then(offered_interval);
        let focus = Focus::from_key(&focus);
        let period = u64::try_from(period_s).ok().and_then(Period::from_seconds);
        #[expect(clippy::cast_possible_truncation, reason = "stored from an f32")]
        let table_share = table_share as f32;
        Ok(interval.zip(focus).zip(period).map(|((interval, focus), period)| AllTargetsSettings {
            trace: TraceSettings { interval, focus, period },
            table_share,
        }))
    }

    /// Stores the overview page's settings.
    ///
    /// # Errors
    ///
    /// [`HistoryError::Db`] if SQLite fails.
    pub fn save_all_targets(&self, settings: AllTargetsSettings) -> Result<(), HistoryError> {
        let interval_ms = i64::try_from(settings.trace.interval.as_millis()).unwrap_or(i64::MAX);
        let period_s = i64::try_from(settings.trace.period.seconds()).unwrap_or(i64::MAX);
        self.connection.execute(
            "INSERT INTO all_targets (id, interval_ms, focus, period_s, table_share) VALUES (1, ?1, ?2, ?3, ?4)
             ON CONFLICT (id) DO UPDATE SET interval_ms = excluded.interval_ms, focus = excluded.focus,
                 period_s = excluded.period_s, table_share = excluded.table_share",
            params![interval_ms, settings.trace.focus.key(), period_s, f64::from(settings.table_share)],
        )?;
        Ok(())
    }

    /// Column widths of table `name` in column order, empty before the first save.
    ///
    /// # Errors
    ///
    /// [`HistoryError::Db`] if SQLite fails.
    pub fn load_columns(&self, name: &str) -> Result<Vec<f32>, HistoryError> {
        let mut statement =
            self.connection.prepare("SELECT width FROM column_widths WHERE table_name = ?1 ORDER BY position")?;
        let widths = statement.query_map([name], |row| row.get::<_, f64>(0))?.collect::<Result<Vec<_>, _>>()?;
        #[expect(clippy::cast_possible_truncation, reason = "stored from an f32")]
        Ok(widths.into_iter().map(|width| width as f32).collect())
    }

    /// Stores `widths` as the column widths of table `name`, replacing the previous ones.
    ///
    /// # Errors
    ///
    /// [`HistoryError::Db`] if SQLite fails; then nothing is stored.
    pub fn save_columns(&mut self, name: &str, widths: &[f32]) -> Result<(), HistoryError> {
        let transaction = self.connection.transaction()?;
        transaction.execute("DELETE FROM column_widths WHERE table_name = ?1", [name])?;
        for (position, width) in widths.iter().enumerate() {
            let position = i64::try_from(position).unwrap_or(i64::MAX);
            transaction.execute(
                "INSERT INTO column_widths (table_name, position, width) VALUES (?1, ?2, ?3)",
                params![name, position, f64::from(*width)],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// Stores `state`.
    ///
    /// # Errors
    ///
    /// [`HistoryError::Db`] if SQLite fails; then nothing is stored.
    pub fn save(&mut self, state: WindowState) -> Result<(), HistoryError> {
        let transaction = self.connection.transaction()?;
        for (key, value) in KEYS.into_iter().zip(state.values()) {
            transaction.execute(
                "INSERT INTO window_state (key, value) VALUES (?1, ?2)
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                params![key, f64::from(value)],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// A plausible state.
    fn state() -> WindowState {
        WindowState {
            x: 120.0,
            y: -8.0,
            width: 1300.0,
            height: 820.0,
            maximized: false,
            minimized: true,
            table_share: 0.6,
        }
    }

    #[test]
    fn nothing_stored_at_first() {
        let store = WindowStore::open_in_memory().expect("store opens");
        assert_eq!(store.load().ok(), Some(None));
    }

    #[test]
    fn state_round_trips() {
        let mut store = WindowStore::open_in_memory().expect("store opens");
        store.save(state()).expect("saved");
        let changed = WindowState { maximized: true, ..state() };
        store.save(changed).expect("saved again");
        assert_eq!(store.load().ok(), Some(Some(changed)));
    }

    #[test]
    fn tabs_round_trip_in_order() {
        let mut store = WindowStore::open_in_memory().expect("store opens");
        assert_eq!(store.load_tabs().ok(), Some(Vec::new()));
        let tabs = vec![
            SavedTab { host: "9.9.9.9".to_owned(), tracing: true, charted: true, shown: false },
            SavedTab { host: "github.com".to_owned(), tracing: false, charted: false, shown: true },
        ];
        store.save_tabs(&tabs).expect("saved");
        assert_eq!(store.load_tabs().ok(), Some(tabs.clone()));
        store.save_tabs(&tabs[..1]).expect("saved fewer");
        assert_eq!(store.load_tabs().map(|loaded| loaded.len()).ok(), Some(1));
    }

    #[test]
    fn all_targets_settings_round_trip() {
        let store = WindowStore::open_in_memory().expect("store opens");
        assert_eq!(store.load_all_targets().ok(), Some(None));
        let settings = AllTargetsSettings {
            trace: TraceSettings {
                interval: Duration::from_millis(500),
                focus: Focus::Minutes5,
                period: Period::Minutes10,
            },
            table_share: 0.5,
        };
        store.save_all_targets(settings).expect("saved");
        let changed = AllTargetsSettings { table_share: 0.7, ..settings };
        store.save_all_targets(changed).expect("saved again");
        assert_eq!(store.load_all_targets().ok(), Some(Some(changed)));
    }

    #[test]
    fn column_widths_round_trip_per_table() {
        let mut store = WindowStore::open_in_memory().expect("store opens");
        assert_eq!(store.load_columns("hops").ok(), Some(Vec::new()));
        store.save_columns("hops", &[40.0, 58.5, 130.0]).expect("saved");
        store.save_columns("targets", &[120.0]).expect("saved");
        store.save_columns("hops", &[44.0, 60.0]).expect("saved again");
        assert_eq!(store.load_columns("hops").ok(), Some(vec![44.0, 60.0]));
        assert_eq!(store.load_columns("targets").ok(), Some(vec![120.0]));
    }

    #[test]
    fn implausible_geometry_is_detected() {
        assert!(state().is_plausible());
        assert!(!WindowState { width: 10.0, ..state() }.is_plausible());
        assert!(!WindowState { x: 90_000.0, ..state() }.is_plausible());
        assert!(!WindowState { height: f32::NAN, ..state() }.is_plausible());
    }
}
