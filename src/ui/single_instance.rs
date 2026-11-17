//! One Ping Watch per Windows session: the first instance owns a named event; a second one (say, started from the
//! Start menu while the first hides in the tray) signals that event, so the first shows its window, and exits.
//! Two instances would ping the same targets twice and overwrite each other's tabs and settings in one database.
//!
//! The first instance also holds a named mutex while it runs and waits on a second event that asks it to quit:
//! the installer and the uninstaller (`infrastructure/inno/PingWatch.iss`) signal it and wait for the mutex to go,
//! as Windows' Restart Manager can't close the window and may not touch an instance with administrator rights.
//!
//! An instance restarted with administrator rights would create these objects with an administrators-only default
//! DACL and a High integrity label, which an ordinary process of the same user can neither open nor signal; so they
//! get a descriptor of their own: the session's interactive user, SYSTEM and administrators may use them, and their
//! label is Medium.

use std::thread;

use windows::Win32::Foundation::{
    CloseHandle, ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, HLOCAL, LocalFree,
};
use windows::Win32::Security::Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1};
use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows::Win32::System::Threading::{
    CreateEventW, CreateMutexW, EVENT_MODIFY_STATE, INFINITE, OpenEventW, SetEvent, WaitForMultipleObjects,
};
use windows::Win32::UI::WindowsAndMessaging::{ASFW_ANY, AllowSetForegroundWindow};
use windows::core::{PCWSTR, w};

/// Names of the objects; `Local\` keeps them to the user's session. The installer script uses the last two.
const NAMES: Names = Names {
    show: w!("Local\\iRingil.PingWatch.Show"),
    quit: w!("Local\\iRingil.PingWatch.Quit"),
    running: w!("Local\\iRingil.PingWatch.Running"),
};
/// Security of the objects: full access (which opening an existing event by `CreateEventW` asks for) to
/// interactive users, SYSTEM and administrators; Medium integrity, so a process without administrator rights may
/// signal or see the objects of an instance that has them.
const SECURITY: PCWSTR = w!("D:(A;;GA;;;IU)(A;;GA;;;SY)(A;;GA;;;BA)S:(ML;;NW;;;ME)");
/// What `WaitForMultipleObjects` returns when the first object is signaled; the others follow it.
const WAIT_OBJECT_0: u32 = 0;

/// Names of the named objects (the tests use their own, so a running Ping Watch doesn't disturb them).
#[derive(Debug, Clone, Copy)]
struct Names {
    /// Event another instance signals to have the window shown.
    show: PCWSTR,
    /// Event the installer signals to have Ping Watch quit.
    quit: PCWSTR,
    /// Mutex held while the first instance runs.
    running: PCWSTR,
}

/// The named objects of the first instance.
#[derive(Debug)]
pub struct FirstInstance {
    /// The event other instances signal.
    show: Handle,
    /// The event asking to quit; `None` if it couldn't be created.
    quit: Option<Handle>,
    /// The mutex telling the installer that Ping Watch runs; `None` if it couldn't be created.
    running: Option<Handle>,
}

/// Whether this process is the first instance.
#[derive(Debug)]
pub enum Claim {
    /// It is: it keeps the objects.
    First(FirstInstance),
    /// Another instance runs and was asked to show its window; this one should exit.
    Other,
    /// The event couldn't be created; this one runs anyway, as if alone.
    Unknown,
}

/// Creates the named objects, or asks the instance that has them to show itself.
pub fn claim() -> Claim {
    claim_named(NAMES)
}

/// [`claim`] with the objects called `names`.
fn claim_named(names: Names) -> Claim {
    let descriptor = Descriptor::new()
        .inspect_err(|error| log::warn!("cannot build the single-instance objects' security: {error}"))
        .ok();
    let attributes = descriptor.as_ref().map(|descriptor| SECURITY_ATTRIBUTES {
        nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>()).unwrap_or(u32::MAX),
        lpSecurityDescriptor: descriptor.0.0,
        bInheritHandle: false.into(),
    });
    let attributes = attributes.as_ref().map(|attributes| &raw const *attributes);
    // SAFETY: `attributes`, if any, points to a local whose descriptor `descriptor` keeps alive across the call;
    // an auto-reset event, not signaled, a NUL-terminated name.
    let created = unsafe { CreateEventW(attributes, false, false, names.show) };
    // SAFETY: no preconditions; reads the error of the call just made on this thread.
    let existed = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    let show = match created {
        Ok(handle) => Handle(handle),
        Err(error) if error.code() == ERROR_ACCESS_DENIED.to_hresult() => return signal_protected(names.show),
        Err(error) => {
            log::warn!("cannot check for another Ping Watch: {error}");
            return Claim::Unknown;
        }
    };
    if !existed {
        // SAFETY: as for the event above.
        let quit = unsafe { CreateEventW(attributes, false, false, names.quit) }
            .inspect_err(|error| log::warn!("cannot create the quit event: {error}"))
            .ok()
            .map(Handle);
        // SAFETY: as for the event above; a mutex not owned by any thread, only its existence matters.
        let running = unsafe { CreateMutexW(attributes, false, names.running) }
            .inspect_err(|error| log::warn!("cannot create the running mark: {error}"))
            .ok()
            .map(Handle);
        return Claim::First(FirstInstance { show, quit, running });
    }
    // The first instance may bring its window to the front although this process is the one the user started.
    // SAFETY: no preconditions; failure only means Windows keeps its focus rules.
    if let Err(error) = unsafe { AllowSetForegroundWindow(ASFW_ANY) } {
        log::debug!("cannot pass the foreground on: {error}");
    }
    // SAFETY: the handle is the open event created above.
    if let Err(error) = unsafe { SetEvent(show.0) } {
        log::warn!("cannot ask the running Ping Watch to show itself: {error}");
    }
    Claim::Other
}

