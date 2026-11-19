//! Removing what Ping Watch left in Windows outside its own folder, for a portable copy about to be deleted (an
//! installed one has its uninstaller): autostart (the `Run` value or the Task Scheduler task), the Windows Firewall
//! rules for TCP probes, the registration of its notifications, and its entry among the notification area icons.
//!
//! The task and the firewall rules take administrator rights to remove. An instance without them starts this
//! executable again with them (the UAC prompt) and [`OPTION`], which removes just those two and ends.

use windows_registry::CURRENT_USER;

use crate::probe::firewall;
use crate::store::autostart::{self, AutostartError};
use crate::ui::notify::REGISTRATION_KEY;

/// Command-line option of an instance started only to remove the task and the firewall rules, with the
/// administrator rights that takes.
pub const OPTION: &str = "--remove-traces";
/// Registry key under which Windows 11 keeps one subkey per notification area icon it has seen, with the path of
/// the icon's program (the list in Settings → Personalization → Taskbar → Other system tray icons).
const TRAY_ICONS_KEY: &str = r"Control Panel\NotifyIconSettings";

/// Error removing the traces.
#[derive(Debug, thiserror::Error)]
pub enum TracesError {
    /// Autostart couldn't be removed.
    #[error(transparent)]
    Autostart(#[from] AutostartError),
    /// Windows Firewall refused (Windows' text).
    #[error("{0}")]
    Firewall(#[from] windows::core::Error),
    /// The executable's own path is unknown (Windows' text).
    #[error(transparent)]
    Program(#[from] std::io::Error),
    /// The registry refused (Windows' text).
    #[error("{0}")]
    Registry(String),
    /// The user said no to the administrator rights that the task and the firewall rules take, or removing them
    /// with those rights failed; nothing was removed.
    #[error("administrator rights")]
    Declined,
}

/// Takes [`OPTION`] out of `args`; returns whether it was there.
pub fn take_option(args: &mut Vec<String>) -> bool {
    let count = args.len();
    args.retain(|arg| arg != OPTION);
    args.len() != count
}

/// Removes everything Ping Watch left in Windows outside its folder; `elevated` tells whether this instance has
/// administrator rights. What takes them goes first (asked for through the UAC prompt without them), so a "no"
/// there leaves everything as it was.
///
/// # Errors
///
/// [`TracesError`] if something couldn't be removed; [`TracesError::Declined`] if the user said no in the UAC
/// prompt.
pub fn remove(elevated: bool) -> Result<(), TracesError> {
    if autostart::task_exists() || firewall::rules_exist() {
        if elevated {
            remove_privileged()?;
        } else if !crate::elevation::run_elevated(&std::env::current_exe()?, OPTION) {
            return Err(TracesError::Declined);
        }
    }
    autostart::remove_run_value()?;
    remove_key(REGISTRATION_KEY)?;
    remove_tray_icon_entries()
}

/// Removes what takes administrator rights to remove: the sign-in task and the Windows Firewall rules.
///
/// # Errors
///
/// [`TracesError`] if Task Scheduler or Windows Firewall refuses (no rights).
pub fn remove_privileged() -> Result<(), TracesError> {
    if autostart::task_exists() {
        autostart::set_task(false)?;
    }
    Ok(firewall::remove_rules()?)
}

/// Removes the key `path` of the current user with everything under it, if it is there.
fn remove_key(path: &str) -> Result<(), TracesError> {
    if CURRENT_USER.open(path).is_err() {
        return Ok(());
    }
    CURRENT_USER.remove_tree(path).map_err(|error| TracesError::Registry(error.to_string()))
}

/// Removes this executable's entries among the notification area icons Windows remembers.
fn remove_tray_icon_entries() -> Result<(), TracesError> {
    let program = std::env::current_exe()?.display().to_string();
    let Ok(icons) = CURRENT_USER.open(TRAY_ICONS_KEY) else {
        return Ok(());
    };
    let ours: Vec<String> = icons
        .keys()
        .map_err(|error| TracesError::Registry(error.to_string()))?
        .filter(|name| {
            let path = icons.open(name).and_then(|icon| icon.get_string("ExecutablePath"));
            path.is_ok_and(|path| path.eq_ignore_ascii_case(&program))
        })
        .collect();
    ours.iter().try_for_each(|name| remove_key(&format!(r"{TRAY_ICONS_KEY}\{name}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_option_is_taken_out() {
        let mut args = vec![OPTION.to_owned(), "1.1.1.1".to_owned()];
        assert!(take_option(&mut args));
        assert_eq!(args, ["1.1.1.1"]);
        assert!(!take_option(&mut args));
    }
}
