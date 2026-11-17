//! The app's settings (the Settings window): what the close button does, the values new targets start with, probe
//! timeout and hop limit, how long history is kept, theme, language and latency zones. Stored as key/value rows in the history database; a missing or
//! unreadable value falls back to its default.

use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, params};

use crate::i18n::Language;
use crate::probe::tracer::Probe;
use crate::store::history::{self, HistoryError};
use crate::store::targets::TraceSettings;
use crate::ui::focus::Focus;
use crate::ui::latency_bars::Zones;
use crate::ui::theme::ThemeChoice;
use crate::ui::timeline::Period;

/// Schema: one row per setting.
const SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS app_settings (
        key   TEXT PRIMARY KEY,
        value TEXT NOT NULL
    ) WITHOUT ROWID;
";

/// Probe intervals offered (the toolbars name them); a stored interval that isn't one of them is not used: an
/// interval of 0 would send probes without pause.
pub const INTERVALS: [Duration; 7] = [
    Duration::from_millis(500),
    Duration::from_secs(1),
    Duration::from_millis(2_500),
    Duration::from_secs(5),
    Duration::from_secs(10),
    Duration::from_secs(30),
    Duration::from_secs(60),
];
/// Probe timeouts offered.
pub const TIMEOUTS: [Duration; 4] =
    [Duration::from_millis(500), Duration::from_secs(1), Duration::from_secs(2), Duration::from_secs(3)];
/// Hop limits offered: at most this many hops are shown, the first ones of a longer route are hidden.
pub const MAX_HOPS: [u8; 4] = [10, 20, 30, 40];
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
    /// How many hops are shown; the first ones of a longer route are hidden.
    pub max_hops: u8,
    /// Hop addresses are named by reverse DNS; off, no lookups are made and addresses are shown as they are.
    pub resolve_names: bool,
    /// How long samples are kept.
    pub retention: Duration,
    /// Dark, light or following Windows.
    pub theme: ThemeChoice,
    /// Look for a new version once a day.
    pub check_updates: bool,
    /// Interface language.
    pub language: Language,
    /// Boundaries of the latency zones.
    pub zones: Zones,
}

