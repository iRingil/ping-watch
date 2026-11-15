//! The Settings window, opened from the gear: pages General, Tracing, Appearance, Language and About. Changes apply
//! at once (the app stores them); there is nothing to confirm.

use std::path::Path;
use std::time::Duration;

use eframe::egui::{self, Align, Grid, Layout, RichText, Ui};

use crate::APP_NAME;
use crate::store::autostart;
use crate::store::settings::{AppSettings, MAX_HOPS, RETENTIONS, TIMEOUTS};
use crate::ui::icons::{self, Icon};
use crate::ui::tab;
use crate::ui::theme;
use crate::ui::timeline::Period;

/// Width of the window's content.
const WIDTH: f32 = 520.0;
/// Least height of a page, so switching pages doesn't make the window jump.
const PAGE_HEIGHT: f32 = 290.0;
/// Side of the logo on the About page.
const LOGO_SIZE: f32 = 96.0;
/// The project's web page.
const SITE_URL: &str = "https://iringil.github.io/ping-watch/";
/// The project's source code.
const REPO_URL: &str = "https://github.com/iRingil/ping-watch";
/// Width of the drop-down lists, the same for all.
const COMBO_WIDTH: f32 = 150.0;
/// Space between the label column and the controls.
const GRID_SPACING: [f32; 2] = [24.0, 12.0];

/// A page of the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Page {
    /// Close button and autostart.
    #[default]
    General,
    /// Values for new targets, probing, history.
    Tracing,
    /// Theme.
    Appearance,
    /// Interface language.
    Language,
    /// Version, data folder, licenses.
    About,
}

impl Page {
    /// All pages in tab order.
    const ALL: [Self; 5] = [Self::General, Self::Tracing, Self::Appearance, Self::Language, Self::About];

    /// Tab title.
    fn title(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Tracing => "Tracing",
            Self::Appearance => "Appearance",
            Self::Language => "Language",
            Self::About => "About",
        }
    }
}

/// State of the Settings window.
#[derive(Default)]
pub struct SettingsWindow {
    /// Whether the window is shown.
    open: bool,
    /// Page shown.
    page: Page,
    /// Whether Windows starts the app, read from the registry when the window opens.
    autostart: bool,
    /// Why reading or changing autostart failed.
    autostart_error: Option<String>,
    /// The app logo for the About page, loaded on first show.
    logo: Option<egui::TextureHandle>,
    /// The update check was asked for (there is nothing to check against yet).
    update_asked: bool,
}

impl std::fmt::Debug for SettingsWindow {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The logo texture has no Debug; whether it is loaded is enough.
        formatter
            .debug_struct("SettingsWindow")
            .field("open", &self.open)
            .field("page", &self.page)
            .field("autostart", &self.autostart)
            .field("autostart_error", &self.autostart_error)
            .field("logo", &self.logo.is_some())
            .field("update_asked", &self.update_asked)
            .finish()
    }
}

impl SettingsWindow {
    /// Shows the window (on the page it was left on).
    pub fn open(&mut self) {
        self.open = true;
        match autostart::is_enabled() {
            Ok(enabled) => {
                self.autostart = enabled;
                self.autostart_error = None;
            }
            Err(error) => self.autostart_error = Some(error.to_string()),
        }
    }

    /// Draws the window if it is open; `settings` are changed in place. `data_folder` is where the history lives.
    pub fn show(&mut self, ctx: &egui::Context, settings: &mut AppSettings, data_folder: Option<&Path>) {
        if !self.open {
            return;
        }
        if self.logo.is_none() {
            self.logo = load_logo(ctx);
        }
        let modal = egui::Modal::new(egui::Id::unique("settings")).show(ctx, |ui| {
            ui.set_width(WIDTH);
            // Title with the close mark at the right end, where windows have it.
            let close = ui
                .horizontal(|ui| {
                    ui.label(RichText::new("Settings").font(theme::heading_font(18.0)));
                    ui.with_layout(Layout::right_to_left(Align::Center), close_mark).inner
                })
                .inner;
            ui.add_space(8.0);
            self.tabs(ui);
            ui.add_space(12.0);
            ui.allocate_ui(egui::vec2(WIDTH, PAGE_HEIGHT), |ui| {
                ui.set_min_height(PAGE_HEIGHT);
                ui.spacing_mut().combo_width = COMBO_WIDTH;
                match self.page {
                    Page::General => self.general(ui, settings),
                    Page::Tracing => tracing(ui, settings),
                    Page::Appearance => appearance(ui),
                    Page::Language => language(ui),
                    Page::About => self.about(ui, data_folder),
                }
            });
            close
        });
        if modal.inner || modal.should_close() {
            self.open = false;
        }
    }

