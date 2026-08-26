//! The app shell: a NavigationView whose pane lists the core's pages and whose
//! content area renders whichever page is selected.

use ryuuji_core::{AppState, Page};
use windows_reactor::*;

use crate::pages;

/// Root render function passed to `App::render`.
pub fn app(cx: &mut RenderCx) -> Element {
    let (state, set_state) = cx.use_state(AppState::default());

    let menu_items = Page::ALL.into_iter().map(|page| {
        NavViewItem::new(page.label())
            .tag(page.tag())
            .icon(icon_for(page))
    });

    let body = pages::render(&state);

    NavigationView::new(menu_items, body)
        .selected_tag(state.page.tag())
        .on_selection_changed({
            let state = state.clone();
            move |tag: String| {
                let mut next = state.clone();
                if next.select_page(&tag) {
                    set_state.call(next);
                }
            }
        })
        .pane_display_mode(NavigationViewPaneDisplayMode::Left)
        // Settings is one of our own pages so it routes like the others.
        .settings_visible(false)
        .into()
}

fn icon_for(page: Page) -> Symbol {
    match page {
        Page::Library => Symbol::Library,
        Page::NowPlaying => Symbol::Play,
        Page::Settings => Symbol::Setting,
    }
}
