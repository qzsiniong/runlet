// Runlet:Tauri 2 入口。启动托盘常驻应用。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    env_logger::init();
    runlet_lib::run();
}
