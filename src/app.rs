//! Main window: a tab bar ("All Targets", one tab per target, "+"), the All Targets overview and the active
//! target tab. Keyboard shortcuts act on the active tab.

use std::path::PathBuf;
use std::time::Duration;

use eframe::egui::{self, Align, Button, Frame, Layout, Margin, RichText, Sense, Stroke, Ui, Vec2};
use egui_extras::{Column, TableBuilder};

use crate::history::{self, Store};
use crate::hop_table::format_ms;
use crate::names::NameCache;
use crate::tab::{self, Action, Tab};
use crate::theme;
use crate::tray::{Tray, TrayCommand};

/// How often the window redraws to show fresh numbers.
const REFRESH: Duration = Duration::from_millis(250);
/// Height of an overview table row.
const ROW_HEIGHT: f32 = 22.0;

/// Which page the window shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    /// Overview of all targets.
    AllTargets,
    /// The target tab at this index.
    Target(usize),
}

/// Application state.
#[derive(Debug)]
pub struct PingWatchApp {
    /// Target tabs in display order.
    tabs: Vec<Tab>,
    /// Page shown.
    page: Page,
    /// Reverse-DNS cache for hop names, shared by all tabs.
    names: NameCache,
    /// History database file, `None` if there is no place for it.
    history_path: Option<PathBuf>,
    /// Read connection to the history for the timelines, `None` if history is off.
    reader: Option<Store>,
    /// Icon in the notification area, `None` if it couldn't be created.
    tray: Option<Tray>,
}

impl PingWatchApp {
    /// Creates the app with one tab per host in `hosts` (all tracing right away), or one empty tab, and the
    /// `tray` icon.
    pub fn new(hosts: Vec<String>, tray: Option<Tray>) -> Self {
        let history_path = history::default_path().inspect_err(|error| log::warn!("{error}")).ok();
        let reader = history_path
            .as_deref()
            .and_then(|path| Store::open(path).inspect_err(|error| log::warn!("{error}")).ok());
        let mut tabs: Vec<Tab> = hosts.into_iter().map(Tab::tracing).collect();
        if tabs.is_empty() {
            tabs.push(Tab::new());
        }
        Self { tabs, page: Page::Target(0), names: NameCache::default(), history_path, reader, tray }
    }

    /// Carries out `action` on the active tab.
    fn apply(&mut self, action: Action) {
        match action {
            Action::NewTarget => {
                self.tabs.push(Tab::new());
                self.page = Page::Target(self.tabs.len().saturating_sub(1));
            }
            Action::Restart => {
                if let Some(tab) = self.active_tab() {
                    tab.restart();
                }
            }
            Action::Close => {
                if let Page::Target(index) = self.page {
                    self.close(index);
                }
            }
        }
    }

    /// The target tab shown, if any.
    fn active_tab(&mut self) -> Option<&mut Tab> {
        match self.page {
            Page::Target(index) => self.tabs.get_mut(index),
            Page::AllTargets => None,
        }
    }

    /// Closes (and stops) tab `index`, showing its neighbour, or the overview after the last tab.
    fn close(&mut self, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        self.tabs.remove(index);
        self.page = match self.page {
            _ if self.tabs.is_empty() => Page::AllTargets,
            Page::Target(shown) if shown > index => Page::Target(shown.saturating_sub(1)),
            Page::Target(shown) if shown == index => Page::Target(index.min(self.tabs.len().saturating_sub(1))),
            page => page,
        };
    }

