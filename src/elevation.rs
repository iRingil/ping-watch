//! Administrator rights: whether Ping Watch runs with them (checked once at start, no prompt), and restarting it
//! with them through the UAC prompt. TCP probes need them; everything else works without.

use std::time::Duration;

use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentProcessId, OpenProcess, OpenProcessToken, PROCESS_SYNCHRONIZE, WaitForSingleObject,
};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{HSTRING, w};

/// Command-line option of a restarted instance: the process id of the instance it replaces.
pub const AFTER_OPTION: &str = "--after";
/// How long a restarted instance waits for the old one to close before going on anyway.
const AFTER_WAIT: Duration = Duration::from_secs(15);
/// `ShellExecuteW` results above this mean success.
const SHELL_EXECUTE_OK: isize = 32;

/// Whether this process runs with administrator rights (elevated); `false` if it can't be told.
pub fn is_elevated() -> bool {
    let mut token = HANDLE::default();
    // SAFETY: the pseudo handle of the current process needs no closing; `token` receives a handle we close below.
    if let Err(error) = unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) } {
        log::warn!("cannot read this process's rights: {error}");
        return false;
    }
    let mut elevation = TOKEN_ELEVATION::default();
    let mut returned = 0_u32;
    // SAFETY: `token` is the open token; the buffer is `elevation` with its exact size.
    let read = unsafe {
        GetTokenInformation(
            token,
            TokenElevation,
            Some((&raw mut elevation).cast()),
            u32::try_from(size_of::<TOKEN_ELEVATION>()).unwrap_or(u32::MAX),
            &raw mut returned,
        )
    };
    // SAFETY: `token` was opened above and isn't used after this.
    if let Err(error) = unsafe { CloseHandle(token) } {
        log::debug!("cannot close the token handle: {error}");
    }
    match read {
        Ok(()) => elevation.TokenIsElevated != 0,
        Err(error) => {
            log::warn!("cannot read this process's rights: {error}");
            false
        }
    }
}

/// Starts Ping Watch again with administrator rights (Windows shows its UAC prompt); the new instance waits for
/// this one to close. Returns `false` if it didn't start (the user said no, or it failed).
pub fn restart_elevated() -> bool {
    let Ok(exe) = std::env::current_exe() else {
        log::warn!("cannot find the program to restart");
        return false;
    };
    // SAFETY: no preconditions.
    let parameters = HSTRING::from(format!("{AFTER_OPTION} {}", unsafe { GetCurrentProcessId() }));
    let file = HSTRING::from(exe.as_os_str());
    // SAFETY: all strings are NUL-terminated and live across the call; no window owns the prompt.
    let result = unsafe { ShellExecuteW(None, w!("runas"), &file, &parameters, None, SW_SHOWNORMAL) };
    let started = result.0 as isize > SHELL_EXECUTE_OK;
    if !started {
        log::info!("the restart with administrator rights didn't happen ({})", result.0 as isize);
    }
    started
}

/// In a restarted instance (`--after <pid>` in `args`), waits for the instance it replaces to close, so it can
/// take over the single-instance claim. Returns the other arguments.
pub fn wait_for_replaced(args: Vec<String>) -> Vec<String> {
    let Some(position) = args.iter().position(|arg| arg == AFTER_OPTION) else {
        return args;
    };
    let pid = args.get(position.saturating_add(1)).and_then(|pid| pid.parse::<u32>().ok());
    if let Some(pid) = pid {
        // SAFETY: no preconditions; a process that is gone already gives an error, which means nothing to wait for.
        match unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) } {
            Ok(process) => {
                let wait_ms = u32::try_from(AFTER_WAIT.as_millis()).unwrap_or(u32::MAX);
                // SAFETY: `process` is the open handle from above.
                if unsafe { WaitForSingleObject(process, wait_ms) } != WAIT_OBJECT_0 {
                    log::warn!("the replaced instance is still running");
                }
                // SAFETY: `process` was opened above and isn't used after this.
                if let Err(error) = unsafe { CloseHandle(process) } {
                    log::debug!("cannot close the process handle: {error}");
                }
            }
            Err(error) => log::debug!("the replaced instance is gone: {error}"),
        }
    }
    let skip = if pid.is_some() { 2 } else { 1 };
    args.into_iter()
        .enumerate()
        .filter(|(index, _)| *index < position || *index >= position.saturating_add(skip))
        .map(|(_, arg)| arg)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_after_option_is_taken_out() {
        let args = |list: &[&str]| list.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
        // A process id that can't exist: nothing to wait for.
        assert_eq!(wait_for_replaced(args(&["--after", "4294967295", "1.1.1.1"])), args(&["1.1.1.1"]));
        assert_eq!(wait_for_replaced(args(&["dns.google"])), args(&["dns.google"]));
        assert_eq!(wait_for_replaced(args(&["--after"])), args(&[]));
    }
}
