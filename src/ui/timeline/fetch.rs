//! Background history access for the timeline: a thread with its own database connection runs the chart queries
//! and saves, edits and deletes comments, so a slow 7-day query never freezes the window.

use std::path::Path;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};

use super::series::{Period, Window};
use crate::store::alerts::JournalEntry;
use crate::store::history::{Bucket, Comment, HistoryError, HopKey, Store};

/// What the chart needs: one hop of one target over one window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Request {
    /// Target id in the history.
    pub target_id: i64,
    /// Hop.
    pub hop: HopKey,
    /// Period the window was built from (two periods may share a bucket size).
    pub period: Period,
    /// Time range and bucket size.
    pub window: Window,
}

/// Answer to a [`Request`].
#[derive(Debug, Clone, PartialEq)]
pub struct Fetched {
    /// The request answered.
    pub request: Request,
    /// The hop's statistics per bucket.
    pub buckets: Vec<Bucket>,
    /// The target's comments in the window.
    pub comments: Vec<Comment>,
    /// The target's alerts touching the window (their marks are drawn on the chart).
    pub alerts: Vec<JournalEntry>,
    /// Time of the hop's oldest stored sample, the left end of the scroll slider.
    pub oldest_ms: Option<i64>,
}

/// A change to the comments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommentChange {
    /// A new comment on target `target_id` at `ts_ms`.
    Add {
        /// Target id in the history.
        target_id: i64,
        /// The moment commented, Unix ms.
        ts_ms: i64,
        /// The note.
        text: String,
    },
    /// New text of comment `id`.
    Edit {
        /// Row id.
        id: i64,
        /// The note.
        text: String,
    },
    /// Delete comment `id`.
    Delete(i64),
}

/// Work for the thread.
#[derive(Debug, Clone)]
enum Job {
    /// Run the chart queries.
    Fetch(Request),
    /// Change the comments.
    Comment(CommentChange),
}

/// Handle of the background thread; stops it on drop.
#[derive(Debug)]
pub struct Fetcher {
    /// Job queue; `None` only while dropping.
    jobs: Option<Sender<Job>>,
    /// Answers to fetch jobs.
    results: Receiver<Result<Fetched, String>>,
    /// Comment changes that failed, as messages for the user.
    failures: Receiver<String>,
    /// The thread.
    worker: Option<JoinHandle<()>>,
}

impl Fetcher {
    /// Opens its own connection to the database at `path` and starts the thread.
    ///
    /// # Errors
    ///
    /// [`HistoryError`] if the database can't be opened or the thread can't be started.
    pub fn start(path: &Path) -> Result<Self, HistoryError> {
        let store = Store::open(path)?;
        let (jobs, job_queue) = mpsc::channel();
        let (result_sender, results) = mpsc::channel();
        let (failure_sender, failures) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("timeline".to_owned())
            .spawn(move || work(&store, &job_queue, &result_sender, &failure_sender))?;
        Ok(Self { jobs: Some(jobs), results, failures, worker: Some(worker) })
    }

    /// Queues the chart queries for `request`; only the newest queued fetch is run.
    pub fn fetch(&self, request: Request) {
        self.send(Job::Fetch(request));
    }

    /// Queues a change to the comments.
    pub fn change_comment(&self, change: CommentChange) {
        self.send(Job::Comment(change));
    }

    /// The next answer, if one arrived.
    pub fn try_result(&self) -> Option<Result<Fetched, String>> {
        self.results.try_recv().ok()
    }

    /// The next failed comment change, if any.
    pub fn try_failure(&self) -> Option<String> {
        self.failures.try_recv().ok()
    }

    /// Queues `job`, logging if the thread is gone.
    fn send(&self, job: Job) {
        if self.jobs.as_ref().is_none_or(|jobs| jobs.send(job).is_err()) {
            log::error!("timeline thread is gone");
        }
    }
}

impl Drop for Fetcher {
    fn drop(&mut self) {
        self.jobs = None;
        if let Some(worker) = self.worker.take()
            && worker.join().is_err()
        {
            log::error!("timeline thread panicked");
        }
    }
}

/// Thread body: runs jobs until the queue closes. Comment changes are made in order; of several queued fetches
/// only the last one runs, since the earlier answers would be stale anyway.
fn work(store: &Store, jobs: &Receiver<Job>, results: &Sender<Result<Fetched, String>>, failures: &Sender<String>) {
    while let Ok(first) = jobs.recv() {
        let mut fetch = None;
        for job in std::iter::once(first).chain(jobs.try_iter()) {
            match job {
                Job::Fetch(request) => fetch = Some(request),
                Job::Comment(change) => {
                    let done = match &change {
                        CommentChange::Add { target_id, ts_ms, text } => store.add_comment(*target_id, *ts_ms, text),
                        CommentChange::Edit { id, text } => store.edit_comment(*id, text),
                        CommentChange::Delete(id) => store.delete_comment(*id),
                    };
                    if let Err(error) = done {
                        log::error!("cannot change the comment: {error}");
                        failures.send(error.to_string()).ok();
                    }
                }
            }
        }
        if let Some(request) = fetch
            && results.send(run(store, request).map_err(|error| error.to_string())).is_err()
        {
            return;
        }
    }
}

/// Runs the chart queries of `request`.
fn run(store: &Store, request: Request) -> Result<Fetched, HistoryError> {
    let Request { target_id, hop, window, .. } = request;
    Ok(Fetched {
        request,
        buckets: store.buckets(target_id, hop, window.from_ms, window.to_ms, window.bucket_ms)?,
        comments: store.comments(target_id, window.from_ms, window.to_ms)?,
        alerts: store.alerts(target_id, window.from_ms, window.to_ms)?,
        oldest_ms: store.oldest_ms(target_id, hop)?,
    })
}
