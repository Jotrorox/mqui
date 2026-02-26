use eframe::egui;

pub(crate) fn qos_picker(ui: &mut egui::Ui, id: &str, value: &mut u8) {
    egui::ComboBox::from_id_salt(id)
        .selected_text(value.to_string())
        .show_ui(ui, |ui| {
            ui.selectable_value(value, 0, "0");
            ui.selectable_value(value, 1, "1");
            ui.selectable_value(value, 2, "2");
        });
}
