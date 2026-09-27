//! What keeping a show in step with a list service looks like from the
//! core: the account signed in, the media a show is tied to, the base a
//! merge measures both sides against, and the work that follows from them.
//! The rows live in `store::lists`, the rule that settles a difference in
//! `merge`, and what the reducer does with each report in `app::sync`.

mod merge;

use std::fmt;
use std::time::SystemTime;

use crate::tagged::tagged_enum;
use crate::{AppState, EntryId, LibraryEntry, ListStatus};

pub use merge::{Asked, Merge, Patch, Write, merge};
pub(crate) use merge::{AutoLink, Settlement, auto_link, settlement};

tagged_enum! {
    /// A list service a show can be kept in step with. Tags are stored in
    /// the `accounts`, `listings` and `updates` tables.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    pub enum Service {
        AniList => "anilist", "AniList",
    }
}

/// A person's identity on a service. A show's base is keyed by this, so
/// signing in as someone else starts every show from a fresh first merge:
/// another account's list says nothing about where this one stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct UserId(pub u64);

/// A show as a service numbers it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MediaId(pub u64);

/// A row of a person's list as a service numbers it. AniList and Kitsu
/// number list rows apart from the media; MyAnimeList does not, so a listed
/// show carries one only where the service has one to give.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ListEntryId(pub u64);

/// What a list holds for a show: the three fields the merge settles one by
/// one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ListState {
    pub status: ListStatus,
    pub progress: u32,
    /// Finished rewatches, as the list counts them.
    pub repeat: u32,
}

/// What a read of the service found for a show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Remote {
    Unlisted,
    Listed {
        id: Option<ListEntryId>,
        state: ListState,
    },
}

/// The last state the list was confirmed to hold for a show, and for whom.
/// The merge measures both sides against it: a field only the list moved
/// is taken, a field only Ryuuji moved is sent. It is keyed by the user
/// because it describes one person's list, and another person's starts
/// from nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ListBase {
    pub user: UserId,
    /// Whether the list held the show. One it lacks is held until it next
    /// changes here, and that change creates the row.
    pub listed: bool,
    pub state: ListState,
}

/// A show as a service describes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Media {
    pub id: MediaId,
    pub title: String,
    /// The episode count, once the service knows it.
    pub episodes: Option<u32>,
    /// The same show on MyAnimeList, which pairs it there with no search.
    pub mal_id: Option<u64>,
}

/// A show a search turned up, with what tells it apart from the others.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub media: Media,
    /// Every title and synonym the service lists for it.
    pub titles: Vec<String>,
    pub format: Option<String>,
    pub year: Option<u16>,
}

tagged_enum! {
    /// Who tied a show to its media: a search that found exactly one fitting
    /// title, or the person from the picker. Tags are stored in the
    /// `listings` table.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum LinkedBy {
        Title => "title", "Linked by its title",
        Person => "person", "Linked by you",
    }
}

/// How a show stands with a service's catalogue. Having no listing row at
/// all is a state of its own: the show has not been looked up yet. That is
/// what a launch reads instead of searching again for what is already
/// answered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Tie {
    /// Looked up and not settled: the search found `found` candidates and
    /// none was the one clear match, so the show waits under Pick.
    Unpicked {
        found: u32,
    },
    Linked {
        media: Media,
        by: LinkedBy,
    },
    /// The media the show was tied to is no longer on the service, so the
    /// show goes back to Pick, still saying what it was called.
    Gone {
        media: Media,
    },
    /// The person said not to keep this show in step.
    Off,
}

/// A show set aside after the service rejected a write. The state it wanted
/// is kept so a retry sends the same thing, and the message so the Library
/// row can say why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Parked {
    pub wanted: ListState,
    pub message: String,
    pub at: SystemTime,
}

