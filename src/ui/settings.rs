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
use crate::ui::theme::{self, ThemeChoice};
use crate::ui::timeline::Period;
use crate::update::{Check, Outcome, REPOSITORY, UpdateError};

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
/// Where problems and ideas are reported (the issue form picker).
const ISSUES_URL: &str = "https://github.com/iRingil/ping-watch/issues/new/choose";
/// Where vulnerabilities are reported privately.
const VULNERABILITY_URL: &str = "https://github.com/iRingil/ping-watch/security/advisories/new";
/// The license's text.
const LICENSE_URL: &str = "https://polyformproject.org/licenses/noncommercial/1.0.0/";
/// Width of the label column of the choice lists.
const LABEL_WIDTH: f32 = 110.0;
/// Most columns of the language list.
const LANGUAGE_COLUMNS: usize = 3;
/// Width of a column of the language list.
const LANGUAGE_COLUMN_WIDTH: f32 = 120.0;
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
    /// The update check: running, or its last result.
    update: UpdateState,
}

/// Where the update check is.
#[derive(Debug, Default)]
enum UpdateState {
    /// Not asked yet.
    #[default]
    Idle,
    /// Asking GitHub.
    Checking(Check),
    /// The answer.
    Done(Result<Outcome, UpdateError>),
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
            .field("update", &self.update)
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
            let pages = Page::ALL.map(|page| (page, page.title()));
            page_tabs(ui, &pages, &mut self.page);
            ui.add_space(12.0);
            ui.allocate_ui(egui::vec2(WIDTH, PAGE_HEIGHT), |ui| {
                ui.set_min_height(PAGE_HEIGHT);
                ui.spacing_mut().combo_width = COMBO_WIDTH;
                match self.page {
                    Page::General => self.general(ui, settings),
                    Page::Tracing => tracing(ui, settings),
                    Page::Appearance => appearance(ui, settings),
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

    /// General: what the close button does, starting with Windows.
    fn general(&mut self, ui: &mut Ui, settings: &mut AppSettings) {
        settings_grid(ui, "settings-general", |ui| {
            label_first_row(ui, "Close button", 2, ui.spacing().interact_size.y);
            ui.vertical(|ui| {
                ui.radio_value(&mut settings.close_to_tray, false, "Quit Ping Watch");
                ui.radio_value(&mut settings.close_to_tray, true, "Hide to the tray and keep tracing");
            });
            ui.end_row();
            ui.weak("Updates");
            ui.checkbox(&mut settings.check_updates, "Check for a new version")
                .on_hover_text("Once a day, in the background; Ping Watch tells you when one is out");
            ui.end_row();
            ui.weak("Windows sign-in");
            ui.vertical(|ui| {
                if ui.checkbox(&mut self.autostart, "Start Ping Watch").changed() {
                    self.autostart_error = autostart::set_enabled(self.autostart).err().map(|error| error.to_string());
                }
                if let Some(error) = &self.autostart_error {
                    ui.colored_label(theme::palette(ui).bad, error);
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
fn appearance(ui: &mut Ui, settings: &mut AppSettings) {
    // One choice under another, the label level with the first.
    label_on_top(ui, "Theme", |ui| {
        ui.vertical(|ui| {
            let themes = [
                (ThemeChoice::Auto, Icon::Auto, "Auto", "Follows the Windows app theme"),
                (ThemeChoice::Dark, Icon::Moon, "Dark", "Graphite panel, light text"),
                (ThemeChoice::Light, Icon::Sun, "Light", "Pale panel, dark text"),
            ];
            for (choice, icon, label, hint) in themes {
                let response = icon_option(ui, icon, label, settings.theme == choice, true).on_hover_text(hint);
                if response.clicked() {
                    settings.theme = choice;
                }
            }
        });
    });
}

/// Language: the interface language (only English so far). The choices run top to bottom, then left to right, in
/// up to [`LANGUAGE_COLUMNS`] columns.
fn language(ui: &mut Ui) {
    let languages = [(Icon::FlagGb, "English")];
    let rows = languages.len().div_ceil(LANGUAGE_COLUMNS).max(1);
    label_on_top(ui, "Interface", |ui| {
        ui.horizontal_top(|ui| {
            for column in languages.chunks(rows) {
                ui.vertical(|ui| {
                    ui.set_width(LANGUAGE_COLUMN_WIDTH);
                    for (icon, label) in column {
                        icon_option(ui, *icon, label, true, false)
                            .on_disabled_hover_text("Other languages aren't ready yet");
                    }
                });
            }
        });
    });
}

/// A muted grid label for a cell holding `lines` rows of `line_height` each, drawn level with the first row (grid
/// rows center their cells, which would put a lone label between the rows).
pub fn label_first_row(ui: &mut Ui, label: &str, lines: u8, line_height: f32) {
    let font = egui::TextStyle::Body.resolve(ui.style());
    let palette = theme::palette(ui);
    let galley = ui.painter().layout_no_wrap(label.to_owned(), font, palette.ink_muted);
    let extra_rows = f32::from(lines.saturating_sub(1));
    let height = line_height + extra_rows * (line_height + ui.spacing().item_spacing.y);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(galley.size().x, height), egui::Sense::hover());
    let top = rect.top() + (line_height - galley.size().y) / 2.0;
    ui.painter().galley(egui::pos2(rect.left(), top), galley, palette.ink_muted);
}

/// A muted `label` on the left, level with the top of `content` (a list of choices) on the right.
fn label_on_top(ui: &mut Ui, label: &str, content: impl FnOnce(&mut Ui)) {
    ui.horizontal_top(|ui| {
        let (rect, _) =
            ui.allocate_exact_size(egui::vec2(LABEL_WIDTH, ui.spacing().interact_size.y), egui::Sense::hover());
        let palette = theme::palette(ui);
        ui.painter().text(
            egui::pos2(rect.left(), rect.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            egui::TextStyle::Body.resolve(ui.style()),
            palette.ink_muted,
        );
        content(ui);
    });
}

/// A choice shown as a radio button, its icon and its label, all clickable as one when `enabled` (disabled while
/// it is the only choice there is).
fn icon_option(ui: &mut Ui, icon: Icon, label: &str, selected: bool, enabled: bool) -> egui::Response {
    let sense = if enabled { egui::Sense::click() } else { egui::Sense::hover() };
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        let radio = ui.add_enabled(enabled, egui::RadioButton::new(selected, ""));
        let (rect, icon_response) = ui.allocate_exact_size(egui::vec2(22.0, 18.0), sense);
        let palette = theme::palette(ui);
        icons::paint(ui, rect.center(), icon, if selected { palette.ink } else { palette.ink_muted });
        let text = ui.add_enabled(enabled, egui::Label::new(label).sense(sense));
        radio.union(icon_response).union(text)
    })
    .inner
}

/// The close mark of the title row; returns whether it was clicked.
pub fn close_mark(ui: &mut Ui) -> bool {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(26.0, 26.0), egui::Sense::click());
    if response.hovered() {
        ui.painter().rect_filled(rect, egui::CornerRadius::same(3), theme::palette(ui).pane_header);
    }
    icons::paint(
        ui,
        rect.center(),
        Icon::Cross,
        if response.hovered() { theme::palette(ui).ink } else { theme::palette(ui).ink_muted },
    );
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
                    let checking = matches!(self.update, UpdateState::Checking(_));
                    if ui.add_enabled(!checking, egui::Button::new("Check for updates")).clicked() {
                        self.update = UpdateState::Checking(Check::start(REPOSITORY, env!("CARGO_PKG_VERSION")));
                    }
                });
                self.update_result(ui);
                ui.add_space(6.0);
                ui.label("Traces the route to your targets, shows each hop's latency and loss, and keeps the history.");
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.hyperlink_to("Website", SITE_URL);
                    // A painted dot: a bigger glyph would move the baseline of the links beside it.
                    let (dot, _) = ui.allocate_exact_size(egui::vec2(6.0, 12.0), egui::Sense::hover());
                    ui.painter().circle_filled(dot.center(), 2.0, theme::palette(ui).ink_muted);
                    ui.hyperlink_to("Source code", REPO_URL);
                });
            });
        });
        ui.add_space(14.0);
        about_details(ui, data_folder);
    }
}

