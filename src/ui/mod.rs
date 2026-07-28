use eframe::egui;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use crate::app::App;
use crate::app::state::{
    ActionableError, ActivityLevel, ErrorScope, MessageFilterMode, PayloadView, TabKind, TabState,
};
use crate::models::ipc::{ClientCommand, ConnectionState};
use crate::models::mqtt::{
    ConnectionInputMode, MqttLoginData, ReceivedMessage, TlsVerificationMode, TransportKind,
    mqtt_topic_matches,
};
use crate::ui::widgets::qos_picker;
use crate::utils::formatting::{format_json, format_payload, format_timestamp};

pub(crate) mod widgets;

const LOGIN_DIALOG_WIDTH: f32 = 520.0;
const LOGIN_LABEL_WIDTH: f32 = 154.0;

fn form_row(ui: &mut egui::Ui, label: &str, add_control: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        ui.add_sized(
            [LOGIN_LABEL_WIDTH, 24.0],
            egui::Label::new(egui::RichText::new(label).color(ui.visuals().weak_text_color()))
                .sense(egui::Sense::hover()),
        );
        add_control(ui);
    });
}

fn text_form_row(ui: &mut egui::Ui, label: &str, value: &mut String) {
    form_row(ui, label, |ui| {
        ui.add(
            egui::TextEdit::singleline(value)
                .desired_width(ui.available_width())
                .margin(egui::Margin::symmetric(8, 6)),
        );
    });
}

fn connection_color(state: ConnectionState, visuals: &egui::Visuals) -> egui::Color32 {
    match state {
        ConnectionState::Connected => egui::Color32::from_rgb(70, 170, 90),
        ConnectionState::Failed => egui::Color32::from_rgb(220, 70, 70),
        ConnectionState::Connecting | ConnectionState::Reconnecting => {
            egui::Color32::from_rgb(230, 170, 40)
        }
        ConnectionState::Disconnecting => visuals.warn_fg_color,
        ConnectionState::Disconnected => visuals.weak_text_color(),
    }
}

fn disabled_reason(action: &str, state: ConnectionState) -> String {
    format!("{action} is unavailable while the connection is {state}.")
}

fn message_matches(
    message: &ReceivedMessage,
    topic_filter: &str,
    mode: MessageFilterMode,
    payload_search: &str,
) -> bool {
    let topic_filter = topic_filter.trim();
    let topic_matches = topic_filter.is_empty()
        || match mode {
            MessageFilterMode::Substring => message.topic.contains(topic_filter),
            MessageFilterMode::MqttTopic => mqtt_topic_matches(topic_filter, &message.topic),
        };
    let payload_search = payload_search.trim();
    topic_matches
        && (payload_search.is_empty()
            || std::str::from_utf8(&message.payload)
                .is_ok_and(|text| text.contains(payload_search)))
}

fn topic_color_for(topic: &str, visuals: &egui::Visuals) -> egui::Color32 {
    let palette = [
        visuals.selection.bg_fill,
        visuals.hyperlink_color,
        visuals.warn_fg_color,
        visuals.widgets.active.fg_stroke.color,
        visuals.widgets.hovered.fg_stroke.color,
        visuals.widgets.inactive.fg_stroke.color,
    ];

    let mut hasher = DefaultHasher::new();
    topic.hash(&mut hasher);
    let index = (hasher.finish() as usize) % palette.len();
    palette[index]
}

fn topic_label(ui: &mut egui::Ui, topic: &str, color: egui::Color32) -> egui::Response {
    if topic.is_empty() {
        return ui.add(
            egui::Label::new(egui::RichText::new("(empty)").weak()).sense(egui::Sense::click()),
        );
    }

    let wildcard_color = ui.visuals().warn_fg_color;
    let mut job = egui::text::LayoutJob::default();
    let mut first = true;
    for segment in topic.split('/') {
        if !first {
            job.append(
                "/",
                0.0,
                egui::TextFormat {
                    color,
                    ..Default::default()
                },
            );
        }
        first = false;

        let segment_color = if segment == "+" || segment == "#" {
            wildcard_color
        } else {
            color
        };

        job.append(
            segment,
            0.0,
            egui::TextFormat {
                color: segment_color,
                ..Default::default()
            },
        );
    }

    ui.add(egui::Label::new(job).sense(egui::Sense::click()))
}