/// One show's standing with one service, as stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listing {
    pub entry: EntryId,
    pub service: Service,
    pub tie: Tie,
    /// The last confirmed list state; `None` until the first settle, which
    /// makes the next merge a first merge.
    pub(crate) base: Option<ListBase>,
    /// The list's repeat count minus the entry's `rewatched`, as of the last
    /// value taken from the list. Ryuuji counts only the rewatches it saw
    /// finish, so the two disagree by however many the list already held;
    /// keeping the difference lets a rewatch finished here add one to the
    /// list's count rather than replace it.
    pub(crate) repeat_offset: i64,
    pub(crate) parked: Option<Parked>,
}

impl Listing {
    /// The state the library wants `user`'s list to hold for `entry`: its
    /// status, its progress, and the rewatches it counted plus the offset,
    /// as the list's repeat count. The offset was taken from the list the
    /// base describes, so it counts only while the base is this user's:
    /// another account's list says nothing about this one's count, and a
    /// first merge asks with Ryuuji's own.
    pub fn wanted(&self, entry: &LibraryEntry, user: UserId) -> ListState {
        let offset = if self.base_for(user).is_some() {
            self.repeat_offset
        } else {
            0
        };
        let repeat = (i64::from(entry.rewatched) + offset).max(0);
        ListState {
            status: ListStatus::of(entry),
            progress: entry.progress,
            repeat: u32::try_from(repeat).unwrap_or(u32::MAX),
        }
    }

    /// The base when it is `user`'s. Another account's base says nothing
    /// about this one's list, so that user starts from a first merge.
    pub fn base_for(&self, user: UserId) -> Option<&ListBase> {
        self.base.as_ref().filter(|base| base.user == user)
    }

    /// The refusal the show is set aside with, while it is.
    pub fn parked(&self) -> Option<&Parked> {
        self.parked.as_ref()
    }
}

/// Identity of a stored update row. Only [`Store`](crate::Store) mints
/// these.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ListUpdateId(pub(crate) i64);

impl ListUpdateId {
    pub fn as_i64(self) -> i64 {
        self.0
    }
}

impl fmt::Display for ListUpdateId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// A value the library took from a list, as stored: where the show stood
/// before and after. Kept apart from the watch log so a relaunch reading the
/// newest history row still finds the viewing's own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListUpdate {
    pub id: ListUpdateId,
    pub entry: EntryId,
    pub service: Service,
    pub progress_before: u32,
    pub progress: u32,
    pub status_before: ListStatus,
    pub status: ListStatus,
    pub at: SystemTime,
}

/// An account signed in to a service, as stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    pub service: Service,
    pub user: UserId,
    pub name: String,
    /// When the token stops working, where the service says.
    pub expires: Option<SystemTime>,
    pub signed: Signed,
}

/// Where an account stands. Signing out keeps the row: the ties and bases
/// made under it stay, and signing back in as the same user carries on
/// from them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Signed {
    In,
    /// The service refused the token; nothing goes out until a new one.
    Refused {
        message: String,
    },
    Out,
}

/// A sign-in the Settings card is waiting on, or how the last one failed.
/// Neither is stored: a token refused before it was ever kept leaves no
/// account row behind, and a check under way is nothing yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Attempt {
    Checking(Service),
    Failed { service: Service, message: String },
}

/// The picker open over a show: what the person asked the service for, and
/// what came back once it has. The search itself is the worker's, asked
/// through [`AppState::work`] while `found` is `None`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Picker {
    pub entry: EntryId,
    pub service: Service,
    pub query: String,
    pub found: Option<Vec<Candidate>>,
}

