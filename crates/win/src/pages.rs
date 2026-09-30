//! Page bodies. The Library lists real entries, and Settings is laid out in
//! the Windows Settings idiom (section headers and cards) with the accounts,
//! the theme picker, the data folder and the way into Diagnostics. Empty
//! pages use a placeholder in the symbolic empty-state style the UI-direction
//! research settled on (heading required, neutral tone). Now playing and
//! History live in modules of their own; History is the one page handed the
//! core handle, since it reads its rows on demand. Details are built by the
//! shell, so this module never sees the log buffer.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use ryuuji_core::{
    Account, AppState, Attempt, Command, DataDir, Detail, EntryId, LibraryEntry, Notice, Page,
    Ryuuji, Service, Signed, StoreError, ThemePreference, TimeZone, WatchStatus, date, error_chain,
};
use ryuuji_sync::{Credentials, SyncError, Syncer, Token, TokenError, authorize_url};
use windows_reactor::*;

use crate::history::{self, HistoryProps};
use crate::now_playing;
use crate::ui::{
    self, APP_VERSION, BUILD_PROFILE, CONTENT_MAX_WIDTH, FOLDER_GLYPH, REPAIR_GLYPH, caption, card,
    card_frame, enum_picker, heading, placeholder, section,
};

const PAGE_PADDING: f64 = 24.0;

/// One library row as the page draws it, worked out by the caller so the
/// page never reasons about the library itself.
#[derive(Clone, Debug, PartialEq, Eq)]
struct LibraryRow {
    entry: EntryId,
    title: String,
    /// `Completed · 3/24 · rewatching`.
    detail: String,
    /// Whether a rewatch is under way, and `None` on a show that is not
    /// finished: picking a finished show back up is the only thing the
    /// switch is for, so it has nothing to say anywhere else.
    rewatch: Option<bool>,
}

/// Every library row, in the order the store keeps them, which is the order
/// the shows were added. Nothing about a rewatch is a reason to re-sort the
/// library under the person while they are reading it.
fn library_rows(library: &[LibraryEntry]) -> Vec<LibraryRow> {
    library
        .iter()
        .map(|entry| LibraryRow {
            entry: entry.id,
            title: entry.title.clone(),
            detail: status_line(entry),
            rewatch: (entry.status == WatchStatus::Completed).then_some(entry.rewatching),
        })
        .collect()
}

/// Builds one page's body. Which page is the shell's decision, not this
/// module's, so nothing here reads [`AppState::page`].
pub fn body(
    page: Page,
    state: &AppState,
    dispatch: Dispatch<Command>,
    dir: &DataDir,
    detection_down: bool,
    core: &Rc<RefCell<Ryuuji>>,
    syncer: &Rc<std::result::Result<Syncer, SyncError>>,
) -> Element {
    match page {
        Page::Library => component(
            library,
            LibraryProps {
                dispatch,
                rows: library_rows(&state.library),
            },
        ),
        Page::NowPlaying => now_playing::body(state, dispatch, detection_down),
        Page::History => component(
            history::history,
            HistoryProps {
                dispatch,
                core: core.clone(),
                library: state.library.clone(),
            },
        ),
        Page::Seasons => placeholder(
            "Seasons isn't built yet",
            "This page will list each season's shows from AniList, so you can add them to your \
             library.",
        ),
        Page::Statistics => placeholder(
            "Statistics isn't built yet",
            "This page will count the shows and episodes Ryuuji has recorded.",
        ),
        Page::Settings => component(
            settings,
            SettingsProps {
                dispatch,
                theme: state.settings.theme,
                root: dir.root().to_path_buf(),
                accounts: state.sync.accounts.clone(),
                attempt: state.sync.attempt.clone(),
                syncer: syncer.clone(),
            },
        ),
    }
}

