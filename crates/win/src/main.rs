#![windows_subsystem = "windows"]

use windows_reactor::*;

mod pages;
mod shell;

fn main() -> Result<()> {
    // Framework-dependent: initialise the Windows App Runtime before any UI.
    bootstrap()?;

    App::new()
        .title("Ryuuji")
        .inner_size(1100.0, 720.0)
        .backdrop(Backdrop::Mica)
        .render(shell::app)
}
