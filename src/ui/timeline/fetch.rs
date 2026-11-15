//! Background history access for the timeline: a thread with its own database connection runs the chart queries
//! and saves comments, so a slow 7-day query never freezes the window.

use std::path::Path;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};

use super::series::{Period, Window};
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
    /// Time of the hop's oldest stored sample, the left end of the scroll slider.
    pub oldest_ms: Option<i64>,
}

/// Work for the thread.
#[derive(Debug, Clone)]
enum Job {
    /// Run the chart queries.
    Fetch(Request),
    /// Save a comment.
    Comment {
        /// Target id in the history.
        target_id: i64,
        /// The moment commented, Unix ms.
        ts_ms: i64,
        /// The note.
        text: String,
    },
}

/// Handle of the background thread; stops it on drop.
#[derive(Debug)]
pub struct Fetcher {
    /// Job queue; `None` only while dropping.
    jobs: Option<Sender<Job>>,
    /// Answers to fetch jobs.
    results: Receiver<Result<Fetched, String>>,
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
        let worker = thread::Builder::new()
            .name("timeline".to_owned())
            .spawn(move || work(&store, &job_queue, &result_sender))?;
        Ok(Self { jobs: Some(jobs), results, worker: Some(worker) })
    }

    /// Queues the chart queries for `request`; only the newest queued fetch is run.
    pub fn fetch(&self, request: Request) {
        self.send(Job::Fetch(request));
    }

    /// Queues saving a comment.
    pub fn add_comment(&self, target_id: i64, ts_ms: i64, text: String) {
        self.send(Job::Comment { target_id, ts_ms, text });
    }

    /// The next answer, if one arrived.
    pub fn try_result(&self) -> Option<Result<Fetched, String>> {
        self.results.try_recv().ok()
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

/// Thread body: runs jobs until the queue closes. Comments are saved in order; of several queued fetches only the
/// last one runs, since the earlier answers would be stale anyway.
fn work(store: &Store, jobs: &Receiver<Job>, results: &Sender<Result<Fetched, String>>) {
    while let Ok(first) = jobs.recv() {
        let mut fetch = None;
        for job in std::iter::once(first).chain(jobs.try_iter()) {
            match job {
                Job::Fetch(request) => fetch = Some(request),
                Job::Comment { target_id, ts_ms, text } => {
                    if let Err(error) = store.add_comment(target_id, ts_ms, &text) {
                        log::error!("cannot save comment: {error}");
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
        oldest_ms: store.oldest_ms(target_id, hop)?,
    })
}