pub(crate) fn render(app: &mut App, ui: &mut egui::Ui) {
    let ctx = ui.ctx().clone();
    let top_bar_fill = ui.style().visuals.panel_fill;

    if let Some(warning) = app.workspace_warning.clone() {
        egui::Panel::top("workspace_warning").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.colored_label(ui.visuals().warn_fg_color, warning);
                if ui.small_button("Dismiss").clicked() {
                    app.workspace_warning = None;
                }
            });
        });
    }

    egui::Panel::top("tab_bar")
        .exact_size(40.0)
        .frame(
            egui::Frame::new()
                .fill(top_bar_fill)
                .inner_margin(egui::Margin::symmetric(6, 5)),
        )
        .show(ui, |ui| {
            let mut tab_to_activate = None;
            let mut tab_to_close = None;
            let mut tab_to_disconnect = None;
            let mut tab_to_force_disconnect = None;
            let mut tab_to_reconnect = None;
            let mut tab_to_duplicate = None;
            let mut tab_to_rename: Option<(u64, String)> = None;
            let mut tab_reorder: Option<(u64, u64)> = None;
            let mut add_tab = false;

            ui.horizontal(|ui| {
                ui.set_height(ui.available_height());
                ui.spacing_mut().item_spacing.x = 2.0;

                egui::ScrollArea::horizontal()
                    .id_salt("tabs_scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            for tab in &app.tabs {
                                let tab_id = tab.id;
                                let tab_title = tab.title.clone();
                                let selected = app.active_tab == Some(tab.id);
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
                                let connection_state = match &tab.state {
                                    TabState::Client {
                                        connection_state, ..
                                    } => *connection_state,
                                };

                                egui::Frame::new()
                                    .fill(frame_fill)
                                    .stroke(frame_stroke)
                                    .corner_radius(2.0)
                                    .inner_margin(egui::Margin::symmetric(12, 7))
                                    .show(ui, |ui| {
                                        ui.spacing_mut().item_spacing.x = 8.0;
                                        ui.colored_label(
                                            connection_color(connection_state, ui.visuals()),
                                            "●",
                                        )
                                        .on_hover_text(
                                            format!("Connection state: {connection_state}"),
                                        );

                                        let tab_response = ui.add(
                                            egui::Label::new(
                                                egui::RichText::new(&tab.title).color(title_color),
                                            )
                                            .sense(egui::Sense::click_and_drag()),
                                        );
                                        if tab_response.clicked() {
                                            tab_to_activate = Some(tab_id);
                                        }

                                        if tab_response.drag_started() {
                                            app.dragging_tab = Some(tab_id);
                                        }

                                        if ui.input(|i| i.pointer.any_released())
                                            && app.dragging_tab.is_some()
                                            && tab_response.hovered()
                                            && let Some(source_id) = app.dragging_tab
                                            && source_id != tab_id
                                        {
                                            tab_reorder = Some((source_id, tab_id));
                                        }

                                        tab_response.context_menu(|ui| {
                                            let disconnect = ui
                                                .add_enabled(
                                                    connection_state.can_disconnect(),
                                                    egui::Button::new("Disconnect"),
                                                )
                                                .on_disabled_hover_text(disabled_reason(
                                                    "Disconnect",
                                                    connection_state,
                                                ));
                                            if disconnect.clicked() {
                                                tab_to_disconnect = Some(tab_id);
                                                ui.close();
                                            }
                                            let force = ui
                                                .add_enabled(
                                                    connection_state.can_force_disconnect(),
                                                    egui::Button::new("Force Disconnect"),
                                                )
                                                .on_disabled_hover_text(disabled_reason(
                                                    "Force Disconnect",
                                                    connection_state,
                                                ));
                                            if force.clicked() {
                                                tab_to_force_disconnect = Some(tab_id);
                                                ui.close();
                                            }
                                            let reconnect = ui
                                                .add_enabled(
                                                    connection_state.can_connect(),
                                                    egui::Button::new(
                                                        if connection_state
                                                            == ConnectionState::Disconnected
                                                        {
                                                            "Connect"
                                                        } else {
                                                            "Reconnect"
                                                        },
                                                    ),
                                                )
                                                .on_disabled_hover_text(disabled_reason(
                                                    "Connect/Reconnect",
                                                    connection_state,
                                                ));
                                            if reconnect.clicked() {
                                                tab_to_reconnect = Some(tab_id);
                                                ui.close();
                                            }
                                            ui.separator();
                                            if ui.button("Close Tab").clicked() {
                                                tab_to_close = Some(tab_id);
                                                ui.close();
                                            }
                                            if ui.button("Duplicate Tab").clicked() {
                                                tab_to_duplicate = Some(tab_id);
                                                ui.close();
                                            }
                                            if ui.button("Rename Tab").clicked() {
                                                tab_to_rename = Some((tab_id, tab_title.clone()));
                                                ui.close();
                                            }
                                        });

                                        if tab_response.hovered() || selected {
                                            let close_response = ui.add(
                                                egui::Button::new(
                                                    egui::RichText::new("x").small().strong(),
                                                )
                                                .small()
                                                .frame(false),
                                            );
                                            if close_response.clicked() {
                                                tab_to_close = Some(tab_id);
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
                app.active_tab = Some(id);
            }

            if ui.input(|i| i.pointer.any_released()) {
                app.dragging_tab = None;
            }

            if let Some((source_id, target_id)) = tab_reorder {
                app.reorder_tabs(source_id, target_id);
            }

            if let Some(id) = tab_to_close {
                app.close_tab(id);
            }

            if let Some(id) = tab_to_disconnect {
                app.disconnect_client(id);
            }

            if let Some(id) = tab_to_force_disconnect {
                app.force_disconnect_client(id);
            }

            if let Some(id) = tab_to_reconnect {
                app.reconnect_client(id);
            }

            if let Some(id) = tab_to_duplicate {
                app.duplicate_tab(id);
            }

            if let Some((id, title)) = tab_to_rename {
                app.renaming_tab = Some(id);
                app.rename_buffer = title;
            }

            if add_tab {
                app.show_mqtt_popup = true;
            }
        });

    if let Some(tab_id) = app.renaming_tab {
        let mut open = true;
        let mut save = false;
        let mut cancel_clicked = false;

        egui::Window::new("Rename Tab")
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(&ctx, |ui| {
                ui.label("Title");
                let response = ui.text_edit_singleline(&mut app.rename_buffer);
                if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    save = true;
                }

                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        cancel_clicked = true;
                    }
                    if ui.button("Save").clicked() {
                        save = true;
                    }
                });
            });

        if cancel_clicked {
            open = false;
        }

        if save {
            app.rename_tab(tab_id, app.rename_buffer.clone());
            app.renaming_tab = None;
            app.rename_buffer.clear();
        } else if !open {
            app.renaming_tab = None;
            app.rename_buffer.clear();
        }
    }

    if app.show_mqtt_popup {
        let mut open = app.show_mqtt_popup;
        let mut create_client = false;
        let mut save_profile = false;
        let mut request_overwrite = false;
        let mut profile_to_load: Option<String> = None;
        let mut load_template = false;
        let mut rename_profile = false;
        let mut delete_profile = false;
        let mut export_profile = false;
        let mut cancel_dialog = false;

        egui::Window::new("New MQTT client")
            .collapsible(false)
            .resizable(false)
            .default_width(LOGIN_DIALOG_WIDTH)
            .min_width(LOGIN_DIALOG_WIDTH)
            .open(&mut open)
            .show(&ctx, |ui| {
                ui.spacing_mut().item_spacing = egui::vec2(10.0, 8.0);
                ui.spacing_mut().button_padding = egui::vec2(12.0, 7.0);

                ui.vertical(|ui| {
                    let max_form_height = (ctx.content_rect().height() - 180.0).clamp(220.0, 600.0);
                    egui::ScrollArea::vertical()
                        .id_salt("mqtt_login_form_scroll")
                        .max_height(max_form_height)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            if let Some(status) = &app.profile_status {
                                egui::Frame::new()
                                    .fill(ui.visuals().widgets.inactive.bg_fill)
                                    .corner_radius(5.0)
                                    .inner_margin(egui::Margin::symmetric(10, 8))
                                    .show(ui, |ui| {
                                        ui.set_width(ui.available_width());
                                        ui.label(status);
                                    });
                            }

                            ui.label(
                                egui::RichText::new(
                                    "Set up a broker connection and save it for later.",
                                )
                                .color(ui.visuals().weak_text_color()),
                            );
                            ui.add_space(4.0);
                            text_form_row(ui, "Connection name", &mut app.mqtt_form.name);
                            ui.add_space(2.0);

                            egui::CollapsingHeader::new("Connection")
                                .default_open(true)
                                .show(ui, |ui| {
                                    ui.add_space(4.0);
                                    form_row(ui, "Connection mode", |ui| {
                                        egui::ComboBox::from_id_salt("connection_mode")
                                            .width(ui.available_width())
                                            .selected_text(app.mqtt_form.connection_mode.label())
                                            .show_ui(ui, |ui| {
                                                ui.selectable_value(
                                                    &mut app.mqtt_form.connection_mode,
                                                    ConnectionInputMode::Structured,
                                                    ConnectionInputMode::Structured.label(),
                                                );
                                                ui.selectable_value(
                                                    &mut app.mqtt_form.connection_mode,
                                                    ConnectionInputMode::Url,
                                                    ConnectionInputMode::Url.label(),
                                                );
                                            });
                                    });

                                    match app.mqtt_form.connection_mode {
                                        ConnectionInputMode::Structured => {
                                            text_form_row(ui, "Broker", &mut app.mqtt_form.broker);
                                            text_form_row(ui, "Port", &mut app.mqtt_form.port);

                                            form_row(ui, "Transport", |ui| {
                                                egui::ComboBox::from_id_salt("transport_kind")
                                                    .width(ui.available_width())
                                                    .selected_text(app.mqtt_form.transport.label())
                                                    .show_ui(ui, |ui| {
                                                        for transport in [
                                                            TransportKind::Tcp,
                                                            TransportKind::Tls,
                                                            TransportKind::Ws,
                                                            TransportKind::Wss,
                                                        ] {
                                                            ui.selectable_value(
                                                                &mut app.mqtt_form.transport,
                                                                transport,
                                                                transport.label(),
                                                            );
                                                        }
                                                    });
                                            });

                                            if app.mqtt_form.transport.uses_websocket() {
                                                text_form_row(
                                                    ui,
                                                    "WebSocket path",
                                                    &mut app.mqtt_form.ws_path,
                                                );
                                            }
                                        }
                                        ConnectionInputMode::Url => {
                                            text_form_row(
                                                ui,
                                                "Connection URL",
                                                &mut app.mqtt_form.connection_url,
                                            );

                                            if !app.mqtt_form.connection_url.trim().is_empty()
                                                && let Err(err) = app.mqtt_form.resolve_connection()
                                            {
                                                ui.colored_label(ui.visuals().warn_fg_color, err);
                                            }
                                        }
                                    }

                                    let active_transport = match app.mqtt_form.connection_mode {
                                        ConnectionInputMode::Structured => {
                                            Some(app.mqtt_form.transport)
                                        }
                                        ConnectionInputMode::Url => app
                                            .mqtt_form
                                            .resolve_connection()
                                            .ok()
                                            .map(|resolved| resolved.transport),
                                    };

                                    if matches!(
                                        active_transport,
                                        Some(TransportKind::Tls | TransportKind::Wss)
                                    ) {
                                        ui.separator();
                                        form_row(ui, "TLS verification", |ui| {
                                            egui::ComboBox::from_id_salt("tls_verification")
                                                .width(ui.available_width())
                                                .selected_text(
                                                    app.mqtt_form.tls_verification.label(),
                                                )
                                                .show_ui(ui, |ui| {
                                                    for mode in [
                                                        TlsVerificationMode::SystemRoots,
                                                        TlsVerificationMode::CustomCa,
                                                        TlsVerificationMode::InsecureSkipVerify,
                                                    ] {
                                                        ui.selectable_value(
                                                            &mut app.mqtt_form.tls_verification,
                                                            mode,
                                                            mode.label(),
                                                        );
                                                    }
                                                });
                                        });

                                        if app.mqtt_form.tls_verification
                                            == TlsVerificationMode::CustomCa
                                        {
                                            form_row(ui, "CA certificate", |ui| {
                                                ui.add(
                                                    egui::TextEdit::singleline(
                                                        &mut app.mqtt_form.tls_ca_cert_path,
                                                    )
                                                    .desired_width(ui.available_width() - 88.0),
                                                );
                                                if ui.button("Browse...").clicked()
                                                    && let Some(path) = rfd::FileDialog::new()
                                                        .add_filter("PEM", &["pem", "crt", "cer"])
                                                        .pick_file()
                                                {
                                                    app.mqtt_form.tls_ca_cert_path =
                                                        path.display().to_string();
                                                }
                                            });
                                        }

                                        if app.mqtt_form.tls_verification
                                            == TlsVerificationMode::InsecureSkipVerify
                                        {
                                            ui.colored_label(
                                        ui.visuals().warn_fg_color,
                                        "Certificate verification is disabled for this connection.",
                                    );
                                        }
                                    }

                                    ui.separator();
                                    form_row(ui, "Keep alive", |ui| {
                                        ui.add(
                                            egui::DragValue::new(
                                                &mut app.mqtt_form.keep_alive_secs,
                                            )
                                            .range(1..=u16::MAX),
                                        );
                                        ui.weak("seconds");
                                    });
                                    form_row(ui, "Reconnect", |ui| {
                                        ui.checkbox(
                                            &mut app.mqtt_form.automatic_reconnect,
                                            "Automatically reconnect",
                                        );
                                    });
                                    if app.mqtt_form.automatic_reconnect {
                                        form_row(ui, "Maximum delay", |ui| {
                                            ui.add(
                                                egui::DragValue::new(
                                                    &mut app.mqtt_form.reconnect_max_delay_secs,
                                                )
                                                .range(1..=u16::MAX),
                                            );
                                            ui.weak("seconds");
                                        });
                                    }

                                    text_form_row(
                                        ui,
                                        "Client ID (optional)",
                                        &mut app.mqtt_form.client_id,
                                    );
                                    ui.add_space(2.0);
                                });

                            egui::CollapsingHeader::new("Login credentials")
                                .default_open(false)
                                .show(ui, |ui| {
                                    ui.add_space(4.0);
                                    text_form_row(ui, "Username", &mut app.mqtt_form.username);
                                    form_row(ui, "Password", |ui| {
                                        ui.add(
                                            egui::TextEdit::singleline(&mut app.mqtt_form.password)
                                                .password(true)
                                                .desired_width(ui.available_width())
                                                .margin(egui::Margin::symmetric(8, 6)),
                                        );
                                    });
                                    ui.add_space(2.0);
                                });

                            egui::CollapsingHeader::new("Testament")
                                .default_open(false)
                                .show(ui, |ui| {
                                    ui.add_space(4.0);
                                    text_form_row(ui, "Topic", &mut app.mqtt_form.testament_topic);

                                    form_row(ui, "Delivery", |ui| {
                                        ui.weak("QoS");
                                        ui.add(
                                            egui::DragValue::new(&mut app.mqtt_form.testament_qos)
                                                .range(0..=2),
                                        );
                                        ui.checkbox(&mut app.mqtt_form.testament_retain, "Retain");
                                    });

                                    text_form_row(
                                        ui,
                                        "Last-will message",
                                        &mut app.mqtt_form.testament_and_last_will,
                                    );
                                    ui.add_space(2.0);
                                });
                        });

                    ui.add_space(10.0);
                    ui.separator();
                    ui.add_space(2.0);
                    ui.horizontal(|ui| {
                        let selected_profile_text = app
                            .selected_profile_id
                            .as_ref()
                            .and_then(|id| app.profile_entries.iter().find(|entry| &entry.id == id))
                            .map_or("Load configuration", |entry| entry.display_name.as_str());

                        egui::ComboBox::from_id_salt("mqtt_config_picker")
                            .width(190.0)
                            .selected_text(selected_profile_text)
                            .show_ui(ui, |ui| {
                                for entry in &app.profile_entries {
                                    let selected = app
                                        .selected_profile_id
                                        .as_ref()
                                        .is_some_and(|current| current == &entry.id);
                                    let label = if entry.warning.is_some() {
                                        format!("⚠ {}", entry.display_name)
                                    } else {
                                        entry.display_name.clone()
                                    };
                                    let response = ui.selectable_label(selected, label);
                                    if let Some(warning) = &entry.warning {
                                        response.clone().on_hover_text(warning);
                                    }
                                    if response.clicked() {
                                        profile_to_load = Some(entry.id.clone());
                                        ui.close();
                                    }
                                }

                                ui.separator();
                                if ui
                                    .selectable_label(false, "Load template from file...")
                                    .clicked()
                                {
                                    load_template = true;
                                    ui.close();
                                }
                            });
                        if ui.button("Save as new").clicked() {
                            save_profile = true;
                        }

                        ui.menu_button("Manage", |ui| {
                            let selected = app.selected_profile_id.is_some();
                            if ui
                                .add_enabled(
                                    selected,
                                    egui::Button::new("Overwrite selected profile"),
                                )
                                .clicked()
                            {
                                request_overwrite = true;
                                ui.close();
                            }
                            if ui
                                .add_enabled(selected, egui::Button::new("Rename..."))
                                .clicked()
                            {
                                rename_profile = true;
                                ui.close();
                            }
                            if ui
                                .add_enabled(selected, egui::Button::new("Delete..."))
                                .clicked()
                            {
                                delete_profile = true;
                                ui.close();
                            }
                            if ui
                                .add_enabled(selected, egui::Button::new("Export..."))
                                .clicked()
                            {
                                export_profile = true;
                                ui.close();
                            }
                        });
                    });

                    ui.add_space(4.0);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let primary = egui::Button::new(
                            egui::RichText::new("Add client")
                                .strong()
                                .color(egui::Color32::WHITE),
                        )
                        .fill(ui.visuals().selection.bg_fill)
                        .min_size(egui::vec2(108.0, 32.0));
                        if ui.add(primary).clicked() {
                            create_client = true;
                        }
                        if ui.button("Cancel").clicked() {
                            cancel_dialog = true;
                        }
                    });
                });
            });

        if cancel_dialog {
            open = false;
        }
        if save_profile {
            app.save_current_profile(false);
        }
        if request_overwrite {
            app.confirm_profile_overwrite = true;
        }

        if let Some(profile_id) = profile_to_load {
            app.load_profile_into_form(&profile_id);
        }

        if load_template {
            app.load_template_from_file_picker();
        }
        if rename_profile
            && let Some(entry) = app
                .selected_profile_id
                .as_ref()
                .and_then(|id| app.profile_entries.iter().find(|entry| &entry.id == id))
        {
            app.profile_rename_buffer.clone_from(&entry.display_name);
            app.profile_rename_open = true;
        }
        if delete_profile {
            app.confirm_profile_delete = true;
        }
        if export_profile {
            app.export_selected_profile();
        }

        if app.confirm_profile_overwrite {
            let mut visible = true;
            egui::Window::new("Overwrite profile?")
                .collapsible(false)
                .resizable(false)
                .open(&mut visible)
                .show(&ctx, |ui| {
                    ui.label("Replace the selected profile with the current form values?");
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            app.confirm_profile_overwrite = false;
                        }
                        if ui.button("Overwrite").clicked() {
                            app.save_current_profile(true);
                        }
                    });
                });
            if !visible {
                app.confirm_profile_overwrite = false;
            }
        }

        if app.profile_rename_open {
            let mut visible = true;
            egui::Window::new("Rename profile")
                .collapsible(false)
                .resizable(false)
                .open(&mut visible)
                .show(&ctx, |ui| {
                    ui.text_edit_singleline(&mut app.profile_rename_buffer);
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            app.profile_rename_open = false;
                        }
                        if ui.button("Rename").clicked() {
                            app.rename_selected_profile();
                        }
                    });
                });
            if !visible {
                app.profile_rename_open = false;
            }
        }

        if app.confirm_profile_delete {
            let mut visible = true;
            egui::Window::new("Delete profile?")
                .collapsible(false)
                .resizable(false)
                .open(&mut visible)
                .show(&ctx, |ui| {
                    ui.label("This permanently deletes the selected profile.");
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            app.confirm_profile_delete = false;
                        }
                        if ui.button("Delete").clicked() {
                            app.delete_selected_profile();
                        }
                    });
                });
            if !visible {
                app.confirm_profile_delete = false;
            }
        }

        if create_client {
            match app.mqtt_form.resolve_connection() {
                Ok(_) => {
                    app.new_tab(TabKind::Client, app.mqtt_form.clone());
                    app.mqtt_form = MqttLoginData::default();
                    app.profile_status = None;
                    open = false;
                }
                Err(err) => {
                    app.profile_status = Some(err);
                }
            }
        }

        app.show_mqtt_popup = open;
    }

    egui::CentralPanel::default().show(ui, |ui| {
        let Some(active_id) = app.active_tab else {
            ui.label("No client open. Press + to add an MQTT client.");
            return;
        };

        let Some(tab) = app.tabs.iter_mut().find(|t| t.id == active_id) else {
            ui.label("Active tab missing");
            return;
        };

        let mut commands_to_send: Vec<ClientCommand> = Vec::new();
        let reconnect_attempt = app.reconnect_attempts.get(&active_id).copied();
        let retry_remaining = app
            .reconnect_deadlines
            .get(&active_id)
            .map(|deadline| deadline.saturating_duration_since(std::time::Instant::now()));
        let mut disconnect_clicked = false;
        let mut force_disconnect_clicked = false;
        let mut reconnect_clicked = false;
        let mut cancel_reconnect_clicked = false;
        let main_viewport_height = ui.available_height();

        egui::ScrollArea::vertical()
            .id_salt(("main_workspace_scroll", active_id))
            .auto_shrink([false, false])
            .show(ui, |ui| match &mut tab.state {
                TabState::Client {
                mqtt_login,
                connection_state,
                current_error,
                activity,
                subscribe_topic,
                subscribe_qos,
                unsubscribe_topic,
                editing_subscription_topic,
                editing_subscription_value,
                editing_subscription_qos,
                publish_topic,
                publish_qos,
                publish_retain,
                publish_payload,
                payload_view,
                topic_filter,
                message_filter_mode,
                payload_search,
                max_messages,
                capture_paused,
                paused_message_count,
                selected_message_id,
                subscriptions,
                messages,
                received_count,
                dropped_message_count,
                published_count,
                ..
                } => {
                ui.heading("MQTT Client");
                ui.label(format!("Broker / transport: {}", mqtt_login.display_connection_label()));
                let status_color = connection_color(*connection_state, ui.visuals());
                ui.colored_label(status_color, format!("Status: {connection_state}"));
                if let Some(attempt) = reconnect_attempt {
                    ui.label(format!("Reconnect attempt: {attempt}"));
                }
                if let Some(remaining) = retry_remaining {
                    ui.label(format!("Next retry in: {:.1}s", remaining.as_secs_f32()));
                }
                ui.horizontal(|ui| {
                    let reconnect = ui
                        .add_enabled(
                            connection_state.can_connect(),
                            egui::Button::new(if *connection_state
                                == ConnectionState::Disconnected
                            {
                                "Connect"
                            } else {
                                "Reconnect"
                            }),
                        )
                        .on_disabled_hover_text(disabled_reason(
                            "Connect/Reconnect",
                            *connection_state,
                        ));
                    reconnect_clicked = reconnect.clicked();
                    let disconnect = ui
                        .add_enabled(
                            connection_state.can_disconnect(),
                            egui::Button::new("Disconnect"),
                        )
                        .on_disabled_hover_text(disabled_reason(
                            "Disconnect",
                            *connection_state,
                        ));
                    disconnect_clicked = disconnect.clicked();
                    let force = ui
                        .add_enabled(
                            connection_state.can_force_disconnect(),
                            egui::Button::new("Force Disconnect"),
                        )
                        .on_disabled_hover_text(disabled_reason(
                            "Force Disconnect",
                            *connection_state,
                        ));
                    force_disconnect_clicked = force.clicked();
                    let cancel = ui
                        .add_enabled(
                            connection_state.can_cancel_reconnect(),
                            egui::Button::new("Cancel reconnect"),
                        )
                        .on_disabled_hover_text(disabled_reason(
                            "Cancel reconnect",
                            *connection_state,
                        ));
                    cancel_reconnect_clicked = cancel.clicked();
                });
                if let Some(err) = current_error.as_ref() {
                    let message = err.message.clone();
                    let mut dismiss = false;
                    ui.horizontal(|ui| {
                        ui.colored_label(
                            egui::Color32::from_rgb(220, 70, 70),
                            format!("Error: {message}"),
                        );
                        dismiss = ui.small_button("Dismiss").clicked();
                    });
                    if dismiss {
                        *current_error = None;
                    }
                }
                if !activity.is_empty() {
                    egui::CollapsingHeader::new(format!("Recent activity ({})", activity.len()))
                        .default_open(true)
                        .show(ui, |ui| {
                            egui::Frame::group(ui.style())
                                .inner_margin(egui::Margin::symmetric(10, 8))
                                .show(ui, |ui| {
                                    ui.set_width(ui.available_width());
                                    egui::ScrollArea::vertical()
                                        .id_salt(("recent_activity_scroll", active_id))
                                        .max_height(140.0)
                                        .auto_shrink([false, true])
                                        .show(ui, |ui| {
                                            for item in activity.iter() {
                                                let (label, color) = match item.level {
                                                    ActivityLevel::Info => {
                                                        ("Info", ui.visuals().text_color())
                                                    }
                                                    ActivityLevel::Success => (
                                                        "Success",
                                                        egui::Color32::from_rgb(70, 170, 90),
                                                    ),
                                                    ActivityLevel::Warning => {
                                                        ("Warning", ui.visuals().warn_fg_color)
                                                    }
                                                };
                                                ui.colored_label(
                                                    color,
                                                    format!(
                                                        "{} · {label}: {}",
                                                        format_timestamp(item.timestamp),
                                                        item.message
                                                    ),
                                                );
                                            }
                                        });
                                });
                        });
                }
                ui.label(format!(
                    "Totals: {} received / {} published",
                    received_count, published_count
                ));
                if *dropped_message_count > 0 {
                    ui.colored_label(
                        ui.visuals().warn_fg_color,
                        format!("{dropped_message_count} messages dropped"),
                    );
                }

                ui.separator();
                ui.heading("Subscriptions");
                ui.horizontal(|ui| {
                    ui.label("Topic");
                    ui.text_edit_singleline(subscribe_topic);
                    ui.label("QoS");
                    qos_picker(ui, &format!("sub_qos_{active_id}"), subscribe_qos);
                    let subscribe = ui
                        .add_enabled(
                            connection_state.can_use_client(),
                            egui::Button::new("Subscribe"),
                        )
                        .on_disabled_hover_text(disabled_reason(
                            "Subscribe",
                            *connection_state,
                        ));
                    if subscribe.clicked() {
                        let topic = subscribe_topic.trim().to_string();
                        if !topic.is_empty() {
                            commands_to_send.push(ClientCommand::Subscribe {
                                topic: topic.clone(),
                                qos: *subscribe_qos,
                            });
                            *unsubscribe_topic = topic;
                        }
                    }
                });

                let mut remove_topic: Option<String> = None;
                let mut edit_topic: Option<(String, u8)> = None;
                egui::ScrollArea::vertical()
                    .id_salt(("subscriptions_scroll", active_id))
                    .max_height(120.0)
                    .show(ui, |ui| {
                        if subscriptions.is_empty() {
                            ui.label("No active subscriptions");
                        } else {
                            for entry in subscriptions.iter() {
                                ui.push_id((&entry.topic, entry.qos), |ui| {
                                    let (topic_response, qos_response) = ui
                                        .horizontal(|ui| {
                                            let color = topic_color_for(&entry.topic, ui.visuals());
                                            let topic_response =
                                                topic_label(ui, &entry.topic, color);
                                            let qos_response = ui.add(
                                                egui::Label::new(format!("(QoS {})", entry.qos))
                                                    .sense(egui::Sense::click()),
                                            );
                                            let remove = ui
                                                .add_enabled(
                                                    connection_state.can_use_client(),
                                                    egui::Button::new("Remove").small(),
                                                )
                                                .on_disabled_hover_text(disabled_reason(
                                                    "Unsubscribe",
                                                    *connection_state,
                                                ));
                                            if remove.clicked() {
                                                remove_topic = Some(entry.topic.clone());
                                            }
                                            (topic_response, qos_response)
                                        })
                                        .inner;

                                    let row_response = topic_response.union(qos_response);

                                    row_response.context_menu(|ui| {
                                        if ui.button("Edit Subscription").clicked() {
                                            edit_topic = Some((entry.topic.clone(), entry.qos));
                                            ui.close();
                                        }
                                        if ui
                                            .add_enabled(
                                                connection_state.can_use_client(),
                                                egui::Button::new("Unsubscribe"),
                                            )
                                            .on_disabled_hover_text(disabled_reason(
                                                "Unsubscribe",
                                                *connection_state,
                                            ))
                                            .clicked()
                                        {
                                            remove_topic = Some(entry.topic.clone());
                                            ui.close();
                                        }
                                    });
                                });
                            }
                        }
                    });
                if let Some((topic, qos)) = edit_topic {
                    *editing_subscription_topic = Some(topic.clone());
                    *editing_subscription_value = topic;
                    *editing_subscription_qos = qos;
                }
                if let Some(topic) = remove_topic {
                    commands_to_send.push(ClientCommand::Unsubscribe {
                        topic: topic.clone(),
                    });
                    *unsubscribe_topic = topic;
                }

                if let Some(original_topic) = editing_subscription_topic.clone() {
                    let mut open = true;
                    let mut apply = false;
                    let mut cancel_clicked = false;

                    egui::Window::new("Edit Subscription")
                        .collapsible(false)
                        .resizable(false)
                        .open(&mut open)
                        .show(&ctx, |ui| {
                            ui.label("Topic");
                            let response = ui.text_edit_singleline(editing_subscription_value);
                            if response.lost_focus()
                                && ui.input(|i| i.key_pressed(egui::Key::Enter))
                            {
                                apply = true;
                            }

                            ui.horizontal(|ui| {
                                ui.label("QoS");
                                qos_picker(
                                    ui,
                                    &format!("edit_sub_qos_{active_id}"),
                                    editing_subscription_qos,
                                );
                            });

                            ui.horizontal(|ui| {
                                if ui.button("Cancel").clicked() {
                                    cancel_clicked = true;
                                }
                                if ui.button("Apply").clicked() {
                                    apply = true;
                                }
                            });
                        });

                    if cancel_clicked {
                        open = false;
                    }

                    if apply {
                        let new_topic = editing_subscription_value.trim().to_string();
                        if new_topic.is_empty() {
                            *current_error = Some(ActionableError {
                                message: "Subscription topic cannot be empty".to_string(),
                                scope: ErrorScope::Subscribe,
                            });
                        } else {
                            let mut changed = new_topic != original_topic;
                            if !changed
                                && let Some(existing) = subscriptions
                                    .iter()
                                    .find(|entry| entry.topic == original_topic)
                            {
                                changed = existing.qos != *editing_subscription_qos;
                            }

                            if changed {
                                commands_to_send.push(ClientCommand::Unsubscribe {
                                    topic: original_topic.clone(),
                                });
                                commands_to_send.push(ClientCommand::Subscribe {
                                    topic: new_topic.clone(),
                                    qos: *editing_subscription_qos,
                                });
                                *unsubscribe_topic = original_topic;
                                *subscribe_topic = new_topic;
                                *subscribe_qos = *editing_subscription_qos;
                            }

                            *editing_subscription_topic = None;
                            editing_subscription_value.clear();
                        }
                    } else if !open {
                        *editing_subscription_topic = None;
                        editing_subscription_value.clear();
                    }
                }

                ui.separator();
                ui.heading("Publish");
                ui.horizontal(|ui| {
                    ui.label("Topic");
                    ui.text_edit_singleline(publish_topic);
                    ui.label("QoS");
                    qos_picker(ui, &format!("pub_qos_{active_id}"), publish_qos);
                    ui.checkbox(publish_retain, "Retain");
                });
                ui.label("Payload");
                ui.add(egui::TextEdit::multiline(publish_payload).desired_rows(3));
                let publish = ui
                    .add_enabled(
                        connection_state.can_use_client(),
                        egui::Button::new("Publish message"),
                    )
                    .on_disabled_hover_text(disabled_reason("Publish", *connection_state));
                if publish.clicked() {
                    let topic = publish_topic.trim().to_string();
                    if !topic.is_empty() {
                        commands_to_send.push(ClientCommand::Publish {
                            topic,
                            payload: publish_payload.as_bytes().to_vec(),
                            qos: *publish_qos,
                            retain: *publish_retain,
                        });
                    }
                }

                ui.separator();
                ui.heading("Messages");
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .button(if *capture_paused {
                            "Resume capture"
                        } else {
                            "Pause capture"
                        })
                        .on_hover_text(
                            "Paused capture still counts incoming messages, but does not store them",
                        )
                        .clicked()
                    {
                        *capture_paused = !*capture_paused;
                    }
                    let status = if *capture_paused { "Paused" } else { "Capturing" };
                    ui.label(format!("{status} · {paused_message_count} skipped while paused"));
                    ui.label("Buffer limit");
                    ui.add(egui::DragValue::new(max_messages).range(1..=1000));
                    if ui
                        .button("Clear")
                        .on_hover_text("Clear the visible buffer and selection; totals are preserved")
                        .clicked()
                    {
                        messages.clear();
                        *selected_message_id = None;
                    }
                });
                while messages.len() > *max_messages {
                    let _ = messages.pop_front();
                }
                if selected_message_id.is_some_and(|selected| {
                    !messages.iter().any(|message| message.id == selected)
                }) {
                    *selected_message_id = None;
                }
                ui.horizontal_wrapped(|ui| {
                    ui.label("Topic filter");
                    ui.text_edit_singleline(topic_filter);
                    egui::ComboBox::from_id_salt(("message_filter_mode", active_id))
                        .selected_text(match message_filter_mode {
                            MessageFilterMode::Substring => "Substring",
                            MessageFilterMode::MqttTopic => "MQTT filter",
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                message_filter_mode,
                                MessageFilterMode::Substring,
                                "Substring",
                            );
                            ui.selectable_value(
                                message_filter_mode,
                                MessageFilterMode::MqttTopic,
                                "MQTT filter (+ and #)",
                            );
                        });
                    ui.label("Payload text");
                    ui.text_edit_singleline(payload_search)
                        .on_hover_text("Searches UTF-8 payloads only");
                });

                let inspector_height = (main_viewport_height - 160.0).max(220.0);
                let selected_for_frame = *selected_message_id;
                let mut next_selection = selected_for_frame;
                let mut list = |ui: &mut egui::Ui| {
                    ui.strong("Message list");
                    egui::ScrollArea::vertical()
                        .id_salt(("messages_scroll", active_id))
                        .max_height(inspector_height)
                        .show(ui, |ui| {
                            let mut shown = 0usize;
                            for msg in messages.iter().rev() {
                                if shown >= *max_messages
                                    || !message_matches(
                                        msg,
                                        topic_filter,
                                        *message_filter_mode,
                                        payload_search,
                                    )
                                {
                                    continue;
                                }
                                let selected = selected_for_frame == Some(msg.id);
                                let summary = format!(
                                    "{}  {}  Q{}{}  {} B\n{}",
                                    format_timestamp(msg.timestamp),
                                    msg.topic,
                                    msg.qos,
                                    if msg.retain { " R" } else { "" },
                                    msg.payload.len(),
                                    msg.preview
                                );
                                if ui
                                    .selectable_label(selected, summary)
                                    .on_hover_text("Select to inspect this message")
                                    .clicked()
                                {
                                    next_selection = Some(msg.id);
                                }
                                shown += 1;
                            }
                            if shown == 0 {
                                ui.label("No messages matched the current filters.");
                            }
                        });
                };

                let mut detail = |ui: &mut egui::Ui| {
                    ui.strong("Selected message");
                    let Some(message) = selected_for_frame
                        .and_then(|id| messages.iter().find(|message| message.id == id))
                    else {
                        ui.label("Select a message from the list.");
                        return;
                    };
                    ui.horizontal_wrapped(|ui| {
                        ui.label(format_timestamp(message.timestamp));
                        ui.label(format!("QoS {}", message.qos));
                        ui.label(if message.retain {
                            "Retained"
                        } else {
                            "Not retained"
                        });
                        ui.label(format!("{} bytes", message.payload.len()));
                    });
                    ui.horizontal_wrapped(|ui| {
                        let color = topic_color_for(&message.topic, ui.visuals());
                        topic_label(ui, &message.topic, color);
                        if ui.button("Copy topic").clicked() {
                            ui.ctx().copy_text(message.topic.clone());
                        }
                        if ui.button("Copy payload").clicked() {
                            ui.ctx().copy_text(
                                std::str::from_utf8(&message.payload)
                                    .map(str::to_owned)
                                    .unwrap_or_else(|_| format_payload(&message.payload, true)),
                            );
                        }
                        if ui.button("Save payload…").clicked()
                            && let Some(path) = rfd::FileDialog::new().save_file()
                            && let Err(error) = std::fs::write(&path, &message.payload)
                        {
                            *current_error = Some(ActionableError {
                                message: format!(
                                    "Could not save payload to {}: {error}",
                                    path.display()
                                ),
                                scope: ErrorScope::General,
                            });
                        }
                        if ui
                            .button("Use for publish")
                            .on_hover_text("Populate the publish form without sending")
                            .clicked()
                        {
                            *publish_topic = message.topic.clone();
                            *publish_qos = message.qos;
                            *publish_retain = message.retain;
                            *publish_payload =
                                String::from_utf8_lossy(&message.payload).into_owned();
                        }
                    });
                    let pretty_json = format_json(&message.payload);
                    ui.horizontal(|ui| {
                        ui.selectable_value(payload_view, PayloadView::Text, "Text");
                        ui.selectable_value(payload_view, PayloadView::Hex, "Hex");
                        let json_valid = pretty_json.is_some();
                        ui.add_enabled_ui(json_valid, |ui| {
                            ui.selectable_value(payload_view, PayloadView::Json, "Pretty JSON");
                        });
                        if !json_valid && *payload_view == PayloadView::Json {
                            *payload_view = PayloadView::Text;
                        }
                    });
                    let payload_text = match payload_view {
                        PayloadView::Text => std::str::from_utf8(&message.payload)
                            .map(str::to_owned)
                            .unwrap_or_else(|_| "Payload is not valid UTF-8. Use Hex.".to_string()),
                        PayloadView::Hex => format_payload(&message.payload, true),
                        PayloadView::Json => pretty_json.unwrap_or_default(),
                    };
                    egui::ScrollArea::both()
                        .id_salt(("message_detail", active_id))
                        .max_height(inspector_height - 80.0)
                        .show(ui, |ui| {
                            ui.add(
                                egui::TextEdit::multiline(&mut payload_text.as_str())
                                    .font(egui::TextStyle::Monospace)
                                    .desired_width(f32::INFINITY),
                            );
                        });
                };

                if ui.available_width() >= 700.0 {
                    ui.columns(2, |columns| {
                        list(&mut columns[0]);
                        detail(&mut columns[1]);
                    });
                } else {
                    list(ui);
                    ui.separator();
                    detail(ui);
                }
                *selected_message_id = next_selection;
                }
            });

        for command in commands_to_send {
            app.send_client_command(active_id, command);
        }
        if reconnect_clicked {
            app.reconnect_client(active_id);
        } else if cancel_reconnect_clicked {
            app.cancel_reconnect(active_id);
        } else if force_disconnect_clicked {
            app.force_disconnect_client(active_id);
        } else if disconnect_clicked {
            app.disconnect_client(active_id);
        }
    });
}

