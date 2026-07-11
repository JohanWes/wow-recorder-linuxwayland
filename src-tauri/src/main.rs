// Prevents an additional console window on Windows in release; harmless for this Linux-only port.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    warcraft_recorder_lib::run();
}