impl SettingsWindow {
    /// Shows the result of the background check in About, as if the button had been clicked.
    pub fn set_update_result(&mut self, result: Result<Outcome, UpdateError>) {
        self.update = UpdateState::Done(result);
    }

    /// The update check's state under the version: a spinner while asking, then what it found.
    fn update_result(&mut self, ui: &mut Ui) {
        if let UpdateState::Checking(check) = &self.update
            && let Some(result) = check.result()
        {
            self.update = UpdateState::Done(result);
        }
        match &self.update {
            UpdateState::Idle => {}
            UpdateState::Checking(_) => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.weak("Asking GitHub…");
                });
            }
            UpdateState::Done(Ok(Outcome::UpToDate)) => {
                ui.colored_label(theme::palette(ui).good, "You have the latest version.");
            }
            UpdateState::Done(Ok(Outcome::NoReleases)) => {
                ui.weak("No releases are published yet.");
            }
            UpdateState::Done(Ok(Outcome::Newer { version, url })) => {
                ui.horizontal(|ui| {
                    ui.colored_label(theme::palette(ui).warn, format!("Version {version} is available."));
                    ui.hyperlink_to("Download", url);
                });
            }
            UpdateState::Done(Err(error)) => {
                ui.colored_label(theme::palette(ui).bad, format!("Couldn't check: {error}"));
            }
        }
    }
}

