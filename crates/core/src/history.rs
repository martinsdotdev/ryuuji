//! The watch log: one row per viewing that reached its threshold, whether
//! it recorded an episode or was declined, plus a viewing whose show was
//! added to the library first. Rows are never deleted; an undo marks its
//! recording and leaves the row where it was.

use std::fmt;
use std::ops::RangeInclusive;
use std::time::SystemTime;

use jiff::tz::TimeZone;
use jiff::{Timestamp, ToSpan, Zoned};

use crate::{Decline, EntryId, LibraryEntry, Link, ListStatus, ListUpdate};

/// Identity of a stored history row. Only [`Store`](crate::Store) mints
/// these.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct HistoryId(pub(crate) i64);

impl HistoryId {
    pub fn as_i64(self) -> i64 {
        self.0
    }
}

impl fmt::Display for HistoryId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// A progress write that watching is about to cause. The titles and player
/// name the proposal it was decided on, since the last match is one row and
/// gets overwritten.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewRecording {
    /// The viewing's own row when it already has an unrecorded one, which
    /// the recording then fills.
    pub watch: Option<HistoryId>,
    pub entry: EntryId,
    /// Every episode the file carries; progress moves to its end.
    pub episode: RangeInclusive<u32>,
    pub raw_title: String,
    pub parsed_title: String,
    pub player: String,
}

/// One viewing in the watch log, as stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Watch {
    pub id: HistoryId,
    /// When the row was first written.
    pub at: SystemTime,
    pub raw_title: String,
    /// Empty for recordings made before the log kept it.
    pub parsed_title: String,
    pub player: String,
    pub episode: Option<RangeInclusive<u32>>,
    /// The proposal's link as of the row's last write.
    pub link: Link,
    pub outcome: WatchOutcome,
    /// When Add to library created the entry this watch links to.
    pub added_at: Option<SystemTime>,
    /// The show a correction moved this recording's episode onto. Set only
    /// on a row undone to make room for that correction, so the history can
    /// say where the episode went rather than only that it was taken back.
    pub moved_to: Option<EntryId>,
    /// The show whose recording this row corrects.
    pub moved_from: Option<EntryId>,
}

/// What a watch came to. A recording is final; a decline is rewritten while
/// the viewing goes on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WatchOutcome {
    /// The show was added to the library and nothing has recorded or
    /// declined since.
    Added,
    Declined(Decline),
    Recorded(Recorded),
    /// The person said not to track this file. Unlike a decline, no gate
    /// refused it, so nothing about the library or the episode can change
    /// it back; only saying to stop ignoring does.
    Ignored,
}

/// The progress write a recorded watch made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Recorded {
    pub progress_before: u32,
    pub progress: u32,
    pub at: SystemTime,
    /// Undoing marks the row and puts `progress_before` back.
    pub undone_at: Option<SystemTime>,
    /// Where the recording moved the show, when it moved it. Undoing puts
    /// `before` back while the show still stands at `after`. `None` for a
    /// recording that moved nothing, and for every row from before the
    /// columns existed.
    pub status: Option<StatusMove>,
}

/// A status a recording moved: where the show stood and where the episode
/// put it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StatusMove {
    pub before: ListStatus,
    pub after: ListStatus,
}

impl StatusMove {
    /// The move from one standing of a show to another, or `None` when the
    /// status did not change.
    pub(crate) fn between(before: &LibraryEntry, after: &LibraryEntry) -> Option<StatusMove> {
        let (before, after) = (ListStatus::of(before), ListStatus::of(after));
        (before != after).then_some(StatusMove { before, after })
    }
}

/// The newest watches, the values taken from a list over the same stretch
/// of time, and whether older watches lie past the limit.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct HistoryPage {
    pub watches: Vec<Watch>,
    /// Newest first. [`Store::history`](crate::Store::history) says which
    /// updates a page carries.
    pub updates: Vec<ListUpdate>,
    pub more: bool,
}

/// When a show last had an episode recorded that still stands, and in which
/// player: what Continue watching says about it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LastWatched {
    pub entry: EntryId,
    pub player: String,
    pub at: SystemTime,
}

impl Watch {
    /// The time a row shows: when it recorded, else when it was written.
    pub fn shown_at(&self) -> SystemTime {
        match self.outcome {
            WatchOutcome::Recorded(recorded) => recorded.at,
            WatchOutcome::Added | WatchOutcome::Declined(_) | WatchOutcome::Ignored => self.at,
        }
    }

