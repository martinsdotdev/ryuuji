//! One reconcile as the worker runs it: read what the list holds, settle
//! the difference with the core's `merge`, write what it asks for, and
//! report what the list holds after. These are the steps the core's test
//! double takes, so what the reducer was tested against is what runs.

use ryuuji_core::{
    Asked, EntryId, ListEntryId, ListState, MediaId, Remote, Report, Service, UserId, Write, merge,
};

use crate::service::Failure;

/// A reconcile job with what the worker knows around it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Reconcile {
    pub service: Service,
    pub user: UserId,
    pub entry: EntryId,
    pub media: MediaId,
    pub wanted: ListState,
    pub base: Option<ListState>,
}

/// One question to the list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Call {
    Read {
        media: MediaId,
    },
    Save {
        media: MediaId,
        id: Option<ListEntryId>,
        write: Write,
    },
}

/// Runs one reconcile through `send`. The read comes first, so a push never
/// lands over progress watched elsewhere; the save carries the id the read
/// returned; the report carries the base the merge measured against and the
/// row as the list holds it after the write, which the settlement reads
/// field by field. A failure is the caller's to turn into a report or into
/// the engine's state.
pub(crate) fn run(
    job: &Reconcile,
    mut send: impl FnMut(Call) -> Result<Remote, Failure>,
) -> Result<Report, Failure> {
    let read = send(Call::Read { media: job.media })?;
    let merged = merge(job.wanted, job.base, &read);
    let remote = match merged.write {
        Some(write) => {
            let id = match read {
                Remote::Listed { id, .. } => id,
                Remote::Unlisted => None,
            };
            send(Call::Save {
                media: job.media,
                id,
                write,
            })?
        }
        None => read,
    };
    Ok(Report::Reconciled {
        service: job.service,
        user: job.user,
        entry: job.entry,
        media: job.media,
        asked: Asked {
            wanted: job.wanted,
            base: merged.base,
        },
        remote,
    })
}

#[cfg(test)]
mod tests {
    use ryuuji_core::{ListStatus, Patch};

    use super::*;
    use crate::engine::tests::entries;

    const MEDIA: MediaId = MediaId(154587);
    const ROW: Option<ListEntryId> = Some(ListEntryId(381_234_567));

    fn state(status: ListStatus, progress: u32, repeat: u32) -> ListState {
        ListState {
            status,
            progress,
            repeat,
        }
    }

    fn listed(state: ListState) -> Remote {
        Remote::Listed { id: ROW, state }
    }

    fn job(entry: EntryId, wanted: ListState, base: Option<ListState>) -> Reconcile {
        Reconcile {
            service: Service::AniList,
            user: UserId(7),
            entry,
            media: MEDIA,
            wanted,
            base,
        }
    }

    /// Runs `job` against a list that answers the read with `read` and any
    /// save with `saved`, and returns the calls it made beside the outcome.
    fn scripted(
        job: &Reconcile,
        read: Result<Remote, Failure>,
        saved: Result<Remote, Failure>,
    ) -> (Vec<Call>, Result<Report, Failure>) {
        let mut calls = Vec::new();
        let outcome = run(job, |call| {
            calls.push(call);
            match call {
                Call::Read { media } => {
                    assert_eq!(media, MEDIA);
                    read.clone()
                }
                Call::Save { media, .. } => {
                    assert_eq!(media, MEDIA);
                    saved.clone()
                }
            }
        });
        (calls, outcome)
    }

    fn reconciled(job: &Reconcile, base: ListState, remote: Remote) -> Result<Report, Failure> {
        Ok(Report::Reconciled {
            service: Service::AniList,
            user: UserId(7),
            entry: job.entry,
            media: MEDIA,
            asked: Asked {
                wanted: job.wanted,
                base,
            },
            remote,
        })
    }

