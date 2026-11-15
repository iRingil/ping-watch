//! The Settings window, opened from the gear: pages General, Tracing, Appearance, Language and About. Changes apply
//! at once (the app stores them); there is nothing to confirm.

use std::path::Path;
use std::time::Duration;

use eframe::egui::{self, Align, Grid, Layout, RichText, Ui};

use crate::APP_NAME;
use crate::store::autostart;
use crate::store::settings::{AppSettings, MAX_HOPS, RETENTIONS, TIMEOUTS};
use crate::ui::tab;
use crate::ui::theme;
use crate::ui::timeline::Period;

/// Width of the window's content.
const WIDTH: f32 = 520.0;
/// Least height of a page, so switching pages doesn't make the window jump.
const PAGE_HEIGHT: f32 = 290.0;
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
#[derive(Debug, Default)]
pub struct SettingsWindow {
    /// Whether the window is shown.
    open: bool,
    /// Page shown.
    page: Page,
    /// Whether Windows starts the app, read from the registry when the window opens.
    autostart: bool,
    /// Why reading or changing autostart failed.
    autostart_error: Option<String>,
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
        let modal = egui::Modal::new(egui::Id::unique("settings")).show(ctx, |ui| {
            ui.set_width(WIDTH);
            ui.label(RichText::new("Settings").font(theme::heading_font(18.0)));
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
                    Page::About => about(ui, data_folder),
                }
            });
            ui.separator();
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| ui.button("Close").clicked()).inner
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

/// Appearance: the theme (only the dark one so far).
fn appearance(ui: &mut Ui) {
    settings_grid(ui, "settings-appearance", |ui| {
        ui.weak("Theme");
        ui.horizontal(|ui| {
            ui.add_enabled(false, egui::RadioButton::new(true, "Dark"));
            ui.add_enabled(false, egui::RadioButton::new(false, "Light"))
                .on_disabled_hover_text("The light theme isn't ready yet");
        });
        ui.end_row();
    });
}

/// Language: the interface language (only English so far).
fn language(ui: &mut Ui) {
    settings_grid(ui, "settings-language", |ui| {
        ui.weak("Interface");
        ui.add_enabled(false, egui::RadioButton::new(true, "English"))
            .on_disabled_hover_text("Other languages aren't ready yet");
        ui.end_row();
    });
}

/// About: name, version, what the app does, where its data lives, the font's license.
fn about(ui: &mut Ui, data_folder: Option<&Path>) {
    ui.label(RichText::new(APP_NAME).font(theme::heading_font(20.0)));
    ui.weak(format!("Version {}", env!("CARGO_PKG_VERSION")));
    ui.add_space(8.0);
    ui.label("Traces the route to your targets, shows each hop's latency and loss, and keeps the history.");
    ui.add_space(14.0);
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
