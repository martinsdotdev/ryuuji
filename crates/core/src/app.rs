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
            && self
                .state
                .last_match
                .as_ref()
                .is_none_or(|m| m.raw_title != event.title);
        if proposes {
            let proposal = matching::propose(&event, &self.state.library);
            self.record_match(proposal);
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
        self.re_resolve();
        Some(entry)
    }

    /// A library write can turn a standing "No library entry" into a match,
    /// and the re-match cannot wait for the next playback event: detection
    /// drops repeat observations, and a paused or stopped player sends none
    /// at all. A proposal that already names an entry is left alone, because
    /// nothing deletes an entry and so a link never goes stale.
    fn re_resolve(&mut self) {
        let Some(standing) = self.state.last_match.clone() else {
            return;
        };
        if standing.confidence != Confidence::Unmatched {
            return;
        }
        let (entry, confidence) = matching::resolve(&standing.parsed_title, &self.state.library);
        if entry.is_some() {
            self.record_match(ProposedMatch {
                entry,
                confidence,
                ..standing
            });
        }
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
        let title = last.shown_title().to_owned();
        let outcome = self.store.add(NewEntry {
            title,
            status: WatchStatus::Watching,
            progress: 0,
            total: None,
        });
        // Not `absorb`: the link is known here, so re-resolving first would
        // save a proposal that the relink below immediately replaces, and a
        // single click would notice a failed last-match write twice.
        let Some(entry) = commit(&mut self.state.notices, "entry", outcome) else {
            return;
        };
        let id = entry.id;
        upsert(&mut self.state.library, entry);
        self.record_match(ProposedMatch {
            entry: Some(id),
            confidence: Confidence::Exact,
            ..last
        });
        tracing::info!(entry = id.as_i64(), "proposal added to library");
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
    use crate::{EntryId, Page, PlaybackSource};

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
        drop(app);

        let reopened = Ryuuji::open(&dir).unwrap();
        assert_eq!(reopened.state().last_match, Some(original));
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
        assert_eq!(reopened.state().last_match, Some(relinked));
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

        // The relink stands even though `resolve` would call an empty parsed
        // title Unmatched, because a linked proposal is never re-resolved.
        let id = app.state().library[0].id;
        app.dispatch(Command::SetProgress { id, progress: 1 });
        let linked = app.state().last_match.clone().unwrap();
        assert_eq!(linked.confidence, Confidence::Exact);
        assert_eq!(linked.entry, Some(id));
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
    fn library_write_re_resolves_the_standing_proposal() {
        let (_tmp, dir) = open_tmp();
        let mut app = Ryuuji::open(&dir).unwrap();
        app.dispatch(Command::Playback(playing("X - 03.mkv")));
        let before = app.state().last_match.clone().unwrap();
        assert_eq!(before.confidence, Confidence::Unmatched);
        assert_eq!(before.entry, None);

        app.dispatch(Command::AddEntry(entry("X")));
        let after = app.state().last_match.clone().unwrap();
        let id = app.state().library[0].id;
        assert_eq!(after.confidence, Confidence::Exact);
        assert_eq!(after.entry, Some(id));
        // The proposal was re-resolved, not re-proposed: it still describes the
        // same observation, so its timestamp does not move.
        assert_eq!(after.at, before.at);

        drop(app);
        let reopened = Ryuuji::open(&dir).unwrap();
        let stored = reopened.state().last_match.clone().unwrap();
        assert_eq!(stored.entry, Some(id));
        assert_eq!(stored.confidence, Confidence::Exact);
        assert_eq!(stored.at, before.at);

        let mut app = reopened;
        app.dispatch(Command::SetProgress { id, progress: 5 });
        assert_eq!(app.state().last_match, Some(stored));
    }

    #[test]
    fn paused_player_sees_the_library_edit_without_a_new_event() {
        let (_tmp, dir) = open_tmp();
        let mut app = Ryuuji::open(&dir).unwrap();
        app.dispatch(Command::Playback(PlaybackEvent {
            status: PlaybackStatus::Paused,
            ..playing("Y - 07.mkv")
        }));
        assert_eq!(
            app.state().last_match.as_ref().unwrap().confidence,
            Confidence::Unmatched
        );

        // A paused player sends nothing more, so the flip has to happen on the
        // library write itself.
        app.dispatch(Command::AddEntry(entry("Y")));
        let linked = app.state().last_match.clone().unwrap();
        assert_eq!(linked.confidence, Confidence::Exact);
        assert_eq!(linked.entry, Some(app.state().library[0].id));
    }

    #[test]
    fn add_proposed_holds_through_the_next_event_for_the_same_title() {
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
