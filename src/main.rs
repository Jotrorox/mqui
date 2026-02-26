#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use eframe::egui;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TabKind {
    Client,
}

#[derive(Clone, Debug)]
pub enum TabState {
    Client { counter: i32 },
}

#[derive(Clone, Debug)]
pub struct Tab {
    pub id: u64,
    pub title: String,
    pub kind: TabKind,
    pub state: TabState,
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions::default();

    eframe::run_native(
        "MQUI",
        options,
        Box::new(|_cc| Ok(Box::new(App::default()))),
    )
}

#[derive(Default)]
struct App {
    next_tab_id: u64,
    tabs: Vec<Tab>,
    active_tab: Option<u64>,
}

impl App {
    fn new_tab(&mut self, kind: TabKind) {
        let id = self.next_tab_id;
        self.next_tab_id += 1;

        let (title, state) = match kind {
            TabKind::Client => ("Home".to_string(), TabState::Client { counter: 0 }),
        };

        self.tabs.push(Tab {
            id,
            title,
            kind,
            state,
        });
        self.active_tab = Some(id);
    }

    fn close_active_tab(&mut self) {
        let Some(active_id) = self.active_tab else {
            return;
        };

        if let Some(idx) = self.tabs.iter().position(|t| t.id == active_id) {
            self.tabs.remove(idx);

            // Choose a new active tab (left neighbor, else first, else none)
            self.active_tab = if self.tabs.is_empty() {
                None
            } else if idx > 0 {
                Some(self.tabs[idx - 1].id)
            } else {
                Some(self.tabs[0].id)
            };
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::TopBottomPanel::top("tab_bar").show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                if ui.button("+").clicked() {
                    self.new_tab(TabKind::Client);
                }

                ui.separator();

                let can_close = self.active_tab.is_some();
                if ui
                    .add_enabled(can_close, egui::Button::new("Close active"))
                    .clicked()
                {
                    self.close_active_tab();
                }

                ui.separator();

                for tab in &self.tabs {
                    let selected = self.active_tab == Some(tab.id);
                    if ui.selectable_label(selected, &tab.title).clicked() {
                        self.active_tab = Some(tab.id);
                    }
                }
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            let Some(active_id) = self.active_tab else {
                ui.label("No tab open");
                return;
            };

            let Some(tab) = self.tabs.iter_mut().find(|t| t.id == active_id) else {
                ui.label("Active tab missing");
                return;
            };

            match &mut tab.state {
                TabState::Client { counter } => {
                    ui.heading("Home");
                    if ui.button("Increment").clicked() {
                        *counter += 1;
                    }
                    ui.label(format!("Counter: {counter}"));
                }
            }
        });
    }
}
