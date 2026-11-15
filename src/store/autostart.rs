//! Starting with Windows: a value under the current user's `Run` registry key pointing at this executable.

use crate::APP_NAME;

/// Registry key whose values Windows starts at sign-in.
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

/// Error reading or changing the autostart entry.
#[derive(Debug, thiserror::Error)]
pub enum AutostartError {
    /// The registry refused (its message).
    #[error("registry: {0}")]
    Registry(String),
    /// The executable's own path is unknown.
    #[error("cannot find the program file: {0}")]
    Exe(#[from] std::io::Error),
}

/// Whether Windows starts this executable at sign-in (an entry under our name pointing at this very file).
///
/// # Errors
///
/// [`AutostartError`] if the registry or the executable's path can't be read.
pub fn is_enabled() -> Result<bool, AutostartError> {
    let Ok(key) = windows_registry::CURRENT_USER.open(RUN_KEY) else {
        return Ok(false);
    };
    let Ok(command) = key.get_string(APP_NAME) else {
        return Ok(false);
    };
    Ok(command.eq_ignore_ascii_case(&command_line()?))
}

/// Turns starting with Windows on or off.
///
/// # Errors
///
/// [`AutostartError`] if the registry can't be changed.
pub fn set_enabled(enabled: bool) -> Result<(), AutostartError> {
    let registry =
        |result: windows_registry::Result<()>| result.map_err(|error| AutostartError::Registry(error.to_string()));
    let key = windows_registry::CURRENT_USER
        .create(RUN_KEY)
        .map_err(|error| AutostartError::Registry(error.to_string()))?;
    if enabled {
        registry(key.set_string(APP_NAME, command_line()?))?;
    } else if key.get_string(APP_NAME).is_ok() {
        registry(key.remove_value(APP_NAME))?;
    }
    Ok(())
}

/// The command Windows runs: this executable, quoted (the path may contain spaces).
fn command_line() -> Result<String, std::io::Error> {
    Ok(format!("\"{}\"", std::env::current_exe()?.display()))
}