/// Data folder and font license rows of the About page.
fn about_details(ui: &mut Ui, data_folder: Option<&Path>) {
    settings_grid(ui, "settings-about", |ui| {
        ui.weak("Data folder");
        ui.horizontal(|ui| match data_folder {
            Some(folder) => {
                ui.label(folder.display().to_string());
                if ui.button("Open").clicked()
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
        // Feedback goes in its own row, apart from the project links next to the name.
        label_first_row(ui, "Feedback", 2, ui.text_style_height(&egui::TextStyle::Body));
        ui.vertical(|ui| {
            ui.hyperlink_to("Report a problem or idea", ISSUES_URL);
            ui.hyperlink_to("Report a vulnerability", VULNERABILITY_URL);
        });
        ui.end_row();
        ui.weak("License");
        ui.hyperlink_to("PolyForm Noncommercial 1.0.0", LICENSE_URL);
        ui.end_row();
        ui.weak("Font");
        ui.label("Roboto Condensed, © The Roboto Project Authors, SIL Open Font License 1.1");
        ui.end_row();
    });
}

/// Page tabs of a dialog: titles on a rule, the shown one (`current`) underlined in the signal color; a click
/// shows another.
pub fn page_tabs<T: Copy + PartialEq>(ui: &mut Ui, pages: &[(T, &str)], current: &mut T) {
    let row = ui
        .horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 18.0;
            for (page, title) in pages {
                let shown = *current == *page;
                let palette = theme::palette(ui);
                let color = if shown { palette.ink } else { palette.ink_muted };
                let text = RichText::new(*title).font(theme::heading_font(15.0)).color(color);
                let response = ui.add(egui::Label::new(text).sense(egui::Sense::click()));
                if shown {
                    let rect = response.rect;
                    ui.painter().hline(rect.x_range(), rect.bottom() + 4.0, egui::Stroke::new(2.0, palette.signal));
                }
                if response.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                    *current = *page;
                }
            }
        })
        .response;
    ui.painter().hline(
        ui.max_rect().x_range(),
        row.rect.bottom() + 5.5,
        egui::Stroke::new(1.0, theme::palette(ui).rule),
    );
}

/// A two-column grid: muted labels on the left, controls on the right.
pub fn settings_grid(ui: &mut Ui, id: &str, rows: impl FnOnce(&mut Ui)) {
    Grid::new(id).num_columns(2).spacing(GRID_SPACING).show(ui, rows);
}

/// Combo box choosing one of `options` (labelled by `label`) for `value`.
pub fn choice<T: Copy + PartialEq>(ui: &mut Ui, id: &str, value: &mut T, options: &[T], label: impl Fn(&T) -> String) {
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
