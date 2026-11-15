//! Probe history in SQLite: a background [`Recorder`] writes samples in batches, [`Store::buckets`] aggregates
//! them into time buckets for the charts; user comments on the timeline live here too. Samples older than the
//! retention period (a setting) are deleted.

use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, params};

use crate::probe::route::{ProbeOutcome, Sample};
use crate::store::alerts::JournalEntry;

/// Rows before the current one in the rolling jitter window (the window holds this + 1 answers). A macro so it can
/// go into the SQL text with `concat!`.
macro_rules! jitter_window_preceding {
    () => {
        "9"
    };
}

/// How often the recorder writes collected samples.
const FLUSH_INTERVAL: Duration = Duration::from_secs(1);
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
        address   BLOB,             -- 4 (IPv4) or 16 (IPv6) bytes
        rtt_us    INTEGER,          -- round trip, microseconds
        at_target INTEGER NOT NULL, -- 1 if the TTL was the route length: the probe was meant for the target
        PRIMARY KEY (target_id, ttl, ts_ms)
    ) WITHOUT ROWID;
    -- The target's own history, whatever its hop number was at the time.
    CREATE INDEX IF NOT EXISTS samples_at_target ON samples (target_id, ts_ms) WHERE at_target = 1;
    -- Notes the user pinned to a moment of a target's timeline.
    CREATE TABLE IF NOT EXISTS comments (
        id        INTEGER PRIMARY KEY,
        target_id INTEGER NOT NULL REFERENCES targets(id),
        ts_ms     INTEGER NOT NULL,
        text      TEXT NOT NULL
    );
    CREATE INDEX IF NOT EXISTS comments_by_time ON comments (target_id, ts_ms);
";

/// Which samples of a target make up one line of history.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HopKey {
    /// The hop with this TTL.
    Ttl(u8),
    /// The target itself, at whatever TTL it was reached (the route may have grown or shrunk).
    Target,
}

impl HopKey {
    /// SQL condition selecting the samples, and the value bound to its `?2`. Both forms take `?2`, so the queries
    /// keep the same parameters.
    fn condition(self) -> (&'static str, u8) {
        match self {
            Self::Ttl(ttl) => ("ttl = ?2", ttl),
            Self::Target => ("at_target = 1 AND ?2 = 0", 0),
        }
    }
}

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
    /// Rolling jitter: the mean absolute difference between consecutive answers over the last ten answers, averaged
    /// over the bucket (the queried period's first answers have shorter windows); `None` if there is none.
    pub jitter: Option<Duration>,
}

/// A note pinned to a moment of a target's timeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comment {
    /// Row id.
    pub id: i64,
    /// The moment, Unix time in milliseconds.
    pub ts_ms: i64,
    /// The note.
    pub text: String,
}

/// Marker file next to the executable that makes the copy portable.
pub const PORTABLE_MARKER: &str = "portable.txt";
/// Folder of a portable copy's data, next to the executable.
const PORTABLE_DATA: &str = "userdata";

/// Default database location: `userdata\history.sqlite` next to the executable of a portable copy (one with
/// [`PORTABLE_MARKER`] beside it), otherwise `%LOCALAPPDATA%\Ping Watch\history.sqlite`.
///
/// # Errors
///
/// [`HistoryError::NoDataDir`] if the copy isn't portable and `%LOCALAPPDATA%` is not set.
pub fn default_path() -> Result<PathBuf, HistoryError> {
    let exe_dir = std::env::current_exe().ok().and_then(|exe| exe.parent().map(Path::to_path_buf));
    let portable = exe_dir.filter(|dir| dir.join(PORTABLE_MARKER).is_file());
    data_dir(portable.as_deref(), std::env::var_os("LOCALAPPDATA").as_deref())
        .map(|dir| dir.join("history.sqlite"))
        .ok_or(HistoryError::NoDataDir)
}

