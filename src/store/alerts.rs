//! Alert rules and the alert journal in the history database. Three rules come with a new database: "route
//! changed" (on, journal and timeline only), "loss above 10 % over 2 minutes" and "no answer for 30 seconds" (off).
//! The journal keeps what happened as kinds and numbers, not as text, so it is worded in the language shown; the
//! timeline marks of alerts are made from it too, so clearing or pruning the journal takes them away.

use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::alerts::{Condition, ConditionKind, RouteNote, Rule, Targets, span};
use crate::i18n::{Language, trf};
use crate::store::history::{self, HistoryError};

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
        kind       TEXT    NOT NULL, -- the rule's condition as stored in alert_rules, at the time
        threshold  REAL    NOT NULL,
        window_s   INTEGER NOT NULL,
        value      REAL,             -- measured when it started; NULL for a route change
        worst      REAL,             -- worst value while it lasted; NULL for a route change
        route      TEXT,             -- a route change (RouteNote::to_text); NULL otherwise
        started_ms INTEGER NOT NULL,
        ended_ms   INTEGER,          -- NULL while it lasts
        recovered  INTEGER NOT NULL DEFAULT 0 -- 1 if it ended because things got better
    );
    CREATE INDEX alert_events_by_time ON alert_events (started_ms);
";
/// Most journal rows shown.
const JOURNAL_LIMIT: i64 = 500;
/// Columns of a journal row, in the order [`entry_of`] reads them.
const ENTRY_COLUMNS: &str = "id, host, kind, threshold, window_s, value, worst, route, started_ms, ended_ms, recovered";

/// What a journal row is about.
#[derive(Debug, Clone, PartialEq)]
pub enum Happening {
    /// A rule's condition held.
    Alert {
        /// The rule's condition at the time.
        condition: Condition,
        /// The value measured when it started.
        value: f64,
        /// The worst value while it lasted.
        worst: f64,
    },
    /// The route changed.
    Route(RouteNote),
}

/// One journal row.
#[derive(Debug, Clone, PartialEq)]
pub struct JournalEntry {
    /// Row id.
    pub id: i64,
    /// The target.
    pub host: String,
    /// What happened.
    pub happening: Happening,
    /// Start, Unix ms.
    pub started_ms: i64,
    /// End, Unix ms; `None` while it lasts.
    pub ended_ms: Option<i64>,
    /// It ended because things got better (not because the rule was turned off or the app closed).
    pub recovered: bool,
}

impl JournalEntry {
    /// What happened as a phrase: the rule's condition, or the route change.
    pub fn what(&self, language: Language) -> String {
        match &self.happening {
            Happening::Alert { condition, .. } => condition.describe(language),
            Happening::Route(note) => trf!(language, "Route changed: {what}", what = note.describe(language)),
        }
    }

    /// The worst value with its unit ("35 %"), `None` for a route change.
    pub fn worst(&self, language: Language) -> Option<String> {
        match &self.happening {
            Happening::Alert { condition, worst, .. } => Some(format!("{worst:.0} {}", condition.unit(language))),
            Happening::Route(_) => None,
        }
    }

    /// How long it lasted ("2 minutes"), `None` while it lasts or for a moment's event (a route change).
    pub fn lasted(&self, language: Language) -> Option<String> {
        let ended = self.ended_ms.filter(|ended| *ended != self.started_ms)?;
        let seconds = u64::try_from(ended.saturating_sub(self.started_ms) / 1_000).unwrap_or(0);
        Some(span(Duration::from_secs(seconds.max(1)), language))
    }

