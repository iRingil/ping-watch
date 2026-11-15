//! Alert rules and the alert journal in the history database. Three rules come with a new database: "route
//! changed" (on, journal and timeline only), "loss above 10 % over 2 minutes" and "no answer for 30 seconds" (off).

use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, params};

use crate::alerts::{Condition, ConditionKind, Rule, Targets};
use crate::store::history::HistoryError;

/// Schema: the rules, and one journal row per alert (a route change is a row whose end is its start).
const SCHEMA: &str = "
    CREATE TABLE alert_rules (
        id              INTEGER PRIMARY KEY,
        kind            TEXT    NOT NULL, -- loss, latency, unreachable, route
        threshold       REAL    NOT NULL, -- percent or ms; 0 when the kind has none
        window_s        INTEGER NOT NULL, -- 0 when the kind has none
        targets         TEXT    NOT NULL, -- '' for all targets, else hosts one per line
        notify          INTEGER NOT NULL,
        notify_recovery INTEGER NOT NULL,
        enabled         INTEGER NOT NULL
    );
    CREATE TABLE alert_events (
        id         INTEGER PRIMARY KEY,
        rule_id    INTEGER NOT NULL,
        host       TEXT    NOT NULL,
        what       TEXT    NOT NULL, -- the rule or the change, as shown in the journal
        started_ms INTEGER NOT NULL,
        ended_ms   INTEGER,          -- NULL while it lasts
        worst      REAL,              -- worst value, NULL for a route change
        unit       TEXT               -- unit of the worst value: %, ms or s
    );
    CREATE INDEX alert_events_by_time ON alert_events (started_ms);
";
/// Most journal rows shown.
const JOURNAL_LIMIT: i64 = 500;

/// One journal row.
#[derive(Debug, Clone, PartialEq)]
pub struct JournalEntry {
    /// Row id.
    pub id: i64,
    /// The target.
    pub host: String,
    /// What happened: the rule's condition or the route change.
    pub what: String,
    /// Start, Unix ms.
    pub started_ms: i64,
    /// End, Unix ms; `None` while it lasts (or if the app was closed meanwhile).
    pub ended_ms: Option<i64>,
    /// Worst value, if the rule measures one.
    pub worst: Option<f64>,
    /// Unit of the worst value.
    pub unit: Option<String>,
}

/// Connection to the stored rules and journal.
#[derive(Debug)]
pub struct AlertStore {
    /// Open SQLite connection (the history database).
    connection: Connection,
}

impl AlertStore {
    /// Opens the store in the database at `path`, creating and seeding the tables on first use. Alerts left open
    /// by an earlier run are closed: nobody watched them since.
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

    /// Creates and seeds the tables if they don't exist; closes open journal rows.
    fn init(mut connection: Connection) -> Result<Self, HistoryError> {
        let exists: Option<i64> = connection
            .query_row("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'alert_rules'", [], |row| {
                row.get(0)
            })
            .optional()?;
        if exists.is_none() {
            let transaction = connection.transaction()?;
            transaction.execute_batch(SCHEMA)?;
            for rule in default_rules() {
                insert_rule(&transaction, &rule)?;
            }
            transaction.commit()?;
        }
        // Rows still open belong to a run that ended: they end where their start is, as nothing is known after.
        connection.execute("UPDATE alert_events SET ended_ms = started_ms WHERE ended_ms IS NULL", [])?;
        Ok(Self { connection })
    }

