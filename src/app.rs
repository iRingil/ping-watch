//! Main window: a tab bar ("All Targets", one tab per target, "+"), the All Targets overview and the active
//! target tab. Keyboard shortcuts act on the active tab.

use std::net::IpAddr;
use std::path::PathBuf;
use std::time::Duration;

use eframe::egui::{
    self, Align, Button, CornerRadius, Frame, Layout, Margin, Rect, RichText, Sense, Stroke, StrokeKind, Ui, Vec2,
};
use egui_extras::{Column, TableBuilder};

use crate::history::{self, Store};
use crate::hop_table::{self, format_ms};
use crate::names::NameCache;
use crate::tab::{self, Action, Tab};
use crate::targets::TargetList;
use crate::theme;
use crate::tray::{Tray, TrayCommand};

/// How often the window redraws to show fresh numbers.
const REFRESH: Duration = Duration::from_millis(250);
/// Height of a tab in the tab bar.
const TAB_HEIGHT: f32 = 30.0;
/// Space between a tab's edge and its title.
const TAB_PADDING: f32 = 12.0;
/// Width of a tab's close mark.
const TAB_CLOSE_WIDTH: f32 = 16.0;
/// Width of the overview's text columns (target, IP, status).
const OVERVIEW_TEXT_WIDTH: f32 = 220.0;
/// Width of the overview's numeric columns.
const OVERVIEW_NUMBER_WIDTH: f32 = 70.0;
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
    /// The user's target list, `None` if history is off.
    targets: Option<TargetList>,
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
        let targets = history_path
            .as_deref()
            .and_then(|path| TargetList::open(path).inspect_err(|error| log::warn!("{error}")).ok());
        Self { tabs, page: Page::Target(0), names: NameCache::default(), history_path, reader, targets, tray }
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

    /// Closes (and stops) tab `index`; if it was shown, the overview is shown instead.
    fn close(&mut self, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        self.tabs.remove(index);
        self.page = match self.page {
            // Closing the shown tab leads to the overview, not to some neighbouring trace.
            Page::Target(shown) if shown == index => Page::AllTargets,
            Page::Target(shown) if shown > index => Page::Target(shown.saturating_sub(1)),
            page => page,
        };
    }

    /// Keeps one tab per traced address: when a tab starts tracing an address another tab already traces, it is
    /// closed and the earlier tab is shown instead.
    fn merge_duplicate_traces(&mut self) {
        let targets: Vec<Option<IpAddr>> = self.tabs.iter().map(Tab::target).collect();
        let duplicate = targets.iter().enumerate().find_map(|(later, target)| {
            let target = (*target)?;
            let earlier = targets.iter().take(later).position(|other| *other == Some(target))?;
            Some((earlier, later))
        });
        if let Some((earlier, later)) = duplicate {
            log::info!("{} is already traced in another tab", self.tabs.get(later).map(Tab::title).unwrap_or_default());
            self.tabs.remove(later);
            self.page = Page::Target(earlier);
        }
    }

    /// Tab bar: All targets, the target tabs with close buttons, and "+". The open tab is underlined in the
    /// signal color.
    fn tab_bar(&mut self, ui: &mut Ui) {
        let mut close = None;
        // The baseline the tabs stand on; the open tab covers it, so it reads as joined to the toolbar below.
        let bar = ui.max_rect();
        ui.painter().hline(bar.x_range(), bar.bottom() - 0.5, Stroke::new(1.0, theme::RULE));
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            if tab_widget(ui, self.page == Page::AllTargets, "All targets", false).0.clicked() {
                self.page = Page::AllTargets;
            }
            for (index, tab) in self.tabs.iter().enumerate() {
                let (response, close_clicked) = tab_widget(ui, self.page == Page::Target(index), &tab.title(), true);
                if close_clicked {
                    close = Some(index);
                } else if response.clicked() {
                    self.page = Page::Target(index);
                }
            }
            let plus = ui.add(Button::new(RichText::new("+").font(theme::heading_font(16.0))).frame(false));
            if plus.on_hover_text("New target (Ctrl+N)").clicked() {
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
        let row_width = OVERVIEW_TEXT_WIDTH * 3.0 + OVERVIEW_NUMBER_WIDTH * 6.0 + ui.spacing().item_spacing.x * 8.0;
        TableBuilder::new(ui)
            .striped(true)
            .sense(Sense::click())
            .cell_layout(Layout::left_to_right(Align::Center))
            .columns(Column::exact(OVERVIEW_TEXT_WIDTH).clip(true), 3)
            .columns(Column::exact(OVERVIEW_NUMBER_WIDTH), 6)
            .header(ROW_HEIGHT, |mut header| {
                // Title and whether the column is numeric (right-aligned).
                let columns = [
                    ("Target", false),
                    ("IP", false),
                    ("Status", false),
                    ("Hops", true),
                    ("Avg", true),
                    ("Min", true),
                    ("Max", true),
                    ("Cur", true),
                    ("PL%", true),
                ];
                for (index, (title, numeric)) in columns.into_iter().enumerate() {
                    header.col(|ui| hop_table::header_cell(ui, index == 0, numeric, title, row_width));
                }
            })
            .body(|mut body| {
                for (index, overview) in overviews.iter().enumerate() {
                    body.row(ROW_HEIGHT, |mut row| {
                        row.col(|ui| {
                            hop_table::cell(ui, false, RichText::new(&overview.title).font(theme::heading_font(15.0)));
                        });
                        row.col(|ui| {
                            hop_table::cell(ui, false, overview.target.map(|ip| ip.to_string()).unwrap_or_default());
                        });
                        row.col(|ui| hop_table::cell(ui, false, overview.status.as_str()));
                        row.col(|ui| {
                            hop_table::cell(ui, true, overview.hops.map(|hops| hops.to_string()).unwrap_or_default());
                        });
                        let stats = overview.target_hop.as_ref().map(|hop| &hop.stats);
                        for value in [
                            stats.and_then(|stats| stats.average()),
                            stats.and_then(|stats| stats.best()),
                            stats.and_then(|stats| stats.worst()),
                            stats.and_then(|stats| stats.last()),
                        ] {
                            row.col(|ui| hop_table::cell(ui, true, format_ms(value)));
                        }
                        let loss = stats.map(|stats| format!("{:.1}", stats.loss_percent())).unwrap_or_default();
                        row.col(|ui| hop_table::cell(ui, true, loss));
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
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                ui.weak("No targets yet.");
                let add = Button::new(RichText::new("Add target").color(theme::CHASSIS)).fill(theme::SIGNAL);
                if ui.add(add).on_hover_text("Ctrl+N").clicked() {
                    self.apply(Action::NewTarget);
                }
            });
        }
    }
}

/// A tab of the tab bar: a box with rounded top corners. Every tab keeps its outline; the open one is filled with
/// the toolbar color (so it joins the toolbar below) and has a signal-blue top edge. Returns the tab's response
/// and whether its close mark was clicked.
fn tab_widget(ui: &mut Ui, open: bool, title: &str, closable: bool) -> (egui::Response, bool) {
    let font = theme::heading_font(14.5);
    let text_color = if open { theme::INK } else { theme::INK_MUTED };
    let galley = ui.painter().layout_no_wrap(title.to_owned(), font, text_color);
    let close_width = if closable { TAB_CLOSE_WIDTH } else { 0.0 };
    let size = Vec2::new(galley.size().x + TAB_PADDING * 2.0 + close_width, TAB_HEIGHT);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());

    let fill = if open {
        theme::CHASSIS
    } else if response.hovered() {
        theme::PANE_HEADER
    } else {
        theme::TAB_IDLE
    };
    let corners = CornerRadius { nw: 5, ne: 5, sw: 0, se: 0 };
    let painter = ui.painter();
    painter.rect(rect, corners, fill, Stroke::new(1.0, theme::RULE), StrokeKind::Inside);
    if open {
        // Open the bottom edge into the toolbar and mark the top.
        let inner = (rect.left() + 1.0)..=(rect.right() - 1.0);
        painter.hline(inner.clone(), rect.bottom() - 0.5, Stroke::new(2.0, fill));
        painter.hline(inner, rect.top() + 1.0, Stroke::new(2.0, theme::SIGNAL));
    }
    let text_pos = egui::pos2(rect.left() + TAB_PADDING, rect.center().y - galley.size().y / 2.0);
    painter.galley(text_pos, galley, text_color);

    let mut close_clicked = false;
    if closable {
        let close_rect = Rect::from_center_size(
            egui::pos2(rect.right() - TAB_PADDING / 2.0 - TAB_CLOSE_WIDTH / 2.0, rect.center().y),
            Vec2::splat(TAB_CLOSE_WIDTH),
        );
        let close = ui.interact(close_rect, response.id.with("close"), Sense::click()).on_hover_text("Close and stop");
        let color = if close.hovered() { theme::BAD } else { theme::INK_MUTED };
        ui.painter()
            .text(close_rect.center(), egui::Align2::CENTER_CENTER, "\u{d7}", theme::heading_font(16.0), color);
        close_clicked = close.clicked();
    }
    (response, close_clicked)
}

impl PingWatchApp {
    /// Status bar: a problem of the shown tab in red (network down, history off, unknown host) or, normally, its key
    /// numbers; on the overview, how many targets are traced and how many have problems.
    fn status_bar(&self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            let tab = match self.page {
                Page::Target(index) => self.tabs.get(index),
                Page::AllTargets => None,
            };
            let Some(tab) = tab else {
                let problems = self.tabs.iter().filter(|tab| tab.problem().is_some()).count();
                status_segment(ui, "Targets", self.tabs.len().to_string());
                if problems > 0 {
                    ui.separator();
                    status_problem(ui, &format!("{problems} with problems"));
                }
                return;
            };
            if let Some(problem) = tab.problem() {
                status_problem(ui, &problem);
                return;
            }
            let overview = tab.overview();
            let Some(target) = overview.target else {
                ui.weak(&overview.status);
                return;
            };
            // "9.9.9.9 / 9.9.9.9" says nothing twice: the host only when it isn't the address itself.
            let target_text = if overview.title == target.to_string() {
                overview.title.clone()
            } else {
                format!("{target} / {}", overview.title)
            };
            status_segment(ui, "Target", target_text);
            let stats = overview.target_hop.as_ref().map(|hop| &hop.stats);
            ui.separator();
            status_segment(ui, "Avg", format!("{} ms", format_ms(stats.and_then(|stats| stats.average()))));
            ui.separator();
            status_segment(
                ui,
                "Loss",
                stats.map_or_else(|| "-".to_owned(), |stats| format!("{:.1} %", stats.loss_percent())),
            );
            ui.separator();
            status_segment(ui, "Hops", overview.hops.map_or_else(|| "-".to_owned(), |hops| hops.to_string()));
            if tab.is_paused() {
                ui.separator();
                ui.colored_label(theme::WARN, "Paused");
            }
        });
    }
}

