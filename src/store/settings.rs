//! The app's settings (the Settings window): what the close button does, the values new targets start with, probe
//! timeout and hop limit, how long history is kept. Stored as key/value rows in the history database; a missing or
//! unreadable value falls back to its default.

use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, params};

use crate::store::history::HistoryError;
use crate::store::targets::TraceSettings;
use crate::ui::focus::Focus;
use crate::ui::timeline::Period;

/// Schema: one row per setting.
const SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS app_settings (
        key   TEXT PRIMARY KEY,
        value TEXT NOT NULL
    ) WITHOUT ROWID;
";

/// Probe timeouts offered.
pub const TIMEOUTS: [Duration; 4] =
    [Duration::from_millis(500), Duration::from_secs(1), Duration::from_secs(2), Duration::from_secs(3)];
/// Hop limits offered.
pub const MAX_HOPS: [u8; 3] = [20, 30, 40];
/// One day.
const DAY: Duration = Duration::from_secs(24 * 60 * 60);
/// History retention periods offered.
pub const RETENTIONS: [Duration; 3] =
    [DAY, Duration::from_secs(3 * 24 * 60 * 60), Duration::from_secs(7 * 24 * 60 * 60)];

/// All settings of the Settings window that live in the database.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppSettings {
    /// The window's close button hides it to the tray instead of quitting.
    pub close_to_tray: bool,
    /// Interval, focus and timeline period of a target traced for the first time.
    pub defaults: TraceSettings,
    /// How long a probe waits for its answer.
    pub timeout: Duration,
    /// Largest TTL probed.
    pub max_hops: u8,
    /// How long samples are kept.
    pub retention: Duration,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            close_to_tray: false,
            defaults: TraceSettings {
                interval: Duration::from_secs(1),
                focus: Focus::default(),
                period: Period::default(),
            },
            timeout: Duration::from_secs(1),
            max_hops: 30,
            // The longest period the timeline shows.
            retention: Duration::from_secs(7 * 24 * 60 * 60),
        }
    }
}

/// Connection to the stored settings.
#[derive(Debug)]
pub struct SettingsStore {
    /// Open SQLite connection (the history database).
    connection: Connection,
}

impl SettingsStore {
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

    /// The stored value of `key`.
    fn get(&self, key: &str) -> Result<Option<String>, HistoryError> {
        Ok(self
            .connection
            .query_row("SELECT value FROM app_settings WHERE key = ?1", [key], |row| row.get(0))
            .optional()?)
    }

    /// The stored settings; each missing or unknown value is its default.
    ///
    /// # Errors
    ///
    /// [`HistoryError::Db`] if SQLite fails.
    pub fn load(&self) -> Result<AppSettings, HistoryError> {
        let fallback = AppSettings::default();
        let number = |key: &str| -> Result<Option<u64>, HistoryError> {
            Ok(self.get(key)?.and_then(|value| value.parse().ok()))
        };
        Ok(AppSettings {
            close_to_tray: self.get("close_to_tray")?.map_or(fallback.close_to_tray, |value| value == "1"),
            defaults: TraceSettings {
                interval: number("interval_ms")?.map_or(fallback.defaults.interval, Duration::from_millis),
                focus: self.get("focus")?.and_then(|key| Focus::from_key(&key)).unwrap_or(fallback.defaults.focus),
                period: number("period_s")?.and_then(Period::from_seconds).unwrap_or(fallback.defaults.period),
            },
            timeout: number("timeout_ms")?
                .map(Duration::from_millis)
                .filter(|timeout| TIMEOUTS.contains(timeout))
                .unwrap_or(fallback.timeout),
            max_hops: number("max_hops")?
                .and_then(|hops| u8::try_from(hops).ok())
                .filter(|hops| MAX_HOPS.contains(hops))
                .unwrap_or(fallback.max_hops),
            retention: number("retention_s")?
                .map(Duration::from_secs)
                .filter(|retention| RETENTIONS.contains(retention))
                .unwrap_or(fallback.retention),
        })
    }

    /// Stores `settings`.
    ///
    /// # Errors
    ///
    /// [`HistoryError::Db`] if SQLite fails; then nothing is stored.
    pub fn save(&mut self, settings: &AppSettings) -> Result<(), HistoryError> {
        let millis = |duration: Duration| duration.as_millis().to_string();
        let values = [
            ("close_to_tray", if settings.close_to_tray { "1".to_owned() } else { "0".to_owned() }),
            ("interval_ms", millis(settings.defaults.interval)),
            ("focus", settings.defaults.focus.key().to_owned()),
            ("period_s", settings.defaults.period.seconds().to_string()),
            ("timeout_ms", millis(settings.timeout)),
            ("max_hops", settings.max_hops.to_string()),
            ("retention_s", settings.retention.as_secs().to_string()),
        ];
        let transaction = self.connection.transaction()?;
        for (key, value) in values {
            transaction.execute(
                "INSERT INTO app_settings (key, value) VALUES (?1, ?2)
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_before_the_first_save() {
        let store = SettingsStore::open_in_memory().expect("store opens");
        assert_eq!(store.load().ok(), Some(AppSettings::default()));
    }

    #[test]
    fn settings_round_trip() {
        let mut store = SettingsStore::open_in_memory().expect("store opens");
        let settings = AppSettings {
            close_to_tray: true,
            defaults: TraceSettings {
                interval: Duration::from_millis(2_500),
                focus: Focus::Minutes5,
                period: Period::Minutes10,
            },
            timeout: Duration::from_secs(2),
            max_hops: 40,
            retention: DAY,
        };
        store.save(&settings).expect("saved");
        assert_eq!(store.load().ok(), Some(settings));
    }

    #[test]
    fn unknown_values_fall_back_to_defaults() {
        let store = SettingsStore::open_in_memory().expect("store opens");
        for (key, value) in [("max_hops", "7"), ("timeout_ms", "abc"), ("focus", "1y")] {
            store
                .connection
                .execute("INSERT INTO app_settings (key, value) VALUES (?1, ?2)", params![key, value])
                .expect("inserted");
        }
        assert_eq!(store.load().ok(), Some(AppSettings::default()));
    }
}