    #[test]
    fn a_list_ahead_is_read_once_and_never_written() {
        let [entry] = entries(1)[..] else {
            unreachable!()
        };
        let base = state(ListStatus::Watching, 3, 0);
        let job = job(entry, state(ListStatus::Watching, 4, 0), Some(base));
        let ahead = listed(state(ListStatus::Watching, 5, 0));
        let (calls, outcome) = scripted(&job, Ok(ahead), Err(Failure::Transient));
        assert_eq!(calls, vec![Call::Read { media: MEDIA }]);
        assert_eq!(outcome, reconciled(&job, base, ahead));
    }

    #[test]
    fn a_library_ahead_reads_then_saves_with_the_reads_id() {
        let [entry] = entries(1)[..] else {
            unreachable!()
        };
        let base = state(ListStatus::Watching, 3, 0);
        let wanted = state(ListStatus::Watching, 4, 0);
        let job = job(entry, wanted, Some(base));
        let (calls, outcome) = scripted(&job, Ok(listed(base)), Ok(listed(wanted)));
        assert_eq!(
            calls,
            vec![
                Call::Read { media: MEDIA },
                Call::Save {
                    media: MEDIA,
                    id: ROW,
                    write: Write::Change(Patch {
                        progress: Some(4),
                        ..Patch::default()
                    }),
                },
            ]
        );
        assert_eq!(outcome, reconciled(&job, base, listed(wanted)));
    }

    #[test]
    fn a_show_the_list_lacks_is_held_while_unchanged_and_created_once_changed() {
        let [entry] = entries(1)[..] else {
            unreachable!()
        };
        let wanted = state(ListStatus::PlanToWatch, 0, 0);
        let first = job(entry, wanted, None);
        let (calls, outcome) = scripted(&first, Ok(Remote::Unlisted), Err(Failure::Transient));
        assert_eq!(calls, vec![Call::Read { media: MEDIA }]);
        assert_eq!(outcome, reconciled(&first, wanted, Remote::Unlisted));

        let unchanged = job(entry, wanted, Some(wanted));
        let (calls, outcome) = scripted(&unchanged, Ok(Remote::Unlisted), Err(Failure::Transient));
        assert_eq!(calls.len(), 1);
        assert_eq!(outcome, reconciled(&unchanged, wanted, Remote::Unlisted));

        let changed = state(ListStatus::Watching, 1, 0);
        let job = job(entry, changed, Some(wanted));
        let (calls, outcome) = scripted(&job, Ok(Remote::Unlisted), Ok(listed(changed)));
        assert_eq!(
            calls[1],
            Call::Save {
                media: MEDIA,
                id: None,
                write: Write::Create(changed),
            }
        );
        assert_eq!(outcome, reconciled(&job, wanted, listed(changed)));
    }

    #[test]
    fn a_save_answer_that_differs_from_what_was_sent_is_what_the_report_carries() {
        let [entry] = entries(1)[..] else {
            unreachable!()
        };
        let base = state(ListStatus::Watching, 27, 0);
        let wanted = state(ListStatus::Watching, 28, 0);
        let job = job(entry, wanted, Some(base));
        // The service completes the show at its last episode itself.
        let completed = listed(state(ListStatus::Completed, 28, 0));
        let (calls, outcome) = scripted(&job, Ok(listed(base)), Ok(completed));
        assert_eq!(calls.len(), 2);
        assert_eq!(outcome, reconciled(&job, base, completed));
    }

    #[test]
    fn a_failure_comes_out_as_it_is_and_a_failed_read_saves_nothing() {
        let [entry] = entries(1)[..] else {
            unreachable!()
        };
        let base = state(ListStatus::Watching, 3, 0);
        let job = job(entry, state(ListStatus::Watching, 4, 0), Some(base));
        let (calls, outcome) = scripted(&job, Err(Failure::Offline), Ok(listed(base)));
        assert_eq!(calls, vec![Call::Read { media: MEDIA }]);
        assert_eq!(outcome, Err(Failure::Offline));
        let (calls, outcome) = scripted(
            &job,
            Ok(listed(base)),
            Err(Failure::Rejected("Validation error".into())),
        );
        assert_eq!(calls.len(), 2);
        assert_eq!(outcome, Err(Failure::Rejected("Validation error".into())));
    }
}
