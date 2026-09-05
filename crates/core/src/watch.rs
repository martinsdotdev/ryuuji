//! The viewing a run of playback events describes.
//!
//! Detection reports one player at a time and repeats itself; the core
//! wants to know when a different title starts. [`WatchSession`] keeps
//! what that takes across events: which player and title are being watched,
//! and where the player last was.

use std::time::{Duration, SystemTime};

use crate::{PlaybackEvent, PlaybackStatus, ProposedMatch};

/// Across-events memory of what is being watched. The key is the viewing's
/// identity. The cursor is where the player last was, and it advances on
/// every event, Stopped and untitled ones included, because a position is
/// only a delta against the one before it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct WatchSession {
    key: Option<(String, String)>,
    last_position: Duration,
    last_seen: Option<SystemTime>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Observed {
    /// A titled player other than the one being watched.
    NewViewing,
    Continues,
}

impl WatchSession {
    /// Picks up the viewing the persisted match describes, so a relaunch
    /// under a still-open player does not propose the same title again.
    pub(crate) fn resume(last_match: Option<&ProposedMatch>) -> WatchSession {
        WatchSession {
            key: last_match.map(|m| (m.player.clone(), m.raw_title.clone())),
            ..WatchSession::default()
        }
    }

    pub(crate) fn observe(&mut self, event: &PlaybackEvent) -> Observed {
        self.last_position = event.position;
        self.last_seen = Some(event.observed_at);
        let titled = matches!(
            event.status,
            PlaybackStatus::Playing | PlaybackStatus::Paused
        ) && !event.title.trim().is_empty();
        if !titled {
            return Observed::Continues;
        }
        let same = self
            .key
            .as_ref()
            .is_some_and(|(player, title)| *player == event.player && *title == event.title);
        if same {
            return Observed::Continues;
        }
        self.key = Some((event.player.clone(), event.title.clone()));
        Observed::NewViewing
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Link, PlaybackSource};

    fn at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn event(title: &str, status: PlaybackStatus, position: u64, seen: u64) -> PlaybackEvent {
        PlaybackEvent {
            player: "mpv".into(),
            title: title.into(),
            status,
            position: Duration::from_secs(position),
            duration: Duration::from_secs(1420),
            observed_at: at(seen),
            source: PlaybackSource::Detected,
        }
    }

    fn playing(title: &str) -> PlaybackEvent {
        event(title, PlaybackStatus::Playing, 305, 1_000)
    }

    #[test]
    fn first_titled_event_starts_a_viewing() {
        let mut session = WatchSession::default();
        assert_eq!(
            session.observe(&playing("Show - 03.mkv")),
            Observed::NewViewing
        );
        let mut paused = WatchSession::default();
        assert_eq!(
            paused.observe(&event("Show - 03.mkv", PlaybackStatus::Paused, 0, 1_000)),
            Observed::NewViewing
        );
    }

    #[test]
    fn same_title_continues_the_viewing() {
        let mut session = WatchSession::default();
        session.observe(&playing("Show - 03.mkv"));
        assert_eq!(
            session.observe(&event("Show - 03.mkv", PlaybackStatus::Playing, 400, 1_060)),
            Observed::Continues
        );
        assert_eq!(
            session.observe(&event("Show - 03.mkv", PlaybackStatus::Paused, 400, 1_070)),
            Observed::Continues
        );
    }

    #[test]
    fn title_change_starts_a_new_viewing() {
        let mut session = WatchSession::default();
        session.observe(&playing("Show - 03.mkv"));
        assert_eq!(
            session.observe(&playing("Show - 04.mkv")),
            Observed::NewViewing
        );
        assert_eq!(
            session.observe(&playing("Show - 04.mkv")),
            Observed::Continues
        );
    }

    #[test]
    fn player_change_starts_a_new_viewing() {
        let mut session = WatchSession::default();
        session.observe(&playing("Show - 03.mkv"));
        let other = PlaybackEvent {
            player: "vlc".into(),
            ..playing("Show - 03.mkv")
        };
        assert_eq!(session.observe(&other), Observed::NewViewing);
        assert_eq!(session.observe(&other), Observed::Continues);
    }

    #[test]
    fn blank_title_neither_starts_nor_ends_a_viewing() {
        let mut session = WatchSession::default();
        assert_eq!(session.observe(&playing("")), Observed::Continues);
        assert_eq!(session.observe(&playing("  \t")), Observed::Continues);
        assert_eq!(session.key, None);

        session.observe(&playing("Show - 03.mkv"));
        assert_eq!(session.observe(&playing("")), Observed::Continues);
        assert_eq!(
            session.observe(&playing("Show - 03.mkv")),
            Observed::Continues
        );
    }

    #[test]
    fn stopped_keeps_the_cursor() {
        let mut session = WatchSession::default();
        session.observe(&playing("Show - 03.mkv"));
        let stopped = event("Show - 03.mkv", PlaybackStatus::Stopped, 1_400, 2_100);
        assert_eq!(session.observe(&stopped), Observed::Continues);
        assert_eq!(session.last_position, Duration::from_secs(1_400));
        assert_eq!(session.last_seen, Some(at(2_100)));
        // Stopped does not end the viewing either: the same title coming
        // back is the same viewing.
        assert_eq!(
            session.observe(&playing("Show - 03.mkv")),
            Observed::Continues
        );
    }

    #[test]
    fn cursor_follows_the_last_position_and_time() {
        let mut session = WatchSession::default();
        assert_eq!(session.last_position, Duration::ZERO);
        assert_eq!(session.last_seen, None);

        session.observe(&event("Show - 03.mkv", PlaybackStatus::Playing, 305, 1_000));
        assert_eq!(session.last_position, Duration::from_secs(305));
        assert_eq!(session.last_seen, Some(at(1_000)));

        session.observe(&event("Show - 03.mkv", PlaybackStatus::Paused, 610, 1_305));
        assert_eq!(session.last_position, Duration::from_secs(610));
        assert_eq!(session.last_seen, Some(at(1_305)));

        session.observe(&event("", PlaybackStatus::Playing, 7, 1_400));
        assert_eq!(session.last_position, Duration::from_secs(7));
        assert_eq!(session.last_seen, Some(at(1_400)));
    }

    #[test]
    fn resume_seeds_the_key_from_a_persisted_match() {
        let persisted = ProposedMatch {
            raw_title: "Show - 03.mkv".into(),
            parsed_title: "Show".into(),
            episode: Some(3..=3),
            season: None,
            release_group: None,
            link: Link::Unmatched,
            player: "mpv".into(),
            at: at(900),
        };
        let mut session = WatchSession::resume(Some(&persisted));
        assert_eq!(
            session.observe(&playing("Show - 03.mkv")),
            Observed::Continues
        );
        assert_eq!(
            session.observe(&playing("Show - 04.mkv")),
            Observed::NewViewing
        );

        let mut empty = WatchSession::resume(None);
        assert_eq!(empty, WatchSession::default());
        assert_eq!(
            empty.observe(&playing("Show - 03.mkv")),
            Observed::NewViewing
        );
    }
}
