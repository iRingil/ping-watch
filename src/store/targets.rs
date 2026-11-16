//! The user's list of targets, kept in the history database: seeded once with well-known targets, extended
//! automatically by every new target traced, editable (address and an optional name) and prunable (presets too).
//! The address family is determined once, when an address is added or changed, and stored with it. Each entry
//! also keeps the trace settings last used for it (interval, focus, timeline period, probe).

use std::net::IpAddr;
use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, params};

use crate::probe::tracer::Probe;
use crate::store::history::{self, HistoryError};
use crate::store::presets;
use crate::store::settings::offered_interval;
use crate::ui::focus::Focus;
use crate::ui::timeline::Period;

/// Schema of the list; `family` is 4, 6 or NULL (a host name not resolved yet); the settings columns are all NULL
/// until the target is first traced.
const SCHEMA: &str = "
    CREATE TABLE saved_targets (
        id          INTEGER PRIMARY KEY,
        address     TEXT NOT NULL UNIQUE,
        name        TEXT NOT NULL DEFAULT '',
        family      INTEGER,
        interval_ms INTEGER, -- probe interval
        focus       TEXT,    -- Focus::key
        period_s    INTEGER  -- timeline period, seconds
    );
";
/// The probe chosen per target, kept apart from the list's own table (created with it or later); a target without
/// a row is probed by ICMP.
const PROBES_SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS target_probes (
        target_id INTEGER PRIMARY KEY, -- saved_targets.id
        probe     TEXT NOT NULL        -- Probe::key
    );
";

/// IP version of a target's address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    /// IPv4.
    V4,
    /// IPv6.
    V6,
}

impl Family {
    /// Family of `ip`.
    pub fn of(ip: IpAddr) -> Self {
        if ip.is_ipv6() { Self::V6 } else { Self::V4 }
    }

    /// Family of an address literal; `None` for a host name.
    pub fn of_literal(address: &str) -> Option<Self> {
        address.parse().ok().map(Self::of)
    }

    /// Label shown in the list.
    pub fn label(self) -> &'static str {
        match self {
            Self::V4 => "ipv4",
            Self::V6 => "ipv6",
        }
    }

    /// Value stored in the database.
    fn code(self) -> i64 {
        match self {
            Self::V4 => 4,
            Self::V6 => 6,
        }
    }

    /// Family of a stored value.
    fn from_code(code: Option<i64>) -> Option<Self> {
        match code {
            Some(4) => Some(Self::V4),
            Some(6) => Some(Self::V6),
            _ => None,
        }
    }
}

/// Trace settings remembered per target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TraceSettings {
    /// Probe interval.
    pub interval: Duration,
    /// Period the hop table summarizes.
    pub focus: Focus,
    /// Period the timeline shows.
    pub period: Period,
    /// What the hops are probed with.
    pub probe: Probe,
}

/// A target's stored settings columns: interval (ms), focus key, period (s) and probe key; each may be NULL.
type StoredSettings = (Option<i64>, Option<String>, Option<i64>, Option<String>);

/// One entry of the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedTarget {
    /// Row id.
    pub id: i64,
    /// Host name or IP address, as traced.
    pub address: String,
    /// Optional name of the resource ("Office router"); may be empty.
    pub name: String,
    /// IP version, `None` for a host name that hasn't been resolved yet.
    pub family: Option<Family>,
}

/// Normal form of an address: trimmed and lowercased, so the same target isn't listed twice.
fn normalize(address: &str) -> String {
    address.trim().to_lowercase()
}

/// Connection to the target list.
#[derive(Debug)]
pub struct TargetList {
    /// Open SQLite connection (the history database).
    connection: Connection,
}

impl TargetList {
    /// Opens the list in the database at `path`, creating and seeding it on first use.
    ///
    /// # Errors
    ///
    /// [`HistoryError`] if the database can't be opened or prepared.
    pub fn open(path: &Path) -> Result<Self, HistoryError> {
        Self::init(history::connect(path)?)
    }

    /// Opens a private list in memory.
    #[cfg(test)]
    fn open_in_memory() -> Result<Self, HistoryError> {
        Self::init(Connection::open_in_memory()?)
    }

    /// Creates the table and fills it with the presets if it doesn't exist yet. Seeding happens only then, so
    /// presets the user deleted stay deleted.
    fn init(mut connection: Connection) -> Result<Self, HistoryError> {
        let exists: Option<i64> = connection
            .query_row("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'saved_targets'", [], |row| {
                row.get(0)
            })
            .optional()?;
        if exists.is_none() {
            let transaction = connection.transaction()?;
            transaction.execute_batch(SCHEMA)?;
            for preset in presets::ALL {
                let family = Family::of_literal(preset.target).map(Family::code);
                transaction.execute(
                    "INSERT INTO saved_targets (address, name, family) VALUES (?1, ?2, ?3)",
                    params![preset.target, preset.owner, family],
                )?;
            }
            transaction.commit()?;
        }
        connection.execute_batch(PROBES_SCHEMA)?;
        Ok(Self { connection })
    }