/// The event `name` exists but may not be opened with full access (made by a build with administrator rights
/// that gave it no descriptor of its own): another instance runs. It is asked to show itself if the event can at
/// least be signaled; this one exits either way, as two instances must not share the database.
fn signal_protected(name: PCWSTR) -> Claim {
    // SAFETY: only the right to signal is asked for; the handle is owned (and closed) by `Handle`.
    match unsafe { OpenEventW(EVENT_MODIFY_STATE, false, name) } {
        Ok(handle) => {
            let event = Handle(handle);
            // SAFETY: the handle is the open event above, opened with the right to signal it.
            if let Err(error) = unsafe { SetEvent(event.0) } {
                log::warn!("cannot ask the running Ping Watch to show itself: {error}");
            }
        }
        Err(error) => log::warn!("another Ping Watch runs, but it can't be asked to show itself: {error}"),
    }
    Claim::Other
}

/// A security descriptor made from [`SECURITY`], freed on drop.
#[derive(Debug)]
struct Descriptor(PSECURITY_DESCRIPTOR);

impl Descriptor {
    /// Builds the descriptor.
    ///
    /// # Errors
    ///
    /// The Windows error if the text can't be converted.
    fn new() -> windows::core::Result<Self> {
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        // SAFETY: a NUL-terminated SDDL string; `descriptor` receives a buffer Windows allocates with
        // `LocalAlloc`, which `Drop` frees; the size isn't asked for.
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(SECURITY, SDDL_REVISION_1, &raw mut descriptor, None)
        }?;
        Ok(Self(descriptor))
    }
}

impl Drop for Descriptor {
    fn drop(&mut self) {
        // SAFETY: the buffer was allocated by `ConvertStringSecurityDescriptorToSecurityDescriptorW` and is freed
        // only here; nothing uses it afterward.
        unsafe { LocalFree(Some(HLOCAL(self.0.0))) };
    }
}

impl FirstInstance {
    /// Runs `show` on a thread of its own every time another instance asks for the window, and `quit` every time
    /// the installer asks the app to quit. The thread keeps the named objects until the process ends: the running
    /// mark must outlast the window, as the program file stays in use until then.
    pub fn watch(self, show: impl Fn() + Send + 'static, quit: impl Fn() + Send + 'static) {
        let Self { show: show_event, quit: quit_event, running } = self;
        let spawned = thread::Builder::new().name("single-instance".to_owned()).spawn(move || {
            // Held, not used: the mark exists while this thread does.
            let _running = running;
            let mut handles = vec![show_event.handle()];
            handles.extend(quit_event.as_ref().map(Handle::handle));
            loop {
                // SAFETY: the handles stay open while `show_event` and `quit_event` live, which is as long as
                // this thread; any one of them wakes the wait.
                let waited = unsafe { WaitForMultipleObjects(&handles, false, INFINITE) };
                match waited.0.checked_sub(WAIT_OBJECT_0) {
                    Some(0) => show(),
                    Some(1) => quit(),
                    _ => {
                        log::warn!("the single-instance wait failed ({})", waited.0);
                        return;
                    }
                }
            }
        });
        if let Err(error) = spawned {
            log::warn!("cannot wait for other instances: {error}");
        }
    }
}

/// An open handle of a named event or mutex, closed on drop.
#[derive(Debug)]
struct Handle(HANDLE);

// SAFETY: a kernel object handle is usable from any thread; `Handle` owns it alone.
unsafe impl Send for Handle {}

impl Handle {
    /// The handle (a method, so a closure moves the whole `Handle`, not just the bare handle).
    fn handle(&self) -> HANDLE {
        self.0
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle was opened by `CreateEventW`, `CreateMutexW` or `OpenEventW` and is closed only here.
        if let Err(error) = unsafe { CloseHandle(self.0) } {
            log::debug!("cannot close a single-instance handle: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;

    /// Names of the test objects.
    const TEST_NAMES: Names = Names {
        show: w!("Local\\iRingil.PingWatch.TestShow"),
        quit: w!("Local\\iRingil.PingWatch.TestQuit"),
        running: w!("Local\\iRingil.PingWatch.TestRunning"),
    };

    #[test]
    fn a_second_claim_finds_the_first_and_the_installer_can_ask_it_to_quit() {
        let first = claim_named(TEST_NAMES);
        let Claim::First(first) = first else { panic!("{first:?}") };
        assert!(first.quit.is_some() && first.running.is_some());
        assert!(matches!(claim_named(TEST_NAMES), Claim::Other));
        let (shown, quitted) = (mpsc::channel(), mpsc::channel());
        let (show_sender, quit_sender) = (shown.0, quitted.0);
        first.watch(
            move || {
                show_sender.send(()).ok();
            },
            move || {
                quit_sender.send(()).ok();
            },
        );
        assert!(matches!(claim_named(TEST_NAMES), Claim::Other));
        assert!(shown.1.recv_timeout(Duration::from_secs(5)).is_ok(), "the window is asked for");
        // What the installer does: open the quit event with the right to signal it, and signal it.
        // SAFETY: as in `signal_protected`.
        let quit = Handle(unsafe { OpenEventW(EVENT_MODIFY_STATE, false, TEST_NAMES.quit) }.expect("the quit event"));
        // SAFETY: the handle is the open event above.
        unsafe { SetEvent(quit.0) }.expect("signaled");
        assert!(quitted.1.recv_timeout(Duration::from_secs(5)).is_ok(), "asked to quit");
    }
}
