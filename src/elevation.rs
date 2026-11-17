//! Administrator rights: whether Ping Watch runs with them or could get them (checked once at start, no prompt),
//! and restarting it with them through the UAC prompt. TCP probes need them; everything else works without.

use std::time::Duration;

use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Security::{
    CheckTokenMembership, CreateWellKnownSid, GetTokenInformation, PSID, SECURITY_MAX_SID_SIZE, TOKEN_ELEVATION_TYPE,
    TOKEN_INFORMATION_CLASS, TOKEN_QUERY, TokenElevationType, TokenElevationTypeLimited, WinBuiltinAdministratorsSid,
};
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentProcessId, OpenProcess, OpenProcessToken, PROCESS_SYNCHRONIZE, WaitForSingleObject,
};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::BOOL;
use windows::core::{HSTRING, w};

/// Command-line option of a restarted instance: the process id of the instance it replaces.
pub const AFTER_OPTION: &str = "--after";
/// How long a restarted instance waits for the old one to close before going on anyway.
const AFTER_WAIT: Duration = Duration::from_secs(15);
/// `ShellExecuteW` results above this mean success.
const SHELL_EXECUTE_OK: isize = 32;

/// What this process may do about administrator rights.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rights {
    /// It runs with them.
    Elevated,
    /// It runs without them in an administrator's account: the UAC prompt only asks that user's yes.
    CanElevate,
    /// It runs in an account without them: the UAC prompt would ask for another account's password, and Ping
    /// Watch would then run in that account's profile, with other data. Not offered.
    Standard,
}

impl Rights {
    /// Whether TCP probes can run.
    pub fn elevated(self) -> bool {
        self == Self::Elevated
    }
}

/// This process's [`Rights`], checked once at start (no prompt); [`Rights::Standard`] if they can't be told.
/// Elevated means the administrators group is in force in its token, which is what the raw socket of TCP probes
/// needs: with UAC off, a token restricted to a basic user still reads as "elevated" otherwise.
pub fn rights() -> Rights {
    match in_administrators() {
        Ok(true) => return Rights::Elevated,
        Ok(false) => {}
        Err(error) => log::warn!("cannot read this process's rights: {error}"),
    }
    let mut token = HANDLE::default();
    // SAFETY: the pseudo handle of the current process needs no closing; `token` receives a handle we close below.
    if let Err(error) = unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) } {
        log::warn!("cannot read this process's rights: {error}");
        return Rights::Standard;
    }
    let mut elevation_type = TOKEN_ELEVATION_TYPE::default();
    let read = token_information(token, TokenElevationType, &mut elevation_type);
    // SAFETY: `token` was opened above and isn't used after this.
    if let Err(error) = unsafe { CloseHandle(token) } {
        log::debug!("cannot close the token handle: {error}");
    }
    match read {
        // The filtered token of an administrator; its full one is a yes away.
        Ok(()) if elevation_type == TokenElevationTypeLimited => Rights::CanElevate,
        Ok(()) => Rights::Standard,
        Err(error) => {
            log::warn!("cannot read this process's rights: {error}");
            Rights::Standard
        }
    }
}

/// Whether the administrators group is enabled in this process's token (not only present, as it is, deny-only, in
/// the filtered token of an administrator).
fn in_administrators() -> windows::core::Result<bool> {
    // A SID buffer of the largest size, aligned as a SID needs.
    let mut buffer = [0_u32; SECURITY_MAX_SID_SIZE as usize / 4];
    let mut size = u32::try_from(size_of_val(&buffer)).unwrap_or(u32::MAX);
    let sid = PSID((&raw mut buffer).cast());
    // SAFETY: `sid` points to `buffer`, writable and `size` bytes long, which lives across both calls.
    unsafe { CreateWellKnownSid(WinBuiltinAdministratorsSid, None, Some(sid), &raw mut size) }?;
    let mut member = BOOL::default();
    // SAFETY: no token: the calling thread's own is checked; `sid` holds the SID written above.
    unsafe { CheckTokenMembership(None, sid, &raw mut member) }?;
    Ok(member.as_bool())
}

/// Reads the `class` information of the open `token` into `value`, the plain struct or enum Windows fills for it.
fn token_information<T>(token: HANDLE, class: TOKEN_INFORMATION_CLASS, value: &mut T) -> windows::core::Result<()> {
    let mut returned = 0_u32;
    // SAFETY: `token` is an open token with query access; the buffer is `value` with its exact size, of the type
    // Windows writes for `class` (the callers pair them).
    unsafe {
        GetTokenInformation(
            token,
            class,
            Some((&raw mut *value).cast()),
            u32::try_from(size_of::<T>()).unwrap_or(u32::MAX),
            &raw mut returned,
        )
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
