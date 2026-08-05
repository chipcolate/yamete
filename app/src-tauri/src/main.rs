// Windows would show a console window without this; harmless elsewhere.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    yamete_app_lib::run()
}
