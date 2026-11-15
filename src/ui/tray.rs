//! Icon in the notification area (system tray): a left click brings the window back, the right-click menu shows it
//! or exits. Always shown for now; settings come later.

use std::sync::mpsc::{self, Receiver};

use eframe::egui;
use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

/// What the user asked for through the tray icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayCommand {
    /// Restore and focus the window.
    Show,
    /// Quit the app.
    Exit,
}

/// Error creating the tray icon.
#[derive(Debug, thiserror::Error)]
pub enum TrayError {
    /// The icon image is unusable.
    #[error("tray icon image: {0}")]
    Image(String),
    /// The menu couldn't be built.
    #[error("tray menu: {0}")]
    Menu(#[from] tray_icon::menu::Error),
    /// Windows refused the icon.
    #[error("tray icon: {0}")]
    Icon(#[from] tray_icon::Error),
}

/// The tray icon; removed from the tray when dropped.
pub struct Tray {
    /// Keeps the icon in the tray.
    _icon: TrayIcon,
    /// The menu's "Show" item, relabelled when the language changes.
    show: MenuItem,
    /// The menu's "Exit" item.
    exit: MenuItem,
    /// Commands from the icon and its menu, sent by the event handlers.
    commands: Receiver<TrayCommand>,
}

impl std::fmt::Debug for Tray {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("Tray").finish_non_exhaustive()
    }
}

impl Tray {
    /// Puts the icon (`png`, the app icon) into the tray with `tooltip`. Clicks wake `ctx`, so they are handled
    /// even while the window is minimized or hidden. Must run on the thread of the window's event loop.
    ///
    /// # Errors
    ///
    /// [`TrayError`] if the image, the menu or the icon can't be created.
    pub fn new(ctx: &egui::Context, png: &[u8], tooltip: &str) -> Result<Self, TrayError> {
        let image = eframe::icon_data::from_png_bytes(png).map_err(|error| TrayError::Image(error.to_string()))?;
        let icon = Icon::from_rgba(image.rgba, image.width, image.height)
            .map_err(|error| TrayError::Image(error.to_string()))?;

        let show = MenuItem::new(format!("Show {tooltip}"), true, None);
        let exit = MenuItem::new("Exit", true, None);
        let menu = Menu::with_items(&[&show, &PredefinedMenuItem::separator(), &exit])?;
        let tray = TrayIconBuilder::new()
            .with_icon(icon)
            .with_tooltip(tooltip)
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .build()?;

        let (sender, commands) = mpsc::channel();
        let click_sender = sender.clone();
        let click_ctx = ctx.clone();
        TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                forward(&click_sender, &click_ctx, TrayCommand::Show);
            }
        }));
        let (show_id, exit_id): (MenuId, MenuId) = (show.id().clone(), exit.id().clone());
        let menu_ctx = ctx.clone();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            if event.id == show_id {
                forward(&sender, &menu_ctx, TrayCommand::Show);
            } else if event.id == exit_id {
                forward(&sender, &menu_ctx, TrayCommand::Exit);
            }
        }));
        Ok(Self { _icon: tray, show, exit, commands })
    }

    /// Relabels the menu (after a language change).
    pub fn set_labels(&self, show: &str, exit: &str) {
        self.show.set_text(show);
        self.exit.set_text(exit);
    }

    /// The next command from the tray, if any.
    pub fn poll(&self) -> Option<TrayCommand> {
        self.commands.try_recv().ok()
    }
}

/// Queues `command` and wakes the UI so it gets handled at once.
fn forward(sender: &mpsc::Sender<TrayCommand>, ctx: &egui::Context, command: TrayCommand) {
    if sender.send(command).is_ok() {
        ctx.request_repaint();
    }
}