    /// The page tabs: titles on a rule, the shown one underlined in the signal color.
    fn tabs(&mut self, ui: &mut Ui) {
        let row = ui
            .horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 18.0;
                for page in Page::ALL {
                    let shown = self.page == page;
                    let color = if shown { theme::INK } else { theme::INK_MUTED };
                    let text = RichText::new(page.title()).font(theme::heading_font(15.0)).color(color);
                    let response = ui.add(egui::Label::new(text).sense(egui::Sense::click()));
                    if shown {
                        let rect = response.rect;
                        ui.painter().hline(rect.x_range(), rect.bottom() + 4.0, egui::Stroke::new(2.0, theme::SIGNAL));
                    }
                    if response.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                        self.page = page;
                    }
                }
            })
            .response;
        ui.painter()
            .hline(ui.max_rect().x_range(), row.rect.bottom() + 5.5, egui::Stroke::new(1.0, theme::RULE));
    }

    /// General: what the close button does, starting with Windows.
    fn general(&mut self, ui: &mut Ui, settings: &mut AppSettings) {
        settings_grid(ui, "settings-general", |ui| {
            ui.weak("Close button");
            ui.vertical(|ui| {
                ui.radio_value(&mut settings.close_to_tray, false, "Quit Ping Watch");
                ui.radio_value(&mut settings.close_to_tray, true, "Hide to the tray and keep tracing");
            });
            ui.end_row();
            ui.weak("Windows sign-in");
            ui.vertical(|ui| {
                if ui.checkbox(&mut self.autostart, "Start Ping Watch").changed() {
                    self.autostart_error = autostart::set_enabled(self.autostart).err().map(|error| error.to_string());
                }
                if let Some(error) = &self.autostart_error {
                    ui.colored_label(theme::BAD, error);
                }
            });
            ui.end_row();
        });
    }
}

/// Tracing: what new targets start with, probe timeout and hop limit, history retention.
fn tracing(ui: &mut Ui, settings: &mut AppSettings) {
    settings_grid(ui, "settings-tracing", |ui| {
        ui.weak("New targets: interval");
        tab::interval_choice(ui, &mut settings.defaults.interval);
        ui.end_row();
        ui.weak("New targets: focus");
        tab::focus_choice(ui, &mut settings.defaults.focus);
        ui.end_row();
        ui.weak("New targets: chart period");
        egui::ComboBox::from_id_salt("default-period")
            .selected_text(settings.defaults.period.label())
            .show_ui(ui, |ui| {
                for period in Period::ALL {
                    ui.selectable_value(&mut settings.defaults.period, period, period.label());
                }
            });
        ui.end_row();
        ui.weak("Wait for an answer");
        choice(ui, "timeout", &mut settings.timeout, &TIMEOUTS, |timeout| format!("{} ms", timeout.as_millis()));
        ui.end_row();
        ui.weak("Hop limit");
        choice(ui, "max-hops", &mut settings.max_hops, &MAX_HOPS, |hops| format!("{hops} hops"));
        ui.end_row();
        ui.weak("Keep history for");
        choice(ui, "retention", &mut settings.retention, &RETENTIONS, |retention| days(*retention));
        ui.end_row();
    });
    ui.add_space(10.0);
    ui.weak("Targets you traced before keep their own interval, focus and period. Timeout and hop limit apply to traces started from now on.");
}

/// Appearance: the theme — follow Windows, dark or light (only dark so far).
fn appearance(ui: &mut Ui) {
    settings_grid(ui, "settings-appearance", |ui| {
        ui.weak("Theme");
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 18.0;
            let themes = [(Icon::Auto, "Auto", false), (Icon::Moon, "Dark", true), (Icon::Sun, "Light", false)];
            for (icon, label, selected) in themes {
                let response = icon_option(ui, icon, label, selected);
                if !selected {
                    response.on_disabled_hover_text("Needs the light theme, which isn't ready yet");
                }
            }
        });
        ui.end_row();
    });
}

/// Language: the interface language (only English so far).
fn language(ui: &mut Ui) {
    settings_grid(ui, "settings-language", |ui| {
        ui.weak("Interface");
        icon_option(ui, Icon::Globe, "English", true).on_disabled_hover_text("Other languages aren't ready yet");
        ui.end_row();
    });
}