/// Wraps a body in what every destination shows around it: the notice bar
/// above, the page padding, and the status bar below.
pub fn chrome(
    notices: &[Notice],
    dispatch: Dispatch<Command>,
    body: Element,
    status: Border,
) -> Element {
    grid((
        notice(notices, dispatch),
        border(body)
            .padding(Thickness::uniform(PAGE_PADDING))
            .grid_row(1),
        status.grid_row(2),
    ))
    .rows([GridLength::Auto, GridLength::STAR, GridLength::Auto])
    .into()
}

/// Shown instead of the shell when the library could not be opened.
pub fn boot_failed(err: &StoreError, dir: &DataDir) -> Element {
    border(placeholder(
        "Ryuuji couldn't open its library",
        format!(
            "{}\nData directory: {}",
            error_chain(err),
            dir.root().display()
        ),
    ))
    .padding(Thickness::uniform(PAGE_PADDING))
    .into()
}

/// Shows the oldest notice, or nothing at all when the queue is empty.
/// Keyed on the queue length and the notice shown, so a dismissal remounts
/// the bar: `IsOpen` is diffed, and WinUI has already closed it. The length
/// alone is not enough, because a reread file replaces its notice in place,
/// and a pop and a replacement handled in one render leave the length as
/// it was.
fn notice(notices: &[Notice], dispatch: Dispatch<Command>) -> Element {
    let head = notices.first();
    let (severity, title, message) = match head {
        Some(Notice::SaveFailed { detail }) => (
            InfoBarSeverity::Error,
            "Couldn't save your change".to_owned(),
            detail.clone(),
        ),
        Some(Notice::FileProblem { file, problems }) => (
            InfoBarSeverity::Warning,
            format!(
                "{} has {}",
                file.file_name(),
                if problems.len() == 1 {
                    "a problem"
                } else {
                    "problems"
                }
            ),
            problems.join("\n"),
        ),
        Some(Notice::LibraryReset { backup }) => (
            InfoBarSeverity::Warning,
            "Your library was reset".to_owned(),
            format!(
                "The previous file wasn't a readable database. It was kept at {}.",
                backup.display()
            ),
        ),
        // The InfoBar has no action button, so each message names its fix.
        Some(Notice::SignInRefused { service }) => (
            InfoBarSeverity::Error,
            format!("{} refused the sign-in", service.label()),
            format!(
                "Nothing is sent to {} until you paste a new token in Settings.",
                service.label()
            ),
        ),
        Some(Notice::ListRejected {
            service,
            show,
            message,
        }) => (
            InfoBarSeverity::Warning,
            format!("{} refused a change to {show}", service.label()),
            format!("{message}. It is sent again when the show next changes."),
        ),
        Some(Notice::ListGone { service, show }) => (
            InfoBarSeverity::Warning,
            format!("{} no longer has {show}", service.label()),
            "Pick the show again to keep it in step.".to_owned(),
        ),
        None => return Element::Empty,
    };
    InfoBar::new(title)
        .message(message)
        .severity(severity)
        .is_open(true)
        .on_closed(move || dispatch.call(Command::DismissNotice))
        .with_key(format!("{} {head:?}", notices.len()))
        .grid_row(0)
        .into()
}

/// What the Library page is handed. `dispatch` is fresh every render and
/// compares by identity, so it comes first and the props compare stops
/// there, as [`NowPlayingProps`] does.
#[derive(Clone, Debug, PartialEq, Eq)]
struct LibraryProps {
    dispatch: Dispatch<Command>,
    rows: Vec<LibraryRow>,
}

