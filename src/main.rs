#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use mimalloc::MiMalloc;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions::default();

    eframe::run_native(
        "MQUI",
        options,
        Box::new(|_cc| Ok(Box::new(mqui::App::default()))),
    )
}