/// A choice shown as its icon and a radio button; disabled while only one choice exists.
fn icon_option(ui: &mut Ui, icon: Icon, label: &str, selected: bool) -> egui::Response {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        let (rect, _) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::hover());
        icons::paint(ui, rect.center(), icon, if selected { theme::INK } else { theme::INK_MUTED });
        ui.add_enabled(false, egui::RadioButton::new(selected, label))
    })
    .inner
}

/// The close mark of the title row; returns whether it was clicked.
fn close_mark(ui: &mut Ui) -> bool {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(26.0, 26.0), egui::Sense::click());
    if response.hovered() {
        ui.painter().rect_filled(rect, egui::CornerRadius::same(3), theme::PANE_HEADER);
    }
    icons::paint(ui, rect.center(), Icon::Cross, if response.hovered() { theme::INK } else { theme::INK_MUTED });
    response.on_hover_text("Close (Esc)").clicked()
}

/// The app logo as a texture, `None` if the embedded PNG can't be decoded.
fn load_logo(ctx: &egui::Context) -> Option<egui::TextureHandle> {
    let icon = eframe::icon_data::from_png_bytes(crate::ICON_PNG)
        .inspect_err(|error| log::warn!("cannot load the logo: {error}"))
        .ok()?;
    let size = [usize::try_from(icon.width).ok()?, usize::try_from(icon.height).ok()?];
    let image = egui::ColorImage::from_rgba_unmultiplied(size, &icon.rgba);
    Some(ctx.load_texture("logo", image, egui::TextureOptions::LINEAR))
}

impl SettingsWindow {
    /// About: logo, name, version with the update check, what the app does, links, where its data lives, the
    /// font's license.
    fn about(&mut self, ui: &mut Ui, data_folder: Option<&Path>) {
        ui.horizontal_top(|ui| {
            if let Some(logo) = &self.logo {
                ui.add(egui::Image::new(logo).fit_to_exact_size(egui::vec2(LOGO_SIZE, LOGO_SIZE)));
                ui.add_space(12.0);
            }
            ui.vertical(|ui| {
                ui.label(RichText::new(APP_NAME).font(theme::heading_font(20.0)));
                ui.horizontal(|ui| {
                    ui.weak(format!("Version {}", env!("CARGO_PKG_VERSION")));
                    if ui.small_button("Check for updates").clicked() {
                        self.update_asked = true;
                    }
                });
                if self.update_asked {
                    ui.weak("No releases are published yet.");
                }
                ui.add_space(6.0);
                ui.label("Traces the route to your targets, shows each hop's latency and loss, and keeps the history.");
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.hyperlink_to("Website", SITE_URL);
                    ui.hyperlink_to("Source code", REPO_URL);
                });
            });
        });
        ui.add_space(14.0);
        about_details(ui, data_folder);
    }
}

/// Data folder and font license rows of the About page.
fn about_details(ui: &mut Ui, data_folder: Option<&Path>) {
    settings_grid(ui, "settings-about", |ui| {
        ui.weak("Data folder");
        ui.horizontal(|ui| match data_folder {
            Some(folder) => {
                ui.label(folder.display().to_string());
                if ui.small_button("Open").clicked()
                    && let Err(error) = std::process::Command::new("explorer").arg(folder).spawn()
                {
                    log::warn!("cannot open {}: {error}", folder.display());
                }
            }
            None => {
                ui.weak("none, history is off");
            }
        });
        ui.end_row();
        ui.weak("Font");
        ui.label("Barlow, © The Barlow Project Authors, SIL Open Font License 1.1");
        ui.end_row();
    });
}

/// A two-column grid: muted labels on the left, controls on the right.
fn settings_grid(ui: &mut Ui, id: &str, rows: impl FnOnce(&mut Ui)) {
    Grid::new(id).num_columns(2).spacing(GRID_SPACING).show(ui, rows);
}

/// Combo box choosing one of `options` (labelled by `label`) for `value`.
fn choice<T: Copy + PartialEq>(ui: &mut Ui, id: &str, value: &mut T, options: &[T], label: impl Fn(&T) -> String) {
    egui::ComboBox::from_id_salt(id).selected_text(label(value)).show_ui(ui, |ui| {
        for option in options {
            ui.selectable_value(value, *option, label(option));
        }
    });
}

/// "1 day", "7 days".
fn days(duration: Duration) -> String {
    match duration.as_secs() / (24 * 60 * 60) {
        1 => "1 day".to_owned(),
        count => format!("{count} days"),
    }
}