    /// All rules in the order they were added.
    ///
    /// # Errors
    ///
    /// [`HistoryError::Db`] if SQLite fails.
    pub fn rules(&self) -> Result<Vec<Rule>, HistoryError> {
        let mut statement = self.connection.prepare(
            "SELECT id, kind, threshold, window_s, targets, notify, notify_recovery, enabled FROM alert_rules ORDER BY id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, f64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)? != 0,
                row.get::<_, i64>(6)? != 0,
                row.get::<_, i64>(7)? != 0,
            ))
        })?;
        let mut rules = Vec::new();
        for row in rows {
            let (id, kind, threshold, window_s, targets, notify, notify_recovery, enabled) = row?;
            // An unknown kind (from a newer version) is skipped rather than guessed.
            let Some(condition) = condition_of(&kind, threshold, window_s) else {
                continue;
            };
            rules.push(Rule { id, targets: targets_of(&targets), condition, notify, notify_recovery, enabled });
        }
        Ok(rules)
    }

    /// Stores `rule`: adds it if its id is 0, otherwise updates it. Returns its id.
    ///
    /// # Errors
    ///
    /// [`HistoryError::Db`] if SQLite fails.
    pub fn save_rule(&self, rule: &Rule) -> Result<i64, HistoryError> {
        if rule.id == 0 {
            return insert_rule(&self.connection, rule);
        }
        let (kind, threshold, window_s) = columns_of(&rule.condition);
        self.connection.execute(
            "UPDATE alert_rules SET kind = ?2, threshold = ?3, window_s = ?4, targets = ?5, notify = ?6,
                 notify_recovery = ?7, enabled = ?8 WHERE id = ?1",
            params![
                rule.id,
                kind,
                threshold,
                window_s,
                targets_text(&rule.targets),
                rule.notify,
                rule.notify_recovery,
                rule.enabled
            ],
        )?;
        Ok(rule.id)
    }

    /// Removes rule `id` (its journal rows stay).
    ///
    /// # Errors
    ///
    /// [`HistoryError::Db`] if SQLite fails.
    pub fn delete_rule(&self, id: i64) -> Result<(), HistoryError> {
        self.connection.execute("DELETE FROM alert_rules WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Opens a journal row for an alert that started; returns its id.
    ///
    /// # Errors
    ///
    /// [`HistoryError::Db`] if SQLite fails.
    pub fn start(
        &self,
        rule_id: i64,
        host: &str,
        what: &str,
        started_ms: i64,
        worst: f64,
        unit: &str,
    ) -> Result<i64, HistoryError> {
        self.connection.execute(
            "INSERT INTO alert_events (rule_id, host, what, started_ms, worst, unit) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![rule_id, host, what, started_ms, worst, unit],
        )?;
        Ok(self.connection.last_insert_rowid())
    }

    /// Closes the open journal row of `rule_id` and `host` at `ended_ms` with its `worst` value.
    ///
    /// # Errors
    ///
    /// [`HistoryError::Db`] if SQLite fails.
    pub fn end(&self, rule_id: i64, host: &str, ended_ms: i64, worst: f64) -> Result<(), HistoryError> {
        self.connection.execute(
            "UPDATE alert_events SET ended_ms = ?3, worst = ?4 WHERE rule_id = ?1 AND host = ?2 AND ended_ms IS NULL",
            params![rule_id, host, ended_ms, worst],
        )?;
        Ok(())
    }

    /// Records a one-moment event (a route change): a row that ends where it starts.
    ///
    /// # Errors
    ///
    /// [`HistoryError::Db`] if SQLite fails.
    pub fn note(&self, rule_id: i64, host: &str, what: &str, at_ms: i64) -> Result<(), HistoryError> {
        self.connection.execute(
            "INSERT INTO alert_events (rule_id, host, what, started_ms, ended_ms) VALUES (?1, ?2, ?3, ?4, ?4)",
            params![rule_id, host, what, at_ms],
        )?;
        Ok(())
    }

    /// The journal, newest first.
    ///
    /// # Errors
    ///
    /// [`HistoryError::Db`] if SQLite fails.
    pub fn journal(&self) -> Result<Vec<JournalEntry>, HistoryError> {
        let mut statement = self.connection.prepare(
            "SELECT id, host, what, started_ms, ended_ms, worst, unit FROM alert_events
             ORDER BY started_ms DESC, id DESC LIMIT ?1",
        )?;
        let rows = statement.query_map([JOURNAL_LIMIT], |row| {
            Ok(JournalEntry {
                id: row.get(0)?,
                host: row.get(1)?,
                what: row.get(2)?,
                started_ms: row.get(3)?,
                ended_ms: row.get(4)?,
                worst: row.get(5)?,
                unit: row.get(6)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Empties the journal (except alerts that are on now).
    ///
    /// # Errors
    ///
    /// [`HistoryError::Db`] if SQLite fails.
    pub fn clear_journal(&self) -> Result<(), HistoryError> {
        self.connection.execute("DELETE FROM alert_events WHERE ended_ms IS NOT NULL", [])?;
        Ok(())
    }
}

/// The rules a new database starts with.
fn default_rules() -> [Rule; 3] {
    [
        Rule {
            id: 0,
            targets: Targets::All,
            condition: Condition::RouteChanged,
            notify: false,
            notify_recovery: false,
            enabled: true,
        },
        Rule {
            id: 0,
            targets: Targets::All,
            condition: Condition::Loss { percent: 10.0, window: Duration::from_secs(120) },
            notify: true,
            notify_recovery: true,
            enabled: false,
        },
        Rule {
            id: 0,
            targets: Targets::All,
            condition: Condition::Unreachable { after: Duration::from_secs(30) },
            notify: true,
            notify_recovery: true,
            enabled: false,
        },
    ]
}

/// Adds `rule` through `connection`; returns its id.
fn insert_rule(connection: &Connection, rule: &Rule) -> Result<i64, HistoryError> {
    let (kind, threshold, window_s) = columns_of(&rule.condition);
    connection.execute(
        "INSERT INTO alert_rules (kind, threshold, window_s, targets, notify, notify_recovery, enabled)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            kind,
            threshold,
            window_s,
            targets_text(&rule.targets),
            rule.notify,
            rule.notify_recovery,
            rule.enabled
        ],
    )?;
    Ok(connection.last_insert_rowid())
}

/// The stored columns of a condition: kind key, threshold, window in seconds.
fn columns_of(condition: &Condition) -> (&'static str, f64, i64) {
    let seconds = |window: Duration| i64::try_from(window.as_secs()).unwrap_or(i64::MAX);
    match condition {
        Condition::Loss { percent, window } => (ConditionKind::Loss.key(), *percent, seconds(*window)),
        Condition::Latency { ms, window } => (ConditionKind::Latency.key(), *ms, seconds(*window)),
        Condition::Unreachable { after } => (ConditionKind::Unreachable.key(), 0.0, seconds(*after)),
        Condition::RouteChanged => (ConditionKind::RouteChanged.key(), 0.0, 0),
    }
}

/// The condition stored as `kind`, `threshold` and `window_s`, `None` for an unknown kind.
fn condition_of(kind: &str, threshold: f64, window_s: i64) -> Option<Condition> {
    let window = Duration::from_secs(u64::try_from(window_s).unwrap_or(0));
    Some(match ConditionKind::from_key(kind)? {
        ConditionKind::Loss => Condition::Loss { percent: threshold, window },
        ConditionKind::Latency => Condition::Latency { ms: threshold, window },
        ConditionKind::Unreachable => Condition::Unreachable { after: window },
        ConditionKind::RouteChanged => Condition::RouteChanged,
    })
}

/// Targets as stored: empty for all, else one host per line.
fn targets_text(targets: &Targets) -> String {
    match targets {
        Targets::All => String::new(),
        Targets::Only(hosts) => hosts.join("\n"),
    }
}

/// Targets from their stored text.
fn targets_of(text: &str) -> Targets {
    let hosts: Vec<String> = text.lines().map(str::trim).filter(|host| !host.is_empty()).map(str::to_owned).collect();
    if hosts.is_empty() { Targets::All } else { Targets::Only(hosts) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_database_has_the_default_rules() {
        let store = AlertStore::open_in_memory().expect("store opens");
        let rules = store.rules().expect("rules read");
        let summary: Vec<(ConditionKind, bool, bool)> =
            rules.iter().map(|rule| (rule.condition.kind(), rule.enabled, rule.notify)).collect();
        assert_eq!(
            summary,
            [
                (ConditionKind::RouteChanged, true, false),
                (ConditionKind::Loss, false, true),
                (ConditionKind::Unreachable, false, true)
            ]
        );
    }

    #[test]
    fn rules_are_added_changed_and_deleted() {
        let store = AlertStore::open_in_memory().expect("store opens");
        let mut rule = Rule {
            id: 0,
            targets: Targets::Only(vec!["1.1.1.1".to_owned(), "github.com".to_owned()]),
            condition: Condition::Latency { ms: 150.0, window: Duration::from_secs(300) },
            notify: true,
            notify_recovery: false,
            enabled: true,
        };
        rule.id = store.save_rule(&rule).expect("added");
        assert_eq!(store.rules().ok().and_then(|rules| rules.last().cloned()), Some(rule.clone()));
        rule.enabled = false;
        rule.targets = Targets::All;
        store.save_rule(&rule).expect("updated");
        assert_eq!(store.rules().ok().and_then(|rules| rules.last().cloned()), Some(rule.clone()));
        store.delete_rule(rule.id).expect("deleted");
        assert_eq!(store.rules().map(|rules| rules.len()).ok(), Some(3));
    }

    #[test]
    fn the_journal_keeps_alerts_and_route_changes() {
        let store = AlertStore::open_in_memory().expect("store opens");
        store.start(2, "1.1.1.1", "loss above 10 % over 2 minutes", 1_000, 20.0, "%").expect("started");
        store.note(1, "1.1.1.1", "hop 2 10.0.0.2 → 10.0.0.5", 1_500).expect("noted");
        store.end(2, "1.1.1.1", 5_000, 35.0).expect("ended");
        let journal = store.journal().expect("read");
        let rows: Vec<(i64, Option<i64>, Option<f64>)> =
            journal.iter().map(|entry| (entry.started_ms, entry.ended_ms, entry.worst)).collect();
        assert_eq!(rows, [(1_500, Some(1_500), None), (1_000, Some(5_000), Some(35.0))]);
        store.clear_journal().expect("cleared");
        assert_eq!(store.journal().map(|journal| journal.len()).ok(), Some(0));
    }
}