/// The interval of `millis` if it is one of the [`INTERVALS`] offered.
pub fn offered_interval(millis: u64) -> Option<Duration> {
    let interval = Duration::from_millis(millis);
    INTERVALS.contains(&interval).then_some(interval)
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            close_to_tray: false,
            defaults: TraceSettings {
                interval: Duration::from_millis(2_500),
                focus: Focus::default(),
                period: Period::default(),
                probe: Probe::Icmp,
            },
            timeout: Duration::from_secs(1),
            max_hops: 30,
            resolve_names: true,
            // The longest period the timeline shows.
            retention: Duration::from_secs(7 * 24 * 60 * 60),
            theme: ThemeChoice::default(),
            check_updates: true,
            language: Language::English,
            zones: Zones::default(),
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
                interval: number("interval_ms")?.and_then(offered_interval).unwrap_or(fallback.defaults.interval),
                focus: self.get("focus")?.and_then(|key| Focus::from_key(&key)).unwrap_or(fallback.defaults.focus),
                period: number("period_s")?.and_then(Period::from_seconds).unwrap_or(fallback.defaults.period),
                probe: self.get("probe")?.and_then(|key| Probe::from_key(&key)).unwrap_or(fallback.defaults.probe),
            },
            timeout: number("timeout_ms")?
                .map(Duration::from_millis)
                .filter(|timeout| TIMEOUTS.contains(timeout))
                .unwrap_or(fallback.timeout),
            max_hops: number("max_hops")?
                .and_then(|hops| u8::try_from(hops).ok())
                .filter(|hops| MAX_HOPS.contains(hops))
                .unwrap_or(fallback.max_hops),
            resolve_names: self.get("resolve_names")?.map_or(fallback.resolve_names, |value| value == "1"),
            retention: number("retention_s")?
                .map(Duration::from_secs)
                .filter(|retention| RETENTIONS.contains(retention))
                .unwrap_or(fallback.retention),
            theme: self.get("theme")?.and_then(|key| ThemeChoice::from_key(&key)).unwrap_or(fallback.theme),
            check_updates: self.get("check_updates")?.map_or(fallback.check_updates, |value| value == "1"),
            // Until chosen, the language Windows itself is shown in.
            language: self
                .get("language")?
                .and_then(|key| Language::from_key(&key))
                .unwrap_or_else(Language::of_windows),
            zones: number("zone_fine_ms")?
                .zip(number("zone_slow_ms")?)
                .and_then(|(fine, slow)| Zones::new(u32::try_from(fine).ok()?, u32::try_from(slow).ok()?))
                .unwrap_or(fallback.zones),
        })
    }

    /// When the last daily update check succeeded, Unix ms; `None` if never.
    ///
    /// # Errors
    ///
    /// [`HistoryError::Db`] if SQLite fails.
    pub fn last_update_check(&self) -> Result<Option<i64>, HistoryError> {
        Ok(self.get("update_checked_ms")?.and_then(|value| value.parse().ok()))
    }

    /// Remembers that the daily update check succeeded at `at_ms`.
    ///
    /// # Errors
    ///
    /// [`HistoryError::Db`] if SQLite fails.
    pub fn set_last_update_check(&self, at_ms: i64) -> Result<(), HistoryError> {
        self.connection.execute(
            "INSERT INTO app_settings (key, value) VALUES ('update_checked_ms', ?1)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            [at_ms.to_string()],
        )?;
        Ok(())
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
            ("probe", settings.defaults.probe.key()),
            ("timeout_ms", millis(settings.timeout)),
            ("max_hops", settings.max_hops.to_string()),
            ("resolve_names", if settings.resolve_names { "1".to_owned() } else { "0".to_owned() }),
            ("retention_s", settings.retention.as_secs().to_string()),
            ("theme", settings.theme.key().to_owned()),
            ("check_updates", if settings.check_updates { "1".to_owned() } else { "0".to_owned() }),
            ("language", settings.language.key().to_owned()),
            ("zone_fine_ms", settings.zones.fine_ms().to_string()),
            ("zone_slow_ms", settings.zones.slow_ms().to_string()),
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
        let loaded = store.load().expect("loads");
        assert_eq!(loaded.language, Language::of_windows(), "until chosen, the language of Windows");
        assert_eq!(AppSettings { language: Language::English, ..loaded }, AppSettings::default());
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
                probe: Probe::Tcp(80),
            },
            timeout: Duration::from_secs(2),
            max_hops: 40,
            resolve_names: false,
            retention: DAY,
            theme: ThemeChoice::Light,
            check_updates: false,
            language: Language::German,
            zones: Zones::new(30, 80).expect("valid zones"),
        };
        store.save(&settings).expect("saved");
        assert_eq!(store.load().ok(), Some(settings));
    }

    #[test]
    fn the_last_update_check_is_remembered() {
        let store = SettingsStore::open_in_memory().expect("store opens");
        assert_eq!(store.last_update_check().ok(), Some(None));
        store.set_last_update_check(1_234).expect("saved");
        store.set_last_update_check(5_678).expect("saved again");
        assert_eq!(store.last_update_check().ok(), Some(Some(5_678)));
    }

    #[test]
    fn unknown_values_fall_back_to_defaults() {
        let store = SettingsStore::open_in_memory().expect("store opens");
        for (key, value) in [("max_hops", "7"), ("timeout_ms", "abc"), ("focus", "1y"), ("zone_fine_ms", "300")] {
            store
                .connection
                .execute("INSERT INTO app_settings (key, value) VALUES (?1, ?2)", params![key, value])
                .expect("inserted");
        }
        let loaded = store.load().expect("loads");
        assert_eq!(AppSettings { language: Language::English, ..loaded }, AppSettings::default());
    }
}
