use crate::settings::{self, SettingsError};
use crate::{
    AppState, Command, Confidence, DataDir, Diagnostics, LibraryEntry, NewEntry, Notice,
    NowPlaying, Opened, PlaybackEvent, PlaybackStatus, ProposedMatch, Settings, Store, StoreError,
    ThemePreference, WatchStatus, error_chain, matching,
};

/// The running application: the store plus the state derived from it.
///
/// Library rows are write-through: a command writes to disk first and touches
/// memory only with what the write returned, so the two never disagree. The
/// theme and the last match are best-effort instead: memory keeps the change
/// even when the file or the row could not be written, so the window still
/// switches and the card still shows the proposal, and the next successful
/// save carries it. Either way a failed write becomes a [`Notice`] in
/// [`AppState::notices`] rather than an error, because the shells call
/// [`Ryuuji::dispatch`] from UI callbacks that cannot propagate one.
pub struct Ryuuji {
    dir: DataDir,
    store: Store,
    state: AppState,
    /// The library changed since the last proposal, so the next playback
    /// event re-matches even for an unchanged title.
    match_stale: bool,
}

impl Ryuuji {
    pub fn open(dir: &DataDir) -> Result<Ryuuji, StoreError> {
        let loaded = settings::load_or_init(dir);
        let Opened { store, recovered } = Store::open(dir)?;
        let library = store.entries()?;
        let last_match = store.last_match().unwrap_or_else(|err| {
            tracing::warn!(error = %error_chain(&err), "last match unreadable");
            None
        });
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
            match_stale: false,
            state: AppState {
                library,
                last_match,
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
                if self.absorb(outcome).is_some() {
                    self.match_stale = true;
                }
            }
            Command::SetProgress { id, progress } => {
                let outcome = self.store.set_progress(id, progress);
                if self.absorb(outcome).is_some() {
                    self.match_stale = true;
                }
            }
            Command::SetStatus { id, status } => {
                let outcome = self.store.set_status(id, status);
                if self.absorb(outcome).is_some() {
                    self.match_stale = true;
                }
            }
            Command::Playback(event) => self.observe_playback(event),
            Command::AddProposedToLibrary => self.add_proposed(),
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
        let proposes = matches!(
            event.status,
            PlaybackStatus::Playing | PlaybackStatus::Paused
        ) && !event.title.trim().is_empty()
            && (self.match_stale
                || self
                    .state
                    .last_match
                    .as_ref()
                    .is_none_or(|m| m.raw_title != event.title));
        if proposes {
            let proposal = matching::propose(&event, &self.state.library);
            self.record_match(proposal);
            self.match_stale = false;
        }
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

    fn absorb(&mut self, outcome: Result<LibraryEntry, StoreError>) -> Option<LibraryEntry> {
        let entry = commit(&mut self.state.notices, "entry", outcome)?;
        upsert(&mut self.state.library, entry.clone());
        Some(entry)
    }

    fn record_match(&mut self, proposal: ProposedMatch) {
        commit(
            &mut self.state.notices,
            "last match",
            self.store.save_last_match(&proposal),
        );
        self.state.last_match = Some(proposal);
    }

    fn add_proposed(&mut self) {
        let Some(last) = self
            .state
            .last_match
            .clone()
            .filter(|m| m.confidence == Confidence::Unmatched)
        else {
            return;
        };
        let title = if last.parsed_title.is_empty() {
            last.raw_title.clone()
        } else {
            last.parsed_title.clone()
        };
        let outcome = self.store.add(NewEntry {
            title,
            status: WatchStatus::Watching,
            progress: 0,
            total: None,
        });
        let Some(entry) = self.absorb(outcome) else {
            return;
        };
        self.record_match(ProposedMatch {
            entry: Some(entry.id),
            confidence: Confidence::Exact,
            ..last
        });
        self.match_stale = false;
        tracing::info!(entry = entry.id.as_i64(), "proposal added to library");
    }

    #[cfg(test)]
    pub(crate) fn store_mut(&mut self) -> &mut Store {
        &mut self.store
    }
}

/// Records a failed save as a [`Notice::SaveFailed`] and hands back what the
/// write returned. Free rather than a method so the caller can pass
/// `&mut self.state.notices` alongside a `&mut self.store` call.
fn commit<T>(
    notices: &mut Vec<Notice>,
    what: &'static str,
    outcome: Result<T, impl std::error::Error>,
) -> Option<T> {
    match outcome {
        Ok(value) => Some(value),
        Err(err) => {
            let detail = error_chain(&err);
            tracing::error!(what, error = %detail, "save failed");
            notices.push(Notice::SaveFailed { detail });
            None
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
    use crate::{EntryId, MatchOutcome, Page, PlaybackSource};

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
    fn playback_changes_now_playing_and_last_match_only() {
        let (_tmp, dir) = open_tmp();
        let mut app = Ryuuji::open(&dir).unwrap();
        app.dispatch(Command::AddEntry(entry("Show")));
        app.dispatch(Command::SelectPage(Page::NowPlaying));
        let before = app.state().clone();

        app.dispatch(Command::Playback(playing("Show - 03.mkv")));
        let after = app.state();
        assert!(matches!(after.now_playing, NowPlaying::Playing { .. }));
        assert!(after.last_match.is_some());
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

    #[test]
    fn playing_proposes_and_persists_the_last_match() {
        let (_tmp, dir) = open_tmp();
        let mut app = Ryuuji::open(&dir).unwrap();
        app.dispatch(Command::Playback(playing("Show - 03.mkv")));
        let original = app.state().last_match.clone().expect("proposal recorded");
        assert_eq!(original.raw_title, "Show - 03.mkv");
        assert_eq!(original.player, "mpv");
        assert_eq!(original.at, SystemTime::UNIX_EPOCH);
        assert_eq!(original.outcome, MatchOutcome::Proposed);
        drop(app);

        let reopened = Ryuuji::open(&dir).unwrap();
        assert_eq!(
            reopened.state().last_match,
            Some(ProposedMatch {
                elements: Vec::new(),
                ..original
            })
        );
    }

    #[test]
    fn same_title_does_not_re_propose() {
        let (_tmp, dir) = open_tmp();
        let mut app = Ryuuji::open(&dir).unwrap();
        app.dispatch(Command::Playback(playing("Show - 03.mkv")));
        let first = app.state().last_match.clone();
        assert!(first.is_some());

        app.dispatch(Command::Playback(PlaybackEvent {
            position: Duration::from_secs(400),
            observed_at: SystemTime::UNIX_EPOCH + Duration::from_secs(60),
            ..playing("Show - 03.mkv")
        }));
        assert_eq!(app.state().last_match, first);
    }

    #[test]
    fn paused_first_event_proposes() {
        let (_tmp, dir) = open_tmp();
        let mut app = Ryuuji::open(&dir).unwrap();
        app.dispatch(Command::Playback(PlaybackEvent {
            status: PlaybackStatus::Paused,
            ..playing("Show - 03.mkv")
        }));
        assert!(app.state().last_match.is_some());
    }

    #[test]
    fn stopped_keeps_the_last_proposal() {
        let (_tmp, dir) = open_tmp();
        let mut app = Ryuuji::open(&dir).unwrap();
        app.dispatch(Command::Playback(playing("Show - 03.mkv")));
        let first = app.state().last_match.clone();
        assert!(first.is_some());

        app.dispatch(Command::Playback(PlaybackEvent {
            status: PlaybackStatus::Stopped,
            ..playing("Other - 01.mkv")
        }));
        assert_eq!(app.state().now_playing, NowPlaying::Idle);
        assert_eq!(app.state().last_match, first);
    }

    #[test]
    fn blank_title_never_proposes() {
        let (_tmp, dir) = open_tmp();
        let mut app = Ryuuji::open(&dir).unwrap();
        app.dispatch(Command::Playback(playing("")));
        app.dispatch(Command::Playback(playing("  \t")));
        assert_eq!(app.state().last_match, None);
    }

    #[test]
    fn failed_last_match_save_keeps_memory_and_notices() {
        let (_tmp, dir) = open_tmp();
        let mut app = Ryuuji::open(&dir).unwrap();
        app.store_mut().execute_raw("DROP TABLE last_match");

        app.dispatch(Command::Playback(playing("Show - 03.mkv")));
        assert!(app.state().last_match.is_some());
        assert!(matches!(
            app.state().notices.as_slice(),
            [Notice::SaveFailed { .. }]
        ));
    }

    #[test]
    fn add_proposed_from_unmatched_creates_watching_entry_and_relinks() {
        let (_tmp, dir) = open_tmp();
        let mut app = Ryuuji::open(&dir).unwrap();
        app.dispatch(Command::Playback(playing(
            "[SubsPlease] Frieren - Beyond Journey's End - 01 (1080p) [ABCD1234].mkv",
        )));
        assert_eq!(
            app.state().last_match.as_ref().unwrap().confidence,
            Confidence::Unmatched
        );

        app.dispatch(Command::AddProposedToLibrary);
        let added = app.state().library.clone();
        assert_eq!(added.len(), 1);
        assert_eq!(added[0].title, "Frieren - Beyond Journey's End");
        assert_eq!(added[0].status, WatchStatus::Watching);
        assert_eq!(added[0].progress, 0);
        assert_eq!(added[0].total, None);
        let relinked = app.state().last_match.clone().unwrap();
        assert_eq!(relinked.entry, Some(added[0].id));
        assert_eq!(relinked.confidence, Confidence::Exact);
        drop(app);

        let reopened = Ryuuji::open(&dir).unwrap();
        assert_eq!(reopened.state().library, added);
        assert_eq!(
            reopened.state().last_match,
            Some(ProposedMatch {
                elements: Vec::new(),
                ..relinked
            })
        );
    }

    #[test]
    fn add_proposed_falls_back_to_raw_title() {
        let (_tmp, dir) = open_tmp();
        let mut app = Ryuuji::open(&dir).unwrap();
        app.dispatch(Command::Playback(playing("[EnigmaBD 1080p].mkv")));
        let proposal = app.state().last_match.clone().unwrap();
        assert!(proposal.parsed_title.is_empty());
        assert_eq!(proposal.confidence, Confidence::Unmatched);

        app.dispatch(Command::AddProposedToLibrary);
        assert_eq!(app.state().library.len(), 1);
        assert_eq!(app.state().library[0].title, "[EnigmaBD 1080p].mkv");
    }

    #[test]
    fn add_proposed_is_a_no_op_without_last_match() {
        let (_tmp, dir) = open_tmp();
        let mut app = Ryuuji::open(&dir).unwrap();
        let before = app.state().clone();
        app.dispatch(Command::AddProposedToLibrary);
        assert_eq!(app.state(), &before);
    }

    #[test]
    fn add_proposed_is_a_no_op_when_confidence_is_not_unmatched() {
        let (_tmp, dir) = open_tmp();
        let mut app = Ryuuji::open(&dir).unwrap();
        app.dispatch(Command::AddEntry(entry("Show")));
        app.dispatch(Command::Playback(playing("Show - 03.mkv")));
        assert_eq!(
            app.state().last_match.as_ref().unwrap().confidence,
            Confidence::Exact
        );
        let before = app.state().clone();

        app.dispatch(Command::AddProposedToLibrary);
        assert_eq!(app.state(), &before);
    }

    #[test]
    fn add_proposed_add_failure_notices_and_changes_nothing() {
        let (_tmp, dir) = open_tmp();
        let mut app = Ryuuji::open(&dir).unwrap();
        app.dispatch(Command::Playback(playing("Show - 03.mkv")));
        app.store_mut().execute_raw("DROP TABLE entries");

        app.dispatch(Command::AddProposedToLibrary);
        assert!(matches!(
            app.state().notices.as_slice(),
            [Notice::SaveFailed { .. }]
        ));
        assert!(app.state().library.is_empty());
        assert_eq!(
            app.state().last_match.as_ref().unwrap().confidence,
            Confidence::Unmatched
        );
    }

    #[test]
    fn add_proposed_relink_save_failure_keeps_memory_and_notices() {
        let (_tmp, dir) = open_tmp();
        let mut app = Ryuuji::open(&dir).unwrap();
        app.dispatch(Command::Playback(playing("Show - 03.mkv")));
        app.store_mut().execute_raw("DROP TABLE last_match");

        app.dispatch(Command::AddProposedToLibrary);
        assert_eq!(app.state().library.len(), 1);
        let relinked = app.state().last_match.clone().unwrap();
        assert_eq!(relinked.entry, Some(app.state().library[0].id));
        assert_eq!(relinked.confidence, Confidence::Exact);
        assert!(matches!(
            app.state().notices.as_slice(),
            [Notice::SaveFailed { .. }]
        ));
    }

    #[test]
    fn library_change_re_proposes_the_same_title() {
        let (_tmp, dir) = open_tmp();
        let mut app = Ryuuji::open(&dir).unwrap();
        app.dispatch(Command::Playback(playing("X - 03.mkv")));
        assert_eq!(
            app.state().last_match.as_ref().unwrap().confidence,
            Confidence::Unmatched
        );

        app.dispatch(Command::AddEntry(entry("X")));
        app.dispatch(Command::Playback(PlaybackEvent {
            observed_at: SystemTime::UNIX_EPOCH + Duration::from_secs(60),
            ..playing("X - 03.mkv")
        }));
        assert_eq!(
            app.state().last_match.as_ref().unwrap().confidence,
            Confidence::Exact
        );

        let id = app.state().library[0].id;
        app.dispatch(Command::Playback(playing("Other - 01.mkv")));
        let before = app.state().last_match.clone().unwrap();
        app.dispatch(Command::SetStatus {
            id,
            status: WatchStatus::Watching,
        });
        app.dispatch(Command::Playback(PlaybackEvent {
            observed_at: SystemTime::UNIX_EPOCH + Duration::from_secs(120),
            ..playing("Other - 01.mkv")
        }));
        let after = app.state().last_match.clone().unwrap();
        assert_ne!(after.at, before.at);

        let before = after;
        app.dispatch(Command::SetProgress { id, progress: 5 });
        app.dispatch(Command::Playback(PlaybackEvent {
            observed_at: SystemTime::UNIX_EPOCH + Duration::from_secs(180),
            ..playing("Other - 01.mkv")
        }));
        assert_ne!(app.state().last_match.as_ref().unwrap().at, before.at);
    }

    #[test]
    fn add_proposed_clears_the_stale_flag() {
        let (_tmp, dir) = open_tmp();
        let mut app = Ryuuji::open(&dir).unwrap();
        app.dispatch(Command::Playback(playing("Show - 03.mkv")));
        app.dispatch(Command::AddProposedToLibrary);
        let relinked = app.state().last_match.clone();
        assert!(relinked.is_some());

        app.dispatch(Command::Playback(PlaybackEvent {
            observed_at: SystemTime::UNIX_EPOCH + Duration::from_secs(60),
            ..playing("Show - 03.mkv")
        }));
        assert_eq!(app.state().last_match, relinked);
    }
}
