#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod debloat;
mod display;
mod gui;
mod patcher;
mod powerplay;
mod quarantine;
mod tweaks;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--daemon") {
        tweaks::run_boot_guard_daemon();
        return;
    }
    if args.iter().any(|a| a == "--install-gui") {
        let _ = tweaks::install_rbridge_gui();
        return;
    }

    if let Err(e) = gui::launch_gui() {
        eprintln!("RBridge GUI failed to start: {}", e);
    }
}