    /// Whether Undo may be offered: a recording not yet undone whose entry
    /// still stands at the progress it wrote. The store refuses any other
    /// undo, so this only keeps the button from promising one.
    pub fn undoable(&self, library: &[LibraryEntry]) -> bool {
        let WatchOutcome::Recorded(recorded) = self.outcome else {
            return false;
        };
        recorded.undone_at.is_none()
            && self.link.entry().is_some_and(|id| {
                library
                    .iter()
                    .any(|entry| entry.id == id && entry.progress == recorded.progress)
            })
    }
}

/// One row of the History page: a watch, or a value the library took from
/// a list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Line<'a> {
    Watch(&'a Watch),
    Update(&'a ListUpdate),
}

impl Line<'_> {
    /// The time the line shows at.
    pub fn at(&self) -> SystemTime {
        match self {
            Line::Watch(watch) => watch.shown_at(),
            Line::Update(update) => update.at,
        }
    }
}

/// Lines that show under the same local day, newest first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Day<'a> {
    /// Today, Yesterday, or the weekday and date.
    pub label: String,
    pub lines: Vec<Line<'a>>,
}

/// Groups a page under the local days its lines show at, the updates
/// slotted among the watches by time. Both come newest first, so the days
/// do too; a run of lines on one date is one day.
pub fn days<'a>(
    watches: &'a [Watch],
    updates: &'a [ListUpdate],
    now: SystemTime,
    zone: &TimeZone,
) -> Vec<Day<'a>> {
    let today = local(now, zone).date();
    let mut days: Vec<(jiff::civil::Date, Day<'a>)> = Vec::new();
    for line in interleave(watches, updates) {
        let shown = local(line.at(), zone);
        match days.last_mut() {
            Some((date, day)) if *date == shown.date() => day.lines.push(line),
            _ => days.push((
                shown.date(),
                Day {
                    label: day_label(&shown, today),
                    lines: vec![line],
                },
            )),
        }
    }
    days.into_iter().map(|(_, day)| day).collect()
}

/// Merges two newest-first lists into one, each keeping its own order. An
/// update goes above a watch only when it is strictly newer, so the two in
/// one second read as the watch first.
fn interleave<'a>(watches: &'a [Watch], updates: &'a [ListUpdate]) -> Vec<Line<'a>> {
    let mut lines = Vec::with_capacity(watches.len() + updates.len());
    let mut watches = watches.iter().peekable();
    let mut updates = updates.iter().peekable();
    loop {
        let update_first = match (watches.peek(), updates.peek()) {
            (Some(watch), Some(update)) => update.at > watch.shown_at(),
            (None, Some(_)) => true,
            (Some(_), None) => false,
            (None, None) => break,
        };
        let line = if update_first {
            updates.next().map(Line::Update)
        } else {
            watches.next().map(Line::Watch)
        };
        lines.extend(line);
    }
    lines
}

/// `23:14`: the local 24-hour clock time of `at`.
pub fn time_of_day(at: SystemTime, zone: &TimeZone) -> String {
    local(at, zone).strftime("%H:%M").to_string()
}

/// `2026-09-15 23:14`: the local date and 24-hour time of `at`, for the
/// places that list rows from more than one day without grouping them.
pub fn date_time(at: SystemTime, zone: &TimeZone) -> String {
    local(at, zone).strftime("%Y-%m-%d %H:%M").to_string()
}

/// `2026-09-15`: the local date of `at` alone, for a day that matters
/// without its clock time, such as the one a token stops working on.
pub fn date(at: SystemTime, zone: &TimeZone) -> String {
    local(at, zone).strftime("%Y-%m-%d").to_string()
}

/// The first instant of "the last 7 days": local midnight six days before
/// today, so the span covers today and the six days before it, the same
/// days the page groups under.
pub fn week_start(now: SystemTime, zone: &TimeZone) -> SystemTime {
    let now = local(now, zone);
    now.checked_sub(6.days())
        .and_then(|day| day.start_of_day())
        .map_or(SystemTime::UNIX_EPOCH, |start| {
            SystemTime::from(start.timestamp())
        })
}

fn day_label(shown: &Zoned, today: jiff::civil::Date) -> String {
    let date = shown.date();
    if date == today {
        "Today".to_owned()
    } else if today.yesterday().is_ok_and(|yesterday| yesterday == date) {
        "Yesterday".to_owned()
    } else if date.year() == today.year() {
        shown.strftime("%A, %-d %B").to_string()
    } else {
        shown.strftime("%A, %-d %B %Y").to_string()
    }
}

