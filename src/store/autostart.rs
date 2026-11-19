//! Starting with Windows, hidden to the tray, in one of two ways the user chooses between. Without administrator
//! rights: a value under the current user's `Run` registry key pointing at this executable. With them (TCP probes
//! need them): a Task Scheduler task that starts it at sign-in with the highest rights the account has, so Windows
//! asks for them once, when the task is made, and not at every sign-in. Only one of the two exists at a time.
//!
//! Making and removing the task needs administrator rights. An instance without them starts this executable again
//! with them (the UAC prompt) and [`TASK_OPTION`], which does only that and ends; the uninstaller removes the task
//! the same way.

use windows::Win32::Foundation::{VARIANT_FALSE, VARIANT_TRUE};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::System::TaskScheduler::{
    IExecAction, ILogonTrigger, ITaskFolder, ITaskService, TASK_ACTION_EXEC, TASK_CREATE_OR_UPDATE,
    TASK_INSTANCES_IGNORE_NEW, TASK_LOGON_INTERACTIVE_TOKEN, TASK_RUNLEVEL_HIGHEST, TASK_TRIGGER_LOGON, TaskScheduler,
};
use windows::Win32::System::Variant::VARIANT;
use windows::core::{BSTR, Interface as _};

use crate::APP_NAME;
use crate::com::Com;
use crate::ui::tray::START_OPTION;

/// Registry key whose values Windows starts at sign-in.
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
/// Command-line option of an instance started only to make (`on`) or remove (`off`) the task, with the
/// administrator rights that takes; the installer's uninstaller uses it too.
pub const TASK_OPTION: &str = "--sign-in-task";
/// Process priority of the task, as Task Scheduler counts it: normal (its default of 7 is below normal, which
/// would delay the probes).
const TASK_PRIORITY: i32 = 4;

/// How Windows starts Ping Watch at sign-in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Autostart {
    /// It doesn't.
    #[default]
    Off,
    /// By the `Run` registry value: without administrator rights.
    Registry,
    /// By the Task Scheduler task: with administrator rights.
    Task,
}

