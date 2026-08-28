// Prevents an extra console window from opening alongside the app on Windows
// release builds. Debug builds keep the console so tracing output is visible.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    aegis_lib::run();
}