/// `at` in `zone`. A time past what jiff can represent, which only a broken
/// row could hold, reads as the last instant it can.
fn local(at: SystemTime, zone: &TimeZone) -> Zoned {
    Timestamp::try_from(at)
        .unwrap_or(Timestamp::MAX)
        .to_zoned(zone.clone())
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use super::*;
    use crate::{ListUpdateId, Service, WatchStatus};

    fn utc(text: &str) -> SystemTime {
        SystemTime::from(text.parse::<Timestamp>().unwrap())
    }

    fn zone(posix: &str) -> TimeZone {
        TimeZone::posix(posix).unwrap()
    }

    fn label(at: &str, now: &str, posix: &str) -> String {
        let zone = zone(posix);
        day_label(&local(utc(at), &zone), local(utc(now), &zone).date())
    }

    #[test]
    fn days_read_today_yesterday_then_weekday_and_date() {
        let now = "2026-09-15T10:00:00Z";
        assert_eq!(label("2026-09-15T00:00:00Z", now, "UTC0"), "Today");
        assert_eq!(label("2026-09-14T23:59:59Z", now, "UTC0"), "Yesterday");
        assert_eq!(
            label("2026-09-12T16:20:00Z", now, "UTC0"),
            "Saturday, 12 September"
        );
        assert_eq!(
            label("2025-12-28T20:00:00Z", "2026-01-02T09:00:00Z", "UTC0"),
            "Sunday, 28 December 2025"
        );
    }

    #[test]
    fn date_time_prints_the_local_date_and_clock() {
        assert_eq!(
            date_time(utc("2026-09-15T02:00:00Z"), &zone("<-03>3")),
            "2026-09-14 23:00"
        );
        assert_eq!(
            date_time(utc("2026-09-15T21:36:00Z"), &zone("UTC0")),
            "2026-09-15 21:36"
        );
    }

    #[test]
    fn a_date_is_the_local_day_alone() {
        assert_eq!(
            date(utc("2026-09-15T02:00:00Z"), &zone("<-03>3")),
            "2026-09-14"
        );
        assert_eq!(
            date(utc("2026-09-15T21:36:00Z"), &zone("UTC0")),
            "2026-09-15"
        );
    }

    #[test]
    fn a_day_is_the_local_one_not_the_utc_one() {
        let now = "2026-09-15T12:00:00Z";
        assert_eq!(label("2026-09-15T02:00:00Z", now, "UTC0"), "Today");
        assert_eq!(label("2026-09-15T02:00:00Z", now, "<-03>3"), "Yesterday");
        assert_eq!(
            time_of_day(utc("2026-09-15T02:00:00Z"), &zone("<-03>3")),
            "23:00"
        );
    }

    #[test]
    fn the_repeated_hour_after_a_fall_back_reads_as_the_clock_showed_it() {
        let eastern = zone("EST5EDT,M3.2.0,M11.1.0");
        assert_eq!(time_of_day(utc("2026-11-01T05:30:00Z"), &eastern), "01:30");
        assert_eq!(time_of_day(utc("2026-11-01T06:30:00Z"), &eastern), "01:30");
        assert_eq!(
            label(
                "2026-11-01T06:30:00Z",
                "2026-11-01T15:00:00Z",
                "EST5EDT,M3.2.0,M11.1.0"
            ),
            "Today"
        );
    }

    #[test]
    fn the_last_seven_days_start_at_local_midnight_six_days_back() {
        assert_eq!(
            week_start(utc("2026-09-15T10:00:00Z"), &zone("UTC0")),
            utc("2026-09-09T00:00:00Z")
        );
        assert_eq!(
            week_start(utc("2026-09-15T02:00:00Z"), &zone("<-03>3")),
            utc("2026-09-08T03:00:00Z")
        );
        // Six days before 3 November is 28 October, still on daylight time.
        assert_eq!(
            week_start(utc("2026-11-03T12:00:00Z"), &zone("EST5EDT,M3.2.0,M11.1.0")),
            utc("2026-10-28T04:00:00Z")
        );
    }

    fn watch_at(text: &str, id: i64) -> Watch {
        Watch {
            id: HistoryId(id),
            at: utc(text),
            outcome: WatchOutcome::Declined(Decline::NotNext),
            ..recorded(2, None)
        }
    }

    fn update_at(text: &str, id: i64) -> ListUpdate {
        ListUpdate {
            id: ListUpdateId(id),
            entry: EntryId(3),
            service: Service::AniList,
            progress_before: 3,
            progress: 5,
            status_before: ListStatus::Watching,
            status: ListStatus::Watching,
            at: utc(text),
        }
    }

    /// `(label, [line ids])` per day; a watch's id as is, an update's
    /// negated, so one list tells them apart.
    fn summary<'d>(days: &'d [Day<'_>]) -> Vec<(&'d str, Vec<i64>)> {
        days.iter()
            .map(|day| {
                let ids = day
                    .lines
                    .iter()
                    .map(|line| match line {
                        Line::Watch(watch) => watch.id.as_i64(),
                        Line::Update(update) => -update.id.as_i64(),
                    })
                    .collect();
                (day.label.as_str(), ids)
            })
            .collect()
    }

    #[test]
    fn days_group_a_page_by_the_local_date_each_row_shows() {
        let page = [
            watch_at("2026-09-15T08:00:00Z", 4),
            watch_at("2026-09-15T01:00:00Z", 3),
            watch_at("2026-09-14T22:00:00Z", 2),
            watch_at("2026-09-12T16:20:00Z", 1),
        ];
        let grouped = days(&page, &[], utc("2026-09-15T10:00:00Z"), &zone("UTC0"));
        assert_eq!(
            summary(&grouped),
            vec![
                ("Today", vec![4, 3]),
                ("Yesterday", vec![2]),
                ("Saturday, 12 September", vec![1]),
            ]
        );
    }

    #[test]
    fn updates_slot_among_a_days_watches_by_time() {
        let watches = [
            watch_at("2026-09-15T08:00:00Z", 4),
            watch_at("2026-09-15T01:00:00Z", 3),
        ];
        let updates = [
            update_at("2026-09-15T09:00:00Z", 12),
            update_at("2026-09-15T05:00:00Z", 11),
            update_at("2026-09-15T01:00:00Z", 10),
        ];
        let grouped = days(
            &watches,
            &updates,
            utc("2026-09-15T10:00:00Z"),
            &zone("UTC0"),
        );
        // The one in the watch's own second reads after the watch.
        assert_eq!(
            summary(&grouped),
            vec![("Today", vec![-12, 4, -11, 3, -10])]
        );
    }

    #[test]
    fn an_update_on_a_day_with_no_watch_makes_its_own_day() {
        let watches = [
            watch_at("2026-09-15T08:00:00Z", 2),
            watch_at("2026-09-12T16:20:00Z", 1),
        ];
        let updates = [update_at("2026-09-13T12:00:00Z", 10)];
        let grouped = days(
            &watches,
            &updates,
            utc("2026-09-15T10:00:00Z"),
            &zone("UTC0"),
        );
        assert_eq!(
            summary(&grouped),
            vec![
                ("Today", vec![2]),
                ("Sunday, 13 September", vec![-10]),
                ("Saturday, 12 September", vec![1]),
            ]
        );
        let alone = days(&[], &updates, utc("2026-09-15T10:00:00Z"), &zone("UTC0"));
        assert_eq!(summary(&alone), vec![("Sunday, 13 September", vec![-10])]);
        assert_eq!(
            days(&[], &[], utc("2026-09-15T10:00:00Z"), &zone("UTC0")),
            vec![]
        );
    }

    fn secs(n: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(n)
    }

    fn show(progress: u32) -> LibraryEntry {
        LibraryEntry {
            id: EntryId(3),
            title: "Show".into(),
            status: WatchStatus::Watching,
            progress,
            total: None,
            rewatching: false,
            rewatched: 0,
        }
    }

    fn recorded(progress: u32, undone_at: Option<SystemTime>) -> Watch {
        Watch {
            id: HistoryId(1),
            at: secs(100),
            raw_title: "Show - 02.mkv".into(),
            parsed_title: "Show".into(),
            player: "mpv".into(),
            episode: Some(progress..=progress),
            link: Link::Exact(EntryId(3)),
            outcome: WatchOutcome::Recorded(Recorded {
                progress_before: progress - 1,
                progress,
                at: secs(160),
                undone_at,
                status: None,
            }),
            added_at: None,
            moved_to: None,
            moved_from: None,
        }
    }

    #[test]
    fn undo_is_offered_only_while_the_entry_stands_at_the_recording() {
        assert!(recorded(2, None).undoable(&[show(2)]));
        assert!(!recorded(2, None).undoable(&[show(3)]));
        assert!(!recorded(2, Some(secs(200))).undoable(&[show(1)]));
        assert!(!recorded(2, Some(secs(200))).undoable(&[show(2)]));
        assert!(!recorded(2, None).undoable(&[]));
        let declined = Watch {
            outcome: WatchOutcome::Declined(Decline::NotNext),
            ..recorded(2, None)
        };
        assert!(!declined.undoable(&[show(2)]));
    }

    #[test]
    fn a_row_shows_when_it_recorded_or_else_when_it_was_written() {
        assert_eq!(recorded(2, None).shown_at(), secs(160));
        let added = Watch {
            outcome: WatchOutcome::Added,
            ..recorded(2, None)
        };
        assert_eq!(added.shown_at(), secs(100));
    }
}
