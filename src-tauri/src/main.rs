// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    if mykvm_lib::handle_process_control_args() {
        return;
    }

    if !mykvm_lib::acquire_single_instance() {
        mykvm_lib::activate_existing_instance();
        return;
    }

    mykvm_lib::run();
}
