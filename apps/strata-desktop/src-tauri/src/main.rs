// Prevent a second console window on Windows release builds; harmless on macOS.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    strata_desktop_lib::run();
}
