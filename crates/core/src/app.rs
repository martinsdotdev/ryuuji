use crate::settings::{self, SettingsError};
use crate::{
    AppState, Command, DataDir, Diagnostics, LibraryEntry, Notice, NowPlaying, Opened,
    PlaybackEvent, PlaybackStatus, Settings, Store, StoreError, ThemePreference, error_chain,
};

/// The running application: the store plus the state derived from it.
///
/// Every command writes through to disk first and touches memory only with
/// what the write returned, so the two never disagree. A failed write becomes
/// a [`Notice`] in [`AppState::notices`] rather than an error, because the
/// shells call [`Ryuuji::dispatch`] from UI callbacks that cannot propagate one.
/// The one exception is a theme change: memory keeps the choice even when
/// `settings.toml` could not be written, so the window still switches and the
/// next successful save carries it.
pub struct Ryuuji {
    dir: DataDir,
    store: Store,
    state: AppState,
}

impl Ryuuji {
    pub fn open(dir: &DataDir) -> Result<Ryuuji, StoreError> {
        let loaded = settings::load_or_init(dir);
        let Opened { store, recovered } = Store::open(dir)?;
        let library = store.entries()?;
        let notices = recovered
            .map(|recovered| Notice::LibraryReset {
                backup: recovered.backup,
            })
            .into_iter()
            .chain(loaded.problem.map(SettingsError::into_notice))
            .collect();
        Ok(Ryuuji {
            dir: dir.clone(),
            store,
            state: AppState {
                library,
                settings: loaded.settings,
                notices,
                ..AppState::default()
            },
        })
    }

    pub fn state(&self) -> &AppState {
        &self.state
    }

    /// A fresh look at the files and schema, taken now.
    pub fn diagnostics(&self) -> Diagnostics {
        Diagnostics::gather(&self.dir, &self.store)
    }

    pub fn dispatch(&mut self, command: Command) {
        match command {
            Command::SelectPage(page) => self.state.page = page,
            Command::SetTheme(theme) => self.set_theme(theme),
            Command::DismissNotice => {
                if !self.state.notices.is_empty() {
                    self.state.notices.remove(0);
                }
            }
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
            Command::Playback(event) => self.observe_playback(event),
        }
    }

    fn observe_playback(&mut self, event: PlaybackEvent) {
        tracing::debug!(
            source = event.source.tag(),
            player = %event.player,
            title = %event.title,
            status = event.status.label(),
            position_s = event.position.as_secs(),
            duration_s = event.duration.as_secs(),
            "playback"
        );
        self.state.now_playing = now_playing_for(event);
    }

    fn set_theme(&mut self, theme: ThemePreference) {
        if theme == self.state.settings.theme {
            return;
        }
        let next = Settings { theme };
        if let Err(err) = settings::save(&self.dir, &next) {
            tracing::error!(error = %error_chain(&err), "settings save failed");
            self.state.notices.push(err.into_notice());
        }
        self.state.settings = next;
    }

    fn absorb(&mut self, outcome: Result<LibraryEntry, StoreError>) {
        match outcome {
            Ok(entry) => upsert(&mut self.state.library, entry),
            Err(err) => {
                let detail = error_chain(&err);
                tracing::error!(error = %detail, "save failed");
                self.state.notices.push(Notice::SaveFailed { detail });
            }
        }
    }
}

