//! The app shell: a NavigationView whose pane lists the core's pages and whose
//! content area renders the selected page, or the detail open over it.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::SystemTime;

use ryuuji_core::{
    AppState, Command, DataDir, NowPlaying, Page, PlaybackEvent, Ryuuji, StoreError,
    ThemePreference, error_chain,
};
use ryuuji_detect::{WatchError, Watcher};
use windows_reactor::{
    Component, Element, NavViewItem, NavigationView, NavigationViewPaneDisplayMode, RenderCx,
    RequestedTheme, Symbol, set_requested_theme,
};

use crate::debug::{self, EventLevelFilter, Report};
use crate::logging::RecentEvents;
use crate::pages;
use crate::ui;

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
        let (filter, set_filter) = cx.use_state(EventLevelFilter::All);
        let (last_action, set_last_action) = cx.use_state(None::<String>);
        let on_detail = state.detail.is_some();
        let wants_timer = on_detail
            || (state.page == Page::NowPlaying
                && matches!(state.now_playing, NowPlaying::Idle)
                && state.last_match.is_some());
        ui::use_refresh(cx, wants_timer);

        let menu_items = Page::ALL.into_iter().map(|page| {
            NavViewItem::new(page.label())
                .tag(page.tag())
                .icon(icon_for(page))
        });

        let detection_down = watcher.is_err();
        let body = pages::render(
            &state,
            dispatch.clone(),
            &self.dir,
            pages::Env {
                detection_down,
                now: SystemTime::now(),
            },
            {
                let core = core.clone();
                let recent = self.recent.clone();
                let dispatch = dispatch.clone();
                move || {
                    let events = recent.snapshot();
                    let sessions = Result::as_ref(&watcher)
                        .map(Watcher::sessions)
                        .map_err(|err| error_chain(err));
                    let report = Report::new(
                        core.borrow().diagnostics(),
                        core.borrow().state().last_match.clone(),
                        events,
                        sessions,
                    );
                    debug::page(
                        &report,
                        filter,
                        set_filter,
                        last_action,
                        set_last_action,
                        dispatch.clone(),
                    )
                }
            },
        );

        let back = {
            let dispatch = dispatch.clone();
            move || dispatch.call(Command::CloseDetail)
        };
        let header = match state.detail {
            Some(detail) => detail.label(),
            None => state.page.label(),
        };

        NavigationView::new(menu_items, body)
            .header(header)
            .selected_tag(state.page.tag())
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
            .back_enabled(on_detail)
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
