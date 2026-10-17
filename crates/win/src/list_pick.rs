//! The card that picks which entry on a list a show is: on Now playing when
//! the playing show matches several entries or lost its own, and on the
//! show's page for Change entry. It names the service, since it only shows
//! when the service needs the person. The search is the core's picker, run
//! by the worker; the card asks it with the show's title when it opens.

use ryuuji_core::{
    Answer, AppState, Candidate, Command, EntryId, LinkState, MediaId, Picker, Service,
};
use windows_reactor::*;

use crate::show_info;
use crate::ui::{self, caption, card_frame};

/// A show's pick on one service, worked out from the state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ListPick {
    pub entry: EntryId,
    pub service: Service,
    /// What the card searches for when it opens: the show's title.
    pub title: String,
    /// Why the card is asking.
    pub reason: String,
    /// The core's picker, while it is open over this show.
    pub picker: Option<Picker>,
    /// The entry the show is linked to now, chosen when the card opens.
    pub current: Option<MediaId>,
}

/// The pick the show needs on the first signed-in service that needs one.
pub(crate) fn needed(state: &AppState, entry: EntryId) -> Option<ListPick> {
    state
        .sync
        .signed_in_accounts()
        .find_map(|account| needed_on(state, entry, account.service))
}

/// The pick the show needs on `service`: it matched several entries, the
/// service refused to search for it, or the one it was linked to is gone.
pub(crate) fn needed_on(state: &AppState, entry: EntryId, service: Service) -> Option<ListPick> {
    let shown = state.entry(entry)?;
    let reason = match state.sync.link_state(shown, service)? {
        LinkState::Unpicked { found } => unpicked_reason(service, &shown.title, found),
        LinkState::SearchRefused { message } => format!(
            "{} refused to search for {}: {}. Search for it by another name.",
            service.label(),
            shown.title,
            message.trim_end_matches('.')
        ),
        LinkState::Gone { show } => format!(
            "{show} is gone from {}. Pick the entry this show is now.",
            service.label()
        ),
        LinkState::Looking
        | LinkState::Merging { .. }
        | LinkState::Waiting { .. }
        | LinkState::InStep { .. }
        | LinkState::Refused { .. }
        | LinkState::Off => return None,
    };
    Some(ListPick {
        entry,
        service,
        title: shown.title.clone(),
        reason,
        picker: open_over(state, entry, service),
        current: None,
    })
}

/// The pick Change entry opens over a show already linked, or one the
/// person stopped syncing.
pub(crate) fn changing(state: &AppState, entry: EntryId, service: Service) -> Option<ListPick> {
    let shown = state.entry(entry)?;
    let current = state
        .sync
        .listing(entry, service)
        .and_then(|listing| listing.tie.linked())
        .map(|media| media.id);
    Some(ListPick {
        entry,
        service,
        title: shown.title.clone(),
        reason: format!("Pick the entry this show is on {}.", service.label()),
        picker: open_over(state, entry, service),
        current,
    })
}

/// The core's picker, while it is open over `entry` on `service`.
pub(crate) fn open_over(state: &AppState, entry: EntryId, service: Service) -> Option<Picker> {
    state
        .sync
        .picker
        .as_ref()
        .filter(|picker| picker.entry == entry && picker.service == service)
        .cloned()
}

fn unpicked_reason(service: Service, title: &str, found: u32) -> String {
    let service = service.label();
    match found {
        0 => format!("Nothing on {service} matched {title}. Search for it by another name."),
        1 => format!(
            "One {service} entry came up for {title}, and its title doesn't match. Link it if \
             it's the one."
        ),
        found => format!(
            "{found} {service} entries could be {title}, and Ryuuji can't tell which. It keeps \
             your pick for this show."
        ),
    }
}

/// `TV · 26 episodes · 2001`: what tells two entries of one title apart.
fn candidate_line(candidate: &Candidate) -> String {
    ui::dot_joined([
        show_info::format_line(candidate.format, candidate.media.episodes),
        candidate.year.map(|year| year.to_string()),
    ])
    .unwrap_or_default()
}

/// What the card is handed. `dispatch` compares by identity, so it comes
/// first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ListPickProps {
    pub dispatch: Dispatch<Command>,
    pub pick: ListPick,
    /// Shown beside Link on the show's page, which opens the card by hand.
    pub cancel: bool,
}

/// The card, keyed on the show and the service so each opens fresh.
pub(crate) fn card(props: ListPickProps) -> Element {
    let key = format!("pick-{}-{}", props.pick.entry, props.pick.service.tag());
    component(list_pick, props).with_key(key)
}

