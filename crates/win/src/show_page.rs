//! One show's page, opened over the Library or Now playing: where the show
//! stands, and for each signed-in list, which entry it is linked to, whether
//! it is in step, the last refusal with the way to try again, and the ways
//! to change the entry, stop syncing it or open it on the site. This is
//! where a list is named when nothing needs the person, since the person
//! came here to ask.

use std::cell::RefCell;
use std::rc::Rc;

use ryuuji_core::{AppState, Command, EntryId, LinkState, MediaId, Ryuuji, Service};
use windows_reactor::*;

use crate::list_pick::{self, ListPickProps};
use crate::show_info;
use crate::ui::{self, CONTENT_MAX_WIDTH, caption, card_frame, placeholder};

/// Where the show stands on one list, in words.
#[derive(Clone, Debug, PartialEq, Eq)]
struct OnList {
    words: String,
    /// The list's last refusal, while the show is set aside on it.
    refused: Option<String>,
    /// What the section offers.
    actions: Vec<Action>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    TryAgain,
    ChangeEntry,
    StopSyncing,
    SyncThisShow,
}

impl Action {
    fn label(self) -> &'static str {
        match self {
            Action::TryAgain => "Try again",
            Action::ChangeEntry => "Change entry",
            Action::StopSyncing => "Stop syncing",
            Action::SyncThisShow => "Sync this show",
        }
    }
}

fn on_list(link: &LinkState, service: Service) -> OnList {
    let label = service.label();
    let linked = |show: &str, words: &str| OnList {
        words: format!("Linked to {show}. {words}"),
        refused: None,
        actions: vec![Action::ChangeEntry, Action::StopSyncing],
    };
    match link {
        LinkState::Looking => OnList {
            words: format!("Looking this show up on {label}\u{2026}"),
            refused: None,
            actions: vec![],
        },
        LinkState::Unpicked { .. } => OnList {
            words: format!("Several {label} entries could be this show."),
            refused: None,
            actions: vec![],
        },
        LinkState::Merging { show } => linked(show, "Reading your list for the first time."),
        LinkState::Waiting { show } => linked(show, "A change waits to go out."),
        LinkState::InStep { show, listed: true } => linked(show, "In step with your list."),
        LinkState::InStep {
            show,
            listed: false,
        } => linked(show, "It goes on your list with its next change."),
        LinkState::Refused { show, message } => OnList {
            words: format!("Linked to {show}."),
            refused: Some(format!("{label} refused the last change: {message}")),
            actions: vec![Action::TryAgain, Action::ChangeEntry, Action::StopSyncing],
        },
        LinkState::Gone { show } => OnList {
            words: format!("{show} is gone from {label}."),
            refused: None,
            actions: vec![],
        },
        LinkState::Off => OnList {
            words: format!("Not kept in step with {label}."),
            refused: None,
            actions: vec![Action::SyncThisShow],
        },
    }
}

/// The page for `entry`, or a placeholder when the library no longer has it.
pub(crate) fn body(
    state: &AppState,
    entry: EntryId,
    dispatch: Dispatch<Command>,
    core: &Rc<RefCell<Ryuuji>>,
) -> Element {
    let Some(shown) = state.entry(entry) else {
        return placeholder(
            "This show isn't in your library",
            "It may have been added again under another title.",
        );
    };
    let mut sections: Vec<Element> = vec![component(
        summary,
        SummaryProps {
            info: show_info::source(state, core, Some(entry)),
            status: show_info::status_line(shown),
            title: shown.title.clone(),
        },
    )];
    for account in state.sync.signed_in_accounts() {
        let service = account.service;
        let Some(link) = state.sync.link_state(shown, service) else {
            continue;
        };
        sections.push(service_card(
            state,
            entry,
            service,
            &link,
            &shown.title,
            &dispatch,
        ));
        let open = list_pick::open_over(state, entry, service).is_some();
        let pick = if open {
            list_pick::changing(state, entry, service)
        } else {
            list_pick::needed_on(state, entry, service)
        };
        if let Some(pick) = pick {
            sections.push(list_pick::card(ListPickProps {
                dispatch: dispatch.clone(),
                pick,
                cancel: open,
            }));
        }
    }
    scroll_viewer(vstack(sections).spacing(16.0).max_width(CONTENT_MAX_WIDTH)).into()
}

/// What the page opens with.
#[derive(Clone, Debug, PartialEq)]
struct SummaryProps {
    info: show_info::Source,
    /// Where the show stands in the library.
    status: String,
    title: String,
}

