#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![cfg_attr(not(windows), allow(dead_code))]

mod hotkey;
mod logging;
mod router;
mod runtime;
mod settings;
mod translate;
mod update;

#[cfg(windows)]
mod platform;
#[cfg(windows)]
mod ui;

#[cfg(windows)]
fn main() {
    logging::init();
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|arg| arg == "--sync-base") {
        update::sync_base();
        return;
    }
    update::init_base(&args);
    if update::launch_newer_at_startup() {
        return;
    }
    // Generous wait: a freshly updated instance starts before the old one has exited.
    let Some(_instance) = platform::instance::acquire(std::time::Duration::from_secs(10)) else {
        return;
    };
    ui::run();
}

#[cfg(not(windows))]
fn main() {
    eprintln!("MiniTranslation runs on Windows only.");
}
