//! Main window: a tab bar ("All Targets", one tab per target, "+"), the All Targets overview and the active
//! target tab. Keyboard shortcuts act on the active tab.

use std::path::PathBuf;
use std::time::Duration;

use eframe::egui::{self, Align, Layout, RichText, Sense, Ui};
use egui_extras::{Column, TableBuilder};

use crate::history::{self, Store};
use crate::hop_table::format_ms;
use crate::names::NameCache;
use crate::tab::{self, Action, Tab};

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
}

impl PingWatchApp {
    /// Creates the app with one tab per host in `hosts` (all tracing right away), or one empty tab.
    pub fn new(hosts: Vec<String>) -> Self {
        let history_path = history::default_path().inspect_err(|error| log::warn!("{error}")).ok();
        let reader = history_path
            .as_deref()
            .and_then(|path| Store::open(path).inspect_err(|error| log::warn!("{error}")).ok());
        let mut tabs: Vec<Tab> = hosts.into_iter().map(Tab::tracing).collect();
        if tabs.is_empty() {
            tabs.push(Tab::new());
        }
        Self { tabs, page: Page::Target(0), names: NameCache::default(), history_path, reader }
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

    /// Tab bar: All Targets, the target tabs with close buttons, and "+".
    fn tab_bar(&mut self, ui: &mut Ui) {
        let mut close = None;
        ui.horizontal(|ui| {
            if ui.selectable_label(self.page == Page::AllTargets, "All Targets").clicked() {
                self.page = Page::AllTargets;
            }
            for (index, tab) in self.tabs.iter().enumerate() {
                ui.separator();
                if ui.selectable_label(self.page == Page::Target(index), tab.title()).clicked() {
                    self.page = Page::Target(index);
                }
                if ui.small_button("\u{2716}").on_hover_text("Close (and stop)").clicked() {
                    close = Some(index);
                }
            }
            ui.separator();
            if ui.button("+").on_hover_text("New target").clicked() {
                self.apply(Action::NewTarget);
            }
        });
        if let Some(index) = close {
            self.close(index);
        }
    }

    /// All Targets overview: one row per target with its final-hop numbers; a click opens the tab.
    fn overview(&mut self, ui: &mut Ui) {
        if self.tabs.is_empty() {
            ui.label("No targets. Press + or Ctrl+N to add one.");
            return;
        }
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
                        ui.strong(title);
                    });
                }
            })
            .body(|mut body| {
                for (index, overview) in overviews.iter().enumerate() {
                    body.row(ROW_HEIGHT, |mut row| {
                        row.col(|ui| {
                            ui.label(RichText::new(&overview.title).strong());
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
    }
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
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        if let Some(action) = shortcut_action(ui.ctx()) {
            self.apply(action);
        }
        egui::Panel::top("tab-bar").show(ui, |ui| self.tab_bar(ui));
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
                egui::CentralPanel::default().show(ui, |ui| self.overview(ui));
                None
            }
            Page::Target(index) => self
                .tabs
                .get_mut(index)
                .and_then(|tab| egui::CentralPanel::default().show(ui, |ui| tab.ui(ui, context)).inner),
        };
        if let Some(action) = action {
            self.apply(action);
        }
        ui.ctx().request_repaint_after(REFRESH);
    }
}