    /// Tab bar: All targets, the target tabs with close buttons, and "+". The open tab is underlined in the
    /// signal color.
    fn tab_bar(&mut self, ui: &mut Ui) {
        let mut close = None;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            if tab_label(ui, self.page == Page::AllTargets, "All targets").clicked() {
                self.page = Page::AllTargets;
            }
            for (index, tab) in self.tabs.iter().enumerate() {
                ui.add_space(6.0);
                if tab_label(ui, self.page == Page::Target(index), &tab.title()).clicked() {
                    self.page = Page::Target(index);
                }
                let close_button = Button::new(RichText::new("\u{d7}").color(theme::INK_MUTED)).frame(false);
                if ui.add(close_button).on_hover_text("Close and stop").clicked() {
                    close = Some(index);
                }
            }
            ui.add_space(6.0);
            if ui.add(Button::new("+").frame(false)).on_hover_text("New target (Ctrl+N)").clicked() {
                self.apply(Action::NewTarget);
            }
        });
        if let Some(index) = close {
            self.close(index);
        }
    }

    /// All Targets overview: one row per target with its final-hop numbers; a click opens the tab.
    fn overview(&mut self, ui: &mut Ui) {
        let overviews: Vec<_> = self.tabs.iter().map(Tab::overview).collect();
        let mut open = None;
        TableBuilder::new(ui)
            .striped(true)
            .sense(Sense::click())
            .cell_layout(Layout::left_to_right(Align::Center))
            .column(Column::initial(220.0).at_least(120.0).clip(true))
            .column(Column::initial(200.0).at_least(100.0).clip(true))
            .column(Column::initial(160.0).at_least(80.0).clip(true))
            .columns(Column::exact(70.0), 6)
            .header(ROW_HEIGHT, |mut header| {
                for title in ["Target", "IP", "Status", "Hops", "Avg", "Min", "Max", "Cur", "PL%"] {
                    header.col(|ui| {
                        ui.painter().rect_filled(ui.max_rect().expand2(Vec2::new(4.0, 0.0)), 0.0, theme::PANE_HEADER);
                        ui.label(RichText::new(title).color(theme::INK_MUTED));
                    });
                }
            })
            .body(|mut body| {
                for (index, overview) in overviews.iter().enumerate() {
                    body.row(ROW_HEIGHT, |mut row| {
                        row.col(|ui| {
                            ui.label(RichText::new(&overview.title).font(theme::heading_font(15.0)));
                        });
                        row.col(|ui| {
                            ui.label(overview.target.map(|ip| ip.to_string()).unwrap_or_default());
                        });
                        row.col(|ui| {
                            ui.label(&overview.status);
                        });
                        row.col(|ui| {
                            ui.label(overview.hops.map(|hops| hops.to_string()).unwrap_or_default());
                        });
                        let stats = overview.target_hop.as_ref().map(|hop| &hop.stats);
                        for value in [
                            stats.and_then(|stats| stats.average()),
                            stats.and_then(|stats| stats.best()),
                            stats.and_then(|stats| stats.worst()),
                            stats.and_then(|stats| stats.last()),
                        ] {
                            row.col(|ui| {
                                ui.label(format_ms(value));
                            });
                        }
                        row.col(|ui| {
                            ui.label(stats.map(|stats| format!("{:.1}", stats.loss_percent())).unwrap_or_default());
                        });
                        if row.response().clicked() {
                            open = Some(index);
                        }
                    });
                }
            });
        if let Some(index) = open {
            self.page = Page::Target(index);
        }
        if self.tabs.is_empty() {
            ui.add_space(12.0);
            ui.weak("No targets yet. Add one with + or Ctrl+N.");
        }
    }
}

/// A tab title in the tab bar, underlined in the signal color when open.
fn tab_label(ui: &mut Ui, open: bool, title: &str) -> egui::Response {
    let text =
        RichText::new(title)
            .font(theme::heading_font(14.5))
            .color(if open { theme::INK } else { theme::INK_MUTED });
    let response = ui.add(Button::new(text).frame(false));
    if open {
        let rect = response.rect;
        ui.painter().hline(rect.x_range(), rect.bottom() + 3.0, Stroke::new(2.0, theme::SIGNAL));
    }
    response
}

/// The action of a pressed keyboard shortcut, if any (consumed so widgets don't see it).
fn shortcut_action(ctx: &egui::Context) -> Option<Action> {
    let shortcuts = [
        (tab::RESTART_SHORTCUT, Action::Restart),
        (tab::NEW_TARGET_SHORTCUT, Action::NewTarget),
        (tab::CLOSE_SHORTCUT, Action::Close),
    ];
    ctx.input_mut(|input| {
        shortcuts
            .into_iter()
            .find(|(shortcut, _)| input.consume_shortcut(shortcut))
            .map(|(_, action)| action)
    })
}

impl eframe::App for PingWatchApp {
    /// Handles the tray icon; runs even while the window is minimized or hidden.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let Some(tray) = &self.tray else {
            return;
        };
        while let Some(command) = tray.poll() {
            match command {
                TrayCommand::Show => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
                TrayCommand::Exit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            }
        }
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        if let Some(action) = shortcut_action(ui.ctx()) {
            self.apply(action);
        }
        let bar_frame = Frame::new().fill(theme::WELL).inner_margin(Margin { left: 10, right: 10, top: 6, bottom: 6 });
        egui::Panel::top("tab-bar")
            .frame(bar_frame)
            .show_separator_line(false)
            .show(ui, |ui| self.tab_bar(ui));
        let context = tab::Context {
            names: &self.names,
            reader: self.reader.as_ref(),
            history_path: self.history_path.as_deref(),
        };
        for tab in &mut self.tabs {
            tab.poll(context);
        }
        let action = match self.page {
            Page::AllTargets => {
                let frame = Frame::new().fill(theme::CHASSIS).inner_margin(Margin::same(10));
                egui::CentralPanel::default().frame(frame).show(ui, |ui| {
                    theme::pane_frame().inner_margin(Margin::same(1)).show(ui, |ui| {
                        self.overview(ui);
                        ui.take_available_space();
                    });
                });
                None
            }
            Page::Target(index) => self.tabs.get_mut(index).and_then(|tab| {
                egui::CentralPanel::default().frame(Frame::new()).show(ui, |ui| tab.ui(ui, context)).inner
            }),
        };
        if let Some(action) = action {
            self.apply(action);
        }
        ui.ctx().request_repaint_after(REFRESH);
    }
}