fn list_pick(props: &ListPickProps, cx: &mut RenderCx) -> Element {
    let pick = &props.pick;
    let (query, set_query) = cx.use_state(
        pick.picker
            .as_ref()
            .map_or_else(|| pick.title.clone(), |picker| picker.query.clone()),
    );
    let (chosen, set_chosen) = cx.use_state(pick.current);
    // Asked once as the card opens; the core keeps the picker across pages.
    cx.use_effect(pick.picker.is_none(), {
        let dispatch = props.dispatch.clone();
        let (entry, service, title) = (pick.entry, pick.service, pick.title.clone());
        let closed = pick.picker.is_none();
        move || {
            if closed {
                dispatch.call(Command::SearchList {
                    entry,
                    service,
                    query: title,
                });
            }
        }
    });
    let search = {
        let dispatch = props.dispatch.clone();
        let (entry, service) = (pick.entry, pick.service);
        move |query: String| {
            dispatch.call(Command::SearchList {
                entry,
                service,
                query,
            });
        }
    };
    let label = pick.service.label();
    let mut children: Vec<Element> = vec![
        text_block(format!("Which {label} entry is this?"))
            .semibold()
            .into(),
        caption(pick.reason.clone()).wrap().into(),
        grid((
            auto_suggest_box(query.clone())
                .placeholder_text(format!("Search {label}"))
                .on_text_changed(set_query)
                .on_query_submitted(search.clone())
                .grid_column(0),
            button("Search")
                .on_click({
                    let query = query.clone();
                    move || search(query.clone())
                })
                .grid_column(1),
        ))
        .columns([GridLength::STAR, GridLength::Auto])
        .column_spacing(8.0)
        .into(),
    ];
    let answer = pick
        .picker
        .as_ref()
        .and_then(|picker| picker.answer.clone());
    match answer {
        None => children.push(
            hstack((
                ProgressRing::indeterminate().width(16.0).height(16.0),
                caption(format!("Searching {label}\u{2026}")),
            ))
            .spacing(8.0)
            .into(),
        ),
        Some(Answer::Refused(message)) => children.push(
            caption(format!(
                "{label} refused that search: {}. Try another name.",
                message.trim_end_matches('.')
            ))
            .wrap()
            .into(),
        ),
        Some(Answer::Found(candidates)) if candidates.is_empty() => {
            children.push(caption("Nothing came up. Try another name.").into());
        }
        Some(Answer::Found(candidates)) => {
            let selected =
                ui::selection_index(&candidates, |candidate| Some(candidate.media.id) == chosen);
            let ids: Vec<MediaId> = candidates
                .iter()
                .map(|candidate| candidate.media.id)
                .collect();
            let rows = candidates.len();
            let list = list_view(candidates, |candidate: &Candidate, _| {
                vstack((
                    text_block(candidate.media.title.clone()).wrap(),
                    caption(candidate_line(candidate)),
                ))
                .spacing(2.0)
            })
            .with_key_selector(|candidate: &Candidate| candidate.media.id.0.to_string())
            .selection_mode(SelectionMode::Single)
            .selected_index(selected)
            .on_selection_changed(move |index: i32| {
                set_chosen.call(
                    usize::try_from(index)
                        .ok()
                        .and_then(|index| ids.get(index).copied()),
                );
            });
            children.push(ui::picker_list(list, rows));
        }
    }
    let media = pick
        .picker
        .as_ref()
        .and_then(|picker| match &picker.answer {
            Some(Answer::Found(found)) => Some(found),
            Some(Answer::Refused(_)) | None => None,
        })
        .and_then(|found| {
            found
                .iter()
                .find(|candidate| Some(candidate.media.id) == chosen)
        })
        .map(|candidate| candidate.media.clone());
    let link = {
        let dispatch = props.dispatch.clone();
        let (entry, service, media) = (pick.entry, pick.service, media.clone());
        move || {
            if let Some(media) = media.clone() {
                dispatch.call(Command::LinkShow {
                    entry,
                    service,
                    media,
                });
            }
        }
    };
    let stop = ui::send(
        &props.dispatch,
        Command::StopSyncing {
            entry: pick.entry,
            service: pick.service,
        },
    );
    let mut buttons: Vec<Element> = vec![button("Don't sync this show").on_click(stop).into()];
    if props.cancel {
        buttons.push(
            button("Cancel")
                .on_click(ui::send(&props.dispatch, Command::ClosePicker))
                .into(),
        );
    }
    buttons.push(
        button("Link")
            .accent()
            .enabled(media.is_some())
            .on_click(link)
            .into(),
    );
    children.push(
        hstack(buttons)
            .spacing(8.0)
            .horizontal_alignment(HorizontalAlignment::Right)
            .into(),
    );
    card_frame(vstack(children).spacing(12.0)).into()
}

#[cfg(test)]
mod tests {
    use ryuuji_core::{
        Account, DataDir, Format, Media, NewEntry, Ryuuji, Signed, UserId, WatchStatus,
    };

    use super::*;

    fn media(id: u64, title: &str, episodes: Option<u32>) -> Media {
        Media {
            id: MediaId(id),
            title: title.to_owned(),
            episodes,
            mal_id: None,
        }
    }

