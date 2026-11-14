//! The user's list of targets, kept in the history database: seeded once with well-known targets, extended
//! automatically by every new target traced, editable (address and an optional name) and prunable (presets too).
//! The address family is determined once, when an address is added or changed, and stored with it.

use std::net::IpAddr;
use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};

use crate::history::HistoryError;
use crate::presets;

/// Schema of the list; `family` is 4, 6 or NULL (a host name not resolved yet).
const SCHEMA: &str = "
    CREATE TABLE saved_targets (
        id      INTEGER PRIMARY KEY,
        address TEXT NOT NULL UNIQUE,
        name    TEXT NOT NULL DEFAULT '',
        family  INTEGER
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

/// One entry of the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedTarget {
    /// Row id.
    pub id: i64,
    /// Host name or IP address, as traced.
    pub address: String,
    /// Optional name of the resource ("DanceHub"); may be empty.
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
        if let Some(folder) = path.parent() {
            std::fs::create_dir_all(folder)?;
        }
        Self::init(Connection::open(path)?)
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

    /// Removes entry `id`.
    ///
    /// # Errors
    ///
    /// [`HistoryError::Db`] if SQLite fails.
    pub fn delete(&self, id: i64) -> Result<(), HistoryError> {
        self.connection.execute("DELETE FROM saved_targets WHERE id = ?1", [id])?;
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
        assert_eq!(Family::of_literal("dancehub.example"), None);
    }
}
