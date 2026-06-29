// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    if mkvm_lib::handle_process_control_args() {
        return;
    }

    if !mkvm_lib::acquire_single_instance() {
        mkvm_lib::activate_existing_instance();
        return;
    }

    mkvm_lib::run();
}