/// Error reading or changing autostart.
#[derive(Debug, thiserror::Error)]
pub enum AutostartError {
    /// The registry refused (Windows' text: it is shown inside a translated sentence).
    #[error("{0}")]
    Registry(String),
    /// The executable's own path is unknown (Windows' text).
    #[error(transparent)]
    Exe(#[from] std::io::Error),
    /// Task Scheduler refused (Windows' text).
    #[error("{0}")]
    Task(#[from] windows::core::Error),
    /// The user said no to the administrator rights the task takes, or the change made with them failed; nothing
    /// changed.
    #[error("administrator rights")]
    Declined,
}

/// How Windows starts Ping Watch at sign-in now: by the task if there is one, else by the `Run` value if it points
/// at this very file.
///
/// # Errors
///
/// [`AutostartError`] if the executable's path can't be read.
pub fn state() -> Result<Autostart, AutostartError> {
    if task_exists() {
        Ok(Autostart::Task)
    } else if run_value_is_ours()? {
        Ok(Autostart::Registry)
    } else {
        Ok(Autostart::Off)
    }
}

/// Makes `wanted` the way Windows starts Ping Watch; `elevated` tells whether this instance has administrator
/// rights. Making or removing the task needs them: without, Windows asks (the UAC prompt). The task is dealt with
/// first, so a "no" there leaves everything as it was.
///
/// # Errors
///
/// [`AutostartError`] if the registry or the task can't be changed; [`AutostartError::Declined`] if the user said
/// no in the UAC prompt.
pub fn set(wanted: Autostart, elevated: bool) -> Result<(), AutostartError> {
    let task_wanted = wanted == Autostart::Task;
    if task_wanted != task_exists() {
        if elevated {
            set_task(task_wanted)?;
        } else {
            let parameters = format!("{TASK_OPTION} {}", if task_wanted { "on" } else { "off" });
            let done = crate::elevation::run_elevated(&std::env::current_exe()?, &parameters);
            if !done || task_wanted != task_exists() {
                return Err(AutostartError::Declined);
            }
        }
    }
    set_run_value(wanted == Autostart::Registry)
}

/// Takes [`TASK_OPTION`] and its value out of `args`: `Some(true)` to make the task, `Some(false)` to remove it
/// (also for a value that is neither `on` nor `off`: removing is the harmless reading); `None` if it isn't there.
pub fn take_task_option(args: &mut Vec<String>) -> Option<bool> {
    let position = args.iter().position(|arg| arg == TASK_OPTION)?;
    args.remove(position);
    let on = args.get(position).is_some_and(|value| value == "on");
    if position < args.len() {
        args.remove(position);
    }
    Some(on)
}

/// Makes (`on`) or removes the task: at this user's sign-in it starts this executable hidden to the tray, with the
/// highest rights the account has. Needs administrator rights.
///
/// # Errors
///
/// [`AutostartError`] if the executable's path is unknown or Task Scheduler refuses (no rights).
pub fn set_task(on: bool) -> Result<(), AutostartError> {
    let _com = Com::init();
    let service = service()?;
    let folder = root_folder(&service)?;
    let name = BSTR::from(task_name());
    if !on {
        // SAFETY: a method call on a live COM object with a string that lives across the call.
        return Ok(unsafe { folder.DeleteTask(&name, 0) }?);
    }
    let program = std::env::current_exe()?;
    let account = BSTR::from(account());
    // SAFETY: method calls on live COM objects with strings that live across each call; the interfaces asked for
    // by `cast` are those of the trigger and the action just made with those types.
    unsafe {
        let task = service.NewTask(0)?;
        let registration = task.RegistrationInfo()?;
        registration.SetAuthor(&BSTR::from(APP_NAME))?;
        registration.SetDescription(&BSTR::from(format!(
            "Starts {APP_NAME} with administrator rights at sign-in, for its TCP probes."
        )))?;
        let principal = task.Principal()?;
        principal.SetUserId(&account)?;
        principal.SetLogonType(TASK_LOGON_INTERACTIVE_TOKEN)?;
        principal.SetRunLevel(TASK_RUNLEVEL_HIGHEST)?;
        // A task's defaults would skip it on battery and stop it after three days.
        let settings = task.Settings()?;
        settings.SetDisallowStartIfOnBatteries(VARIANT_FALSE)?;
        settings.SetStopIfGoingOnBatteries(VARIANT_FALSE)?;
        settings.SetExecutionTimeLimit(&BSTR::from("PT0S"))?;
        settings.SetPriority(TASK_PRIORITY)?;
        settings.SetMultipleInstances(TASK_INSTANCES_IGNORE_NEW)?;
        settings.SetAllowDemandStart(VARIANT_TRUE)?;
        let trigger: ILogonTrigger = task.Triggers()?.Create(TASK_TRIGGER_LOGON)?.cast()?;
        trigger.SetUserId(&account)?;
        let action: IExecAction = task.Actions()?.Create(TASK_ACTION_EXEC)?.cast()?;
        action.SetPath(&BSTR::from(program.display().to_string()))?;
        action.SetArguments(&BSTR::from(START_OPTION))?;
        folder.RegisterTaskDefinition(
            &name,
            &task,
            TASK_CREATE_OR_UPDATE.0,
            &VARIANT::default(),
            &VARIANT::default(),
            TASK_LOGON_INTERACTIVE_TOKEN,
            &VARIANT::default(),
        )?;
    }
    Ok(())
}

/// A connection to this computer's Task Scheduler.
fn service() -> windows::core::Result<ITaskService> {
    // SAFETY: COM is initialized for this thread by the callers' `Com`; the class and the interface belong
    // together; the method call is on a live COM object, with values that live across it.
    unsafe {
        let service: ITaskService = CoCreateInstance(&TaskScheduler, None, CLSCTX_INPROC_SERVER)?;
        service.Connect(&VARIANT::default(), &VARIANT::default(), &VARIANT::default(), &VARIANT::default())?;
        Ok(service)
    }
}

/// Task Scheduler's root folder, where the task lives.
fn root_folder(service: &ITaskService) -> windows::core::Result<ITaskFolder> {
    // SAFETY: a method call on a live COM object with a string that lives across the call.
    unsafe { service.GetFolder(&BSTR::from(r"\")) }
}

/// Removes the `Run` value, if it is there.
///
/// # Errors
///
/// [`AutostartError`] if the registry refuses.
pub fn remove_run_value() -> Result<(), AutostartError> {
    set_run_value(false)
}

/// Whether the task exists (as far as this instance can see it).
pub fn task_exists() -> bool {
    let _com = Com::init();
    // SAFETY: a method call on a live COM object with a string that lives across the call.
    service()
        .and_then(|service| root_folder(&service))
        .and_then(|folder| unsafe { folder.GetTask(&BSTR::from(task_name())) })
        .is_ok()
}

/// Name of the task: one per Windows user (the installer's uninstaller looks for it by this name).
fn task_name() -> String {
    format!("{APP_NAME} ({})", std::env::var("USERNAME").unwrap_or_default())
}

/// The Windows account of this process, as `DOMAIN\user`.
fn account() -> String {
    let variable = |name: &str| std::env::var(name).unwrap_or_default();
    format!(r"{}\{}", variable("USERDOMAIN"), variable("USERNAME"))
}

/// Whether the `Run` value under our name points at this very file.
fn run_value_is_ours() -> Result<bool, AutostartError> {
    let Ok(key) = windows_registry::CURRENT_USER.open(RUN_KEY) else {
        return Ok(false);
    };
    let Ok(command) = key.get_string(APP_NAME) else {
        return Ok(false);
    };
    Ok(command.eq_ignore_ascii_case(&command_line()?))
}

/// Writes or removes the `Run` value.
fn set_run_value(enabled: bool) -> Result<(), AutostartError> {
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

/// The command Windows runs: this executable, quoted (the path may contain spaces), started hidden to the tray.
fn command_line() -> Result<String, std::io::Error> {
    Ok(format!("\"{}\" {START_OPTION}", std::env::current_exe()?.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_task_option_is_taken_out_with_its_value() {
        let args = |list: &[&str]| list.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
        let mut on = args(&["--sign-in-task", "on"]);
        assert_eq!(take_task_option(&mut on), Some(true));
        assert!(on.is_empty());
        let mut off = args(&["1.1.1.1", "--sign-in-task", "off"]);
        assert_eq!(take_task_option(&mut off), Some(false));
        assert_eq!(off, ["1.1.1.1"]);
        // No value, or an unknown one: removing.
        assert_eq!(take_task_option(&mut args(&["--sign-in-task"])), Some(false));
        assert_eq!(take_task_option(&mut args(&["--sign-in-task", "maybe"])), Some(false));
        assert_eq!(take_task_option(&mut args(&["1.1.1.1"])), None);
    }
}