fn library(props: &LibraryProps, _cx: &mut RenderCx) -> Element {
    if props.rows.is_empty() {
        return placeholder(
            "Your library is empty",
            "Shows you track will appear here. Add one from Now playing when an episode is detected.",
        );
    }
    let dispatch = props.dispatch.clone();
    list_view(props.rows.clone(), move |row: &LibraryRow, _| {
        let main = vstack((
            text_block(row.title.clone()).semibold(),
            text_block(row.detail.clone()).foreground(ThemeRef::SecondaryText),
        ))
        .spacing(2.0)
        .vertical_alignment(VerticalAlignment::Center)
        .grid_column(0);
        let action: Element = match row.rewatch {
            Some(rewatching) => {
                let dispatch = dispatch.clone();
                let id = row.entry;
                button(rewatch_label(rewatching))
                    .on_click(move || {
                        dispatch.call(Command::SetRewatching {
                            id,
                            rewatching: !rewatching,
                        });
                    })
                    .vertical_alignment(VerticalAlignment::Center)
                    .grid_column(1)
                    .into()
            }
            None => Element::Empty,
        };
        grid(vec![main.into(), action]).columns([GridLength::STAR, GridLength::Auto])
    })
    .with_key_selector(|row: &LibraryRow| row.entry.to_string())
    .into()
}

/// What the row offers a finished show: the way into a rewatch, or the way
/// back out of one.
fn rewatch_label(rewatching: bool) -> &'static str {
    if rewatching {
        "Stop rewatching"
    } else {
        "Rewatch"
    }
}

fn status_line(entry: &LibraryEntry) -> String {
    let mut line = match entry.total {
        Some(total) => format!("{} · {}/{}", entry.status.label(), entry.progress, total),
        None => format!("{} · {}", entry.status.label(), entry.progress),
    };
    // Said on the row rather than in place of the status: the show is still
    // completed, and the count underneath it is a rewatch rather than a
    // finished show that somehow slipped back.
    if entry.rewatching {
        line.push_str(" · rewatching");
    }
    line
}

/// What Settings shows. The outcome of the last folder action is the page's
/// own, so it lives in the page rather than in the shell.
#[derive(Clone)]
struct SettingsProps {
    dispatch: Dispatch<Command>,
    theme: ThemePreference,
    root: PathBuf,
    accounts: Vec<Account>,
    attempt: Option<Attempt>,
    /// The sync worker, or why it is not running; the cards sign in and out
    /// through it.
    syncer: Rc<std::result::Result<Syncer, SyncError>>,
}

/// Identity for the worker handle, contents for the rest. `dispatch` is
/// fresh every render, so this never holds today.
impl PartialEq for SettingsProps {
    fn eq(&self, other: &SettingsProps) -> bool {
        self.dispatch == other.dispatch
            && self.theme == other.theme
            && self.root == other.root
            && self.accounts == other.accounts
            && self.attempt == other.attempt
            && Rc::ptr_eq(&self.syncer, &other.syncer)
    }
}

fn settings(props: &SettingsProps, cx: &mut RenderCx) -> Element {
    let (folder_action, set_folder_action) = cx.use_state(None::<String>);

    let dispatch = props.dispatch.clone();
    let open_diagnostics = {
        let dispatch = dispatch.clone();
        move || dispatch.call(Command::OpenDetail(Detail::Diagnostics))
    };
    // One card per service, whatever its standing, so the way in is always
    // where it was.
    let accounts: Vec<Element> = std::iter::once(section("Accounts").into())
        .chain(Service::ALL.into_iter().map(|service| {
            component(
                account_card,
                AccountCardProps {
                    service,
                    account: props
                        .accounts
                        .iter()
                        .find(|account| account.service == service)
                        .cloned(),
                    attempt: props.attempt.clone(),
                    syncer: props.syncer.clone(),
                },
            )
        }))
        .collect();
    scroll_viewer(
        vstack((
            vstack(accounts).spacing(8.0),
            vstack((section("Appearance"), theme_card(props.theme, dispatch))).spacing(8.0),
            vstack((
                section("Your data"),
                card(
                    Some(FOLDER_GLYPH),
                    "Data folder",
                    props.root.display().to_string(),
                    hstack((
                        ui::open_folder_button(props.root.clone(), set_folder_action),
                        caption(folder_action.unwrap_or_default())
                            .vertical_alignment(VerticalAlignment::Center),
                    ))
                    .spacing(12.0)
                    .into(),
                ),
                card(
                    Some(REPAIR_GLYPH),
                    "Diagnostics",
                    "Paths, schema version and recent log events",
                    button("Open diagnostics").on_click(open_diagnostics).into(),
                ),
            ))
            .spacing(8.0),
            caption(format!("Ryuuji {APP_VERSION} · {BUILD_PROFILE} build")),
        ))
        .spacing(24.0)
        .max_width(CONTENT_MAX_WIDTH),
    )
    .into()
}