/// A "label value" pair of the status bar.
fn status_segment(ui: &mut Ui, label: &str, value: String) {
    ui.weak(label);
    ui.label(value);
}

/// A problem in the status bar: a red dot and the message.
fn status_problem(ui: &mut Ui, message: &str) {
    let (dot, _) = ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
    ui.painter().circle_filled(dot.center(), 4.0, theme::BAD);
    ui.colored_label(theme::BAD, message);
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
        // Tabs that resolved last frame are checked before anything borrows them for drawing.
        self.merge_duplicate_traces();
        let bar_frame = Frame::new().fill(theme::WELL).inner_margin(Margin { left: 10, right: 10, top: 6, bottom: 0 });
        egui::Panel::top("tab-bar")
            .frame(bar_frame)
            .show_separator_line(false)
            .show(ui, |ui| self.tab_bar(ui));
        let status_frame = Frame::new().fill(theme::WELL).inner_margin(Margin::symmetric(12, 5));
        egui::Panel::bottom("status-bar")
            .frame(status_frame)
            .resizable(false)
            .show(ui, |ui| self.status_bar(ui));
        let context = tab::Context {
            names: &self.names,
            reader: self.reader.as_ref(),
            history_path: self.history_path.as_deref(),
            targets: self.targets.as_ref(),
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
