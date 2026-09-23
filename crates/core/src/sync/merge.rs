//! The one place a difference between the library and a list is settled.
//! A reconcile is a read, this merge, an optional write and a report; the
//! settlement then reads the report back into the library field by field.
//! Both are pure, so the rule is pinned here and the worker only carries
//! values.

use crate::matching::normalize_title;
use crate::{
    Candidate, LibraryEntry, ListBase, ListState, ListStatus, Listing, Media, Remote, UserId,
};

/// What one reconcile does with a show: the base the merge measured
/// against, and the write that brings the list to the merged state when
/// one is needed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Merge {
    pub base: ListState,
    pub write: Option<Write>,
}

/// The write a merge asks for: the show added to the list, or fields of its
/// row changed. Every value is absolute, never an increment, so a write that
/// runs twice lands the same.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Write {
    Create(ListState),
    Change(Patch),
}

/// The fields of a list row to change, each `None` where the list already
/// holds what was merged.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Patch {
    pub status: Option<ListStatus>,
    pub progress: Option<u32>,
    pub repeat: Option<u32>,
}

impl Patch {
    /// The fields where `to` differs from `from`; `None` when none does.
    pub fn between(from: ListState, to: ListState) -> Option<Patch> {
        let patch = Patch {
            status: (to.status != from.status).then_some(to.status),
            progress: (to.progress != from.progress).then_some(to.progress),
            repeat: (to.repeat != from.repeat).then_some(to.repeat),
        };
        (patch != Patch::default()).then_some(patch)
    }

    /// `state` with the patch's fields changed.
    pub fn apply(self, state: ListState) -> ListState {
        ListState {
            status: self.status.unwrap_or(state.status),
            progress: self.progress.unwrap_or(state.progress),
            repeat: self.repeat.unwrap_or(state.repeat),
        }
    }
}

/// Settles what the library wants for a show against what the list holds,
/// measured from the base both were last confirmed to agree on. A show the
/// list lacks is held until it next changes in Ryuuji, so signing in does
/// not pour the whole library onto the list: with no base the wanted state
/// becomes the base, and a later change against that base is what creates
/// the row. A listed show is merged three ways; one with no base yet starts
/// from a first merge, where the side further along wins.
pub fn merge(wanted: ListState, base: Option<ListState>, remote: &Remote) -> Merge {
    match *remote {
        Remote::Unlisted => match base {
            Some(base) if base != wanted => Merge {
                base,
                write: Some(Write::Create(wanted)),
            },
            Some(base) => Merge { base, write: None },
            None => Merge {
                base: wanted,
                write: None,
            },
        },
        Remote::Listed { state: held, .. } => {
            let base = base.unwrap_or_else(|| first_base(wanted, held));
            let merged = three_way(wanted, base, held);
            Merge {
                base,
                write: Patch::between(held, merged).map(Write::Change),
            }
        }
    }
}

/// The base for a show with none: the side not further along, so the merge
/// reads the other side as the one that moved. The library ahead sends its
/// status and progress; the list ahead, or level, has its status and
/// progress taken. The repeat count is always the list's, since Ryuuji has
/// counted no rewatch of a show it has not yet kept in step.
fn first_base(wanted: ListState, held: ListState) -> ListState {
    let side = if wanted.progress > held.progress {
        held
    } else {
        wanted
    };
    ListState {
        repeat: wanted.repeat,
        ..side
    }
}

/// A field only one side moved is that side's. A field both moved goes to
/// the higher progress, since a recording never lowers and a list ahead is
/// taken; to the higher repeat count on the same reasoning; and to the
/// status of the side further along, Ryuuji's on a tie.
fn three_way(l: ListState, b: ListState, r: ListState) -> ListState {
    let status = if r.progress > l.progress {
        r.status
    } else {
        l.status
    };
    ListState {
        status: field(l.status, b.status, r.status, status),
        progress: field(
            l.progress,
            b.progress,
            r.progress,
            l.progress.max(r.progress),
        ),
        repeat: field(l.repeat, b.repeat, r.repeat, l.repeat.max(r.repeat)),
    }
}

fn field<T: PartialEq>(l: T, b: T, r: T, conflict: T) -> T {
    if l == b {
        r
    } else if r == b || l == r {
        l
    } else {
        conflict
    }
}

