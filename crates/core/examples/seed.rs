//! Boots Ryuuji the way a shell does and adds three entries to the library so
//! the shell has rows to show. Point `RYUUJI_DATA_DIR` at a scratch directory
//! to keep them out of the real one.

use ryuuji_core::{Command, DataDir, NewEntry, Notice, Ryuuji, WatchStatus};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = DataDir::resolve()?;
    println!("data dir: {}", dir.root().display());

    let mut app = Ryuuji::open(&dir)?;
    report(&app.state().notices);

    let seeds = [
        (
            "Frieren: Beyond Journey's End",
            WatchStatus::Watching,
            7,
            Some(28),
        ),
        ("Delicious in Dungeon", WatchStatus::Completed, 24, Some(24)),
        ("Mushishi", WatchStatus::PlanToWatch, 0, Some(26)),
    ];
    let seen = app.state().notices.len();
    for (title, status, progress, total) in seeds {
        app.dispatch(Command::AddEntry(NewEntry {
            title: title.into(),
            status,
            progress,
            total,
            rewatching: false,
        }));
        match app.state().library.last() {
            Some(stored) if stored.title == title => {
                println!("added #{} {}", stored.id, stored.title);
            }
            _ => {
                report(&app.state().notices[seen..]);
                return Err(format!("could not add {title}").into());
            }
        }
    }
    Ok(())
}

fn report(notices: &[Notice]) {
    for notice in notices {
        match notice {
            Notice::SaveFailed { detail } => println!("save failed: {detail}"),
            Notice::SettingsUnreadable { detail } => println!("settings unreadable: {detail}"),
            Notice::LibraryReset { backup } => {
                println!("library reset; the old file is at {}", backup.display());
            }
        }
    }
}