/// What one service's card on Settings is handed. The card is a component of
/// its own because the paste belongs to the card that takes it: it lives in
/// the card's state until Sign in is pressed and goes nowhere else, never
/// into a command, the app state, a notice or a log.
#[derive(Clone)]
struct AccountCardProps {
    service: Service,
    account: Option<Account>,
    attempt: Option<Attempt>,
    syncer: Rc<std::result::Result<Syncer, SyncError>>,
}

/// Identity for the worker handle, contents for the rest.
impl PartialEq for AccountCardProps {
    fn eq(&self, other: &AccountCardProps) -> bool {
        self.service == other.service
            && self.account == other.account
            && self.attempt == other.attempt
            && Rc::ptr_eq(&self.syncer, &other.syncer)
    }
}

fn account_card(props: &AccountCardProps, cx: &mut RenderCx) -> Element {
    let (pasted, set_pasted) = cx.use_state(String::new());
    let (problem, set_problem) = cx.use_state(None::<&'static str>);
    let service = props.service;
    let shown = account_card_state(
        service,
        props.account.as_ref(),
        props.attempt.as_ref(),
        props.syncer.is_ok(),
        &ui::local_zone(),
    );
    let body: Element = match shown {
        AccountCard::Unavailable(text) => heading(service.label(), text).into(),
        // A ring, not a bar: the person waits on this one.
        AccountCard::Checking(text) => vstack((
            text_block(service.label()),
            hstack((
                ProgressRing::indeterminate().width(20.0).height(20.0),
                caption(text).vertical_alignment(VerticalAlignment::Center),
            ))
            .spacing(12.0),
        ))
        .spacing(8.0)
        .into(),
        AccountCard::SignedIn { who, until } => {
            let syncer = props.syncer.clone();
            vstack((
                text_block(service.label()),
                text_block(who),
                caption(until),
                button("Sign out")
                    .on_click(move || {
                        if let Ok(syncer) = &*syncer {
                            syncer.sign_out(service);
                        }
                    })
                    .horizontal_alignment(HorizontalAlignment::Left),
            ))
            .spacing(4.0)
            .into()
        }
        AccountCard::Paste {
            caption: why,
            refused,
            failed,
        } => {
            let mut lines: Vec<Element> = vec![heading(service.label(), why).into()];
            if let Some(refused) = refused {
                lines.push(caption(refused).wrap().into());
            }
            lines.push(
                HyperlinkButton::new("Get a token")
                    .navigate_uri(token_page(service))
                    .into(),
            );
            let sign_in = {
                let syncer = props.syncer.clone();
                let set_pasted = set_pasted.clone();
                let set_problem = set_problem.clone();
                let pasted = pasted.clone();
                move || match parse_paste(service, &pasted) {
                    Ok(token) => {
                        set_problem.call(None);
                        set_pasted.call(String::new());
                        if let Ok(syncer) = &*syncer {
                            syncer.sign_in(service, Credentials::Token(token));
                        }
                    }
                    Err(problem) => set_problem.call(Some(problem)),
                }
            };
            lines.push(
                hstack((
                    PasswordBox::new()
                        .value(pasted.clone())
                        .placeholder_text("Token")
                        .on_password_changed(set_pasted.clone())
                        // A row gives the box all the width it asks for, and a
                        // pasted token is about a thousand characters: a fixed
                        // width keeps Sign in on the card.
                        .width(360.0),
                    button("Sign in").accent().on_click(sign_in),
                ))
                .spacing(8.0)
                .into(),
            );
            // The paste's own problem is the newer word, so it stands in
            // for a failed check's message until the next Sign in.
            if let Some(under) = problem.map(str::to_owned).or(failed) {
                lines.push(caption(under).wrap().into());
            }
            vstack(lines).spacing(8.0).into()
        }
    };
    card_frame(body).into()
}

/// What the account card shows for one service, decided from the state
/// alone so it can be pinned without WinUI.
#[derive(Clone, Debug, PartialEq, Eq)]
enum AccountCard {
    /// The worker never started, so nothing can be checked or sent.
    Unavailable(String),
    /// A pasted token is being checked; the person waits on the answer.
    Checking(String),
    /// The paste controls under `caption`: before any sign-in, after a
    /// sign-out, after a refusal (`refused` says why, above the controls)
    /// and after a failed check (`failed` says why, under the box).
    Paste {
        caption: String,
        refused: Option<String>,
        failed: Option<String>,
    },
    /// Signed in: who, and until when the token works.
    SignedIn { who: String, until: String },
}

fn account_card_state(
    service: Service,
    account: Option<&Account>,
    attempt: Option<&Attempt>,
    available: bool,
    zone: &TimeZone,
) -> AccountCard {
    let label = service.label();
    if !available {
        return AccountCard::Unavailable(format!(
            "Sync isn't running, so nothing can be sent to {label} this time. Diagnostics says what went wrong."
        ));
    }
    if matches!(attempt, Some(Attempt::Checking(checking)) if *checking == service) {
        return AccountCard::Checking(format!("Checking with {label}\u{2026}"));
    }
    let failed = match attempt {
        Some(Attempt::Failed {
            service: failed,
            message,
        }) if *failed == service => Some(message.clone()),
        _ => None,
    };
    let caption = format!("Paste the token {label} shows after you allow Ryuuji.");
    match account.map(|account| (&account.signed, account)) {
        Some((Signed::In, account)) => AccountCard::SignedIn {
            who: format!("Signed in as {}", account.name),
            until: match account.expires {
                Some(at) => format!("Token works until {}", date(at, zone)),
                None => "No expiry date in the token".to_owned(),
            },
        },
        Some((Signed::Refused { message }, _)) => AccountCard::Paste {
            caption,
            refused: Some(format!("{label} refused the token: {message}")),
            failed,
        },
        Some((Signed::Out, _)) | None => AccountCard::Paste {
            caption,
            refused: None,
            failed,
        },
    }
}

/// The token in a paste, or what to say under the box instead. [`Token::parse`]
/// is the one judge, so a paste it refuses is never sent; the words are the
/// card's.
fn parse_paste(service: Service, pasted: &str) -> std::result::Result<Token, &'static str> {
    Token::parse(pasted).map_err(|error| match (service, error) {
        (Service::AniList, TokenError::Empty) => "Paste the token first.",
        (Service::AniList, TokenError::NotAToken) => "That doesn't look like an AniList token.",
    })
}