/// What the sync worker sends back, each taken by the reducer through
/// `Command::Synced`. No variant carries a token: the token lives in the
/// worker and the vault, never in state the shell clones on every dispatch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Report {
    /// A pasted token is being checked against the service.
    Checking(Service),
    /// The token works, and this is whose it is.
    SignedIn(Account),
    /// The pasted token did not work, so nothing was kept.
    NotSignedIn {
        service: Service,
        message: String,
    },
    SignedOut(Service),
    /// The kept token stopped working; nothing goes out until a new one.
    Refused {
        service: Service,
        message: String,
    },
    /// What a search for `query` turned up for `entry`.
    Found {
        service: Service,
        entry: EntryId,
        query: String,
        candidates: Vec<Candidate>,
    },
    /// A reconcile ran for `user`'s list: what it asked with, and what the
    /// list holds for `media` after its write, if it made one.
    Reconciled {
        service: Service,
        user: UserId,
        entry: EntryId,
        media: MediaId,
        asked: Asked,
        remote: Remote,
    },
    /// The service refused to write `wanted` for `entry`.
    Rejected {
        service: Service,
        entry: EntryId,
        media: MediaId,
        wanted: ListState,
        message: String,
    },
    /// The media `entry` is tied to is no longer on the service.
    Gone {
        service: Service,
        entry: EntryId,
        media: MediaId,
    },
}

/// The sync side of [`AppState`].
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct SyncState {
    pub accounts: Vec<Account>,
    pub listings: Vec<Listing>,
    /// The Settings card's sign-in in progress, or how it failed.
    pub attempt: Option<Attempt>,
    /// The shows the watched-ahead rule asked a list about and has not
    /// heard back on. Each gives a reconcile at once, in step or not.
    pub checks: Vec<(Service, EntryId)>,
    pub picker: Option<Picker>,
}

impl SyncState {
    /// Where a show stands with a service, for the Library row to say;
    /// `None` while no account is signed in to that service.
    pub fn link_state(&self, entry: &LibraryEntry, service: Service) -> Option<LinkState> {
        let account = self.signed_in(service)?;
        let Some(listing) = self.listing(entry.id, service) else {
            return Some(LinkState::Looking);
        };
        Some(match &listing.tie {
            Tie::Unpicked { found } => LinkState::Unpicked { found: *found },
            Tie::Gone { media } => LinkState::Gone {
                show: media.title.clone(),
            },
            Tie::Off => LinkState::Off,
            Tie::Linked { media, .. } => {
                let show = media.title.clone();
                if let Some(parked) = &listing.parked {
                    return Some(LinkState::Refused {
                        show,
                        message: parked.message.clone(),
                    });
                }
                match listing.base_for(account.user) {
                    None => LinkState::Merging { show },
                    Some(base) if base.state == listing.wanted(entry, account.user) => {
                        LinkState::InStep {
                            show,
                            listed: base.listed,
                        }
                    }
                    Some(_) => LinkState::Waiting { show },
                }
            }
        })
    }

    /// The account for `service`, whatever its standing.
    pub(crate) fn account(&self, service: Service) -> Option<&Account> {
        self.accounts
            .iter()
            .find(|account| account.service == service)
    }

    pub(crate) fn signed_in(&self, service: Service) -> Option<&Account> {
        self.account(service)
            .filter(|account| matches!(account.signed, Signed::In))
    }

    pub(crate) fn listing(&self, entry: EntryId, service: Service) -> Option<&Listing> {
        self.listings
            .iter()
            .find(|listing| listing.entry == entry && listing.service == service)
    }

    /// The search one show gives one account, if any: the picker's query at
    /// once while it is open over this show and unanswered, else the show's
    /// own title in the background while it has never been looked up. One
    /// or the other, never both, so two answers cannot race to tie it.
    fn find(&self, entry: &LibraryEntry, service: Service) -> Option<Job> {
        if let Some(picker) = &self.picker
            && picker.entry == entry.id
            && picker.service == service
            && picker.found.is_none()
        {
            return Some(Job {
                entry: entry.id,
                task: Task::Find {
                    title: picker.query.clone(),
                },
                when: When::Now,
            });
        }
        self.listing(entry.id, service).is_none().then(|| Job {
            entry: entry.id,
            task: Task::Find {
                title: entry.title.clone(),
            },
            when: When::Background,
        })
    }

