//! The window's state kept between runs: position and size (of the normal, not maximized window), whether it was
//! maximized or minimized, the split between the hop table and the timeline, and the open tabs (their targets, which
//! one was shown, whether each was tracing). Stored in the history database.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};

use crate::history::HistoryError;

/// Schema: window geometry as key/value rows, and the open tabs in order.
const SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS window_state (
        key   TEXT PRIMARY KEY,
        value REAL NOT NULL
    ) WITHOUT ROWID;
    CREATE TABLE IF NOT EXISTS open_tabs (
        position INTEGER PRIMARY KEY,
        host     TEXT NOT NULL,
        tracing  INTEGER NOT NULL, -- 1 tracing, 0 paused or not started
        shown    INTEGER NOT NULL  -- 1 for the tab that was shown
    );
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
    /// Whether it was the shown tab.
    pub shown: bool,
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
        if let Some(folder) = path.parent() {
            std::fs::create_dir_all(folder)?;
        }
        Self::init(Connection::open(path)?)
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
        let mut statement = self.connection.prepare("SELECT host, tracing, shown FROM open_tabs ORDER BY position")?;
        let rows = statement.query_map([], |row| {
            Ok(SavedTab { host: row.get(0)?, tracing: row.get::<_, i64>(1)? != 0, shown: row.get::<_, i64>(2)? != 0 })
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
                "INSERT INTO open_tabs (position, host, tracing, shown) VALUES (?1, ?2, ?3, ?4)",
                params![position, tab.host, tab.tracing, tab.shown],
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
            SavedTab { host: "9.9.9.9".to_owned(), tracing: true, shown: false },
            SavedTab { host: "github.com".to_owned(), tracing: false, shown: true },
        ];
        store.save_tabs(&tabs).expect("saved");
        assert_eq!(store.load_tabs().ok(), Some(tabs.clone()));
        store.save_tabs(&tabs[..1]).expect("saved fewer");
        assert_eq!(store.load_tabs().map(|loaded| loaded.len()).ok(), Some(1));
    }

    #[test]
    fn implausible_geometry_is_detected() {
        assert!(state().is_plausible());
        assert!(!WindowState { width: 10.0, ..state() }.is_plausible());
        assert!(!WindowState { x: 90_000.0, ..state() }.is_plausible());
        assert!(!WindowState { height: f32::NAN, ..state() }.is_plausible());
    }
}