    #[test]
    fn a_candidate_says_what_tells_it_apart() {
        let candidate = |format, episodes, year| Candidate {
            media: media(1, "Fruits Basket", episodes),
            titles: vec![],
            format,
            year,
        };
        assert_eq!(
            candidate_line(&candidate(Some(Format::Tv), Some(26), Some(2001))),
            "TV \u{b7} 26 episodes \u{b7} 2001"
        );
        assert_eq!(
            candidate_line(&candidate(Some(Format::TvShort), Some(1), None)),
            "TV short \u{b7} 1 episode"
        );
        assert_eq!(candidate_line(&candidate(None, None, None)), "");
    }

    /// A signed-in app with one show, `Fruits Basket`, whose AniList tie is
    /// what `tie` makes it.
    fn app_with(tie: impl FnOnce(&mut Ryuuji, EntryId)) -> (tempfile::TempDir, Ryuuji, EntryId) {
        let tmp = tempfile::tempdir().unwrap();
        let dir = DataDir::at(tmp.path()).unwrap();
        let mut app = Ryuuji::open(&dir).unwrap();
        app.dispatch(Command::Synced(ryuuji_core::Report::SignedIn(Account {
            service: Service::AniList,
            user: UserId(7),
            name: "someone".into(),
            expires: None,
            signed: Signed::In,
        })));
        app.dispatch(Command::AddEntry(NewEntry {
            title: "Fruits Basket".to_owned(),
            status: WatchStatus::PlanToWatch,
            progress: 0,
            total: None,
            rewatching: false,
        }));
        let entry = app.state().library[0].id;
        tie(&mut app, entry);
        (tmp, app, entry)
    }

    fn found(app: &mut Ryuuji, entry: EntryId, candidates: Vec<Candidate>) {
        app.dispatch(Command::Synced(ryuuji_core::Report::Found {
            service: Service::AniList,
            entry,
            query: "Fruits Basket".to_owned(),
            candidates,
        }));
    }

    fn candidate(id: u64, title: &str, year: u16) -> Candidate {
        Candidate {
            media: media(id, title, Some(25)),
            // Both entries list the bare title, which is what makes the
            // show ambiguous.
            titles: vec![title.to_owned(), "Fruits Basket".to_owned()],
            format: Some(Format::Tv),
            year: Some(year),
        }
    }

    #[test]
    fn a_show_that_matches_several_entries_needs_a_pick() {
        let (_tmp, app, entry) = app_with(|app, entry| {
            found(
                app,
                entry,
                vec![
                    candidate(1, "Fruits Basket", 2001),
                    candidate(2, "Fruits Basket 1st Season", 2019),
                ],
            );
        });
        let pick = needed(app.state(), entry).unwrap();
        assert_eq!(pick.service, Service::AniList);
        assert_eq!(pick.title, "Fruits Basket");
        assert_eq!(
            pick.reason,
            "2 AniList entries could be Fruits Basket, and Ryuuji can't tell which. It keeps \
             your pick for this show."
        );
        assert_eq!(pick.picker, None);
        assert_eq!(pick.current, None);
    }

    #[test]
    fn a_show_in_step_or_never_looked_up_needs_nothing() {
        let (_tmp, app, entry) = app_with(|_, _| {});
        assert_eq!(needed(app.state(), entry), None);
        let (_tmp, app, entry) = app_with(|app, entry| {
            app.dispatch(Command::LinkShow {
                entry,
                service: Service::AniList,
                media: media(1, "Fruits Basket", Some(26)),
            });
        });
        assert_eq!(needed(app.state(), entry), None);
    }

    #[test]
    fn a_show_the_search_refused_needs_a_pick_and_says_why() {
        let (_tmp, app, entry) = app_with(|app, entry| {
            app.dispatch(Command::Synced(ryuuji_core::Report::SearchRefused {
                service: Service::AniList,
                entry,
                query: "Fruits Basket".to_owned(),
                message: "Validation error.".to_owned(),
            }));
        });
        let pick = needed(app.state(), entry).unwrap();
        // The service's own full stop is dropped, so the sentence does not
        // end in two.
        assert_eq!(
            pick.reason,
            "AniList refused to search for Fruits Basket: Validation error. Search for it by \
             another name."
        );
    }

    #[test]
    fn nothing_found_or_one_that_does_not_fit_says_so() {
        assert_eq!(
            unpicked_reason(Service::AniList, "Show", 0),
            "Nothing on AniList matched Show. Search for it by another name."
        );
        assert_eq!(
            unpicked_reason(Service::AniList, "Show", 1),
            "One AniList entry came up for Show, and its title doesn't match. Link it if it's \
             the one."
        );
    }

    #[test]
    fn an_open_picker_over_the_show_rides_along() {
        let (_tmp, mut app, entry) = app_with(|app, entry| {
            found(
                app,
                entry,
                vec![
                    candidate(1, "Fruits Basket", 2001),
                    candidate(2, "Fruits Basket 1st Season", 2019),
                ],
            );
        });
        app.dispatch(Command::SearchList {
            entry,
            service: Service::AniList,
            query: "Furuba".to_owned(),
        });
        let picker = needed(app.state(), entry).unwrap().picker.unwrap();
        assert_eq!((picker.query.as_str(), picker.answer), ("Furuba", None));
    }
}
