//! Probe history in SQLite: a background [`Recorder`] writes samples in batches, [`Store::buckets`] aggregates
//! them into time buckets for the charts; user comments on the timeline live here too. Samples older than
//! [`RETENTION`] are deleted.

use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, params};

use crate::route::{ProbeOutcome, Sample};

/// How long samples are kept: the longest period the timeline can show.
pub const RETENTION: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// How often the recorder writes collected samples.
const FLUSH_INTERVAL: Duration = Duration::from_secs(1);
/// How often the recorder deletes expired samples.
const PRUNE_INTERVAL: Duration = Duration::from_secs(60 * 60);
/// How long a query waits for another connection's write lock.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Database schema; safe to run on every open.
const SCHEMA: &str = "
    PRAGMA journal_mode = WAL;
    CREATE TABLE IF NOT EXISTS targets (
        id   INTEGER PRIMARY KEY,
        name TEXT NOT NULL UNIQUE
    );
    -- One row per probe. A lost probe has NULL address and rtt_us.
    CREATE TABLE IF NOT EXISTS samples (
        target_id INTEGER NOT NULL REFERENCES targets(id),
        ttl       INTEGER NOT NULL,
        ts_ms     INTEGER NOT NULL, -- Unix time, milliseconds
        address   BLOB,             -- 4 (IPv4) or 16 (IPv6) bytes; rows from before IPv6 hold a u32
        rtt_us    INTEGER,          -- round trip, microseconds
        PRIMARY KEY (target_id, ttl, ts_ms)
    ) WITHOUT ROWID;
    -- Notes the user pinned to a moment of a target's timeline.
    CREATE TABLE IF NOT EXISTS comments (
        id        INTEGER PRIMARY KEY,
        target_id INTEGER NOT NULL REFERENCES targets(id),
        ts_ms     INTEGER NOT NULL,
        text      TEXT NOT NULL
    );
    CREATE INDEX IF NOT EXISTS comments_by_time ON comments (target_id, ts_ms);
";

/// Error of the history database.
#[derive(Debug, thiserror::Error)]
pub enum HistoryError {
    /// SQLite failed.
    #[error("history database: {0}")]
    Db(#[from] rusqlite::Error),
    /// The database folder or the recorder thread couldn't be created.
    #[error("history: {0}")]
    Io(#[from] std::io::Error),
    /// `%LOCALAPPDATA%` is not set, so there is nowhere to keep the database.
    #[error("history: LOCALAPPDATA is not set")]
    NoDataDir,
}

/// Probe statistics of one time bucket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bucket {
    /// Start of the bucket, Unix time in milliseconds.
    pub start_ms: i64,
    /// Probes sent in the bucket.
    pub sent: u32,
    /// Probes answered in the bucket.
    pub received: u32,
    /// Fastest answer, `None` if nothing was answered.
    pub min: Option<Duration>,
    /// Mean answer time, `None` if nothing was answered.
    pub avg: Option<Duration>,
    /// Slowest answer, `None` if nothing was answered.
    pub max: Option<Duration>,
    /// Mean absolute difference between consecutive answers (the first answer of the queried period has no
    /// predecessor), `None` if there is no such difference in the bucket.
    pub jitter: Option<Duration>,
}

/// A note pinned to a moment of a target's timeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comment {
    /// The moment, Unix time in milliseconds.
    pub ts_ms: i64,
    /// The note.
    pub text: String,
}

/// Default database location: `%LOCALAPPDATA%\ping-watch\history.sqlite`.
///
/// # Errors
///
/// [`HistoryError::NoDataDir`] if `%LOCALAPPDATA%` is not set.
pub fn default_path() -> Result<PathBuf, HistoryError> {
    let base = std::env::var_os("LOCALAPPDATA").ok_or(HistoryError::NoDataDir)?;
    Ok(PathBuf::from(base).join("ping-watch").join("history.sqlite"))
}

/// Converts a wall-clock time to Unix milliseconds (0 for times before 1970).
pub fn unix_ms(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .map_or(0, |since| i64::try_from(since.as_millis()).unwrap_or(i64::MAX))
}

/// Converts an optional SQL integer of microseconds to a duration.
fn micros(value: Option<i64>) -> Option<Duration> {
    value.and_then(|us| u64::try_from(us).ok()).map(Duration::from_micros)
}

/// Connection to the history database.
#[derive(Debug)]
pub struct Store {
    /// Open SQLite connection.
    connection: Connection,
}

impl Store {
    /// Opens (creating if needed) the database at `path`.
    ///
    /// # Errors
    ///
    /// [`HistoryError`] if the folder can't be created or the database can't be opened.
    pub fn open(path: &Path) -> Result<Self, HistoryError> {
        if let Some(folder) = path.parent() {
            std::fs::create_dir_all(folder)?;
        }
        Self::init(Connection::open(path)?)
    }