#[cfg(test)]
mod inspector_tests {
    use super::*;
    use std::time::SystemTime;

    fn message(topic: &str, payload: &[u8]) -> ReceivedMessage {
        ReceivedMessage::new(
            1,
            SystemTime::now(),
            topic.into(),
            0,
            false,
            payload.to_vec(),
        )
    }

    #[test]
    fn filters_topic_and_payload_text() {
        let message = message("sensors/kitchen/temp", b"warm and dry");
        assert!(message_matches(
            &message,
            "kitchen",
            MessageFilterMode::Substring,
            "warm"
        ));
        assert!(message_matches(
            &message,
            "sensors/+/temp",
            MessageFilterMode::MqttTopic,
            "dry"
        ));
        assert!(!message_matches(
            &message,
            "sensors/+/humidity",
            MessageFilterMode::MqttTopic,
            ""
        ));
        assert!(!message_matches(
            &message,
            "",
            MessageFilterMode::Substring,
            "cold"
        ));
        assert!(!message_matches(
            &message,
            "sensors/#/invalid",
            MessageFilterMode::MqttTopic,
            ""
        ));
        let binary =
            ReceivedMessage::new(2, SystemTime::now(), String::new(), 0, false, vec![0xff]);
        assert!(!message_matches(
            &binary,
            "",
            MessageFilterMode::Substring,
            "anything"
        ));
    }
}