    /// The whole list in the order entries were added.
    ///
    /// # Errors
    ///
    /// [`HistoryError::Db`] if SQLite fails.
    pub fn all(&self) -> Result<Vec<SavedTarget>, HistoryError> {
        let mut statement =
            self.connection.prepare_cached("SELECT id, address, name, family FROM saved_targets ORDER BY id")?;
        let rows = statement.query_map([], |row| {
            Ok(SavedTarget {
                id: row.get(0)?,
                address: row.get(1)?,
                name: row.get(2)?,
                family: Family::from_code(row.get(3)?),
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Records that `address` was traced and resolved to `resolved`: adds it if it isn't listed, and fills in the
    /// family of a listed host name that didn't have one yet.
    ///
    /// # Errors
    ///
    /// [`HistoryError::Db`] if SQLite fails.
    pub fn remember(&self, address: &str, resolved: IpAddr) -> Result<(), HistoryError> {
        let family = Family::of(resolved).code();
        self.connection.execute(
            "INSERT INTO saved_targets (address, family) VALUES (?1, ?2)
             ON CONFLICT (address) DO UPDATE SET family = COALESCE(family, excluded.family)",
            params![normalize(address), family],
        )?;
        Ok(())
    }

    /// Changes entry `id` to `address` and `name`. The family is determined again only if the address changed:
    /// from the literal, or unknown for a host name until it is traced.
    ///
    /// # Errors
    ///
    /// [`HistoryError::Db`] if SQLite fails, e.g. when another entry already has that address.
    pub fn update(&self, id: i64, address: &str, name: &str) -> Result<(), HistoryError> {
        let address = normalize(address);
        let family = Family::of_literal(&address).map(Family::code);
        self.connection.execute(
            "UPDATE saved_targets
             SET family = CASE WHEN address = ?2 THEN family ELSE ?4 END, address = ?2, name = ?3
             WHERE id = ?1",
            params![id, address, name.trim(), family],
        )?;
        Ok(())
    }

    /// Name given to `address` in the list, `None` if it isn't listed or has no name.
    ///
    /// # Errors
    ///
    /// [`HistoryError::Db`] if SQLite fails.
    pub fn name(&self, address: &str) -> Result<Option<String>, HistoryError> {
        let name: Option<String> = self
            .connection
            .query_row("SELECT name FROM saved_targets WHERE address = ?1", [normalize(address)], |row| row.get(0))
            .optional()?;
        Ok(name.filter(|name| !name.trim().is_empty()))
    }

    /// Settings last used for tracing `address`, `None` if it was never traced (or the stored values are unknown).
    ///
    /// # Errors
    ///
    /// [`HistoryError::Db`] if SQLite fails.
    pub fn settings(&self, address: &str) -> Result<Option<TraceSettings>, HistoryError> {
        let row: Option<StoredSettings> = self
            .connection
            .query_row(
                "SELECT interval_ms, focus, period_s, probe FROM saved_targets
                 LEFT JOIN target_probes ON target_id = saved_targets.id WHERE address = ?1",
                [normalize(address)],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;
        let Some((Some(interval_ms), Some(focus), Some(period_s), probe)) = row else {
            return Ok(None);
        };
        let probe = probe.and_then(|key| Probe::from_key(&key)).unwrap_or_default();
        let interval = u64::try_from(interval_ms).ok().and_then(offered_interval);
        let focus = Focus::from_key(&focus);
        let period = u64::try_from(period_s).ok().and_then(Period::from_seconds);
        Ok(interval.zip(focus).zip(period).map(|((interval, focus), period)| TraceSettings {
            interval,
            focus,
            period,
            probe,
        }))
    }

    /// Remembers `settings` for `address` (which must already be listed, see [`Self::remember`]).
    ///
    /// # Errors
    ///
    /// [`HistoryError::Db`] if SQLite fails.
    pub fn save_settings(&self, address: &str, settings: TraceSettings) -> Result<(), HistoryError> {
        let interval_ms = i64::try_from(settings.interval.as_millis()).unwrap_or(i64::MAX);
        let period_s = i64::try_from(settings.period.seconds()).unwrap_or(i64::MAX);
        let transaction = self.connection.unchecked_transaction()?;
        transaction.execute(
            "UPDATE saved_targets SET interval_ms = ?2, focus = ?3, period_s = ?4 WHERE address = ?1",
            params![normalize(address), interval_ms, settings.focus.key(), period_s],
        )?;
        transaction.execute(
            "INSERT INTO target_probes (target_id, probe) SELECT id, ?2 FROM saved_targets WHERE address = ?1
             ON CONFLICT (target_id) DO UPDATE SET probe = excluded.probe",
            params![normalize(address), settings.probe.key()],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Removes entry `id`.
    ///
    /// # Errors
    ///
    /// [`HistoryError::Db`] if SQLite fails; then nothing is deleted.
    pub fn delete(&self, id: i64) -> Result<(), HistoryError> {
        let transaction = self.connection.unchecked_transaction()?;
        transaction.execute("DELETE FROM target_probes WHERE target_id = ?1", [id])?;
        transaction.execute("DELETE FROM saved_targets WHERE id = ?1", [id])?;
        transaction.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, Ipv6Addr};

    use super::*;

    /// Fresh in-memory list.
    fn list() -> TargetList {
        TargetList::open_in_memory().expect("in-memory list opens")
    }

    /// Entry with `address`, if listed.
    fn find(list: &TargetList, address: &str) -> Option<SavedTarget> {
        list.all().expect("list reads").into_iter().find(|target| target.address == address)
    }

    #[test]
    fn seeded_with_presets_and_their_families() {
        let list = list();
        assert_eq!(list.all().map(|all| all.len()).ok(), Some(presets::ALL.len()));
        assert_eq!(find(&list, "8.8.8.8").and_then(|target| target.family), Some(Family::V4));
        assert_eq!(find(&list, "2620:fe::fe").and_then(|target| target.family), Some(Family::V6));
        assert_eq!(find(&list, "google.com").map(|target| target.family), Some(None));
    }

    #[test]
    fn traced_targets_are_added_once() {
        let list = list();
        let ip = IpAddr::V4(Ipv4Addr::new(169, 58, 108, 229));
        list.remember(" 169.58.108.229 ", ip).expect("remembered");
        list.remember("169.58.108.229", ip).expect("remembered again");
        let all = list.all().expect("list reads");
        assert_eq!(all.iter().filter(|target| target.address == "169.58.108.229").count(), 1);
        assert_eq!(all.last().and_then(|target| target.family), Some(Family::V4));
    }

    #[test]
    fn tracing_a_host_name_fills_in_its_family() {
        let list = list();
        list.remember("Google.com", IpAddr::V6(Ipv6Addr::LOCALHOST)).expect("remembered");
        assert_eq!(find(&list, "google.com").and_then(|target| target.family), Some(Family::V6));
    }

    #[test]
    fn edit_renames_and_redetects_only_on_address_change() {
        let list = list();
        let id = find(&list, "google.com").map(|target| target.id).expect("preset listed");
        list.remember("google.com", IpAddr::V4(Ipv4Addr::LOCALHOST)).expect("remembered");
        list.update(id, "google.com", "Search").expect("renamed");
        let renamed = find(&list, "google.com").expect("still listed");
        assert_eq!((renamed.name.as_str(), renamed.family), ("Search", Some(Family::V4)));

        list.update(id, "2001:db8::1", "").expect("address changed");
        let changed = find(&list, "2001:db8::1").expect("listed under the new address");
        assert_eq!((changed.name.as_str(), changed.family), ("", Some(Family::V6)));
    }

    #[test]
    fn names_are_looked_up_by_address() {
        let list = list();
        list.remember("example.org", IpAddr::from([192, 0, 2, 1])).expect("remembered");
        assert_eq!(list.name("example.org").ok(), Some(None), "no name given");
        let id = find(&list, "example.org").map(|target| target.id).expect("listed");
        list.update(id, "example.org", "Example").expect("updated");
        assert_eq!(list.name(" Example.ORG ").ok(), Some(Some("Example".to_owned())));
        assert_eq!(list.name("unknown.org").ok(), Some(None));
    }

    #[test]
    fn settings_are_remembered_per_target() {
        let list = list();
        let ip = IpAddr::V4(Ipv4Addr::new(9, 9, 9, 9));
        assert_eq!(list.settings("9.9.9.9").ok(), Some(None), "never traced yet");
        list.remember("9.9.9.9", ip).expect("remembered");
        let settings = TraceSettings {
            interval: Duration::from_millis(500),
            focus: Focus::Minutes5,
            period: Period::Hour,
            probe: Probe::Tcp(8443),
        };
        list.save_settings("9.9.9.9", settings).expect("saved");
        assert_eq!(list.settings(" 9.9.9.9 ").ok(), Some(Some(settings)));
        let icmp = TraceSettings { probe: Probe::Icmp, ..settings };
        list.save_settings("9.9.9.9", icmp).expect("saved again");
        assert_eq!(list.settings("9.9.9.9").ok(), Some(Some(icmp)));
        assert_eq!(list.settings("1.1.1.1").ok(), Some(None), "other targets keep their own");
    }

    #[test]
    fn presets_can_be_deleted_for_good() {
        let list = list();
        let id = find(&list, "1.1.1.1").map(|target| target.id).expect("preset listed");
        list.delete(id).expect("deleted");
        assert_eq!(find(&list, "1.1.1.1"), None);
    }

    #[test]
    fn family_of_literals() {
        assert_eq!(Family::of_literal("169.58.108.229"), Some(Family::V4));
        assert_eq!(Family::of_literal("::1"), Some(Family::V6));
        assert_eq!(Family::of_literal("router.example"), None);
    }
}
