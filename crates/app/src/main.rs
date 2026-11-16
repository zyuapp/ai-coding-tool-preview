//! The desktop app: startup, the one workspace, the effect runner and the window's views.

mod app_menu;
mod desktop_capture;
mod effects;
mod keys;
mod licenses;
mod placement;
mod resources;
mod serve;
mod startup;
mod theme_bridge;
mod views;
mod workspace;
mod zoom;

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if serve::asked(&arguments) {
        std::process::exit(serve::main(arguments));
    }
    let Some(launch) = startup::prepare() else { return };
    startup::run(launch);
}