/// The data folder: `userdata` in `portable_dir` (the folder of a portable copy), else `Ping Watch` in
/// `local_app_data`; `None` if neither is known.
fn data_dir(portable_dir: Option<&Path>, local_app_data: Option<&std::ffi::OsStr>) -> Option<PathBuf> {
    match portable_dir {
        Some(dir) => Some(dir.join(PORTABLE_DATA)),
        None => local_app_data.map(|base| PathBuf::from(base).join(crate::APP_NAME)),
    }
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

/// Opens the database file at `path` (creating its folder if needed) with a busy timeout, so a write that meets
/// another connection's write (the recorders write every second) waits instead of failing at once.
///
/// # Errors
///
/// [`HistoryError`] if the folder can't be created or the database can't be opened.
pub(crate) fn connect(path: &Path) -> Result<Connection, HistoryError> {
    if let Some(folder) = path.parent() {
        std::fs::create_dir_all(folder)?;
    }
    let connection = Connection::open(path)?;
    connection.busy_timeout(BUSY_TIMEOUT)?;
    // Takes effect only while the file is still empty: a new database gives pages back after pruning.
    connection.execute_batch("PRAGMA auto_vacuum = INCREMENTAL;")?;
    Ok(connection)
}

impl Store {
    /// Opens (creating if needed) the database at `path`.
    ///
    /// # Errors
    ///
    /// [`HistoryError`] if the folder can't be created or the database can't be opened.
    pub fn open(path: &Path) -> Result<Self, HistoryError> {
        Self::init(connect(path)?)
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
                "INSERT OR REPLACE INTO samples (target_id, ttl, ts_ms, address, rtt_us, at_target)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
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
                statement.execute(params![target_id, sample.ttl, sample.at_ms, address, rtt_us, sample.at_target])?;
            }
        }
        transaction.commit()?;
        Ok(())
    }

    /// Deletes everything older than `before_ms`: samples, comments and the alert journal (with the timeline marks
    /// made from it), then gives the freed pages back to the file system. Returns how many samples were deleted.
    ///
    /// # Errors
    ///
    /// [`HistoryError::Db`] if SQLite fails.
    pub fn prune(&self, before_ms: i64) -> Result<usize, HistoryError> {
        let samples = self.connection.execute("DELETE FROM samples WHERE ts_ms < ?1", [before_ms])?;
        self.connection.execute("DELETE FROM comments WHERE ts_ms < ?1", [before_ms])?;
        crate::store::alerts::prune_journal(&self.connection, before_ms)?;
        // Only shrinks a database made with auto_vacuum = INCREMENTAL (see `connect`); a no-op otherwise.
        self.connection.execute_batch("PRAGMA incremental_vacuum;")?;
        Ok(samples)
    }

    /// Statistics of hop `hop` of target `target_id` in `[from_ms, to_ms)`, grouped into buckets of
    /// `bucket_ms` aligned to `from_ms`. Buckets without samples are omitted.
    ///
    /// # Errors
    ///
    /// [`HistoryError::Db`] if SQLite fails.
    pub fn buckets(
        &self,
        target_id: i64,
        hop: HopKey,
        from_ms: i64,
        to_ms: i64,
        bucket_ms: i64,
    ) -> Result<Vec<Bucket>, HistoryError> {
        // Jitter, as in PingPlotter's graph: the mean absolute difference between consecutive *answered* samples
        // over the last JITTER_WINDOW answers, so a spike shows as a plateau rather than two needles. Computed over
        // answered samples only and joined back by timestamp (unique per target and hop).
        let (condition, key) = hop.condition();
        let mut statement = self.connection.prepare_cached(&format!(
            concat!(
                "SELECT ?3 + (s.ts_ms - ?3) / ?5 * ?5 AS start,
                    COUNT(*), COUNT(s.rtt_us), MIN(s.rtt_us), CAST(ROUND(AVG(s.rtt_us)) AS INTEGER), MAX(s.rtt_us),
                    CAST(ROUND(AVG(j.jitter)) AS INTEGER)
             FROM samples AS s
             LEFT JOIN (
                 SELECT ts_ms, AVG(step) OVER (ORDER BY ts_ms ROWS BETWEEN ",
                jitter_window_preceding!(),
                " PRECEDING AND CURRENT ROW) AS jitter
                 FROM (
                     SELECT ts_ms, ABS(rtt_us - LAG(rtt_us) OVER (ORDER BY ts_ms)) AS step
                     FROM samples
                     WHERE target_id = ?1 AND {condition} AND ts_ms >= ?3 AND ts_ms < ?4 AND rtt_us IS NOT NULL
                 )
             ) AS j ON j.ts_ms = s.ts_ms
             WHERE s.target_id = ?1 AND {condition} AND s.ts_ms >= ?3 AND s.ts_ms < ?4
             GROUP BY start
             ORDER BY start"
            ),
            condition = condition
        ))?;
        let rows = statement.query_map(params![target_id, key, from_ms, to_ms, bucket_ms.max(1)], |row| {
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

    /// Time of the oldest stored sample of hop `hop` of target `target_id`, `None` if there is none.
    ///
    /// # Errors
    ///
    /// [`HistoryError::Db`] if SQLite fails.
    pub fn oldest_ms(&self, target_id: i64, hop: HopKey) -> Result<Option<i64>, HistoryError> {
        let (condition, key) = hop.condition();
        Ok(self.connection.query_row(
            &format!("SELECT MIN(ts_ms) FROM samples WHERE target_id = ?1 AND {condition}"),
            params![target_id, key],
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

    /// Changes the text of comment `id`.
    ///
    /// # Errors
    ///
    /// [`HistoryError::Db`] if SQLite fails.
    pub fn edit_comment(&self, id: i64, text: &str) -> Result<(), HistoryError> {
        self.connection.execute("UPDATE comments SET text = ?2 WHERE id = ?1", params![id, text])?;
        Ok(())
    }

    /// Deletes comment `id`.
    ///
    /// # Errors
    ///
    /// [`HistoryError::Db`] if SQLite fails.
    pub fn delete_comment(&self, id: i64) -> Result<(), HistoryError> {
        self.connection.execute("DELETE FROM comments WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Comments of target `target_id` in `[from_ms, to_ms)`, oldest first.
    ///
    /// # Errors
    ///
    /// [`HistoryError::Db`] if SQLite fails.
    pub fn comments(&self, target_id: i64, from_ms: i64, to_ms: i64) -> Result<Vec<Comment>, HistoryError> {
        let mut statement = self.connection.prepare_cached(
            "SELECT id, ts_ms, text FROM comments WHERE target_id = ?1 AND ts_ms >= ?2 AND ts_ms < ?3 ORDER BY ts_ms",
        )?;
        let rows = statement.query_map(params![target_id, from_ms, to_ms], |row| {
            Ok(Comment { id: row.get(0)?, ts_ms: row.get(1)?, text: row.get(2)? })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// The alert journal rows of target `target_id` touching `[from_ms, to_ms]`, for the timeline marks.
    ///
    /// # Errors
    ///
    /// [`HistoryError::Db`] if SQLite fails.
    pub fn alerts(&self, target_id: i64, from_ms: i64, to_ms: i64) -> Result<Vec<JournalEntry>, HistoryError> {
        crate::store::alerts::entries_of_target(&self.connection, target_id, from_ms, to_ms)
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

/// Writer body: collects samples and writes them every [`FLUSH_INTERVAL`].
fn record_loop(mut store: Store, target_id: i64, receiver: &Receiver<Sample>) {
    let mut batch = Vec::new();
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
    }
}

/// Deletes what is older than `retention` from the database at `path` on a thread of its own (a large delete
/// takes a while); the app runs it hourly and when the retention setting changes.
pub fn prune_in_background(path: &Path, retention: Duration) {
    let path = path.to_path_buf();
    let spawned = thread::Builder::new().name("prune".to_owned()).spawn(move || {
        let retention_ms = i64::try_from(retention.as_millis()).unwrap_or(i64::MAX);
        let before_ms = unix_ms(SystemTime::now()).saturating_sub(retention_ms);
        match Store::open(&path).and_then(|store| store.prune(before_ms)) {
            Ok(deleted) => log::debug!("pruned {deleted} expired samples"),
            Err(error) => log::error!("cannot prune the history: {error}"),
        }
    });
    if let Err(error) = spawned {
        log::error!("cannot start pruning the history: {error}");
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
        let outcome = ProbeOutcome::Reply { from: ROUTER, rtt: Duration::from_millis(rtt_ms) };
        Sample { at_ms, ttl, outcome, at_target: false }
    }

    /// Lost sample of hop `ttl` at `at_ms`.
    fn lost(ttl: u8, at_ms: i64) -> Sample {
        Sample { at_ms, ttl, outcome: ProbeOutcome::Lost, at_target: false }
    }

    /// In-memory store with one registered target.
    fn store_with_target() -> (Store, i64) {
        let store = Store::open_in_memory().expect("in-memory database opens");
        let target = store.target_id("example").expect("target registers");
        (store, target)
    }

    #[test]
    fn portable_copies_keep_data_beside_the_program() {
        let local = std::ffi::OsStr::new(r"C:\Users\me\AppData\Local");
        assert_eq!(
            data_dir(Some(Path::new(r"E:\Tools\Ping Watch")), Some(local)),
            Some(PathBuf::from(r"E:\Tools\Ping Watch\userdata"))
        );
        assert_eq!(data_dir(None, Some(local)), Some(PathBuf::from(r"C:\Users\me\AppData\Local\Ping Watch")));
        assert_eq!(data_dir(None, None), None);
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

        let buckets = store.buckets(target, HopKey::Ttl(1), 1_000, 3_000, 1_000).expect("query works");
        let ms = |value| Some(Duration::from_millis(value));
        assert_eq!(
            buckets,
            [
                // Steps between answers: 30 after 10 → 20, the lost probe skipped, 20 after 30 → 10. Rolling jitter:
                // 20 at 1.5 s, (20 + 10) / 2 = 15 at 2 s.
                Bucket { start_ms: 1_000, sent: 3, received: 2, min: ms(10), avg: ms(20), max: ms(30), jitter: ms(20) },
                Bucket { start_ms: 2_000, sent: 1, received: 1, min: ms(20), avg: ms(20), max: ms(20), jitter: ms(15) },
            ]
        );
    }

    #[test]
    fn target_history_follows_the_route_length() {
        let (mut store, target) = store_with_target();
        let at_target = |sample: Sample| Sample { at_target: true, ..sample };
        // The route grows from 2 to 3 hops at 2 s; hop 2 then is a router.
        let samples = [
            at_target(reply(2, 1_000, 10)),
            at_target(lost(2, 1_500)),
            reply(2, 2_000, 5),
            at_target(reply(3, 2_000, 30)),
        ];
        store.insert(target, &samples).expect("insert works");
        let buckets = store.buckets(target, HopKey::Target, 0, 3_000, 1_000).expect("query works");
        let summary: Vec<(i64, u32, u32)> =
            buckets.iter().map(|bucket| (bucket.start_ms, bucket.sent, bucket.received)).collect();
        assert_eq!(summary, [(1_000, 2, 1), (2_000, 1, 1)]);
        assert_eq!(buckets.last().and_then(|bucket| bucket.avg), Some(Duration::from_millis(30)));
        assert_eq!(store.oldest_ms(target, HopKey::Target).ok(), Some(Some(1_000)));
        assert_eq!(store.oldest_ms(target, HopKey::Ttl(3)).ok(), Some(Some(2_000)));
    }

    #[test]
    fn a_spike_raises_jitter_for_the_whole_window() {
        let (mut store, target) = store_with_target();
        let samples: Vec<Sample> = (0_i64..25)
            .map(|second| reply(1, second.saturating_mul(1_000), if second == 5 { 300 } else { 50 }))
            .collect();
        store.insert(target, &samples).expect("insert works");
        let buckets = store.buckets(target, HopKey::Ttl(1), 0, 25_000, 1_000).expect("query works");
        let jitter_at = |second: usize| buckets.get(second).and_then(|bucket| bucket.jitter);
        // The jump up (at 5 s) and back down (at 6 s) stay in the 10-answer window until 15 s.
        assert_eq!(jitter_at(4), Some(Duration::ZERO));
        assert!((5..=15).all(|second| jitter_at(second) > Some(Duration::ZERO)), "plateau while in the window");
        assert_eq!(jitter_at(16), Some(Duration::ZERO));
    }

    #[test]
    fn bucket_of_only_losses_has_no_times() {
        let (mut store, target) = store_with_target();
        store.insert(target, &[lost(1, 0), lost(1, 10)]).expect("insert works");
        let buckets = store.buckets(target, HopKey::Ttl(1), 0, 100, 100).expect("query works");
        assert_eq!(
            buckets,
            [Bucket { start_ms: 0, sent: 2, received: 0, min: None, avg: None, max: None, jitter: None }]
        );
    }

    #[test]
    fn oldest_sample_per_hop() {
        let (mut store, target) = store_with_target();
        assert_eq!(store.oldest_ms(target, HopKey::Ttl(1)).ok(), Some(None));
        store.insert(target, &[reply(1, 500, 1), reply(1, 300, 1), reply(2, 100, 1)]).expect("insert works");
        assert_eq!(store.oldest_ms(target, HopKey::Ttl(1)).ok(), Some(Some(300)));
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
        store.add_comment(target, 100, "old").expect("comment saved");
        store.add_comment(target, 300, "new").expect("comment saved");
        assert_eq!(store.prune(250).ok(), Some(2));
        let buckets = store.buckets(target, HopKey::Ttl(1), 0, 1_000, 1_000).expect("query works");
        assert_eq!(buckets.first().map(|bucket| bucket.sent), Some(1));
        let comments = store.comments(target, 0, 1_000).expect("query works");
        assert_eq!(comments.iter().map(|comment| comment.text.as_str()).collect::<Vec<_>>(), ["new"]);
    }

    #[test]
    fn comments_are_edited_and_deleted() {
        let (store, target) = store_with_target();
        store.add_comment(target, 1_000, "typo").expect("comment saved");
        let texts = |store: &Store| -> Vec<(i64, String)> {
            let comments = store.comments(target, 0, 2_000).unwrap_or_default();
            comments.into_iter().map(|comment| (comment.id, comment.text)).collect()
        };
        let id = texts(&store).first().map(|(id, _)| *id).expect("listed");
        store.edit_comment(id, "fixed").expect("edited");
        assert_eq!(texts(&store), [(id, "fixed".to_owned())]);
        store.delete_comment(id).expect("deleted");
        assert!(texts(&store).is_empty());
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
        let buckets = store.buckets(target, HopKey::Ttl(1), now, now.saturating_add(1), 1).expect("query works");
        drop(store);
        std::fs::remove_dir_all(&folder).ok();
        assert_eq!(buckets.first().map(|bucket| bucket.sent), Some(1));
    }
}
