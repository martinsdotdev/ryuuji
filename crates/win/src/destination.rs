//! What the shell is showing.
//!
//! The pane and the content area disagree while a detail is open: the header
//! names the detail, the highlight stays on the page under it, and the back
//! arrow comes alive. [`Destination::of`] takes that decision once so the
//! shell reads it instead of deriving it four times. Nothing here touches
//! WinUI, so the routing is testable without a host.

use ryuuji_core::{AppState, Detail, Page};

/// The view filling the content area. [`Page`] and [`Detail`] stay apart so
/// that adding either is one variant and one arm where the element is built.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Body {
    Page(Page),
    Detail(Detail),
}

/// Everything the NavigationView needs from [`AppState`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Destination {
    pub body: Body,
    /// Text for the header. Never empty: the toolkit has no way to clear a
    /// header once one has been set. A show's page is headed by its title.
    pub header: String,
    /// Tag of the pane item to highlight, always a [`Page`]. A tag naming no
    /// item is a silent no-op that leaves the previous highlight frozen, so
    /// a detail lights the page it opened over rather than nothing.
    pub tag: &'static str,
    /// The arrow stays visible either way and only its enabled state moves,
    /// so that showing it does not shift the pane under the pointer.
    pub back_enabled: bool,
}

impl Destination {
    pub fn of(state: &AppState) -> Destination {
        let tag = state.page.tag();
        match state.detail {
            Some(detail) => Destination {
                body: Body::Detail(detail),
                header: match detail {
                    Detail::Entry(entry) => state.entry(entry).map(|shown| shown.title.clone()),
                    Detail::Diagnostics => None,
                }
                .unwrap_or_else(|| detail.label().to_owned()),
                tag,
                back_enabled: true,
            },
            None => Destination {
                body: Body::Page(state.page),
                header: state.page.label().to_owned(),
                tag,
                back_enabled: false,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every kind of detail, a show's page included, with an id only a store
    /// can mint.
    fn details() -> Vec<Detail> {
        vec![Detail::Diagnostics, Detail::Entry(show().id)]
    }

    fn show() -> ryuuji_core::LibraryEntry {
        let tmp = tempfile::tempdir().unwrap();
        let dir = ryuuji_core::DataDir::at(tmp.path()).unwrap();
        let ryuuji_core::Opened { mut store, .. } = ryuuji_core::Store::open(&dir).unwrap();
        store
            .add(ryuuji_core::NewEntry {
                title: "Frieren".to_owned(),
                status: ryuuji_core::WatchStatus::Watching,
                progress: 7,
                total: Some(28),
                rewatching: false,
            })
            .unwrap()
    }

    fn showing(page: Page, detail: Option<Detail>) -> Destination {
        Destination::of(&AppState {
            page,
            detail,
            ..AppState::default()
        })
    }

    #[test]
    fn a_page_names_itself_and_leaves_back_disabled() {
        assert_eq!(
            showing(Page::Library, None),
            Destination {
                body: Body::Page(Page::Library),
                header: "Library".to_owned(),
                tag: "library",
                back_enabled: false,
            }
        );
    }

    #[test]
    fn a_detail_takes_the_header_and_enables_back() {
        assert_eq!(
            showing(Page::Settings, Some(Detail::Diagnostics)),
            Destination {
                body: Body::Detail(Detail::Diagnostics),
                header: "Diagnostics".to_owned(),
                tag: "settings",
                back_enabled: true,
            }
        );
    }

    #[test]
    fn a_shows_page_is_headed_by_its_title() {
        let show = show();
        let state = |library| AppState {
            page: Page::Library,
            detail: Some(Detail::Entry(show.id)),
            library,
            ..AppState::default()
        };
        assert_eq!(
            Destination::of(&state(vec![show.clone()])).header,
            "Frieren"
        );
        assert_eq!(Destination::of(&state(vec![])).header, "Show");
    }

    #[test]
    fn the_pane_stays_on_the_page_under_an_open_detail() {
        for page in Page::ALL {
            let open = showing(page, Some(Detail::Diagnostics));
            assert_eq!(open.tag, showing(page, None).tag);
            assert_eq!(open.tag, page.tag());
        }
    }

    #[test]
    fn every_destination_highlights_a_real_pane_item() {
        for page in Page::ALL {
            for detail in [None].into_iter().chain(details().into_iter().map(Some)) {
                let tag = showing(page, detail).tag;
                assert_eq!(Page::from_tag(tag), Some(page), "{page:?} over {detail:?}");
            }
        }
    }

    #[test]
    fn every_destination_names_what_it_shows() {
        for page in Page::ALL {
            assert_eq!(
                showing(page, None),
                Destination {
                    body: Body::Page(page),
                    header: page.label().to_owned(),
                    tag: page.tag(),
                    back_enabled: false,
                }
            );
            for detail in details() {
                assert_eq!(
                    showing(page, Some(detail)),
                    Destination {
                        body: Body::Detail(detail),
                        header: detail.label().to_owned(),
                        tag: page.tag(),
                        back_enabled: true,
                    }
                );
            }
        }
    }
}
