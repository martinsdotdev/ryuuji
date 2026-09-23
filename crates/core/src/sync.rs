//! What keeping a show in step with a list service looks like from the
//! core: the account signed in, the media a show is tied to, and the base
//! a merge measures both sides against. The rows live in `store::lists`;
//! the merge rule and the commands that drive it come later.

use std::time::SystemTime;

use crate::tagged::tagged_enum;
use crate::{EntryId, ListStatus};

tagged_enum! {
    /// A list service a show can be kept in step with. Tags are stored in
    /// the `accounts`, `listings` and `updates` tables.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

/// The sync side of [`AppState`](crate::AppState).
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct SyncState {
    pub accounts: Vec<Account>,
    pub listings: Vec<Listing>,
}
