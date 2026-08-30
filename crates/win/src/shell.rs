//! The app shell: a NavigationView whose pane lists the core's pages and whose
//! content area renders whichever page is selected.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use ryuuji_core::{
    AppState, Command, DataDir, Page, Ryuuji, StoreError, ThemePreference, error_chain,
};
use windows_reactor::{
    Component, DispatcherTimer, Element, NavViewItem, NavigationView,
    NavigationViewPaneDisplayMode, RenderCx, RequestedTheme, Symbol, set_requested_theme,
};

use crate::debug::{self, EventLevelFilter, Report};
use crate::logging::RecentEvents;
use crate::pages;

const DIAGNOSTICS_REFRESH: Duration = Duration::from_secs(2);

/// Root component. Owns the core for the life of the window; the reducer
/// hook mirrors its state so the tree rerenders after every command.
pub struct Shell {
    dir: DataDir,
    core: Result<Rc<RefCell<Ryuuji>>, StoreError>,
    recent: RecentEvents,
}

impl Shell {
    pub fn boot(dir: DataDir, recent: RecentEvents) -> Shell {
        let core = Ryuuji::open(&dir).map(|core| Rc::new(RefCell::new(core)));
        if let Err(err) = &core {
            tracing::error!(error = %error_chain(err), "boot failed");
        }
        Shell { dir, core, recent }
    }
}

impl Component for Shell {
    fn render(&self, _props: &(), cx: &mut RenderCx) -> Element {
        let core = match &self.core {
            Ok(core) => core,
            Err(err) => return pages::boot_failed(err, &self.dir),
        };

        // Runs once, during the first render: the host only accepts theme
        // requests while rendering, and this lands before the first commit.
        cx.use_memo((), {
            let theme = core.borrow().state().settings.theme;
            move || set_requested_theme(requested_theme(theme))
        });

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
        // The tick has no reader; each bump only forces a rerender so the
        // diagnostics page gathers fresh values.
        let (_tick, bump) = cx.use_reducer(0u32);
        let (filter, set_filter) = cx.use_state(EventLevelFilter::All);
        let (last_action, set_last_action) = cx.use_state(None::<String>);
        let on_debug = state.page == Page::Debug;
        cx.use_effect_with_cleanup(on_debug, move || {
            if !on_debug {
                return None;
            }
            match DispatcherTimer::new(DIAGNOSTICS_REFRESH, move || {
                bump.call(|n| n.wrapping_add(1));
            }) {
                Ok(timer) => Some(move || drop(timer)),
                Err(err) => {
                    tracing::warn!(%err, "diagnostics timer not started");
                    None
                }
            }
        });

        let menu_items = Page::NAV.into_iter().map(|page| {
            NavViewItem::new(page.label())
                .tag(page.tag())
                .icon(icon_for(page))
        });

        let body = pages::render(&state, dispatch.clone(), &self.dir, {
            let core = core.clone();
            let recent = self.recent.clone();
            move || {
                let events = recent.snapshot();
                let report = Report::new(core.borrow().diagnostics(), events);
                debug::page(&report, filter, set_filter, last_action, set_last_action)
            }
        });

        let highlighted = if on_debug { Page::Settings } else { state.page };
        let back = {
            let dispatch = dispatch.clone();
            move || dispatch.call(Command::SelectPage(Page::Settings))
        };

        NavigationView::new(menu_items, body)
            .header(state.page.label())
            .selected_tag(highlighted.tag())
            .on_selection_changed(move |tag: String| {
                if let Some(page) = Page::from_tag(&tag) {
                    dispatch.call(Command::SelectPage(page));
                }
            })
            // The toolkit only pushes IsBackButtonVisible when it is false, so
            // a later `true` never re-shows the arrow; keep it visible and gate
            // it through `back_enabled` instead.
            .back_enabled(on_debug)
            .on_back_requested(back)
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
        Page::Debug => Symbol::Repair,
    }
}
