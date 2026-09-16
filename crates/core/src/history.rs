//! The watch log: one row per viewing that reached its threshold, whether
//! it recorded an episode or was declined, plus a viewing whose show was
//! added to the library first. Rows are never deleted; an undo marks its
//! recording and leaves the row where it was.

use std::fmt;
use std::ops::RangeInclusive;
use std::time::SystemTime;

use jiff::tz::TimeZone;
use jiff::{Timestamp, ToSpan, Zoned};

use crate::{Decline, EntryId, LibraryEntry, Link};

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
}

/// The newest watches, and whether older ones lie past the limit.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct HistoryPage {
    pub watches: Vec<Watch>,
    pub more: bool,
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

/// Watches that show under the same local day, in the page's order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Day<'a> {
    /// Today, Yesterday, or the weekday and date.
    pub label: String,
    pub watches: Vec<&'a Watch>,
}

/// Groups a page of watches under the local days they show at. A run of
/// watches on one date is one day; the page is newest first, so the days
/// are too.
pub fn days<'a>(watches: &'a [Watch], now: SystemTime, zone: &TimeZone) -> Vec<Day<'a>> {
    let today = local(now, zone).date();
    let mut days: Vec<(jiff::civil::Date, Day<'a>)> = Vec::new();
    for watch in watches {
        let shown = local(watch.shown_at(), zone);
        match days.last_mut() {
            Some((date, day)) if *date == shown.date() => day.watches.push(watch),
            _ => days.push((
                shown.date(),
                Day {
                    label: day_label(&shown, today),
                    watches: vec![watch],
                },
            )),
        }
    }
    days.into_iter().map(|(_, day)| day).collect()
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
    use crate::WatchStatus;

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

    #[test]
    fn days_group_a_page_by_the_local_date_each_row_shows() {
        let at = |text: &str, id: i64| Watch {
            id: HistoryId(id),
            at: utc(text),
            outcome: WatchOutcome::Declined(Decline::NotNext),
            ..recorded(2, None)
        };
        let page = [
            at("2026-09-15T08:00:00Z", 4),
            at("2026-09-15T01:00:00Z", 3),
            at("2026-09-14T22:00:00Z", 2),
            at("2026-09-12T16:20:00Z", 1),
        ];
        let grouped = days(&page, utc("2026-09-15T10:00:00Z"), &zone("UTC0"));
        let summary: Vec<(&str, Vec<i64>)> = grouped
            .iter()
            .map(|day| {
                (
                    day.label.as_str(),
                    day.watches.iter().map(|watch| watch.id.as_i64()).collect(),
                )
            })
            .collect();
        assert_eq!(
            summary,
            vec![
                ("Today", vec![4, 3]),
                ("Yesterday", vec![2]),
                ("Saturday, 12 September", vec![1]),
            ]
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
            }),
            added_at: None,
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
