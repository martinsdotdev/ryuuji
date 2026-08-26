#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use ryuuji_core::DataDir;
use windows_reactor::{App, Backdrop, bootstrap};

mod logging;
mod pages;
mod shell;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = DataDir::resolve()?;
    let _guard = logging::init(&dir.logs());
    tracing::info!(data_dir = %dir.root().display(), "starting");

    // Framework-dependent: initialise the Windows App Runtime before any UI.
    bootstrap()?;

    App::new()
        .title("Ryuuji")
        .inner_size(1100.0, 720.0)
        .backdrop(Backdrop::Mica)
        .run(move || shell::Shell::boot(dir))?;
    Ok(())
}