/// Where the person is sent for a token.
fn token_page(service: Service) -> String {
    match service {
        Service::AniList => authorize_url(),
    }
}

/// The theme picker, a drop-down like Windows Settings' "Choose your mode".
fn theme_card(theme: ThemePreference, dispatch: Dispatch<Command>) -> Border {
    let combo = enum_picker(
        &ThemePreference::ALL,
        ThemePreference::label,
        theme,
        move |chosen| dispatch.call(Command::SetTheme(chosen)),
    )
    .min_width(160.0);
    card(
        None,
        "Theme",
        "Follow Windows, or pick light or dark.",
        combo.into(),
    )
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use ryuuji_core::{NewEntry, NewRecording, Opened, Store, UserId, WatchStatus};

    use super::*;

    /// A JWT with a fixed header, the payload `{"exp":1790121600,"sub":"7"}`
    /// and a signature nothing verifies: what a good paste looks like.
    const GOOD_PASTE: &str = "eyJhbGciOiJSUzI1NiIsInR5cCI6IkpXVCJ9.eyJleHAiOjE3OTAxMjE2MDAsInN1YiI6IjcifQ.bm90IGEgcmVhbCBzaWduYXR1cmU";

    fn account(signed: Signed, expires: Option<SystemTime>) -> Account {
        Account {
            service: Service::AniList,
            user: UserId(7),
            name: "someone".into(),
            expires,
            signed,
        }
    }

    fn paste(refused: Option<&str>, failed: Option<&str>) -> AccountCard {
        AccountCard::Paste {
            caption: "Paste the token AniList shows after you allow Ryuuji.".into(),
            refused: refused.map(str::to_owned),
            failed: failed.map(str::to_owned),
        }
    }

    #[test]
    fn the_account_card_says_where_the_sign_in_stands() {
        let card = |account: Option<&Account>, attempt: Option<&Attempt>| {
            account_card_state(Service::AniList, account, attempt, true, &TimeZone::UTC)
        };
        assert_eq!(card(None, None), paste(None, None));
        assert_eq!(
            card(Some(&account(Signed::Out, None)), None),
            paste(None, None)
        );
        assert_eq!(
            card(None, Some(&Attempt::Checking(Service::AniList))),
            AccountCard::Checking("Checking with AniList\u{2026}".into())
        );
        let failed = Attempt::Failed {
            service: Service::AniList,
            message: "Invalid token".into(),
        };
        assert_eq!(
            card(None, Some(&failed)),
            paste(None, Some("Invalid token"))
        );
        let refused = account(
            Signed::Refused {
                message: "Unauthorized.".into(),
            },
            None,
        );
        assert_eq!(
            card(Some(&refused), None),
            paste(Some("AniList refused the token: Unauthorized."), None)
        );
        // A paste that failed after a refusal is the newer word, and both
        // are said in their places.
        assert_eq!(
            card(Some(&refused), Some(&failed)),
            paste(
                Some("AniList refused the token: Unauthorized."),
                Some("Invalid token")
            )
        );
    }

    #[test]
    fn a_signed_in_card_names_the_person_and_the_local_day_the_token_works_until() {
        // 2026-09-23T00:00:00Z: the 22nd three hours west of Greenwich.
        let expires = UNIX_EPOCH + Duration::from_secs(1_790_121_600);
        let signed_in = account(Signed::In, Some(expires));
        assert_eq!(
            account_card_state(
                Service::AniList,
                Some(&signed_in),
                None,
                true,
                &TimeZone::UTC
            ),
            AccountCard::SignedIn {
                who: "Signed in as someone".into(),
                until: "Token works until 2026-09-23".into(),
            }
        );
        let west = TimeZone::posix("<-03>3").unwrap();
        assert_eq!(
            account_card_state(Service::AniList, Some(&signed_in), None, true, &west),
            AccountCard::SignedIn {
                who: "Signed in as someone".into(),
                until: "Token works until 2026-09-22".into(),
            }
        );
        assert_eq!(
            account_card_state(
                Service::AniList,
                Some(&account(Signed::In, None)),
                None,
                true,
                &TimeZone::UTC
            ),
            AccountCard::SignedIn {
                who: "Signed in as someone".into(),
                until: "No expiry date in the token".into(),
            }
        );
    }

    #[test]
    fn a_worker_that_never_started_leaves_one_caption_whatever_the_account() {
        let unavailable = AccountCard::Unavailable(
            "Sync isn't running, so nothing can be sent to AniList this time. Diagnostics says what went wrong.".into(),
        );
        assert_eq!(
            account_card_state(Service::AniList, None, None, false, &TimeZone::UTC),
            unavailable
        );
        assert_eq!(
            account_card_state(
                Service::AniList,
                Some(&account(Signed::In, None)),
                Some(&Attempt::Checking(Service::AniList)),
                false,
                &TimeZone::UTC
            ),
            unavailable
        );
    }

    #[test]
    fn a_paste_is_judged_by_token_parse_and_a_bad_one_is_named_under_the_box() {
        assert_eq!(
            parse_paste(Service::AniList, "abc"),
            Err("That doesn't look like an AniList token.")
        );
        assert_eq!(
            parse_paste(Service::AniList, "a.b.c.d"),
            Err("That doesn't look like an AniList token.")
        );
        assert_eq!(
            parse_paste(Service::AniList, "  \n"),
            Err("Paste the token first.")
        );
        let token = parse_paste(Service::AniList, &format!(" {GOOD_PASTE}\r\n")).unwrap();
        assert_eq!(token, Token::parse(GOOD_PASTE).unwrap());
    }

    #[test]
    fn the_token_page_is_the_services_authorize_url() {
        assert_eq!(token_page(Service::AniList), authorize_url());
        assert!(token_page(Service::AniList).contains(ryuuji_sync::CLIENT_ID));
    }

    /// Only the store mints ids, so the Recorded arm needs a show and a
    /// recording made for real: episode 1, which leaves progress at 1.
    fn recorded_show() -> (LibraryEntry, ryuuji_core::HistoryId) {
        let tmp = tempfile::tempdir().unwrap();
        let dir = DataDir::at(tmp.path()).unwrap();
        let Opened { mut store, .. } = Store::open(&dir).unwrap();
        let entry = store
            .add(NewEntry {
                title: "Show".to_owned(),
                status: WatchStatus::Watching,
                progress: 0,
                total: None,
                rewatching: false,
            })
            .unwrap();
        let recording = store
            .record(NewRecording {
                watch: None,
                entry: entry.id,
                episode: 1..=1,
                raw_title: "Show - 01.mkv".to_owned(),
                parsed_title: "Show".to_owned(),
                player: "mpv".to_owned(),
            })
            .unwrap();
        (recording.entry, recording.watch.id)
    }

    /// The seeded show shelved somewhere else; only the status, the numbers
    /// and the flag matter here, and the shell cannot mint an entry id.
    fn shelved(
        entry: &LibraryEntry,
        status: WatchStatus,
        progress: u32,
        total: Option<u32>,
        rewatching: bool,
    ) -> LibraryEntry {
        LibraryEntry {
            status,
            progress,
            total,
            rewatching,
            ..entry.clone()
        }
    }

    #[test]
    fn only_a_finished_show_is_offered_a_rewatch() {
        let (entry, _) = recorded_show();
        let rows = library_rows(&[
            shelved(&entry, WatchStatus::Watching, 7, Some(28), false),
            shelved(&entry, WatchStatus::Completed, 24, Some(24), false),
            shelved(&entry, WatchStatus::Completed, 3, Some(24), true),
            shelved(&entry, WatchStatus::Completed, 5, None, true),
        ]);
        // Picking a finished show back up is the only thing the switch is
        // for, so a show still being watched is offered nothing.
        assert_eq!(
            rows.iter().map(|row| row.rewatch).collect::<Vec<_>>(),
            vec![None, Some(false), Some(true), Some(true)]
        );
        assert_eq!(rows[0].detail, "Watching \u{b7} 7/28");
        assert_eq!(rows[1].detail, "Completed \u{b7} 24/24");
        assert_eq!(rows[2].detail, "Completed \u{b7} 3/24 \u{b7} rewatching");
        assert_eq!(rows[3].detail, "Completed \u{b7} 5 \u{b7} rewatching");
    }

    #[test]
    fn the_rewatch_button_says_which_way_it_goes() {
        assert_eq!(rewatch_label(false), "Rewatch");
        assert_eq!(rewatch_label(true), "Stop rewatching");
    }
}