    /// Opens a private database in memory.
    #[cfg(test)]
    fn open_in_memory() -> Result<Self, HistoryError> {
        Self::init(Connection::open_in_memory()?)
    }

    /// Configures `connection` and creates the schema.
    fn init(connection: Connection) -> Result<Self, HistoryError> {
        connection.busy_timeout(BUSY_TIMEOUT)?;
        connection.execute_batch(SCHEMA)?;
        Ok(Self { connection })
    }

    /// Id of the target called `name`, registering it on first use.
    ///
    /// # Errors
    ///
    /// [`HistoryError::Db`] if SQLite fails.
    pub fn target_id(&self, name: &str) -> Result<i64, HistoryError> {
        self.connection.execute("INSERT OR IGNORE INTO targets (name) VALUES (?1)", [name])?;
        Ok(self.connection.query_row("SELECT id FROM targets WHERE name = ?1", [name], |row| row.get(0))?)
    }

    /// Stores `samples` of target `target_id` in one transaction.
    ///
    /// # Errors
    ///
    /// [`HistoryError::Db`] if SQLite fails; then nothing is stored.
    pub fn insert(&mut self, target_id: i64, samples: &[Sample]) -> Result<(), HistoryError> {
        let transaction = self.connection.transaction()?;
        {
            let mut statement = transaction.prepare_cached(
                "INSERT OR REPLACE INTO samples (target_id, ttl, ts_ms, address, rtt_us) VALUES (?1, ?2, ?3, ?4, ?5)",
            )?;
            for sample in samples {
                let (address, rtt_us) = match sample.outcome {
                    ProbeOutcome::Reply { from, rtt } => {
                        let octets = match from {
                            IpAddr::V4(ip) => ip.octets().to_vec(),
                            IpAddr::V6(ip) => ip.octets().to_vec(),
                        };
                        (Some(octets), Some(i64::try_from(rtt.as_micros()).unwrap_or(i64::MAX)))
                    }
                    ProbeOutcome::Lost => (None, None),
                };
                statement.execute(params![target_id, sample.ttl, sample.at_ms, address, rtt_us])?;
            }
        }
        transaction.commit()?;
        Ok(())
    }

    /// Deletes samples taken before `before_ms`; returns how many were deleted.
    ///
    /// # Errors
    ///
    /// [`HistoryError::Db`] if SQLite fails.
    pub fn prune(&self, before_ms: i64) -> Result<usize, HistoryError> {
        Ok(self.connection.execute("DELETE FROM samples WHERE ts_ms < ?1", [before_ms])?)
    }

