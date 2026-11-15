//! One Ping Watch per Windows session: the first instance owns a named event; a second one (say, started from the
//! Start menu while the first hides in the tray) signals that event, so the first shows its window, and exits.
//! Two instances would ping the same targets twice and overwrite each other's tabs and settings in one database.

use std::thread;

use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
use windows::Win32::System::Threading::{CreateEventW, INFINITE, SetEvent, WaitForSingleObject};
use windows::Win32::UI::WindowsAndMessaging::{ASFW_ANY, AllowSetForegroundWindow};
use windows::core::{PCWSTR, w};

/// The event's name; `Local\` keeps it to the user's session.
const EVENT_NAME: PCWSTR = w!("Local\\iRingil.PingWatch.Show");

/// The named event of the first instance.
#[derive(Debug)]
pub struct FirstInstance(Event);

/// Whether this process is the first instance.
#[derive(Debug)]
pub enum Claim {
    /// It is: it keeps the event.
    First(FirstInstance),
    /// Another instance runs and was asked to show its window; this one should exit.
    Other,
    /// The event couldn't be created; this one runs anyway, as if alone.
    Unknown,
}

/// Creates the named event, or asks the instance that has it to show itself.
pub fn claim() -> Claim {
    claim_named(EVENT_NAME)
}

/// [`claim`] with the event called `name` (the tests use their own, so a running Ping Watch doesn't disturb them).
fn claim_named(name: PCWSTR) -> Claim {
    // SAFETY: no security attributes, an auto-reset event, not signalled, a NUL-terminated name.
    let created = unsafe { CreateEventW(None, false, false, name) };
    // SAFETY: no preconditions; reads the error of the call just made on this thread.
    let existed = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    let event = match created {
        Ok(handle) => Event(handle),
        Err(error) => {
            log::warn!("cannot check for another Ping Watch: {error}");
            return Claim::Unknown;
        }
    };
    if !existed {
        return Claim::First(FirstInstance(event));
    }
    // The first instance may bring its window to the front although this process is the one the user started.
    // SAFETY: no preconditions; failure only means Windows keeps its focus rules.
    if let Err(error) = unsafe { AllowSetForegroundWindow(ASFW_ANY) } {
        log::debug!("cannot pass the foreground on: {error}");
    }
    // SAFETY: the handle is the open event created above.
    if let Err(error) = unsafe { SetEvent(event.0) } {
        log::warn!("cannot ask the running Ping Watch to show itself: {error}");
    }
    Claim::Other
}

impl FirstInstance {
    /// Runs `show` on a thread of its own every time another instance asks for the window.
    pub fn on_show(self, show: impl Fn() + Send + 'static) {
        let event = self.0;
        let spawned = thread::Builder::new().name("single-instance".to_owned()).spawn(move || {
            loop {
                // SAFETY: the handle stays open while `event` lives, which is as long as this thread.
                let waited = unsafe { WaitForSingleObject(event.handle(), INFINITE) };
                if waited.0 != 0 {
                    log::warn!("the single-instance wait failed ({})", waited.0);
                    return;
                }
                show();
            }
        });
        if let Err(error) = spawned {
            log::warn!("cannot wait for other instances: {error}");
        }
    }
}

/// An open event handle, closed on drop.
#[derive(Debug)]
struct Event(HANDLE);

// SAFETY: an event handle is a kernel object handle, usable from any thread; `Event` owns it alone.
unsafe impl Send for Event {}

impl Event {
    /// The handle (a method, so a closure moves the whole `Event`, not just the bare handle).
    fn handle(&self) -> HANDLE {
        self.0
    }
}

impl Drop for Event {
    fn drop(&mut self) {
        // SAFETY: the handle was opened by `CreateEventW` and is closed only here.
        if let Err(error) = unsafe { CloseHandle(self.0) } {
            log::debug!("cannot close the event: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_claim_finds_the_first() {
        let name = w!("Local\\iRingil.PingWatch.Test");
        let first = claim_named(name);
        assert!(matches!(first, Claim::First(_)), "{first:?}");
        assert!(matches!(claim_named(name), Claim::Other));
        drop(first);
        assert!(matches!(claim_named(name), Claim::First(_)), "the event is gone with the first instance");
    }
}
