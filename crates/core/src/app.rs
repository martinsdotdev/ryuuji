use crate::{AppState, Command, DataDir, LibraryEntry, Notice, Store, StoreError, error_chain};

/// The running application: the store plus the state derived from it.
///
/// Every command writes through to the store first and touches memory only
/// with the row the store returned, so the two never disagree. A failed
/// write becomes [`AppState::notice`] rather than an error, because the
/// shells call [`Ryuuji::dispatch`] from UI callbacks that cannot propagate one.
pub struct Ryuuji {
    store: Store,
    state: AppState,
}

impl Ryuuji {
    pub fn open(dir: &DataDir) -> Result<Ryuuji, StoreError> {
        let store = Store::open(dir)?;
        let library = store.entries()?;
        Ok(Ryuuji {
            store,
            state: AppState {
                library,
                ..AppState::default()
            },
        })
    }

    pub fn state(&self) -> &AppState {
        &self.state
    }

    pub fn dispatch(&mut self, command: Command) {
        match command {
            Command::SelectPage(page) => self.state.page = page,
            Command::DismissNotice => self.state.notice = None,
            Command::AddEntry(entry) => {
                let outcome = self.store.add(entry);
                self.absorb(outcome);
            }
            Command::SetProgress { id, progress } => {
                let outcome = self.store.set_progress(id, progress);
                self.absorb(outcome);
            }
            Command::SetStatus { id, status } => {
                let outcome = self.store.set_status(id, status);
                self.absorb(outcome);
            }
        }
    }

    fn absorb(&mut self, outcome: Result<LibraryEntry, StoreError>) {
        match outcome {
            Ok(entry) => upsert(&mut self.state.library, entry),
            Err(err) => {
                let detail = error_chain(&err);
                tracing::error!(error = %detail, "save failed");
                self.state.notice = Some(Notice::SaveFailed { detail });
            }
        }
    }
}

fn upsert(library: &mut Vec<LibraryEntry>, entry: LibraryEntry) {
    match library.iter_mut().find(|existing| existing.id == entry.id) {
        Some(existing) => *existing = entry,
        None => library.push(entry),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EntryId, NewEntry, Page, WatchStatus};

    fn open_tmp() -> (tempfile::TempDir, DataDir) {
        let tmp = tempfile::tempdir().unwrap();
        let dir = DataDir::at(tmp.path()).unwrap();
        (tmp, dir)
    }

    fn entry(title: &str) -> NewEntry {
        NewEntry {
            title: title.into(),
            status: WatchStatus::PlanToWatch,
            progress: 0,
            total: Some(12),
        }
    }

    #[test]
    fn open_loads_the_library_and_add_entry_survives_reopen() {
        let (_tmp, dir) = open_tmp();
        let mut app = Ryuuji::open(&dir).unwrap();
        assert!(app.state().library.is_empty());

        app.dispatch(Command::AddEntry(entry("Show")));
        let added = app.state().library.clone();
        assert_eq!(added.len(), 1);
        assert_eq!(added[0].title, "Show");
        assert_eq!(app.state().notice, None);
        drop(app);

        let reopened = Ryuuji::open(&dir).unwrap();
        assert_eq!(reopened.state().library, added);
    }

    #[test]
    fn set_progress_updates_the_row_in_place() {
        let (_tmp, dir) = open_tmp();
        let mut app = Ryuuji::open(&dir).unwrap();
        app.dispatch(Command::AddEntry(entry("Show")));
        let id = app.state().library[0].id;
        app.dispatch(Command::SetProgress { id, progress: 5 });
        app.dispatch(Command::SetStatus {
            id,
            status: WatchStatus::Watching,
        });
        assert_eq!(app.state().library.len(), 1);
        assert_eq!(app.state().library[0].progress, 5);
        assert_eq!(app.state().library[0].status, WatchStatus::Watching);
    }

    #[test]
    fn failed_save_becomes_a_notice_and_dismiss_clears_it() {
        let (_tmp, dir) = open_tmp();
        let mut app = Ryuuji::open(&dir).unwrap();
        app.dispatch(Command::AddEntry(entry("Show")));
        let before = app.state().library.clone();

        app.dispatch(Command::SetProgress {
            id: EntryId(9999),
            progress: 3,
        });
        assert!(matches!(
            &app.state().notice,
            Some(Notice::SaveFailed { detail }) if detail.contains("9999")
        ));
        assert_eq!(app.state().library, before);

        app.dispatch(Command::DismissNotice);
        assert_eq!(app.state().notice, None);
    }

    #[test]
    fn select_page_changes_only_the_page() {
        let (_tmp, dir) = open_tmp();
        let mut app = Ryuuji::open(&dir).unwrap();
        app.dispatch(Command::AddEntry(entry("Show")));
        let before = app.state().clone();

        app.dispatch(Command::SelectPage(Page::Settings));
        let after = app.state();
        assert_eq!(after.page, Page::Settings);
        assert_eq!(after.library, before.library);
        assert_eq!(after.now_playing, before.now_playing);
        assert_eq!(after.notice, before.notice);
        assert_eq!(
            Store::open(&dir).unwrap().entries().unwrap(),
            before.library
        );
    }
}
