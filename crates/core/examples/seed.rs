//! Adds three entries to the library so the shell has rows to show.
//! Point `RYUUJI_DATA_DIR` at a scratch directory to keep them out of the
//! real one.

use ryuuji_core::{DataDir, NewEntry, Store, WatchStatus};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = DataDir::resolve()?;
    println!("data dir: {}", dir.root().display());

    let mut store = Store::open(&dir)?;
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
    for (title, status, progress, total) in seeds {
        let stored = store.add(NewEntry {
            title: title.into(),
            status,
            progress,
            total,
        })?;
        println!("added #{} {}", stored.id, stored.title);
    }
    Ok(())
}
