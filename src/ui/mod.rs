use eframe::egui;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use crate::app::App;
use crate::app::state::{TabKind, TabState};
use crate::models::ipc::ClientCommand;
use crate::models::mqtt::MqttLoginData;
use crate::ui::widgets::qos_picker;
use crate::utils::formatting::{format_payload, format_timestamp};

pub(crate) mod widgets;

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

fn topic_label(ui: &mut egui::Ui, topic: &str, color: egui::Color32) {
    if topic.is_empty() {
        ui.label(egui::RichText::new("(empty)").weak());
        return;
    }

    let wildcard_color = ui.visuals().warn_fg_color;
    let mut first = true;
    for segment in topic.split('/') {
        if !first {
            ui.label(egui::RichText::new("/").color(color));
        }
        first = false;

        let segment_color = if segment == "+" || segment == "#" {
            wildcard_color
        } else {
            color
        };
        ui.label(egui::RichText::new(segment).color(segment_color));
    }
}

pub(crate) fn render(app: &mut App, ctx: &egui::Context) {
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
                                            if ui.button("Disconnect").clicked() {
                                                tab_to_disconnect = Some(tab_id);
                                                ui.close();
                                            }
                                            if ui.button("Force Disconnect").clicked() {
                                                tab_to_force_disconnect = Some(tab_id);
                                                ui.close();
                                            }
                                            if ui.button("Reconnect").clicked() {
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
                                                    egui::RichText::new("✕").small().strong(),
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
            .show(ctx, |ui| {
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
        let mut profile_to_load: Option<String> = None;
        let mut load_template = false;

        egui::Window::new("MQTT Login")
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.vertical(|ui| {
                    if let Some(status) = &app.profile_status {
                        ui.label(status);
                    }

                    ui.label("Name");
                    ui.text_edit_singleline(&mut app.mqtt_form.name);

                    ui.label("Broker");
                    ui.text_edit_singleline(&mut app.mqtt_form.broker);

                    ui.label("Port");
                    ui.text_edit_singleline(&mut app.mqtt_form.port);

                    egui::CollapsingHeader::new("Connection")
                        .default_open(false)
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label("Keep alive (seconds)");
                                ui.add(
                                    egui::DragValue::new(&mut app.mqtt_form.keep_alive_secs)
                                        .range(1..=u16::MAX),
                                );
                            });

                            ui.label("Client ID (optional)");
                            ui.text_edit_singleline(&mut app.mqtt_form.client_id);
                        });

                    egui::CollapsingHeader::new("Login credentials")
                        .default_open(false)
                        .show(ui, |ui| {
                            ui.label("Username (optional)");
                            ui.text_edit_singleline(&mut app.mqtt_form.username);

                            ui.label("Password (optional)");
                            ui.add(
                                egui::TextEdit::singleline(&mut app.mqtt_form.password)
                                    .password(true),
                            );
                        });

                    egui::CollapsingHeader::new("Testament")
                        .default_open(false)
                        .show(ui, |ui| {
                            ui.label("Topic (optional)");
                            ui.text_edit_singleline(&mut app.mqtt_form.testament_topic);

                            ui.horizontal(|ui| {
                                ui.label("QoS");
                                ui.add(
                                    egui::DragValue::new(&mut app.mqtt_form.testament_qos)
                                        .range(0..=2),
                                );
                                ui.checkbox(&mut app.mqtt_form.testament_retain, "Retain");
                            });

                            ui.label("testament and last will");
                            ui.text_edit_singleline(&mut app.mqtt_form.testament_and_last_will);
                        });

                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        let selected_profile_text = app
                            .selected_profile_name
                            .as_deref()
                            .unwrap_or("Load configuration");

                        if ui.button("Save template").clicked() {
                            save_profile = true;
                        }

                        egui::ComboBox::from_id_salt("mqtt_config_picker")
                            .selected_text(selected_profile_text)
                            .show_ui(ui, |ui| {
                                for entry in &app.profile_entries {
                                    let selected = app
                                        .selected_profile_name
                                        .as_ref()
                                        .is_some_and(|current| current == &entry.display_name);
                                    if ui
                                        .selectable_label(selected, &entry.display_name)
                                        .clicked()
                                    {
                                        profile_to_load = Some(entry.display_name.clone());
                                        ui.close();
                                    }
                                }

                                ui.separator();
                                if ui.selectable_label(false, "Load template from file...").clicked() {
                                    load_template = true;
                                    ui.close();
                                }
                            });

                        if ui.button("Add client").clicked() {
                            create_client = true;
                        }
                    });
                });
            });

        if save_profile {
            app.save_current_profile();
        }

        if let Some(profile_name) = profile_to_load {
            app.load_profile_into_form(&profile_name);
        }

        if load_template {
            app.load_template_from_file_picker();
        }

        if create_client {
            app.new_tab(TabKind::Client, app.mqtt_form.clone());
            app.mqtt_form = MqttLoginData::default();
            open = false;
        }

        app.show_mqtt_popup = open;
    }

    egui::CentralPanel::default().show(ctx, |ui| {
        let Some(active_id) = app.active_tab else {
            ui.label("No client open. Press + to add an MQTT client.");
            return;
        };

        let Some(tab) = app.tabs.iter_mut().find(|t| t.id == active_id) else {
            ui.label("Active tab missing");
            return;
        };

        let mut commands_to_send: Vec<ClientCommand> = Vec::new();

        match &mut tab.state {
            TabState::Client {
                mqtt_login,
                connection_status,
                last_error,
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
                payload_view_hex,
                topic_filter,
                max_messages,
                subscriptions,
                messages,
                received_count,
                published_count,
            } => {
                ui.heading("MQTT Client");
                ui.label(format!("Broker: {}", mqtt_login.broker_addr()));
                ui.label(format!("Status: {connection_status}"));
                if let Some(err) = last_error {
                    ui.colored_label(ui.visuals().warn_fg_color, format!("Info: {err}"));
                }
                ui.label(format!(
                    "Totals: {} received / {} published",
                    received_count, published_count
                ));

                ui.separator();
                ui.heading("Subscriptions");
                ui.horizontal(|ui| {
                    ui.label("Topic");
                    ui.text_edit_singleline(subscribe_topic);
                    ui.label("QoS");
                    qos_picker(ui, &format!("sub_qos_{active_id}"), subscribe_qos);
                    if ui.button("Subscribe").clicked() {
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

                ui.horizontal(|ui| {
                    ui.label("Unsubscribe topic");
                    ui.text_edit_singleline(unsubscribe_topic);
                    if ui.button("Unsubscribe").clicked() {
                        let topic = unsubscribe_topic.trim().to_string();
                        if !topic.is_empty() {
                            commands_to_send.push(ClientCommand::Unsubscribe { topic });
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
                                    let row_response = ui
                                        .horizontal(|ui| {
                                            let color = topic_color_for(&entry.topic, ui.visuals());
                                            topic_label(ui, &entry.topic, color);
                                            ui.label(format!("(QoS {})", entry.qos));
                                            if ui.small_button("Remove").clicked() {
                                                remove_topic = Some(entry.topic.clone());
                                            }
                                        })
                                        .response;

                                    row_response.context_menu(|ui| {
                                        if ui.button("Edit Subscription").clicked() {
                                            edit_topic = Some((entry.topic.clone(), entry.qos));
                                            ui.close();
                                        }
                                        if ui.button("Unsubscribe").clicked() {
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
                        .show(ctx, |ui| {
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
                            *last_error = Some("Subscription topic cannot be empty".to_string());
                        } else {
                            let mut changed = new_topic != original_topic;
                            if !changed
                                && let Some(existing) =
                                    subscriptions.iter().find(|entry| entry.topic == original_topic)
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
                if ui.button("Publish message").clicked() {
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
                ui.horizontal(|ui| {
                    ui.label("Filter");
                    ui.text_edit_singleline(topic_filter);
                    ui.label("Max rows");
                    ui.add(egui::DragValue::new(max_messages).range(1..=1000));
                    ui.checkbox(payload_view_hex, "Hex payload");
                    if ui.button("Clear").clicked() {
                        messages.clear();
                    }
                });

                egui::ScrollArea::vertical()
                    .id_salt(("messages_scroll", active_id))
                    .show(ui, |ui| {
                        let filter = topic_filter.trim();
                        let mut shown = 0usize;

                        for msg in messages.iter().rev() {
                            if !filter.is_empty() && !msg.topic.contains(filter) {
                                continue;
                            }
                            if shown >= *max_messages {
                                break;
                            }

                            let ts = format_timestamp(msg.timestamp);
                            let payload_text = format_payload(&msg.payload, *payload_view_hex);
                            ui.group(|ui| {
                                ui.horizontal_wrapped(|ui| {
                                    ui.label(format!("[{ts}] "));
                                    let color = topic_color_for(&msg.topic, ui.visuals());
                                    topic_label(ui, &msg.topic, color);
                                });
                                ui.label(format!("QoS {} | retain {}", msg.qos, msg.retain));
                                ui.label(payload_text);
                            });
                            shown += 1;
                        }

                        if shown == 0 {
                            ui.label("No messages matched current filter.");
                        }
                    });
            }
        }

        for command in commands_to_send {
            app.send_client_command(active_id, command);
        }
    });
}
