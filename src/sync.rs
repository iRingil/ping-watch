//! Small synchronization helpers shared by modules that use background threads.

use std::sync::{Mutex, MutexGuard, PoisonError};

/// Locks `mutex`, ignoring poisoning: every guarded update in this crate is a single method call, so the data
/// stays consistent even if a thread panicked while holding the lock.
pub fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
