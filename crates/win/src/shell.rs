//! The app shell: a NavigationView whose pane lists the core's pages and whose
//! content area renders the selected page, or the detail open over it. Which
//! of the two, and the header, highlight and back arrow that go with it, come
//! from [`Destination`]; this module only builds the WinUI that consumes it.

use std::cell::RefCell;
use std::rc::Rc;

use ryuuji_core::{
    AppState, Command, DataDir, Detail, Page, PlaybackEvent, Ryuuji, StoreError, ThemePreference,
    error_chain,
};
use ryuuji_detect::{WatchError, Watcher};
use windows_reactor::{
    Component, Element, NavViewItem, NavigationView, NavigationViewPaneDisplayMode, RenderCx,
    RequestedTheme, Symbol, component, set_requested_theme,
};

use crate::debug;
use crate::destination::{Body, Destination};
use crate::logging::RecentEvents;
use crate::pages;

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

        let (state, dispatch) = cx.use_reducer_fn(
            {
                let core = core.clone();
                move |_prev: AppState, command: Command| {
                    core.borrow_mut().dispatch(command);
                    core.borrow().state().clone()
                }
            },
            core.borrow().state().clone(),
        );
        // A memo, not an effect: the host only accepts theme requests while
        // rendering, and d72b518 recorded a post-commit effect painting the
        // first frame in the system theme. Keyed on the preference, so it
        // fires on the first render and on every change after it.
        let theme = state.settings.theme;
        cx.use_memo(theme, move || {
            set_requested_theme(requested_theme(theme));
        });
        // The watcher lives for the Shell's life; process exit ends its worker.
        let (latest, set_latest) = cx.use_async_state::<Option<PlaybackEvent>>(None);
        let watcher: Rc<Result<Watcher, WatchError>> = cx.use_memo((), move || {
            Rc::new(ryuuji_detect::watch(move |event| {
                set_latest.call(Some(event));
            }))
        });
        cx.use_effect(latest.clone(), {
            let dispatch = dispatch.clone();
            move || {
                if let Some(event) = latest {
                    dispatch.call(Command::Playback(event));
                }
            }
        });

        let menu_items = Page::ALL.into_iter().map(|page| {
            NavViewItem::new(page.label())
                .tag(page.tag())
                .icon(icon_for(page))
        });

        let detection_down = watcher.is_err();
        let showing = Destination::of(&state);
        // The one place a body is chosen, so a new `Detail` stops the build
        // here until it is given an arm.
        let body = match showing.body {
            Body::Page(page) => {
                pages::body(page, &state, dispatch.clone(), &self.dir, detection_down)
            }
            // Built here rather than in `pages`: the props carry the core
            // handle, the log buffer and the watcher, all owned by `Shell`.
            Body::Detail(Detail::Diagnostics) => component(
                debug::diagnostics,
                debug::DiagnosticsProps {
                    dispatch: dispatch.clone(),
                    core: core.clone(),
                    recent: self.recent.clone(),
                    watcher: watcher.clone(),
                },
            ),
        };
        let body = pages::chrome(&state.notices, dispatch.clone(), body);

        let back = {
            let dispatch = dispatch.clone();
            move || dispatch.call(Command::CloseDetail)
        };

        NavigationView::new(menu_items, body)
            .header(showing.header)
            .selected_tag(showing.tag)
            .on_selection_changed(move |tag: String| {
                if let Some(page) = Page::from_tag(&tag) {
                    dispatch.call(Command::SelectPage(page));
                }
            })
            // A collapsed back button reserves no layout space, so toggling
            // visibility would shift the pane under the pointer; Microsoft's
            // guidance is to leave the arrow up and disable it instead, "to
            // minimize UI elements moving around". The toolkit forces the same
            // shape anyway: it only pushes IsBackButtonVisible when it is
            // false, so a later `true` never re-shows the arrow.
            .back_enabled(showing.back_enabled)
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
    }
}