/// What a reconcile job asked the list with: the state the library wanted
/// when the job was built, and the base its merge measured against. The
/// settlement reads both, since the library may have moved on since.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Asked {
    pub wanted: ListState,
    pub base: ListState,
}

/// What a report settles into: the fields the library takes from the list,
/// and the base the next merge measures against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Settlement {
    pub take_status: Option<ListStatus>,
    pub take_progress: Option<u32>,
    pub base: ListBase,
    /// The listing's offset, moved when the repeat count is taken.
    pub repeat_offset: i64,
}

/// Settles what the list holds after a job, field by field, against what
/// the job asked with. Where the list holds what Ryuuji asked for, that is
/// the base. Where it holds something else, the list moved the field (a
/// change made on the site, or the service adjusting what was sent) and the
/// library takes it, unless the library changed that field since the job
/// was built: then nothing is taken, the field's base stays where the job's
/// was, and the next reconcile weighs both sides again. Recording 4 while
/// the list holds 5 and undoing to 3 before the report lands is that case:
/// the base stays 3, the next merge takes 5, and 3 is never sent. A show
/// the list lacks keeps the base the job asked with, as unlisted. The
/// repeat count is taken by moving the listing's offset, never the entry's
/// own count; on a first merge for `user` the offset was another account's
/// list's, or nothing, so it starts over from nothing unless a repeat is
/// taken.
pub(crate) fn settlement(
    asked: &Asked,
    remote: &Remote,
    user: UserId,
    entry: &LibraryEntry,
    listing: &Listing,
) -> Settlement {
    let kept = if listing.base_for(user).is_some() {
        listing.repeat_offset
    } else {
        0
    };
    let Remote::Listed { state: held, .. } = *remote else {
        return Settlement {
            take_status: None,
            take_progress: None,
            base: ListBase {
                user,
                listed: false,
                state: asked.base,
            },
            repeat_offset: kept,
        };
    };
    let now = listing.wanted(entry, user);
    let (take_status, status) = settle(
        held.status,
        asked.wanted.status,
        asked.base.status,
        now.status,
    );
    let (take_progress, progress) = settle(
        held.progress,
        asked.wanted.progress,
        asked.base.progress,
        now.progress,
    );
    let (take_repeat, repeat) = settle(
        held.repeat,
        asked.wanted.repeat,
        asked.base.repeat,
        now.repeat,
    );
    Settlement {
        take_status,
        take_progress,
        base: ListBase {
            user,
            listed: true,
            state: ListState {
                status,
                progress,
                repeat,
            },
        },
        repeat_offset: match take_repeat {
            Some(repeat) => i64::from(repeat) - i64::from(entry.rewatched),
            None => kept,
        },
    }
}

/// One field of a settlement: what the library takes, and what its base
/// becomes.
fn settle<T: Copy + PartialEq>(held: T, asked: T, base: T, now: T) -> (Option<T>, T) {
    if held == asked {
        (None, held)
    } else if now == asked {
        (Some(held), held)
    } else {
        (None, base)
    }
}

/// How a search settled a show: tied to the one candidate that fits, or
/// left to the person with how many were found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AutoLink {
    Link(Media),
    Pick { found: u32 },
}