/// The cover, where the show stands, the synopsis and the details. A
/// component, so the store is read only when the show is described again.
fn summary(props: &SummaryProps, cx: &mut RenderCx) -> Element {
    let described = show_info::use_described(cx, &props.info);
    let info = described.as_ref().map(|described| &described.info);
    let mut beside: Vec<Element> = vec![caption(props.status.clone()).into()];
    if let Some(synopsis) = info.and_then(show_info::synopsis) {
        beside.push(synopsis);
    }
    let mut sections: Vec<Element> =
        vec![show_info::beside_cover(described.as_ref(), vstack(beside).spacing(12.0)).into()];
    if let Some(details) = info.and_then(|info| show_info::details(info, &props.title)) {
        sections.push(details);
    }
    vstack(sections).spacing(16.0).into()
}

fn service_card(
    state: &AppState,
    entry: EntryId,
    service: Service,
    link: &LinkState,
    title: &str,
    dispatch: &Dispatch<Command>,
) -> Element {
    let on_list = on_list(link, service);
    let mut children: Vec<Element> = vec![
        text_block(service.label()).semibold().into(),
        text_block(on_list.words.clone()).wrap().into(),
    ];
    if let Some(refused) = &on_list.refused {
        children.push(
            caption(refused.clone())
                .foreground(ThemeRef::SystemCaution)
                .wrap()
                .into(),
        );
    }
    let mut buttons: Vec<Element> = on_list
        .actions
        .iter()
        .map(|&action| {
            let command = match action {
                Action::TryAgain => Command::RetrySync { entry, service },
                Action::StopSyncing => Command::StopSyncing { entry, service },
                Action::ChangeEntry | Action::SyncThisShow => Command::SearchList {
                    entry,
                    service,
                    query: title.to_owned(),
                },
            };
            button(action.label())
                .on_click(ui::send(dispatch, command))
                .into()
        })
        .collect();
    if let Some(media) = linked_media(state, entry, service) {
        buttons.push(
            HyperlinkButton::new(format!("Open on {}", service.label()))
                .navigate_uri(ryuuji_sync::site_url(service, media))
                .into(),
        );
    }
    if !buttons.is_empty() {
        children.push(hstack(buttons).spacing(8.0).into());
    }
    card_frame(vstack(children).spacing(8.0)).into()
}

fn linked_media(state: &AppState, entry: EntryId, service: Service) -> Option<MediaId> {
    Some(state.sync.listing(entry, service)?.tie.linked()?.id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn said(link: LinkState) -> (String, Option<String>, Vec<Action>) {
        let on_list = on_list(&link, Service::AniList);
        (on_list.words, on_list.refused, on_list.actions)
    }

    #[test]
    fn each_list_says_where_the_show_stands_and_what_can_be_done() {
        let show = || "Sousou no Frieren".to_owned();
        let linked = vec![Action::ChangeEntry, Action::StopSyncing];
        assert_eq!(
            said(LinkState::Looking),
            (
                "Looking this show up on AniList\u{2026}".to_owned(),
                None,
                vec![]
            )
        );
        assert_eq!(
            said(LinkState::InStep {
                show: show(),
                listed: true
            }),
            (
                "Linked to Sousou no Frieren. In step with your list.".to_owned(),
                None,
                linked.clone()
            )
        );
        assert_eq!(
            said(LinkState::InStep {
                show: show(),
                listed: false
            })
            .0,
            "Linked to Sousou no Frieren. It goes on your list with its next change."
        );
        assert_eq!(
            said(LinkState::Waiting { show: show() }).0,
            "Linked to Sousou no Frieren. A change waits to go out."
        );
        assert_eq!(
            said(LinkState::Refused {
                show: show(),
                message: "Validation error".to_owned()
            }),
            (
                "Linked to Sousou no Frieren.".to_owned(),
                Some("AniList refused the last change: Validation error".to_owned()),
                vec![Action::TryAgain, Action::ChangeEntry, Action::StopSyncing]
            )
        );
        assert_eq!(
            said(LinkState::Gone { show: show() }),
            (
                "Sousou no Frieren is gone from AniList.".to_owned(),
                None,
                vec![]
            )
        );
        assert_eq!(
            said(LinkState::Off),
            (
                "Not kept in step with AniList.".to_owned(),
                None,
                vec![Action::SyncThisShow]
            )
        );
        assert_eq!(said(LinkState::Unpicked { found: 3 }).2, vec![]);
    }
}