    /// The reconcile one show gives one account, if any. A linked show is
    /// reconciled when what the library wants differs from its base, or
    /// from a first merge when this user has no base for it, which asks
    /// with Ryuuji's own repeat count; one the watched-ahead rule asked
    /// about is reconciled at once, in step or not, since the read is the
    /// check. One parked on the very state it still wants is left alone,
    /// since sending it again would meet the same refusal. An unpicked,
    /// gone or off show has nothing to send.
    fn reconcile(&self, entry: &LibraryEntry, account: &Account) -> Option<Job> {
        let listing = self.listing(entry.id, account.service)?;
        let Tie::Linked { media, .. } = &listing.tie else {
            return None;
        };
        let wanted = listing.wanted(entry, account.user);
        if listing
            .parked
            .as_ref()
            .is_some_and(|parked| parked.wanted == wanted)
        {
            return None;
        }
        let checking = self.checks.contains(&(account.service, entry.id));
        let base = listing.base_for(account.user);
        if !checking && base.is_some_and(|base| base.state == wanted) {
            return None;
        }
        Some(Job {
            entry: entry.id,
            task: Task::Reconcile {
                media: media.id,
                wanted,
                base: base.map(|base| base.state),
            },
            when: if checking { When::Now } else { When::Settled },
        })
    }
}

impl AppState {
    /// What the sync worker should be doing now, derived from the library
    /// and the listings rather than kept anywhere: a show is pending while
    /// what the library wants differs from its base, so nothing on the
    /// recording, undo or status paths needs to know a list exists. One
    /// entry per signed-in account, each with the jobs its shows give it,
    /// at most one search and one reconcile per show; the shell posts the
    /// whole thing whenever it changes.
    pub fn work(&self) -> Work {
        let services = self
            .sync
            .accounts
            .iter()
            .filter(|account| matches!(account.signed, Signed::In))
            .map(|account| ServiceWork {
                service: account.service,
                user: account.user,
                jobs: self
                    .library
                    .iter()
                    .flat_map(|entry| {
                        self.sync
                            .find(entry, account.service)
                            .into_iter()
                            .chain(self.sync.reconcile(entry, account))
                    })
                    .collect(),
            })
            .collect();
        Work { services }
    }
}

/// Where a show stands with a service, as the Library row says it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkState {
    /// Not looked up yet.
    Looking,
    /// The search found `found` candidates and none was the one clear match.
    Unpicked {
        found: u32,
    },
    /// Linked, and this user has no base for it yet: the first merge is
    /// still to come.
    Merging {
        show: String,
    },
    /// Linked, and the library wants something the base does not hold.
    Waiting {
        show: String,
    },
    /// The base holds what the library wants; `listed` is false while the
    /// show is not on the list yet, which its next change will fix.
    InStep {
        show: String,
        listed: bool,
    },
    /// The service refused the last write.
    Refused {
        show: String,
        message: String,
    },
    Gone {
        show: String,
    },
    Off,
}