    /// Its marks on the target's timeline: the start, and the recovery if it got better.
    pub fn marks(&self, language: Language) -> Vec<(i64, String)> {
        let mut marks = Vec::new();
        match &self.happening {
            Happening::Alert { condition, value, worst } => {
                let measured = condition.measured(*value, language);
                let rule = condition.describe(language);
                marks.push((
                    self.started_ms,
                    trf!(language, "Alert: {measured} (rule: {rule})", measured = measured, rule = rule),
                ));
                if let (true, Some(ended), Some(lasted)) = (self.recovered, self.ended_ms, self.lasted(language)) {
                    let worst = condition.measured(*worst, language);
                    marks.push((
                        ended,
                        trf!(
                            language,
                            "Back to normal after {lasted} (worst: {worst})",
                            lasted = lasted,
                            worst = worst
                        ),
                    ));
                }
            }
            Happening::Route(_) => marks.push((self.started_ms, self.what(language))),
        }
        marks
    }
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
        Self::init(history::connect(path)?)
    }

    /// Opens a private store in memory.
    #[cfg(test)]
    fn open_in_memory() -> Result<Self, HistoryError> {
        Self::init(Connection::open_in_memory()?)
    }

    /// Creates and seeds the tables if they don't exist; closes open journal rows.
    fn init(mut connection: Connection) -> Result<Self, HistoryError> {
        if !has_tables(&connection)? {
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

    /// Opens a journal row for an alert of `condition` that started at `started_ms` with `value`; returns its id.
    ///
    /// # Errors
    ///
    /// [`HistoryError::Db`] if SQLite fails.
    pub fn start(
        &self,
        rule_id: i64,
        host: &str,
        condition: &Condition,
        started_ms: i64,
        value: f64,
    ) -> Result<i64, HistoryError> {
        let (kind, threshold, window_s) = columns_of(condition);
        self.connection.execute(
            "INSERT INTO alert_events (rule_id, host, kind, threshold, window_s, value, worst, started_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6, ?7)",
            params![rule_id, host, kind, threshold, window_s, value, started_ms],
        )?;
        Ok(self.connection.last_insert_rowid())
    }

    /// Closes the open journal row of `rule_id` and `host` at `ended_ms` with its `worst` value; `recovered` tells
    /// whether things got better.
    ///
    /// # Errors
    ///
    /// [`HistoryError::Db`] if SQLite fails.
    pub fn end(
        &self,
        rule_id: i64,
        host: &str,
        ended_ms: i64,
        worst: f64,
        recovered: bool,
    ) -> Result<(), HistoryError> {
        self.connection.execute(
            "UPDATE alert_events SET ended_ms = ?3, worst = ?4, recovered = ?5
             WHERE rule_id = ?1 AND host = ?2 AND ended_ms IS NULL",
            params![rule_id, host, ended_ms, worst, recovered],
        )?;
        Ok(())
    }

    /// Records a route change at `at_ms`: a row that ends where it starts.
    ///
    /// # Errors
    ///
    /// [`HistoryError::Db`] if SQLite fails.
    pub fn note(&self, rule_id: i64, host: &str, note: RouteNote, at_ms: i64) -> Result<(), HistoryError> {
        let (kind, threshold, window_s) = columns_of(&Condition::RouteChanged);
        self.connection.execute(
            "INSERT INTO alert_events (rule_id, host, kind, threshold, window_s, route, started_ms, ended_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
            params![rule_id, host, kind, threshold, window_s, note.to_text(), at_ms],
        )?;
        Ok(())
    }

    /// The journal, newest first.
    ///
    /// # Errors
    ///
    /// [`HistoryError::Db`] if SQLite fails.
    pub fn journal(&self) -> Result<Vec<JournalEntry>, HistoryError> {
        let mut statement = self
            .connection
            .prepare(&format!("SELECT {ENTRY_COLUMNS} FROM alert_events ORDER BY started_ms DESC, id DESC LIMIT ?1"))?;
        let rows = statement.query_map([JOURNAL_LIMIT], entry_of)?;
        Ok(rows.filter_map(Result::transpose).collect::<Result<_, _>>()?)
    }

    /// Empties the journal (except alerts that are on now), and with it the alert marks on the timelines.
    ///
    /// # Errors
    ///
    /// [`HistoryError::Db`] if SQLite fails.
    pub fn clear_journal(&self) -> Result<(), HistoryError> {
        self.connection.execute("DELETE FROM alert_events WHERE ended_ms IS NOT NULL", [])?;
        Ok(())
    }
}

/// Whether the alert tables exist in the database of `connection`.
fn has_tables(connection: &Connection) -> Result<bool, HistoryError> {
    let exists: Option<i64> = connection
        .query_row("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'alert_rules'", [], |row| row.get(0))
        .optional()?;
    Ok(exists.is_some())
}

/// The journal rows of the target stored as `target_id` (in the history's `targets` table) that touch
/// the time from `from_ms` to `to_ms`, oldest first, for the timeline marks; none if the alert tables don't exist yet.
///
/// # Errors
///
/// [`HistoryError::Db`] if SQLite fails.
pub(crate) fn entries_of_target(
    connection: &Connection,
    target_id: i64,
    from_ms: i64,
    to_ms: i64,
) -> Result<Vec<JournalEntry>, HistoryError> {
    if !has_tables(connection)? {
        return Ok(Vec::new());
    }
    let mut statement = connection.prepare(&format!(
        "SELECT {ENTRY_COLUMNS} FROM alert_events
         WHERE lower(host) = (SELECT name FROM targets WHERE id = ?1)
           AND started_ms <= ?3 AND (ended_ms IS NULL OR ended_ms >= ?2)
         ORDER BY started_ms, id"
    ))?;
    let rows = statement.query_map(params![target_id, from_ms, to_ms], entry_of)?;
    Ok(rows.filter_map(Result::transpose).collect::<Result<_, _>>()?)
}

/// Deletes the journal rows of the host stored as `name` (lowercase) that ended before `before_ms`; none if the
/// alert tables don't exist yet. Rows still going on stay: the alert engine ends them. Returns how many.
///
/// # Errors
///
/// [`HistoryError::Db`] if SQLite fails.
pub(crate) fn forget_target(connection: &Connection, name: &str, before_ms: i64) -> Result<usize, HistoryError> {
    if !has_tables(connection)? {
        return Ok(0);
    }
    Ok(connection.execute(
        "DELETE FROM alert_events WHERE lower(host) = ?1 AND ended_ms IS NOT NULL AND ended_ms < ?2",
        params![name, before_ms],
    )?)
}

/// Deletes the journal rows that ended before `before_ms` (with their timeline marks); none if the alert tables
/// don't exist yet. Returns how many.
///
/// # Errors
///
/// [`HistoryError::Db`] if SQLite fails.
pub(crate) fn prune_journal(connection: &Connection, before_ms: i64) -> Result<usize, HistoryError> {
    if !has_tables(connection)? {
        return Ok(0);
    }
    Ok(connection.execute("DELETE FROM alert_events WHERE ended_ms < ?1", [before_ms])?)
}

/// A journal row read with [`ENTRY_COLUMNS`]; `None` for a kind this version doesn't know.
fn entry_of(row: &Row<'_>) -> rusqlite::Result<Option<JournalEntry>> {
    let kind: String = row.get(2)?;
    let Some(condition) = condition_of(&kind, row.get(3)?, row.get(4)?) else {
        return Ok(None);
    };
    let happening = match condition {
        Condition::RouteChanged => {
            Happening::Route(RouteNote::from_text(&row.get::<_, Option<String>>(7)?.unwrap_or_default()))
        }
        condition => Happening::Alert {
            condition,
            value: row.get::<_, Option<f64>>(5)?.unwrap_or_default(),
            worst: row.get::<_, Option<f64>>(6)?.unwrap_or_default(),
        },
    };
    Ok(Some(JournalEntry {
        id: row.get(0)?,
        host: row.get(1)?,
        happening,
        started_ms: row.get(8)?,
        ended_ms: row.get(9)?,
        recovered: row.get::<_, i64>(10)? != 0,
    }))
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
    use std::net::IpAddr;

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
    fn the_journal_keeps_alerts_and_route_changes_and_words_them_when_read() {
        let store = AlertStore::open_in_memory().expect("store opens");
        let loss = Condition::Loss { percent: 10.0, window: Duration::from_secs(120) };
        store.start(2, "1.1.1.1", &loss, 1_000, 20.0).expect("started");
        let (old, new): (IpAddr, IpAddr) = ("10.0.0.2".parse().expect("valid"), "10.0.0.5".parse().expect("valid"));
        store.note(1, "1.1.1.1", RouteNote { hop: Some((2, old, new)), hops: None }, 1_500).expect("noted");
        store.end(2, "1.1.1.1", 121_000, 35.0, true).expect("ended");
        let journal = store.journal().expect("read");
        let english: Vec<(String, Option<String>, Option<String>)> = journal
            .iter()
            .map(|entry| {
                (entry.what(Language::English), entry.lasted(Language::English), entry.worst(Language::English))
            })
            .collect();
        assert_eq!(
            english,
            [
                ("Route changed: hop 2 10.0.0.2 \u{2192} 10.0.0.5".to_owned(), None, None),
                ("Loss above 10 % over 2 minutes".to_owned(), Some("2 minutes".to_owned()), Some("35 %".to_owned())),
            ]
        );
        let alert = journal.get(1).expect("the alert");
        assert_eq!(
            alert.marks(Language::English),
            [
                (1_000, "Alert: Loss 20 % (rule: Loss above 10 % over 2 minutes)".to_owned()),
                (121_000, "Back to normal after 2 minutes (worst: Loss 35 %)".to_owned())
            ]
        );
        assert_ne!(alert.what(Language::Russian), alert.what(Language::English), "worded in the language shown");
        store.start(2, "GitHub.com", &loss, 1_000, 20.0).expect("started");
        store.start(2, "1.1.1.1", &loss, 200_000, 20.0).expect("started");
        assert_eq!(
            forget_target(&store.connection, "1.1.1.1", 300_000).ok(),
            Some(2),
            "the ended alert and the route change; the ongoing alert and other hosts stay"
        );
        assert_eq!(store.journal().map(|journal| journal.len()).ok(), Some(2));
        store.end(2, "GitHub.com", 250_000, 20.0, true).expect("ended");
        store.end(2, "1.1.1.1", 250_000, 20.0, true).expect("ended");
        store.note(1, "1.1.1.1", RouteNote { hop: Some((2, old, new)), hops: None }, 1_500).expect("noted");
        assert_eq!(prune_journal(&store.connection, 2_000).ok(), Some(1), "the route change ended before 2 s");
        store.clear_journal().expect("cleared");
        assert_eq!(store.journal().map(|journal| journal.len()).ok(), Some(0));
    }
}
