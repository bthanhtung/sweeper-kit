// Hides the console window on Windows release builds (planned platform).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    sweeper_app::run();
}