/// Everything the worker has to do, one entry per signed-in account.
/// Derived from [`AppState`] on every read, never stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Work {
    pub services: Vec<ServiceWork>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServiceWork {
    pub service: Service,
    pub user: UserId,
    pub jobs: Vec<Job>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Job {
    pub entry: EntryId,
    pub task: Task,
    pub when: When,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Task {
    /// Look the show up by its title.
    Find { title: String },
    /// Read what the list holds, merge, and write what differs. `base` is
    /// `None` for a first merge.
    Reconcile {
        media: MediaId,
        wanted: ListState,
        base: Option<ListState>,
    },
}

/// How soon a job runs: at once, after the last edit settles, or when
/// nothing else is waiting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum When {
    Now,
    Settled,
    Background,
}

#[cfg(test)]
mod tests {
    use std::time::UNIX_EPOCH;

    use super::*;
    use crate::WatchStatus;

    fn account(signed: Signed) -> Account {
        Account {
            service: Service::AniList,
            user: UserId(7),
            name: "someone".into(),
            expires: None,
            signed,
        }
    }

    fn show(id: i64, progress: u32) -> LibraryEntry {
        LibraryEntry {
            id: EntryId(id),
            title: format!("Show {id}"),
            status: WatchStatus::Watching,
            progress,
            total: Some(12),
            rewatching: false,
            rewatched: 1,
        }
    }

    fn media() -> Media {
        Media {
            id: MediaId(5),
            title: "Show on AniList".into(),
            episodes: Some(12),
            mal_id: None,
        }
    }

    fn state(status: ListStatus, progress: u32, repeat: u32) -> ListState {
        ListState {
            status,
            progress,
            repeat,
        }
    }

    fn based(user: u64, listed: bool, state: ListState) -> Option<ListBase> {
        Some(ListBase {
            user: UserId(user),
            listed,
            state,
        })
    }

    fn listing(entry: i64, tie: Tie, base: Option<ListBase>) -> Listing {
        Listing {
            entry: EntryId(entry),
            service: Service::AniList,
            tie,
            base,
            repeat_offset: 2,
            parked: None,
        }
    }

    fn linked(entry: i64, base: Option<ListBase>) -> Listing {
        listing(
            entry,
            Tie::Linked {
                media: media(),
                by: LinkedBy::Title,
            },
            base,
        )
    }

    fn parked(wanted: ListState) -> Option<Parked> {
        Some(Parked {
            wanted,
            message: "Validation error".into(),
            at: UNIX_EPOCH,
        })
    }

    fn app(accounts: Vec<Account>, library: Vec<LibraryEntry>, listings: Vec<Listing>) -> AppState {
        AppState {
            library,
            sync: SyncState {
                accounts,
                listings,
                ..SyncState::default()
            },
            ..AppState::default()
        }
    }

    fn reconcile(entry: i64, wanted: ListState, base: Option<ListBase>) -> Job {
        Job {
            entry: EntryId(entry),
            task: Task::Reconcile {
                media: MediaId(5),
                wanted,
                base: base.map(|base| base.state),
            },
            when: When::Settled,
        }
    }

    const LISTED: bool = true;

    #[test]
    fn what_a_listing_wants_is_the_entry_plus_the_offset_while_the_base_is_the_users() {
        let entry = show(1, 4);
        let base = based(7, LISTED, state(ListStatus::Watching, 3, 3));
        let wanted = linked(1, base).wanted(&entry, UserId(7));
        assert_eq!(wanted, state(ListStatus::Watching, 4, 3));
        // The offset came from user 7's list. Another user, or none with a
        // base yet, gets the entry's own count.
        assert_eq!(linked(1, base).wanted(&entry, UserId(8)).repeat, 1);
        assert_eq!(linked(1, None).wanted(&entry, UserId(7)).repeat, 1);
        let negative = Listing {
            repeat_offset: -5,
            ..linked(1, base)
        };
        assert_eq!(negative.wanted(&entry, UserId(7)).repeat, 0);
        let rewatch = LibraryEntry {
            rewatching: true,
            status: WatchStatus::Completed,
            ..entry
        };
        assert_eq!(
            linked(1, base).wanted(&rewatch, UserId(7)).status,
            ListStatus::Rewatching
        );
    }

    #[test]
    fn a_base_is_only_its_own_users() {
        let base = based(7, LISTED, state(ListStatus::Watching, 4, 3));
        let listing = linked(1, base);
        assert_eq!(listing.base_for(UserId(7)), base.as_ref());
        assert_eq!(listing.base_for(UserId(8)), None);
        assert_eq!(linked(1, None).base_for(UserId(7)), None);
    }

    #[test]
    fn a_signed_out_or_refused_account_gives_no_work() {
        let library = vec![show(1, 4)];
        assert_eq!(
            app(vec![account(Signed::Out)], library.clone(), vec![]).work(),
            Work { services: vec![] }
        );
        let refused = Signed::Refused {
            message: "Invalid token".into(),
        };
        assert_eq!(
            app(vec![account(refused)], library, vec![]).work(),
            Work { services: vec![] }
        );
    }

    #[test]
    fn a_show_never_looked_up_is_found_in_the_background() {
        let work = app(vec![account(Signed::In)], vec![show(1, 4)], vec![]).work();
        assert_eq!(
            work,
            Work {
                services: vec![ServiceWork {
                    service: Service::AniList,
                    user: UserId(7),
                    jobs: vec![Job {
                        entry: EntryId(1),
                        task: Task::Find {
                            title: "Show 1".into()
                        },
                        when: When::Background,
                    }],
                }]
            }
        );
    }

    /// A first merge, this user's first or after another's, asks with the
    /// entry's own repeat count: the offset was that other list's.
    #[test]
    fn a_linked_show_is_reconciled_while_its_base_differs_from_what_it_wants() {
        let wanted = state(ListStatus::Watching, 4, 3);
        let own = state(ListStatus::Watching, 4, 1);
        let in_step = based(7, LISTED, wanted);
        let behind = based(7, LISTED, state(ListStatus::Watching, 3, 3));
        let unlisted = based(7, false, state(ListStatus::PlanToWatch, 0, 3));
        let someone_elses = based(8, LISTED, wanted);
        let library = vec![show(1, 4), show(2, 4), show(3, 4), show(4, 4), show(5, 4)];
        let listings = vec![
            linked(1, None),
            linked(2, behind),
            linked(3, in_step),
            linked(4, unlisted),
            linked(5, someone_elses),
        ];
        let work = app(vec![account(Signed::In)], library, listings).work();
        assert_eq!(
            work.services[0].jobs,
            vec![
                reconcile(1, own, None),
                reconcile(2, wanted, behind),
                reconcile(4, wanted, unlisted),
                reconcile(5, own, None),
            ]
        );
    }

    #[test]
    fn a_parked_show_is_sent_again_only_once_it_wants_something_else() {
        let wanted = state(ListStatus::Watching, 4, 3);
        let base = based(7, LISTED, state(ListStatus::Watching, 3, 3));
        let same = Listing {
            parked: parked(wanted),
            ..linked(1, base)
        };
        let other = Listing {
            parked: parked(state(ListStatus::Watching, 5, 3)),
            ..linked(2, base)
        };
        let work = app(
            vec![account(Signed::In)],
            vec![show(1, 4), show(2, 4)],
            vec![same, other],
        )
        .work();
        assert_eq!(work.services[0].jobs, vec![reconcile(2, wanted, base)]);
    }

    #[test]
    fn an_unpicked_gone_or_off_show_gives_nothing() {
        let listings = vec![
            listing(1, Tie::Unpicked { found: 3 }, None),
            listing(2, Tie::Gone { media: media() }, None),
            listing(3, Tie::Off, None),
        ];
        let work = app(
            vec![account(Signed::In)],
            vec![show(1, 4), show(2, 4), show(3, 4)],
            listings,
        )
        .work();
        assert_eq!(
            work,
            Work {
                services: vec![ServiceWork {
                    service: Service::AniList,
                    user: UserId(7),
                    jobs: vec![],
                }]
            }
        );
    }

    #[test]
    fn a_link_state_says_where_each_show_stands() {
        let entry = show(1, 4);
        let wanted = state(ListStatus::Watching, 4, 3);
        let show = || "Show on AniList".to_owned();
        let signed_in = vec![account(Signed::In)];
        let state_of = |accounts: &[Account], listings: Vec<Listing>| {
            SyncState {
                accounts: accounts.to_vec(),
                listings,
                ..SyncState::default()
            }
            .link_state(&entry, Service::AniList)
        };
        assert_eq!(state_of(&[account(Signed::Out)], vec![]), None);
        assert_eq!(state_of(&[], vec![linked(1, None)]), None);
        assert_eq!(state_of(&signed_in, vec![]), Some(LinkState::Looking));
        assert_eq!(
            state_of(
                &signed_in,
                vec![listing(1, Tie::Unpicked { found: 3 }, None)]
            ),
            Some(LinkState::Unpicked { found: 3 })
        );
        assert_eq!(
            state_of(&signed_in, vec![linked(1, None)]),
            Some(LinkState::Merging { show: show() })
        );
        assert_eq!(
            state_of(&signed_in, vec![linked(1, based(8, LISTED, wanted))]),
            Some(LinkState::Merging { show: show() })
        );
        assert_eq!(
            state_of(
                &signed_in,
                vec![linked(
                    1,
                    based(7, LISTED, state(ListStatus::Watching, 3, 3))
                )]
            ),
            Some(LinkState::Waiting { show: show() })
        );
        assert_eq!(
            state_of(&signed_in, vec![linked(1, based(7, LISTED, wanted))]),
            Some(LinkState::InStep {
                show: show(),
                listed: true
            })
        );
        assert_eq!(
            state_of(&signed_in, vec![linked(1, based(7, false, wanted))]),
            Some(LinkState::InStep {
                show: show(),
                listed: false
            })
        );
        let refused = Listing {
            parked: parked(wanted),
            ..linked(1, based(7, LISTED, wanted))
        };
        assert_eq!(
            state_of(&signed_in, vec![refused]),
            Some(LinkState::Refused {
                show: show(),
                message: "Validation error".into()
            })
        );
        assert_eq!(
            state_of(
                &signed_in,
                vec![listing(1, Tie::Gone { media: media() }, None)]
            ),
            Some(LinkState::Gone { show: show() })
        );
        assert_eq!(
            state_of(&signed_in, vec![listing(1, Tie::Off, None)]),
            Some(LinkState::Off)
        );
    }

    /// The check is the read: a show in step is reconciled anyway, at once,
    /// and a pending one moves up from Settled. A check on a show that is
    /// not linked has nothing to read.
    #[test]
    fn a_checked_show_is_reconciled_at_once_even_in_step() {
        let wanted = state(ListStatus::Watching, 4, 3);
        let in_step = based(7, LISTED, wanted);
        let behind = based(7, LISTED, state(ListStatus::Watching, 3, 3));
        let mut app = app(
            vec![account(Signed::In)],
            vec![show(1, 4), show(2, 4), show(3, 4)],
            vec![
                linked(1, in_step),
                linked(2, behind),
                listing(3, Tie::Unpicked { found: 2 }, None),
            ],
        );
        app.sync.checks = vec![
            (Service::AniList, EntryId(1)),
            (Service::AniList, EntryId(2)),
            (Service::AniList, EntryId(3)),
        ];
        let now = |job: Job| Job {
            when: When::Now,
            ..job
        };
        assert_eq!(
            app.work().services[0].jobs,
            vec![
                now(reconcile(1, wanted, in_step)),
                now(reconcile(2, wanted, behind)),
            ]
        );
    }

    /// The picker's query goes out at once and in place of the background
    /// search, so one show is never searched for twice over; once answered
    /// it asks nothing more, and a reconcile the show has stands beside it.
    #[test]
    fn an_open_picker_asks_its_query_at_once_in_place_of_the_background_search() {
        let mut app = app(
            vec![account(Signed::In)],
            vec![show(1, 4), show(2, 4)],
            vec![linked(2, None)],
        );
        let picker = |entry, found| Picker {
            entry: EntryId(entry),
            service: Service::AniList,
            query: "Sousou no Frieren".into(),
            found,
        };
        let asked = |entry| Job {
            entry: EntryId(entry),
            task: Task::Find {
                title: "Sousou no Frieren".into(),
            },
            when: When::Now,
        };
        let background = Job {
            entry: EntryId(1),
            task: Task::Find {
                title: "Show 1".into(),
            },
            when: When::Background,
        };
        let wanted = state(ListStatus::Watching, 4, 1);

        app.sync.picker = Some(picker(1, None));
        assert_eq!(
            app.work().services[0].jobs,
            vec![asked(1), reconcile(2, wanted, None)]
        );
        app.sync.picker = Some(picker(2, None));
        assert_eq!(
            app.work().services[0].jobs,
            vec![background.clone(), asked(2), reconcile(2, wanted, None)]
        );
        app.sync.picker = Some(picker(1, Some(vec![])));
        assert_eq!(
            app.work().services[0].jobs,
            vec![background, reconcile(2, wanted, None)]
        );
    }
}