/// Ties a show to a search result on its own when exactly one fits: a
/// candidate whose title or synonym folds to the entry's title, with an
/// episode count that agrees with the entry's total when that is known, or
/// at least reaches its progress when it is not. Fruits Basket 2001 and 2019
/// share their titles, so with no total both fit and the show waits for the
/// person; a total of 25 tells them apart.
pub(crate) fn auto_link(entry: &LibraryEntry, found: &[Candidate]) -> AutoLink {
    let title = normalize_title(&entry.title);
    let fits = |candidate: &&Candidate| {
        let named = std::iter::once(&candidate.media.title)
            .chain(&candidate.titles)
            .any(|name| normalize_title(name) == title);
        let counted = match (candidate.media.episodes, entry.total) {
            (None, _) => true,
            (Some(episodes), Some(total)) => episodes == total,
            (Some(episodes), None) => episodes >= entry.progress,
        };
        named && counted
    };
    let mut fitting = found.iter().filter(fits);
    match (fitting.next(), fitting.next()) {
        (Some(one), None) => AutoLink::Link(one.media.clone()),
        _ => AutoLink::Pick {
            found: u32::try_from(found.len()).unwrap_or(u32::MAX),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EntryId, LinkedBy, ListEntryId, MediaId, Service, Tie, WatchStatus};

    fn state(status: ListStatus, progress: u32, repeat: u32) -> ListState {
        ListState {
            status,
            progress,
            repeat,
        }
    }

    fn listed(state: ListState) -> Remote {
        Remote::Listed {
            id: Some(ListEntryId(99)),
            state,
        }
    }

    fn media(id: u64, title: &str, episodes: Option<u32>) -> Media {
        Media {
            id: MediaId(id),
            title: title.into(),
            episodes,
            mal_id: None,
        }
    }

    /// An entry and its listing standing at `state`: the entry holds the
    /// status and progress and has counted the rewatches itself, so the
    /// listing's offset is zero.
    fn standing(state: ListState) -> (LibraryEntry, Listing) {
        let (status, rewatching) = state.status.columns();
        let entry = LibraryEntry {
            id: EntryId(1),
            title: "Show".into(),
            status,
            progress: state.progress,
            total: None,
            rewatching,
            rewatched: state.repeat,
        };
        let listing = Listing {
            entry: EntryId(1),
            service: Service::AniList,
            tie: Tie::Linked {
                media: media(5, "Show", None),
                by: LinkedBy::Title,
            },
            base: None,
            repeat_offset: 0,
            parked: None,
        };
        (entry, listing)
    }

    /// The entry and listing as `Store::settle` leaves them, and what the
    /// listing then wants.
    fn settled(
        settlement: &Settlement,
        entry: &LibraryEntry,
        listing: &Listing,
    ) -> (LibraryEntry, Listing, ListState) {
        let mut entry = entry.clone();
        if let Some(status) = settlement.take_status {
            (entry.status, entry.rewatching) = status.columns();
        }
        if let Some(progress) = settlement.take_progress {
            entry.progress = progress;
        }
        let listing = Listing {
            base: Some(settlement.base),
            repeat_offset: settlement.repeat_offset,
            ..listing.clone()
        };
        let wanted = listing.wanted(&entry, settlement.base.user);
        (entry, listing, wanted)
    }

    fn progress_written(merge: &Merge) -> Option<u32> {
        match merge.write {
            Some(Write::Change(patch)) => patch.progress,
            Some(Write::Create(state)) => Some(state.progress),
            None => None,
        }
    }

    #[test]
    fn a_show_the_list_lacks_is_held_until_it_changes_and_then_created() {
        let wanted = state(ListStatus::Watching, 4, 0);
        assert_eq!(
            merge(wanted, None, &Remote::Unlisted),
            Merge {
                base: wanted,
                write: None
            }
        );
        assert_eq!(
            merge(wanted, Some(wanted), &Remote::Unlisted),
            Merge {
                base: wanted,
                write: None
            }
        );
        let held = state(ListStatus::Watching, 3, 0);
        assert_eq!(
            merge(wanted, Some(held), &Remote::Unlisted),
            Merge {
                base: held,
                write: Some(Write::Create(wanted))
            }
        );
    }

    #[test]
    fn a_field_one_side_moved_is_that_sides_and_both_moved_goes_by_the_rule() {
        use ListStatus::*;
        let base = state(Watching, 3, 1);
        let change = |status, progress, repeat| {
            Some(Write::Change(Patch {
                status,
                progress,
                repeat,
            }))
        };
        for (case, wanted, held, write) in [
            (
                "local only",
                state(Watching, 4, 1),
                base,
                change(None, Some(4), None),
            ),
            ("remote only", base, state(Completed, 5, 2), None),
            (
                "progress both moved: the higher wins, a list ahead is taken",
                state(Watching, 4, 1),
                state(Watching, 5, 1),
                None,
            ),
            (
                "progress both moved: the library ahead is sent",
                state(Watching, 6, 1),
                state(Watching, 5, 1),
                change(None, Some(6), None),
            ),
            (
                "repeat both moved: the higher wins",
                state(Watching, 3, 2),
                state(Watching, 3, 3),
                None,
            ),
            (
                "status both moved: the side further along wins",
                state(OnHold, 3, 1),
                state(Dropped, 4, 1),
                None,
            ),
            (
                "status both moved: Ryuuji wins a tie",
                state(OnHold, 3, 1),
                state(Dropped, 3, 1),
                change(Some(OnHold), None, None),
            ),
            (
                "status both moved to the same place",
                state(OnHold, 3, 1),
                state(OnHold, 3, 1),
                None,
            ),
            (
                "an undo lowers the list",
                state(Watching, 2, 1),
                base,
                change(None, Some(2), None),
            ),
        ] {
            assert_eq!(
                merge(wanted, Some(base), &listed(held)),
                Merge { base, write },
                "{case}"
            );
        }
    }

    #[test]
    fn a_first_merge_goes_to_the_side_further_along_and_takes_the_lists_repeat() {
        use ListStatus::*;
        let ahead = state(Watching, 5, 1);
        let behind = state(OnHold, 3, 2);
        assert_eq!(
            merge(ahead, None, &listed(behind)),
            Merge {
                base: state(OnHold, 3, 1),
                write: Some(Write::Change(Patch {
                    status: Some(Watching),
                    progress: Some(5),
                    repeat: None,
                })),
            }
        );
        let far = state(Completed, 12, 2);
        assert_eq!(
            merge(behind, None, &listed(far)),
            Merge {
                base: behind,
                write: None
            }
        );
        let level = state(Dropped, 3, 4);
        assert_eq!(
            merge(behind, None, &listed(level)),
            Merge {
                base: behind,
                write: None
            }
        );
    }

    #[test]
    fn a_patch_holds_only_what_differs_and_applies_back() {
        let from = state(ListStatus::Watching, 3, 1);
        assert_eq!(Patch::between(from, from), None);
        let to = state(ListStatus::Completed, 3, 2);
        let patch = Patch::between(from, to).unwrap();
        assert_eq!(
            patch,
            Patch {
                status: Some(ListStatus::Completed),
                progress: None,
                repeat: Some(2),
            }
        );
        assert_eq!(patch.apply(from), to);
    }

    /// Every triple in a small domain: one round of merge and settlement
    /// with no edit in flight leaves the library, the base and the list at
    /// one state, and a second merge writes nothing. The two progress rules
    /// are asserted alongside.
    #[test]
    fn every_triple_converges_in_one_round() {
        let mut states = Vec::new();
        for status in ListStatus::ALL {
            for progress in 0..=3 {
                for repeat in 0..=2 {
                    states.push(state(status, progress, repeat));
                }
            }
        }
        assert_eq!(states.len(), 72);
        let user = UserId(7);
        for &l in &states {
            let (entry, listing) = standing(l);
            for &b in &states {
                for &r in &states {
                    let case = format!("{l:?} / {b:?} / {r:?}");
                    let m = merge(l, Some(b), &listed(r));
                    assert_eq!(m.base, b, "{case}");
                    let r2 = match m.write {
                        None => r,
                        Some(Write::Change(patch)) => patch.apply(r),
                        Some(Write::Create(_)) => panic!("a listed show is never created: {case}"),
                    };
                    let asked = Asked {
                        wanted: l,
                        base: m.base,
                    };
                    let s = settlement(&asked, &listed(r2), user, &entry, &listing);
                    let (_, _, l2) = settled(&s, &entry, &listing);
                    assert_eq!(l2, r2, "{case}");
                    assert_eq!(s.base.state, r2, "{case}");
                    assert!(s.base.listed);
                    assert_eq!(
                        merge(l2, Some(s.base.state), &listed(r2)).write,
                        None,
                        "{case}"
                    );
                    let written = progress_written(&m);
                    if l.progress >= b.progress
                        && r.progress > l.progress
                        && r.progress != b.progress
                    {
                        assert_eq!(written, None, "a list ahead is taken: {case}");
                    }
                    if l.progress < b.progress && r.progress == b.progress {
                        assert_eq!(written, Some(l.progress), "an undo lowers: {case}");
                    }
                }
            }
        }
    }

    /// Base 3, Ryuuji records 4 while AniList holds 5, and the person undoes
    /// to 3 before the report lands. AniList is never sent 3.
    #[test]
    fn an_undo_in_flight_keeps_the_base_and_the_next_round_takes_the_list() {
        let base = state(ListStatus::Watching, 3, 0);
        let wanted = state(ListStatus::Watching, 4, 0);
        let held = state(ListStatus::Watching, 5, 0);
        let m = merge(wanted, Some(base), &listed(held));
        assert_eq!(m, Merge { base, write: None });
        let (entry, listing) = standing(base);
        let asked = Asked {
            wanted,
            base: m.base,
        };
        let s = settlement(&asked, &listed(held), UserId(7), &entry, &listing);
        assert_eq!((s.take_status, s.take_progress), (None, None));
        assert_eq!(s.base.state, base);
        let (entry, listing, now) = settled(&s, &entry, &listing);
        assert_eq!(now, base);

        let m = merge(now, Some(s.base.state), &listed(held));
        assert_eq!(m, Merge { base, write: None });
        let asked = Asked {
            wanted: now,
            base: m.base,
        };
        let s = settlement(&asked, &listed(held), UserId(7), &entry, &listing);
        assert_eq!(s.take_progress, Some(5));
        assert_eq!(s.take_status, None);
        assert_eq!(s.base.state, held);
        let (_, _, now) = settled(&s, &entry, &listing);
        assert_eq!(now, held);
    }

    /// Ryuuji sends 4 and the person undoes to 3 before the report lands:
    /// the base becomes what was written, and the next reconcile sends the
    /// undo.
    #[test]
    fn an_undo_after_a_write_lands_is_sent_by_the_next_round() {
        let base = state(ListStatus::Watching, 3, 0);
        let wanted = state(ListStatus::Watching, 4, 0);
        let m = merge(wanted, Some(base), &listed(base));
        assert_eq!(progress_written(&m), Some(4));
        let (entry, listing) = standing(base);
        let asked = Asked {
            wanted,
            base: m.base,
        };
        let s = settlement(&asked, &listed(wanted), UserId(7), &entry, &listing);
        assert_eq!((s.take_status, s.take_progress), (None, None));
        assert_eq!(s.base.state, wanted);
        let (_, _, now) = settled(&s, &entry, &listing);
        assert_eq!(now, base);
        let m = merge(now, Some(s.base.state), &listed(wanted));
        assert_eq!(progress_written(&m), Some(3));
    }

    #[test]
    fn a_field_the_service_adjusted_is_taken_and_a_repeat_moves_the_offset() {
        let base = state(ListStatus::Watching, 11, 1);
        let wanted = state(ListStatus::Watching, 12, 1);
        let (entry, listing) = standing(wanted);
        let asked = Asked { wanted, base };
        // Kitsu completes the show at its total and counts a rewatch of
        // its own.
        let held = state(ListStatus::Completed, 12, 3);
        let s = settlement(&asked, &listed(held), UserId(7), &entry, &listing);
        assert_eq!(s.take_status, Some(ListStatus::Completed));
        assert_eq!(s.take_progress, None);
        assert_eq!(s.base.state, held);
        assert_eq!(s.repeat_offset, 2);
        let (_, _, now) = settled(&s, &entry, &listing);
        assert_eq!(now, held);
    }

    #[test]
    fn a_show_the_list_lacks_settles_as_unlisted_with_the_base_it_asked_with() {
        let base = state(ListStatus::Watching, 3, 0);
        let wanted = state(ListStatus::Watching, 4, 0);
        let (entry, listing) = standing(wanted);
        let listing = Listing {
            repeat_offset: 2,
            ..listing
        };
        let asked = Asked { wanted, base };
        let listing = Listing {
            base: Some(ListBase {
                user: UserId(7),
                listed: false,
                state: base,
            }),
            ..listing
        };
        let s = settlement(&asked, &Remote::Unlisted, UserId(7), &entry, &listing);
        assert_eq!(
            s,
            Settlement {
                take_status: None,
                take_progress: None,
                base: ListBase {
                    user: UserId(7),
                    listed: false,
                    state: base,
                },
                repeat_offset: 2,
            }
        );
    }

    /// User 7 had the show listed with two repeats more than Ryuuji counted,
    /// and user 8 signs in. The offset was 7's list's, so 8's first merge
    /// starts it over: from nothing where their list lacks the show, from
    /// their list's count where it holds it.
    #[test]
    fn a_first_merge_for_another_user_starts_the_repeat_offset_over() {
        let wanted = state(ListStatus::Watching, 4, 0);
        let (entry, listing) = standing(wanted);
        let listing = Listing {
            base: Some(ListBase {
                user: UserId(7),
                listed: true,
                state: state(ListStatus::Watching, 4, 2),
            }),
            repeat_offset: 2,
            ..listing
        };
        let asked = Asked {
            wanted,
            base: wanted,
        };
        let s = settlement(&asked, &Remote::Unlisted, UserId(8), &entry, &listing);
        assert_eq!(
            s.base,
            ListBase {
                user: UserId(8),
                listed: false,
                state: wanted,
            }
        );
        assert_eq!(s.repeat_offset, 0);
        let (_, settled_listing, now) = settled(&s, &entry, &listing);
        assert_eq!(now, wanted);
        assert_eq!(
            merge(now, Some(s.base.state), &Remote::Unlisted).write,
            None
        );
        assert_eq!(settled_listing.wanted(&entry, UserId(8)), wanted);

        let held = state(ListStatus::Watching, 4, 3);
        let s = settlement(&asked, &listed(held), UserId(8), &entry, &listing);
        assert_eq!(s.repeat_offset, 3);
        assert_eq!(s.base.state, held);
        let (_, _, now) = settled(&s, &entry, &listing);
        assert_eq!(now, held);

        // For user 7 the base is theirs, and the offset stays.
        let theirs = Asked {
            wanted: listing.wanted(&entry, UserId(7)),
            base: state(ListStatus::Watching, 4, 2),
        };
        assert_eq!(theirs.wanted.repeat, 2);
        let s = settlement(&theirs, &Remote::Unlisted, UserId(7), &entry, &listing);
        assert_eq!(s.repeat_offset, 2);
    }

    fn fruits_basket() -> Vec<Candidate> {
        vec![
            Candidate {
                media: media(1, "Fruits Basket", Some(26)),
                titles: vec!["Fruits Basket".into(), "Furuba".into()],
                format: Some("TV".into()),
                year: Some(2001),
            },
            Candidate {
                media: media(2, "Fruits Basket (2019)", Some(25)),
                titles: vec!["Fruits Basket".into(), "Fruits Basket 1st Season".into()],
                format: Some("TV".into()),
                year: Some(2019),
            },
        ]
    }

    fn entry(title: &str, progress: u32, total: Option<u32>) -> LibraryEntry {
        LibraryEntry {
            id: EntryId(1),
            title: title.into(),
            status: WatchStatus::Watching,
            progress,
            total,
            rewatching: false,
            rewatched: 0,
        }
    }

    #[test]
    fn one_fitting_title_links_and_a_tie_waits_for_a_pick() {
        let found = fruits_basket();
        assert_eq!(
            auto_link(&entry("Fruits Basket", 0, None), &found),
            AutoLink::Pick { found: 2 }
        );
        assert_eq!(
            auto_link(&entry("Fruits Basket", 0, Some(25)), &found),
            AutoLink::Link(found[1].media.clone())
        );
        assert_eq!(
            auto_link(&entry("Fruits Basket (2019)", 0, None), &found),
            AutoLink::Link(found[1].media.clone())
        );
        // A synonym, and a full-width title, fold onto the same candidate.
        assert_eq!(
            auto_link(&entry("Furuba", 0, None), &found),
            AutoLink::Link(found[0].media.clone())
        );
        assert_eq!(
            auto_link(&entry("Ｆｒｕｉｔｓ　Ｂａｓｋｅｔ", 0, Some(26)), &found),
            AutoLink::Link(found[0].media.clone())
        );
    }

    #[test]
    fn a_count_that_does_not_fit_rules_a_candidate_out_and_an_unknown_one_fits() {
        let found = fruits_basket();
        assert_eq!(
            auto_link(&entry("Fruits Basket", 0, Some(24)), &found),
            AutoLink::Pick { found: 2 }
        );
        // With no total, a candidate shorter than what was watched is out.
        assert_eq!(
            auto_link(&entry("Fruits Basket", 26, None), &found),
            AutoLink::Link(found[0].media.clone())
        );
        assert_eq!(
            auto_link(&entry("Fruits Basket", 30, None), &found),
            AutoLink::Pick { found: 2 }
        );
        let uncounted = vec![Candidate {
            media: media(3, "Fruits Basket", None),
            titles: vec![],
            format: None,
            year: None,
        }];
        assert_eq!(
            auto_link(&entry("Fruits Basket", 30, Some(63)), &uncounted),
            AutoLink::Link(uncounted[0].media.clone())
        );
        assert_eq!(
            auto_link(&entry("Fruits Basket", 0, None), &[]),
            AutoLink::Pick { found: 0 }
        );
        assert_eq!(
            auto_link(&entry("Something Else", 0, None), &found),
            AutoLink::Pick { found: 2 }
        );
    }
}
