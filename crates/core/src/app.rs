use crate::settings::{self, SettingsError};
use crate::watch::{Accrual, WatchSession};
use crate::{
    AppState, Command, DataDir, Decline, Diagnostics, LibraryEntry, Link, NewEntry, NewWatchEvent,
    Notice, NowPlaying, Opened, PlaybackEvent, ProposedMatch, RecordOutcome, Recording, Settings,
    Store, StoreError, ThemePreference, WatchEventId, WatchProgress, WatchStatus, error_chain,
    matching,
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
    session: WatchSession,
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
            session: WatchSession::resume(last_match.as_ref()),
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
            Command::SelectPage(page) => {
                self.state.page = page;
                self.state.detail = None;
            }
            Command::OpenDetail(detail) => self.state.detail = Some(detail),
            Command::CloseDetail => self.state.detail = None,
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
            Command::UndoRecording(id) => self.undo_recording(id),
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
            foreground = ?event.foreground,
            "playback"
        );
        let observed = self.session.observe(&event);
        if observed.new_viewing {
            let proposal = matching::propose(&event, &self.state.library);
            self.record_match(proposal);
        }
        let previous = self.state.watch_progress.map(|progress| progress.outcome);
        self.state.watch_progress = observed.progress.map(|accrual| WatchProgress {
            accrued: accrual.accrued,
            threshold: accrual.threshold,
            outcome: self.record_outcome(accrual, previous),
        });
        self.state.now_playing = NowPlaying::of(&event);
    }

    /// Writes the episode the standing viewing has earned, once per viewing,
    /// and says what came of it. Once the session is recorded the standing
    /// outcome carries forward instead of the gates running again, since
    /// the id they minted lives nowhere else. A failed write leaves the
    /// session unrecorded, so the next event past the threshold tries again
    /// and the notice says why.
    fn record_outcome(
        &mut self,
        accrual: Accrual,
        previous: Option<RecordOutcome>,
    ) -> RecordOutcome {
        if accrual.recorded {
            return previous
                .filter(|outcome| {
                    matches!(outcome, RecordOutcome::Recorded(_) | RecordOutcome::Undone)
                })
                .unwrap_or(RecordOutcome::Counting);
        }
        if accrual.accrued < accrual.threshold {
            return RecordOutcome::Counting;
        }
        let write = match self.earned() {
            Ok(write) => write,
            Err(decline) => return RecordOutcome::Declined(decline),
        };
        let outcome = self.store.record(write);
        let Some(Recording { entry, event }) = commit(&mut self.state.notices, "progress", outcome)
        else {
            return RecordOutcome::Counting;
        };
        upsert(&mut self.state.library, entry);
        self.session.mark_recorded();
        tracing::info!(
            entry = event.entry.as_i64(),
            event = event.id.as_i64(),
            progress = event.progress,
            "episode recorded"
        );
        RecordOutcome::Recorded(event.id)
    }

    /// The write the standing viewing has earned, or the first gate that
    /// refuses it. The gates run in the order a decline would be explained
    /// in: the match, then the entry.
    fn earned(&self) -> Result<NewWatchEvent, Decline> {
        let last = self.state.last_match.as_ref().ok_or(Decline::NotExact)?;
        let Link::Exact(id) = last.link else {
            return Err(Decline::NotExact);
        };
        let entry = self
            .state
            .library
            .iter()
            .find(|entry| entry.id == id)
            .ok_or(Decline::NotExact)?;
        if entry.status == WatchStatus::Completed && !entry.rewatching {
            return Err(Decline::Completed);
        }
        let range = last.episode.as_ref().ok_or(Decline::NoEpisode)?;
        // A batch spanning the next episode was watched through its end.
        if !entry
            .progress
            .checked_add(1)
            .is_some_and(|next| range.contains(&next))
        {
            return Err(Decline::NotNext);
        }
        if entry.total.is_some_and(|total| *range.end() > total) {
            return Err(Decline::PastTotal);
        }
        Ok(NewWatchEvent {
            entry: id,
            episode: range.clone(),
            raw_title: last.raw_title.clone(),
            player: last.player.clone(),
        })
    }

    /// Not `absorb`: the link is unchanged, so re-resolving would be a store
    /// read for nothing. The session stays recorded, so watching on does not
    /// write again; a relaunch and rewatch does, because the range gate
    /// permits `progress + 1` once more, which is the right answer to "I
    /// undid it and watched it again".
    fn undo_recording(&mut self, id: WatchEventId) {
        let outcome = self.store.undo(id);
        let Some(Recording { entry, event }) = commit(&mut self.state.notices, "undo", outcome)
        else {
            return;
        };
        upsert(&mut self.state.library, entry);
        if let Some(progress) = self.state.watch_progress.as_mut()
            && progress.outcome == RecordOutcome::Recorded(id)
        {
            progress.outcome = RecordOutcome::Undone;
        }
        tracing::info!(
            entry = event.entry.as_i64(),
            event = id.as_i64(),
            progress = event.progress_before,
            "recording undone"
        );
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
        if standing.link.names_entry() {
            return;
        }
        match Link::from(matching::resolve(
            &standing.parsed_title,
            &self.state.library,
        )) {
            Link::Unmatched => {}
            link => self.record_match(ProposedMatch { link, ..standing }),
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
            .filter(|m| !m.link.names_entry())
        else {
            return;
        };
        let title = last.shown_title().to_owned();
        let outcome = self.store.add(NewEntry {
            title,
            status: WatchStatus::Watching,
            progress: 0,
            total: None,
            rewatching: false,
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
            link: Link::Exact(id),
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

fn upsert(library: &mut Vec<LibraryEntry>, entry: LibraryEntry) {
    match library.iter_mut().find(|existing| existing.id == entry.id) {
        Some(existing) => *existing = entry,
        None => library.push(entry),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::ops::RangeInclusive;
    use std::time::{Duration, SystemTime};

    use super::*;
    use crate::{Detail, EntryId, Page, PlaybackSource, PlaybackStatus, WatchEvent};

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
            rewatching: false,
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
            foreground: None,
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
    fn detail_opens_over_the_page_and_closes_on_back_or_selection() {
        let (_tmp, dir) = open_tmp();
        let mut app = Ryuuji::open(&dir).unwrap();
        app.dispatch(Command::SelectPage(Page::Settings));

        app.dispatch(Command::OpenDetail(Detail::Diagnostics));
        assert_eq!(app.state().detail, Some(Detail::Diagnostics));
        assert_eq!(app.state().page, Page::Settings);

        app.dispatch(Command::CloseDetail);
        assert_eq!(app.state().detail, None);
        assert_eq!(app.state().page, Page::Settings);

        app.dispatch(Command::OpenDetail(Detail::Diagnostics));
        app.dispatch(Command::SelectPage(Page::Library));
        assert_eq!(app.state().detail, None);
        assert_eq!(app.state().page, Page::Library);
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
        assert_eq!(after.detail, before.detail);
        assert_eq!(after.library, before.library);
        assert_eq!(after.now_playing, before.now_playing);
        assert_eq!(after.watch_progress, before.watch_progress);
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
    fn a_first_event_changes_now_playing_last_match_and_watch_progress_only() {
        let (_tmp, dir) = open_tmp();
        let mut app = Ryuuji::open(&dir).unwrap();
        app.dispatch(Command::AddEntry(entry("Show")));
        app.dispatch(Command::SelectPage(Page::NowPlaying));
        let before = app.state().clone();

        app.dispatch(Command::Playback(playing("Show - 03.mkv")));
        let after = app.state();
        assert!(matches!(after.now_playing, NowPlaying::Playing { .. }));
        assert!(after.last_match.is_some());
        assert_eq!(
            after.watch_progress,
            Some(WatchProgress {
                accrued: Duration::ZERO,
                threshold: Duration::from_secs(710),
                outcome: RecordOutcome::Counting,
            })
        );
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
    fn reopen_does_not_re_propose_the_title_still_playing() {
        let (_tmp, dir) = open_tmp();
        let mut app = Ryuuji::open(&dir).unwrap();
        app.dispatch(Command::Playback(playing("Show - 03.mkv")));
        let first = app.state().last_match.clone();
        assert!(first.is_some());
        drop(app);

        let mut app = Ryuuji::open(&dir).unwrap();
        app.dispatch(Command::Playback(PlaybackEvent {
            observed_at: SystemTime::UNIX_EPOCH + Duration::from_secs(60),
            ..playing("Show - 03.mkv")
        }));
        assert_eq!(app.state().last_match, first);

        app.dispatch(Command::Playback(PlaybackEvent {
            observed_at: SystemTime::UNIX_EPOCH + Duration::from_secs(120),
            ..playing("Show - 04.mkv")
        }));
        assert_ne!(app.state().last_match, first);
    }

    #[test]
    fn same_title_in_another_player_re_proposes() {
        let (_tmp, dir) = open_tmp();
        let mut app = Ryuuji::open(&dir).unwrap();
        app.dispatch(Command::Playback(playing("Show - 03.mkv")));
        app.dispatch(Command::Playback(PlaybackEvent {
            player: "vlc".into(),
            observed_at: SystemTime::UNIX_EPOCH + Duration::from_secs(60),
            ..playing("Show - 03.mkv")
        }));
        let moved = app.state().last_match.clone().unwrap();
        assert_eq!(moved.player, "vlc");
        assert_eq!(moved.at, SystemTime::UNIX_EPOCH + Duration::from_secs(60));
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
            app.state().last_match.as_ref().unwrap().link,
            Link::Unmatched
        );

        app.dispatch(Command::AddProposedToLibrary);
        let added = app.state().library.clone();
        assert_eq!(added.len(), 1);
        assert_eq!(added[0].title, "Frieren - Beyond Journey's End");
        assert_eq!(added[0].status, WatchStatus::Watching);
        assert_eq!(added[0].progress, 0);
        assert_eq!(added[0].total, None);
        let relinked = app.state().last_match.clone().unwrap();
        assert_eq!(relinked.link, Link::Exact(added[0].id));
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
        assert_eq!(proposal.link, Link::Unmatched);

        app.dispatch(Command::AddProposedToLibrary);
        assert_eq!(app.state().library.len(), 1);
        assert_eq!(app.state().library[0].title, "[EnigmaBD 1080p].mkv");

        // The relink stands even though `resolve` would call an empty parsed
        // title Unmatched, because a linked proposal is never re-resolved.
        let id = app.state().library[0].id;
        app.dispatch(Command::SetProgress { id, progress: 1 });
        let linked = app.state().last_match.clone().unwrap();
        assert_eq!(linked.link, Link::Exact(id));
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
        let id = app.state().library[0].id;
        assert_eq!(
            app.state().last_match.as_ref().unwrap().link,
            Link::Exact(id)
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
            app.state().last_match.as_ref().unwrap().link,
            Link::Unmatched
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
        assert_eq!(relinked.link, Link::Exact(app.state().library[0].id));
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
        assert_eq!(before.link, Link::Unmatched);

        app.dispatch(Command::AddEntry(entry("X")));
        let after = app.state().last_match.clone().unwrap();
        let id = app.state().library[0].id;
        assert_eq!(after.link, Link::Exact(id));
        // The proposal was re-resolved, not re-proposed: it still describes the
        // same observation, so its timestamp does not move.
        assert_eq!(after.at, before.at);

        drop(app);
        let reopened = Ryuuji::open(&dir).unwrap();
        let stored = reopened.state().last_match.clone().unwrap();
        assert_eq!(stored.link, Link::Exact(id));
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
            app.state().last_match.as_ref().unwrap().link,
            Link::Unmatched
        );

        // A paused player sends nothing more, so the flip has to happen on the
        // library write itself.
        app.dispatch(Command::AddEntry(entry("Y")));
        let linked = app.state().last_match.clone().unwrap();
        assert_eq!(linked.link, Link::Exact(app.state().library[0].id));
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

    /// The same title `secs` later and `secs` further in, so the credit is
    /// the full gap. The viewing's threshold is 710 s.
    fn later(title: &str, secs: u64) -> PlaybackEvent {
        PlaybackEvent {
            position: Duration::from_secs(305 + secs),
            observed_at: SystemTime::UNIX_EPOCH + Duration::from_secs(secs),
            ..playing(title)
        }
    }

    fn watch_past_threshold(app: &mut Ryuuji, title: &str) {
        app.dispatch(Command::Playback(playing(title)));
        app.dispatch(Command::Playback(later(title, 720)));
    }

    fn stored_progress(dir: &DataDir, id: EntryId) -> u32 {
        Store::open(dir)
            .unwrap()
            .store
            .entries()
            .unwrap()
            .into_iter()
            .find(|entry| entry.id == id)
            .unwrap()
            .progress
    }

    fn stored_events(dir: &DataDir) -> Vec<WatchEvent> {
        Store::open(dir).unwrap().store.watch_events().unwrap()
    }

    fn outcome(app: &Ryuuji) -> RecordOutcome {
        app.state().watch_progress.unwrap().outcome
    }

    /// The `(episode, progress_before, progress)` of every stored event,
    /// which is what a recording test cares about.
    fn stored_writes(dir: &DataDir) -> Vec<(RangeInclusive<u32>, u32, u32)> {
        stored_events(dir)
            .into_iter()
            .map(|event| (event.episode, event.progress_before, event.progress))
            .collect()
    }

    #[test]
    fn watching_past_half_the_episode_records_it_once() {
        let (_tmp, dir) = open_tmp();
        let mut app = Ryuuji::open(&dir).unwrap();
        app.dispatch(Command::AddEntry(entry("Show")));
        let id = app.state().library[0].id;

        app.dispatch(Command::Playback(playing("Show - 01.mkv")));
        assert_eq!(app.state().library[0].progress, 0);
        assert_eq!(outcome(&app), RecordOutcome::Counting);
        assert_eq!(stored_events(&dir), vec![]);

        app.dispatch(Command::Playback(later("Show - 01.mkv", 720)));
        assert_eq!(app.state().library[0].progress, 1);
        assert_eq!(stored_progress(&dir, id), 1);
        assert!(app.state().notices.is_empty());
        let events = stored_events(&dir);
        assert_eq!(events.len(), 1);
        assert_eq!(outcome(&app), RecordOutcome::Recorded(events[0].id));
        assert_eq!(events[0].entry, id);
        assert_eq!(events[0].episode, 1..=1);
        assert_eq!(events[0].progress_before, 0);
        assert_eq!(events[0].progress, 1);
        assert_eq!(events[0].raw_title, "Show - 01.mkv");
        assert_eq!(events[0].player, "mpv");
        assert_eq!(events[0].undone_at, None);

        app.dispatch(Command::Playback(later("Show - 01.mkv", 1_400)));
        assert_eq!(app.state().library[0].progress, 1);
        assert_eq!(stored_progress(&dir, id), 1);
        assert_eq!(stored_events(&dir), events);
        assert_eq!(outcome(&app), RecordOutcome::Recorded(events[0].id));
    }

    #[test]
    fn a_batch_records_its_last_episode() {
        let (_tmp, dir) = open_tmp();
        let mut app = Ryuuji::open(&dir).unwrap();
        app.dispatch(Command::AddEntry(entry("Show")));
        watch_past_threshold(&mut app, "Show - 01-12.mkv");
        assert_eq!(
            app.state().last_match.as_ref().unwrap().episode,
            Some(1..=12)
        );
        assert_eq!(app.state().library[0].progress, 12);
        assert_eq!(stored_writes(&dir), vec![(1..=12, 0, 12)]);
    }

    #[test]
    fn short_of_the_threshold_does_not_record() {
        let (_tmp, dir) = open_tmp();
        let mut app = Ryuuji::open(&dir).unwrap();
        app.dispatch(Command::AddEntry(entry("Show")));
        app.dispatch(Command::Playback(playing("Show - 01.mkv")));
        app.dispatch(Command::Playback(later("Show - 01.mkv", 700)));
        assert_eq!(app.state().library[0].progress, 0);
        assert_eq!(outcome(&app), RecordOutcome::Counting);
        assert_eq!(stored_events(&dir), vec![]);
    }

    #[test]
    fn a_likely_match_does_not_record() {
        let (_tmp, dir) = open_tmp();
        let mut app = Ryuuji::open(&dir).unwrap();
        app.dispatch(Command::AddEntry(entry("Frieren Beyond Journeys End")));
        watch_past_threshold(&mut app, "Frieren Beyond Journey End - 01.mkv");
        assert!(matches!(
            app.state().last_match.as_ref().unwrap().link,
            Link::Likely(_)
        ));
        assert_eq!(app.state().library[0].progress, 0);
        assert_eq!(outcome(&app), RecordOutcome::Declined(Decline::NotExact));
        assert_eq!(stored_events(&dir), vec![]);
    }

    #[test]
    fn a_completed_entry_records_only_when_rewatching() {
        let (_tmp, dir) = open_tmp();
        let mut app = Ryuuji::open(&dir).unwrap();
        app.dispatch(Command::AddEntry(entry("Show")));
        let id = app.state().library[0].id;
        app.dispatch(Command::SetStatus {
            id,
            status: WatchStatus::Completed,
        });
        watch_past_threshold(&mut app, "Show - 01.mkv");
        assert_eq!(app.state().library[0].progress, 0);
        assert_eq!(outcome(&app), RecordOutcome::Declined(Decline::Completed));
        assert_eq!(stored_events(&dir), vec![]);

        // Nothing sets the flag before M5, so it goes in underneath.
        app.store_mut()
            .execute_raw("UPDATE entries SET rewatching = 1");
        drop(app);
        let mut app = Ryuuji::open(&dir).unwrap();
        assert!(app.state().library[0].rewatching);
        watch_past_threshold(&mut app, "Show - 01.mkv");
        assert_eq!(app.state().library[0].progress, 1);
        assert_eq!(stored_writes(&dir), vec![(1..=1, 0, 1)]);
    }

    #[test]
    fn an_episode_other_than_the_next_does_not_record() {
        let (_tmp, dir) = open_tmp();
        let mut app = Ryuuji::open(&dir).unwrap();
        app.dispatch(Command::AddEntry(entry("Show")));
        let id = app.state().library[0].id;
        app.dispatch(Command::SetProgress { id, progress: 4 });

        for title in ["Show - 03.mkv", "Show - 04.mkv", "Show - 06.mkv"] {
            watch_past_threshold(&mut app, title);
            assert_eq!(app.state().library[0].progress, 4, "{title}");
            assert_eq!(
                outcome(&app),
                RecordOutcome::Declined(Decline::NotNext),
                "{title}"
            );
        }
        watch_past_threshold(&mut app, "Show.mkv");
        assert_eq!(app.state().library[0].progress, 4);
        assert_eq!(outcome(&app), RecordOutcome::Declined(Decline::NoEpisode));
        assert_eq!(stored_events(&dir), vec![]);

        watch_past_threshold(&mut app, "Show - 05.mkv");
        assert_eq!(app.state().library[0].progress, 5);
        assert_eq!(stored_writes(&dir), vec![(5..=5, 4, 5)]);
    }

    #[test]
    fn an_episode_past_a_known_total_does_not_record() {
        let (_tmp, dir) = open_tmp();
        let mut app = Ryuuji::open(&dir).unwrap();
        app.dispatch(Command::AddEntry(entry("Show")));
        let id = app.state().library[0].id;
        assert_eq!(app.state().library[0].total, Some(12));

        watch_past_threshold(&mut app, "Show - 01-13.mkv");
        assert_eq!(app.state().library[0].progress, 0);
        assert_eq!(outcome(&app), RecordOutcome::Declined(Decline::PastTotal));

        app.dispatch(Command::SetProgress { id, progress: 12 });
        watch_past_threshold(&mut app, "Show - 13.mkv");
        assert_eq!(app.state().library[0].progress, 12);
        assert_eq!(outcome(&app), RecordOutcome::Declined(Decline::PastTotal));
        assert_eq!(stored_events(&dir), vec![]);
    }

    #[test]
    fn an_unknown_total_lets_any_next_episode_record() {
        let (_tmp, dir) = open_tmp();
        let mut app = Ryuuji::open(&dir).unwrap();
        app.dispatch(Command::AddEntry(NewEntry {
            total: None,
            ..entry("Show")
        }));
        let id = app.state().library[0].id;
        app.dispatch(Command::SetProgress { id, progress: 12 });

        watch_past_threshold(&mut app, "Show - 13.mkv");
        assert_eq!(app.state().library[0].progress, 13);
        assert_eq!(stored_writes(&dir), vec![(13..=13, 12, 13)]);
    }

    #[test]
    fn a_failed_progress_write_notices_and_tries_again() {
        let (_tmp, dir) = open_tmp();
        let mut app = Ryuuji::open(&dir).unwrap();
        app.dispatch(Command::AddEntry(entry("Show")));
        app.dispatch(Command::Playback(playing("Show - 01.mkv")));
        app.store_mut().execute_raw("DROP TABLE watch_events");

        app.dispatch(Command::Playback(later("Show - 01.mkv", 720)));
        assert_eq!(app.state().library[0].progress, 0);
        assert_eq!(stored_progress(&dir, app.state().library[0].id), 0);
        // Past the threshold and still counting, which the shell shows as
        // `Recording in 0:00`: the next event tries again.
        let progress = app.state().watch_progress.unwrap();
        assert_eq!(progress.outcome, RecordOutcome::Counting);
        assert!(progress.accrued >= progress.threshold);
        assert!(matches!(
            app.state().notices.as_slice(),
            [Notice::SaveFailed { .. }]
        ));

        app.dispatch(Command::Playback(later("Show - 01.mkv", 730)));
        assert!(matches!(
            app.state().notices.as_slice(),
            [Notice::SaveFailed { .. }, Notice::SaveFailed { .. }]
        ));
    }

    /// Records episode 1 of a fresh "Show" and hands back the event id.
    fn record_first_episode(app: &mut Ryuuji, dir: &DataDir) -> WatchEventId {
        app.dispatch(Command::AddEntry(entry("Show")));
        watch_past_threshold(app, "Show - 01.mkv");
        assert_eq!(app.state().library[0].progress, 1);
        let events = stored_events(dir);
        assert_eq!(events.len(), 1);
        events[0].id
    }

    #[test]
    fn undo_restores_the_progress_in_memory_and_on_disk() {
        let (_tmp, dir) = open_tmp();
        let mut app = Ryuuji::open(&dir).unwrap();
        let event = record_first_episode(&mut app, &dir);
        let id = app.state().library[0].id;

        app.dispatch(Command::UndoRecording(event));
        assert_eq!(app.state().library[0].progress, 0);
        assert_eq!(stored_progress(&dir, id), 0);
        assert_eq!(outcome(&app), RecordOutcome::Undone);
        assert!(app.state().notices.is_empty());
        let events = stored_events(&dir);
        assert_eq!(events.len(), 1);
        assert!(events[0].undone_at.is_some());
    }

    #[test]
    fn a_batch_undoes_to_where_it_started() {
        let (_tmp, dir) = open_tmp();
        let mut app = Ryuuji::open(&dir).unwrap();
        app.dispatch(Command::AddEntry(entry("Show")));
        watch_past_threshold(&mut app, "Show - 01-12.mkv");
        assert_eq!(app.state().library[0].progress, 12);
        let event = stored_events(&dir)[0].id;

        app.dispatch(Command::UndoRecording(event));
        assert_eq!(app.state().library[0].progress, 0);
        assert_eq!(stored_progress(&dir, app.state().library[0].id), 0);
    }

    #[test]
    fn undoing_twice_notices_and_leaves_the_progress_alone() {
        let (_tmp, dir) = open_tmp();
        let mut app = Ryuuji::open(&dir).unwrap();
        let event = record_first_episode(&mut app, &dir);
        let id = app.state().library[0].id;
        app.dispatch(Command::UndoRecording(event));
        app.dispatch(Command::SetProgress { id, progress: 5 });

        app.dispatch(Command::UndoRecording(event));
        assert!(matches!(
            app.state().notices.as_slice(),
            [Notice::SaveFailed { detail }] if detail.contains("already undone")
        ));
        assert_eq!(app.state().library[0].progress, 5);
        assert_eq!(stored_progress(&dir, id), 5);
        assert_eq!(outcome(&app), RecordOutcome::Undone);
    }

    #[test]
    fn watching_on_after_undo_does_not_record_again() {
        let (_tmp, dir) = open_tmp();
        let mut app = Ryuuji::open(&dir).unwrap();
        let event = record_first_episode(&mut app, &dir);
        app.dispatch(Command::UndoRecording(event));

        app.dispatch(Command::Playback(later("Show - 01.mkv", 1_400)));
        assert_eq!(app.state().library[0].progress, 0);
        assert_eq!(outcome(&app), RecordOutcome::Undone);
        assert_eq!(stored_events(&dir).len(), 1);
    }

    #[test]
    fn a_relaunch_and_rewatch_after_undo_records_a_second_row() {
        let (_tmp, dir) = open_tmp();
        let mut app = Ryuuji::open(&dir).unwrap();
        let event = record_first_episode(&mut app, &dir);
        app.dispatch(Command::UndoRecording(event));
        drop(app);

        let mut app = Ryuuji::open(&dir).unwrap();
        app.dispatch(Command::Playback(later("Show - 01.mkv", 2_000)));
        app.dispatch(Command::Playback(later("Show - 01.mkv", 2_720)));
        assert_eq!(app.state().library[0].progress, 1);
        let events = stored_events(&dir);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].id, event);
        assert!(events[0].undone_at.is_some());
        assert_eq!(events[1].undone_at, None);
        assert_eq!(stored_writes(&dir), vec![(1..=1, 0, 1), (1..=1, 0, 1)]);
        assert_eq!(outcome(&app), RecordOutcome::Recorded(events[1].id));
    }

    /// `recorded` lives in memory only. On relaunch `resume` seeds the key
    /// from the persisted match so the title is not re-proposed, and the
    /// accrual starts over; what stops a second write is the range gate,
    /// since `progress + 1` is no longer in `1..=1`.
    #[test]
    fn a_relaunch_under_the_same_title_does_not_record_again() {
        let (_tmp, dir) = open_tmp();
        let mut app = Ryuuji::open(&dir).unwrap();
        app.dispatch(Command::AddEntry(entry("Show")));
        let id = app.state().library[0].id;
        watch_past_threshold(&mut app, "Show - 01.mkv");
        assert_eq!(app.state().library[0].progress, 1);
        let proposed = app.state().last_match.clone();
        drop(app);

        let mut app = Ryuuji::open(&dir).unwrap();
        assert_eq!(app.state().watch_progress, None);
        app.dispatch(Command::Playback(later("Show - 01.mkv", 2_000)));
        app.dispatch(Command::Playback(later("Show - 01.mkv", 2_720)));
        assert_eq!(app.state().last_match, proposed);
        assert!(app.state().watch_progress.unwrap().accrued >= Duration::from_secs(710));
        assert_eq!(outcome(&app), RecordOutcome::Declined(Decline::NotNext));
        assert_eq!(app.state().library[0].progress, 1);
        assert_eq!(stored_progress(&dir, id), 1);
        assert_eq!(stored_writes(&dir), vec![(1..=1, 0, 1)]);
        assert!(app.state().notices.is_empty());
    }
}