    /// Statistics of hop `ttl` of target `target_id` in `[from_ms, to_ms)`, grouped into buckets of
    /// `bucket_ms` aligned to `from_ms`. Buckets without samples are omitted.
    ///
    /// # Errors
    ///
    /// [`HistoryError::Db`] if SQLite fails.
    pub fn buckets(
        &self,
        target_id: i64,
        ttl: u8,
        from_ms: i64,
        to_ms: i64,
        bucket_ms: i64,
    ) -> Result<Vec<Bucket>, HistoryError> {
        // Jitter needs the previous *answered* sample, so it is computed over answered samples only and joined
        // back by timestamp (unique per target and hop).
        let mut statement = self.connection.prepare_cached(
            "SELECT ?3 + (s.ts_ms - ?3) / ?5 * ?5 AS start,
                    COUNT(*), COUNT(s.rtt_us), MIN(s.rtt_us), CAST(ROUND(AVG(s.rtt_us)) AS INTEGER), MAX(s.rtt_us),
                    CAST(ROUND(AVG(j.jitter)) AS INTEGER)
             FROM samples AS s
             LEFT JOIN (
                 SELECT ts_ms, ABS(rtt_us - LAG(rtt_us) OVER (ORDER BY ts_ms)) AS jitter
                 FROM samples
                 WHERE target_id = ?1 AND ttl = ?2 AND ts_ms >= ?3 AND ts_ms < ?4 AND rtt_us IS NOT NULL
             ) AS j ON j.ts_ms = s.ts_ms
             WHERE s.target_id = ?1 AND s.ttl = ?2 AND s.ts_ms >= ?3 AND s.ts_ms < ?4
             GROUP BY start
             ORDER BY start",
        )?;
        let rows = statement.query_map(params![target_id, ttl, from_ms, to_ms, bucket_ms.max(1)], |row| {
            Ok(Bucket {
                start_ms: row.get(0)?,
                sent: u32::try_from(row.get::<_, i64>(1)?).unwrap_or(u32::MAX),
                received: u32::try_from(row.get::<_, i64>(2)?).unwrap_or(u32::MAX),
                min: micros(row.get(3)?),
                avg: micros(row.get(4)?),
                max: micros(row.get(5)?),
                jitter: micros(row.get(6)?),
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Time of the oldest stored sample of hop `ttl` of target `target_id`, `None` if there is none.
    ///
    /// # Errors
    ///
    /// [`HistoryError::Db`] if SQLite fails.
    pub fn oldest_ms(&self, target_id: i64, ttl: u8) -> Result<Option<i64>, HistoryError> {
        Ok(self.connection.query_row(
            "SELECT MIN(ts_ms) FROM samples WHERE target_id = ?1 AND ttl = ?2",
            params![target_id, ttl],
            |row| row.get(0),
        )?)
    }

    /// Pins `text` to the moment `ts_ms` of target `target_id`'s timeline.
    ///
    /// # Errors
    ///
    /// [`HistoryError::Db`] if SQLite fails.
    pub fn add_comment(&self, target_id: i64, ts_ms: i64, text: &str) -> Result<(), HistoryError> {
        self.connection.execute(
            "INSERT INTO comments (target_id, ts_ms, text) VALUES (?1, ?2, ?3)",
            params![target_id, ts_ms, text],
        )?;
        Ok(())
    }

    /// Comments of target `target_id` in `[from_ms, to_ms)`, oldest first.
    ///
    /// # Errors
    ///
    /// [`HistoryError::Db`] if SQLite fails.
    pub fn comments(&self, target_id: i64, from_ms: i64, to_ms: i64) -> Result<Vec<Comment>, HistoryError> {
        let mut statement = self.connection.prepare_cached(
            "SELECT ts_ms, text FROM comments WHERE target_id = ?1 AND ts_ms >= ?2 AND ts_ms < ?3 ORDER BY ts_ms",
        )?;
        let rows = statement.query_map(params![target_id, from_ms, to_ms], |row| {
            Ok(Comment { ts_ms: row.get(0)?, text: row.get(1)? })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

/// Background writer of one target's samples; flushes and stops on drop.
#[derive(Debug)]
pub struct Recorder {
    /// Channel the tracer sends samples into; `None` only while dropping.
    sender: Option<Sender<Sample>>,
    /// Writer thread.
    worker: Option<JoinHandle<()>>,
}

impl Recorder {
    /// Opens the database at `path` and starts recording samples of the target called `target`.
    ///
    /// # Errors
    ///
    /// [`HistoryError`] if the database can't be opened or the thread can't be started.
    pub fn start(path: &Path, target: &str) -> Result<Self, HistoryError> {
        let store = Store::open(path)?;
        let target_id = store.target_id(target)?;
        let (sender, receiver) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("history".to_owned())
            .spawn(move || record_loop(store, target_id, &receiver))?;
        Ok(Self { sender: Some(sender), worker: Some(worker) })
    }

    /// A sender for the tracer's workers.
    pub fn sender(&self) -> Option<Sender<Sample>> {
        self.sender.clone()
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        // The loop ends once every sender (ours and the tracer's) is gone, after a final flush.
        self.sender = None;
        if let Some(worker) = self.worker.take()
            && worker.join().is_err()
        {
            log::error!("history thread panicked");
        }
    }
}

/// Writer body: collects samples, writes them every [`FLUSH_INTERVAL`], prunes every [`PRUNE_INTERVAL`].
fn record_loop(mut store: Store, target_id: i64, receiver: &Receiver<Sample>) {
    let mut batch = Vec::new();
    let mut last_prune: Option<Instant> = None;
    let mut connected = true;
    while connected {
        let flush_at = Instant::now().checked_add(FLUSH_INTERVAL);
        loop {
            let wait = flush_at.map_or(FLUSH_INTERVAL, |at| at.saturating_duration_since(Instant::now()));
            match receiver.recv_timeout(wait) {
                Ok(sample) => batch.push(sample),
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => {
                    connected = false;
                    break;
                }
            }
        }
        if !batch.is_empty() {
            if let Err(error) = store.insert(target_id, &batch) {
                log::error!("cannot save {} samples: {error}", batch.len());
            }
            batch.clear();
        }
        if last_prune.is_none_or(|at| at.elapsed() >= PRUNE_INTERVAL) {
            let retention_ms = i64::try_from(RETENTION.as_millis()).unwrap_or(i64::MAX);
            match store.prune(unix_ms(SystemTime::now()).saturating_sub(retention_ms)) {
                Ok(deleted) => log::debug!("pruned {deleted} expired samples"),
                Err(error) => log::error!("cannot prune history: {error}"),
            }
            last_prune = Some(Instant::now());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use super::*;

    /// Router address used in samples.
    const ROUTER: IpAddr = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));

    /// Answered sample of hop `ttl` at `at_ms` with a round trip of `rtt_ms`.
    fn reply(ttl: u8, at_ms: i64, rtt_ms: u64) -> Sample {
        Sample { at_ms, ttl, outcome: ProbeOutcome::Reply { from: ROUTER, rtt: Duration::from_millis(rtt_ms) } }
    }

    /// Lost sample of hop `ttl` at `at_ms`.
    fn lost(ttl: u8, at_ms: i64) -> Sample {
        Sample { at_ms, ttl, outcome: ProbeOutcome::Lost }
    }

    /// In-memory store with one registered target.
    fn store_with_target() -> (Store, i64) {
        let store = Store::open_in_memory().expect("in-memory database opens");
        let target = store.target_id("example").expect("target registers");
        (store, target)
    }

    #[test]
    fn target_id_is_stable() {
        let (store, target) = store_with_target();
        assert_eq!(store.target_id("example").ok(), Some(target));
        assert_ne!(store.target_id("other").ok(), Some(target));
    }

    #[test]
    fn buckets_aggregate_per_hop_and_period() {
        let (mut store, target) = store_with_target();
        let samples = [
            reply(1, 1_000, 10),
            reply(1, 1_500, 30),
            lost(1, 1_900),
            reply(1, 2_000, 20),
            reply(2, 1_000, 99), // other hop
            reply(1, 3_000, 50), // outside the period
        ];
        store.insert(target, &samples).expect("insert works");

        let buckets = store.buckets(target, 1, 1_000, 3_000, 1_000).expect("query works");
        let ms = |value| Some(Duration::from_millis(value));
        assert_eq!(
            buckets,
            [
                // Jitter: 30 after 10 → 20; the lost probe is skipped; 20 after 30 → 10.
                Bucket { start_ms: 1_000, sent: 3, received: 2, min: ms(10), avg: ms(20), max: ms(30), jitter: ms(20) },
                Bucket { start_ms: 2_000, sent: 1, received: 1, min: ms(20), avg: ms(20), max: ms(20), jitter: ms(10) },
            ]
        );
    }

    #[test]
    fn bucket_of_only_losses_has_no_times() {
        let (mut store, target) = store_with_target();
        store.insert(target, &[lost(1, 0), lost(1, 10)]).expect("insert works");
        let buckets = store.buckets(target, 1, 0, 100, 100).expect("query works");
        assert_eq!(
            buckets,
            [Bucket { start_ms: 0, sent: 2, received: 0, min: None, avg: None, max: None, jitter: None }]
        );
    }

    #[test]
    fn oldest_sample_per_hop() {
        let (mut store, target) = store_with_target();
        assert_eq!(store.oldest_ms(target, 1).ok(), Some(None));
        store.insert(target, &[reply(1, 500, 1), reply(1, 300, 1), reply(2, 100, 1)]).expect("insert works");
        assert_eq!(store.oldest_ms(target, 1).ok(), Some(Some(300)));
    }

    #[test]
    fn comments_are_listed_by_period() {
        let (store, target) = store_with_target();
        let other = store.target_id("other").expect("target registers");
        store.add_comment(target, 2_000, "later").expect("comment saved");
        store.add_comment(target, 1_000, "outage").expect("comment saved");
        store.add_comment(other, 1_500, "elsewhere").expect("comment saved");
        store.add_comment(target, 5_000, "outside").expect("comment saved");
        let texts: Vec<String> = store
            .comments(target, 0, 3_000)
            .expect("query works")
            .into_iter()
            .map(|comment| comment.text)
            .collect();
        assert_eq!(texts, ["outage", "later"]);
    }

    #[test]
    fn prune_deletes_only_old_samples() {
        let (mut store, target) = store_with_target();
        store.insert(target, &[reply(1, 100, 1), reply(1, 200, 1), reply(1, 300, 1)]).expect("insert works");
        assert_eq!(store.prune(250).ok(), Some(2));
        let buckets = store.buckets(target, 1, 0, 1_000, 1_000).expect("query works");
        assert_eq!(buckets.first().map(|bucket| bucket.sent), Some(1));
    }

    #[test]
    fn recorder_flushes_on_drop() {
        let folder = std::env::temp_dir().join(format!("ping-watch-test-{}", std::process::id()));
        let path = folder.join("history.sqlite");
        let recorder = Recorder::start(&path, "example").expect("recorder starts");
        let sender = recorder.sender().expect("sender available");
        // A current timestamp, or the recorder's first prune would delete the sample as expired.
        let now = unix_ms(SystemTime::now());
        sender.send(reply(1, now, 10)).expect("recorder is listening");
        drop(sender);
        drop(recorder);

        let store = Store::open(&path).expect("database reopens");
        let target = store.target_id("example").expect("target exists");
        let buckets = store.buckets(target, 1, now, now.saturating_add(1), 1).expect("query works");
        drop(store);
        std::fs::remove_dir_all(&folder).ok();
        assert_eq!(buckets.first().map(|bucket| bucket.sent), Some(1));
    }
}
