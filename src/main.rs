#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use eframe::egui;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TabKind {
    Client,
}

#[derive(Clone, Debug)]
pub enum TabState {
    Client { mqtt_login: MqttLoginData },
}

#[derive(Clone, Debug, Default)]
pub struct MqttLoginData {
    pub broker: String,
    pub port: String,
    pub username: String,
    pub password: String,
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
    show_mqtt_popup: bool,
    mqtt_form: MqttLoginData,
}

impl App {
    fn new_tab(&mut self, kind: TabKind, mqtt_login: MqttLoginData) {
        let id = self.next_tab_id;
        self.next_tab_id += 1;

        let (title, state) = match kind {
            TabKind::Client => {
                let title = if mqtt_login.broker.is_empty() {
                    format!("Client {id}")
                } else {
                    mqtt_login.broker.clone()
                };
                (title, TabState::Client { mqtt_login })
            }
        };

        self.tabs.push(Tab {
            id,
            title,
            kind,
            state,
        });
        self.active_tab = Some(id);
    }

    fn close_tab(&mut self, tab_id: u64) {
        let Some(idx) = self.tabs.iter().position(|t| t.id == tab_id) else {
            return;
        };

        self.tabs.remove(idx);

        if self.active_tab == Some(tab_id) {
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
        let top_bar_fill = ctx.style().visuals.panel_fill;

        egui::TopBottomPanel::top("tab_bar")
            .exact_height(40.0)
            .frame(
                egui::Frame::new()
                    .fill(top_bar_fill)
                    .inner_margin(egui::Margin::symmetric(6, 5)),
            )
            .show(ctx, |ui| {
            let mut tab_to_activate = None;
            let mut tab_to_close = None;
            let mut add_tab = false;

            ui.horizontal(|ui| {
                ui.set_height(ui.available_height());
                ui.spacing_mut().item_spacing.x = 2.0;

                egui::ScrollArea::horizontal()
                    .id_salt("tabs_scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            for tab in &self.tabs {
                                let selected = self.active_tab == Some(tab.id);
                                let frame_fill = if selected {
                                    ui.visuals().selection.bg_fill
                                } else {
                                    ui.visuals().widgets.inactive.bg_fill
                                };
                                let frame_stroke = if selected {
                                    ui.visuals().selection.stroke
                                } else {
                                    ui.visuals().widgets.inactive.bg_stroke
                                };
                                let title_color = if selected {
                                    ui.visuals().selection.stroke.color
                                } else {
                                    ui.visuals().text_color()
                                };

                                egui::Frame::new()
                                    .fill(frame_fill)
                                    .stroke(frame_stroke)
                                    .corner_radius(2.0)
                                    .inner_margin(egui::Margin::symmetric(12, 7))
                                    .show(ui, |ui| {
                                    ui.spacing_mut().item_spacing.x = 8.0;

                                    let tab_response = ui.add(
                                        egui::Label::new(
                                            egui::RichText::new(&tab.title).color(title_color),
                                        )
                                        .sense(egui::Sense::click()),
                                    );
                                    if tab_response.clicked() {
                                        tab_to_activate = Some(tab.id);
                                    }

                                    if tab_response.hovered() || selected {
                                        let close_response = ui.add(
                                            egui::Button::new(
                                                egui::RichText::new("✕").small().strong(),
                                            )
                                            .small()
                                            .frame(false),
                                        );
                                        if close_response.clicked() {
                                            tab_to_close = Some(tab.id);
                                        }
                                    } else {
                                        ui.add_space(12.0);
                                    }
                                });
                            }
                        });
                    });

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    add_tab = ui
                        .add(
                            egui::Button::new(egui::RichText::new("+").strong())
                                .small()
                                .min_size(egui::vec2(26.0, 28.0)),
                        )
                        .clicked();
                });
            });

            if let Some(id) = tab_to_activate {
                self.active_tab = Some(id);
            }

            if let Some(id) = tab_to_close {
                self.close_tab(id);
            }

            if add_tab {
                self.show_mqtt_popup = true;
            }
        });

        if self.show_mqtt_popup {
            let mut open = self.show_mqtt_popup;
            let mut create_client = false;

            egui::Window::new("MQTT Login")
                .collapsible(false)
                .resizable(false)
                .open(&mut open)
                .show(ctx, |ui| {
                    ui.vertical(|ui| {
                        ui.label("Broker");
                        ui.text_edit_singleline(&mut self.mqtt_form.broker);

                        ui.label("Port");
                        ui.text_edit_singleline(&mut self.mqtt_form.port);

                        ui.label("Username");
                        ui.text_edit_singleline(&mut self.mqtt_form.username);

                        ui.label("Password");
                        ui.add(egui::TextEdit::singleline(&mut self.mqtt_form.password).password(true));

                        ui.add_space(8.0);
                        if ui.button("Add client").clicked() {
                            create_client = true;
                        }
                    });
                });

            if create_client {
                self.new_tab(TabKind::Client, self.mqtt_form.clone());
                self.mqtt_form = MqttLoginData::default();
                open = false;
            }

            self.show_mqtt_popup = open;
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            let Some(active_id) = self.active_tab else {
                ui.label("No client open. Press + to add an MQTT client.");
                return;
            };

            let Some(tab) = self.tabs.iter_mut().find(|t| t.id == active_id) else {
                ui.label("Active tab missing");
                return;
            };

            match &mut tab.state {
                TabState::Client { mqtt_login } => {
                    ui.heading("MQTT Login Data");
                    ui.label(format!("Broker: {}", mqtt_login.broker));
                    ui.label(format!("Port: {}", mqtt_login.port));
                    ui.label(format!("Username: {}", mqtt_login.username));
                    ui.label(format!("Password: {}", mqtt_login.password));
                }
            }
        });
    }
}
