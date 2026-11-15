//! Windows notifications (toasts) for alerts. An unpackaged app has to register its AppUserModelID under the
//! current user's `Software\Classes\AppUserModelId` (name and icon), or Windows shows the toast under another
//! program's name; the icon is written next to the history, where the registration points.

use std::path::{Path, PathBuf};
use std::thread;

use tauri_winrt_notification::{Sound, Toast};

use crate::APP_NAME;

/// The app's AppUserModelID.
const APP_ID: &str = "iRingil.PingWatch";
/// Registry key of the registration.
const REGISTRATION_KEY: &str = r"Software\Classes\AppUserModelId\iRingil.PingWatch";

/// Shows notifications under the app's name.
#[derive(Debug, Clone)]
pub struct Notifier {
    /// Whether the registration worked; without it toasts would carry another program's name, so none are shown.
    registered: bool,
}

impl Notifier {
    /// Registers the app for notifications, with `icon_png` written into `data_folder` as its picture.
    pub fn new(data_folder: Option<&Path>, icon_png: &[u8]) -> Self {
        let icon = data_folder.and_then(|folder| write_icon(folder, icon_png));
        let registered =
            register(icon.as_deref()).inspect_err(|error| log::warn!("notifications are off: {error}")).is_ok();
        Self { registered }
    }

    /// Shows a notification with `title` and `text` and the Windows default sound, on a thread of its own (the
    /// first toast starts the notification machinery, which takes a moment).
    pub fn show(&self, title: &str, text: &str) {
        if !self.registered {
            return;
        }
        let (title, text) = (title.to_owned(), text.to_owned());
        let spawned = thread::Builder::new().name("notification".to_owned()).spawn(move || {
            let toast = Toast::new(APP_ID).title(&title).text1(&text).sound(Some(Sound::Default));
            if let Err(error) = toast.show() {
                log::warn!("cannot show a notification: {error}");
            }
        });
        if let Err(error) = spawned {
            log::warn!("cannot start the notification thread: {error}");
        }
    }
}

/// Writes the app icon as `notification-icon.png` into `folder`; returns its path.
fn write_icon(folder: &Path, icon_png: &[u8]) -> Option<PathBuf> {
    let path = folder.join("notification-icon.png");
    std::fs::create_dir_all(folder)
        .and_then(|()| std::fs::write(&path, icon_png))
        .inspect_err(|error| log::warn!("cannot write {}: {error}", path.display()))
        .ok()?;
    Some(path)
}

/// Registers the AppUserModelID with the app's name and, if given, its icon.
fn register(icon: Option<&Path>) -> Result<(), String> {
    let key = windows_registry::CURRENT_USER.create(REGISTRATION_KEY).map_err(|error| error.to_string())?;
    key.set_string("DisplayName", APP_NAME).map_err(|error| error.to_string())?;
    if let Some(icon) = icon {
        key.set_string("IconUri", icon.display().to_string()).map_err(|error| error.to_string())?;
    }
    Ok(())
}