fn now_playing_for(event: PlaybackEvent) -> NowPlaying {
    match event.status {
        PlaybackStatus::Stopped => NowPlaying::Idle,
        PlaybackStatus::Playing | PlaybackStatus::Paused if event.title.trim().is_empty() => {
            NowPlaying::Detecting
        }
        PlaybackStatus::Playing | PlaybackStatus::Paused => NowPlaying::Playing {
            title: event.title,
            player: event.player,
            status: event.status,
            position: event.position,
            duration: event.duration,
        },
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
    use std::fs;
    use std::time::{Duration, SystemTime};

    use super::*;
    use crate::{EntryId, NewEntry, Page, PlaybackSource, WatchStatus};

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

    fn playing(title: &str) -> PlaybackEvent {
        PlaybackEvent {
            player: "mpv".into(),
            title: title.into(),
            status: PlaybackStatus::Playing,
            position: Duration::from_secs(305),
            duration: Duration::from_secs(1420),
            observed_at: SystemTime::UNIX_EPOCH,
            source: PlaybackSource::Detected,
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
        assert!(app.state().notices.is_empty());
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
            app.state().notices.as_slice(),
            [Notice::SaveFailed { detail }] if detail.contains("9999")
        ));
        assert_eq!(app.state().library, before);

        app.dispatch(Command::DismissNotice);
        assert!(app.state().notices.is_empty());
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
        assert_eq!(after.settings, before.settings);
        assert_eq!(after.notices, before.notices);
        assert_eq!(
            Store::open(&dir).unwrap().store.entries().unwrap(),
            before.library
        );
    }

    #[test]
    fn playback_playing_sets_now_playing_with_title_player_and_times() {
        let (_tmp, dir) = open_tmp();
        let mut app = Ryuuji::open(&dir).unwrap();
        app.dispatch(Command::Playback(playing("Show - 03.mkv")));
        assert_eq!(
            app.state().now_playing,
            NowPlaying::Playing {
                title: "Show - 03.mkv".into(),
                player: "mpv".into(),
                status: PlaybackStatus::Playing,
                position: Duration::from_secs(305),
                duration: Duration::from_secs(1420),
            }
        );
    }

    #[test]
    fn playback_paused_keeps_the_title_and_marks_paused() {
        let (_tmp, dir) = open_tmp();
        let mut app = Ryuuji::open(&dir).unwrap();
        app.dispatch(Command::Playback(playing("Show - 03.mkv")));
        app.dispatch(Command::Playback(PlaybackEvent {
            status: PlaybackStatus::Paused,
            ..playing("Show - 03.mkv")
        }));
        assert!(matches!(
            &app.state().now_playing,
            NowPlaying::Playing { title, status: PlaybackStatus::Paused, .. }
                if title == "Show - 03.mkv"
        ));
    }

    #[test]
    fn playback_stopped_returns_to_idle() {
        let (_tmp, dir) = open_tmp();
        let mut app = Ryuuji::open(&dir).unwrap();
        app.dispatch(Command::Playback(playing("Show - 03.mkv")));
        app.dispatch(Command::Playback(PlaybackEvent {
            status: PlaybackStatus::Stopped,
            ..playing("Show - 03.mkv")
        }));
        assert_eq!(app.state().now_playing, NowPlaying::Idle);
    }

    #[test]
    fn playback_without_a_title_is_detecting() {
        let (_tmp, dir) = open_tmp();
        let mut app = Ryuuji::open(&dir).unwrap();
        app.dispatch(Command::Playback(playing("")));
        assert_eq!(app.state().now_playing, NowPlaying::Detecting);
        app.dispatch(Command::Playback(PlaybackEvent {
            status: PlaybackStatus::Paused,
            ..playing("  \t")
        }));
        assert_eq!(app.state().now_playing, NowPlaying::Detecting);
    }

    #[test]
    fn playback_changes_only_now_playing() {
        let (_tmp, dir) = open_tmp();
        let mut app = Ryuuji::open(&dir).unwrap();
        app.dispatch(Command::AddEntry(entry("Show")));
        app.dispatch(Command::SelectPage(Page::NowPlaying));
        let before = app.state().clone();

        app.dispatch(Command::Playback(playing("Show - 03.mkv")));
        let after = app.state();
        assert!(matches!(after.now_playing, NowPlaying::Playing { .. }));
        assert_eq!(after.page, before.page);
        assert_eq!(after.library, before.library);
        assert_eq!(after.settings, before.settings);
        assert_eq!(after.notices, before.notices);
        assert_eq!(
            Store::open(&dir).unwrap().store.entries().unwrap(),
            before.library
        );
    }

    #[test]
    fn playback_source_does_not_change_the_rule() {
        let (_tmp, dir) = open_tmp();
        let mut app = Ryuuji::open(&dir).unwrap();
        app.dispatch(Command::Playback(playing("Show - 03.mkv")));
        let detected = app.state().now_playing.clone();

        app.dispatch(Command::Playback(PlaybackEvent {
            source: PlaybackSource::Injected,
            ..playing("Show - 03.mkv")
        }));
        assert_eq!(app.state().now_playing, detected);

        app.dispatch(Command::Playback(PlaybackEvent {
            source: PlaybackSource::Injected,
            ..playing("")
        }));
        assert_eq!(app.state().now_playing, NowPlaying::Detecting);
    }

    #[test]
    fn boot_problems_queue_oldest_first_and_dismiss_in_order() {
        let (_tmp, dir) = open_tmp();
        fs::write(dir.library_db(), b"not a database").unwrap();
        fs::write(dir.settings_file(), "theme = \"blue\"\n").unwrap();

        let mut app = Ryuuji::open(&dir).unwrap();
        assert!(matches!(
            app.state().notices.as_slice(),
            [
                Notice::LibraryReset { .. },
                Notice::SettingsUnreadable { .. }
            ]
        ));
        assert_eq!(app.state().settings, Settings::default());

        app.dispatch(Command::DismissNotice);
        assert!(matches!(
            app.state().notices.as_slice(),
            [Notice::SettingsUnreadable { .. }]
        ));
        app.dispatch(Command::DismissNotice);
        assert!(app.state().notices.is_empty());
        app.dispatch(Command::DismissNotice);
        assert!(app.state().notices.is_empty());
    }

    #[test]
    fn set_theme_persists_and_survives_reopen() {
        let (_tmp, dir) = open_tmp();
        let mut app = Ryuuji::open(&dir).unwrap();
        app.dispatch(Command::SetTheme(ThemePreference::Dark));
        assert_eq!(app.state().settings.theme, ThemePreference::Dark);
        assert!(app.state().notices.is_empty());
        let text = fs::read_to_string(dir.settings_file()).unwrap();
        assert!(text.contains("theme = \"dark\"\n"));
        drop(app);

        let reopened = Ryuuji::open(&dir).unwrap();
        assert_eq!(reopened.state().settings.theme, ThemePreference::Dark);
        assert!(reopened.state().notices.is_empty());
    }

    #[test]
    fn unwritable_settings_keep_the_choice_in_memory_and_warn() {
        let (_tmp, dir) = open_tmp();
        fs::create_dir(dir.settings_file()).unwrap();

        let mut app = Ryuuji::open(&dir).unwrap();
        assert!(matches!(
            app.state().notices.as_slice(),
            [Notice::SettingsUnreadable { .. }]
        ));

        app.dispatch(Command::SetTheme(ThemePreference::Light));
        assert_eq!(app.state().settings.theme, ThemePreference::Light);
        assert!(matches!(
            app.state().notices.as_slice(),
            [Notice::SettingsUnreadable { .. }, Notice::SaveFailed { .. }]
        ));
        assert!(dir.settings_file().is_dir());
    }
}
