//! The app shell: a NavigationView whose pane lists the core's pages and whose
//! content area renders whichever page is selected.

use std::cell::RefCell;
use std::rc::Rc;

use ryuuji_core::{
    AppState, Command, DataDir, Page, Ryuuji, StoreError, ThemePreference, error_chain,
};
use windows_reactor::{
    Component, Element, NavViewItem, NavigationView, NavigationViewPaneDisplayMode, RenderCx,
    RequestedTheme, Symbol, set_requested_theme,
};

use crate::pages;

/// Root component. Owns the core for the life of the window; the reducer
/// hook mirrors its state so the tree rerenders after every command.
pub struct Shell {
    dir: DataDir,
    core: Result<Rc<RefCell<Ryuuji>>, StoreError>,
}

impl Shell {
    pub fn boot(dir: DataDir) -> Shell {
        let core = Ryuuji::open(&dir).map(|core| Rc::new(RefCell::new(core)));
        match &core {
            // Before the first render, so the first frame already carries the
            // saved theme instead of flashing the system one.
            Ok(core) => set_requested_theme(requested_theme(core.borrow().state().settings.theme)),
            Err(err) => tracing::error!(error = %error_chain(err), "boot failed"),
        }
        Shell { dir, core }
    }
}

impl Component for Shell {
    fn render(&self, _props: &(), cx: &mut RenderCx) -> Element {
        let core = match &self.core {
            Ok(core) => core,
            Err(err) => return pages::boot_failed(err, &self.dir),
        };

        let (state, dispatch) = cx.use_reducer_fn(
            {
                let core = core.clone();
                move |_prev: AppState, command: Command| {
                    let before = core.borrow().state().settings.theme;
                    core.borrow_mut().dispatch(command);
                    let state = core.borrow().state().clone();
                    if state.settings.theme != before {
                        set_requested_theme(requested_theme(state.settings.theme));
                    }
                    state
                }
            },
            core.borrow().state().clone(),
        );

        let menu_items = Page::ALL.into_iter().map(|page| {
            NavViewItem::new(page.label())
                .tag(page.tag())
                .icon(icon_for(page))
        });

        let body = pages::render(&state, dispatch.clone());

        NavigationView::new(menu_items, body)
            .selected_tag(state.page.tag())
            .on_selection_changed(move |tag: String| {
                if let Some(page) = Page::from_tag(&tag) {
                    dispatch.call(Command::SelectPage(page));
                }
            })
            .pane_display_mode(NavigationViewPaneDisplayMode::Left)
            // Settings is one of our own pages so it routes like the others.
            .settings_visible(false)
            .into()
    }
}

fn requested_theme(theme: ThemePreference) -> RequestedTheme {
    match theme {
        ThemePreference::System => RequestedTheme::Default,
        ThemePreference::Light => RequestedTheme::Light,
        ThemePreference::Dark => RequestedTheme::Dark,
    }
}

fn icon_for(page: Page) -> Symbol {
    match page {
        Page::Library => Symbol::Library,
        Page::NowPlaying => Symbol::Play,
        Page::Settings => Symbol::Setting,
    }
}
