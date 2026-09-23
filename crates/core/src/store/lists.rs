//! The tables a show's standing with a list service lives in: `accounts`,
//! one row per service, `listings`, one row per show and service, and
//! `updates`, one row per value the library took from a list. The first two
//! are replaced in place and never deleted: signing out marks the account
//! row, and a show taken out of step keeps its row saying so.

use rusqlite::{Connection, OptionalExtension, Row, params};
use tracing::{debug, info, info_span};

use super::{
    Store, StoreError, fetch, query_failed, set_column, unix_now, unix_secs, unix_time,
    write_standing,
};
use crate::{
    Account, EntryId, LibraryEntry, LinkedBy, ListBase, ListState, ListStatus, ListUpdate,
    ListUpdateId, Listing, Media, MediaId, Parked, Service, Settlement, Signed, Tie, UserId,
};

const SELECT_ACCOUNT: &str = "SELECT service, user_id, name, expires_at, refused_at, refusal, \
                              signed_out_at FROM accounts";

const SELECT_LISTING: &str = "SELECT entry_id, service, tie, found, media_id, media_title, \
                              media_episodes, mal_id, linked_by, base_user, base_listed, \
                              base_status, base_progress, base_repeat, based_at, repeat_offset, \
                              parked_status, parked_progress, parked_repeat, parked, parked_at \
                              FROM listings";

const SELECT_UPDATE: &str = "SELECT id, entry_id, service, progress_before, progress, \
                             status_before, status, at FROM updates";

/// The `tie` column's values, one per [`Tie`] variant.
const TIE_UNPICKED: &str = "unpicked";
const TIE_LINKED: &str = "linked";
const TIE_GONE: &str = "gone";
const TIE_OFF: &str = "off";

/// A tie as stored, with the entry when tying it filled in a total the
/// entry lacked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tied {
    pub listing: Listing,
    /// The entry with the media's episode count as its total, when it had
    /// none before; `None` when the entry was left alone.
    pub entry: Option<LibraryEntry>,
}

/// A settlement as stored: the entry with what it took, the listing with
/// its new base, and the row that says what was taken, when something was.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settled {
    pub entry: LibraryEntry,
    pub listing: Listing,
    pub update: Option<ListUpdate>,
}

impl Store {
    /// Every account, in service order.
    pub fn accounts(&self) -> Result<Vec<Account>, StoreError> {
        let _span = info_span!("store.accounts").entered();
        let mut stmt = self
            .conn
            .prepare(&format!("{SELECT_ACCOUNT} ORDER BY service"))
            .map_err(query_failed)?;
        let rows = stmt.query_map([], RawAccount::read).map_err(query_failed)?;
        let accounts = rows
            .map(|row| row.map_err(query_failed)?.parse())
            .collect::<Result<Vec<_>, _>>()?;
        debug!(count = accounts.len(), "accounts loaded");
        Ok(accounts)
    }

    /// Writes where an account stands, replacing the service's row as
    /// `aliases` and `ignored` replace theirs. Each standing sets its own
    /// mark and clears the other: a refusal sets `refused_at` and its
    /// message, a sign-out sets `signed_out_at`, and a sign-in clears both.
    /// `signed_in_at` is stamped by a sign-in and kept through the other
    /// two, since it says when the account last signed in; a first row that
    /// is not a sign-in takes now, so the column is never empty.
    pub fn save_account(&mut self, account: &Account) -> Result<Account, StoreError> {
        let _span = info_span!("store.save_account", service = account.service.tag()).entered();
        let tx = self.conn.transaction().map_err(query_failed)?;
        let now = unix_now();
        let (signed_in_at, refused_at, refusal, signed_out_at) = match &account.signed {
            Signed::In => (Some(now), None, None, None),
            Signed::Refused { message } => (None, Some(now), Some(message.as_str()), None),
            Signed::Out => (None, None, None, Some(now)),
        };
        tx.execute(
            "INSERT INTO accounts (service, user_id, name, expires_at, signed_in_at, \
             refused_at, refusal, signed_out_at) \
             VALUES (?1, ?2, ?3, ?4, COALESCE(?5, ?9), ?6, ?7, ?8) \
             ON CONFLICT(service) DO UPDATE SET user_id = excluded.user_id, \
             name = excluded.name, expires_at = excluded.expires_at, \
             signed_in_at = COALESCE(?5, accounts.signed_in_at), \
             refused_at = excluded.refused_at, refusal = excluded.refusal, \
             signed_out_at = excluded.signed_out_at",
            params![
                account.service.tag(),
                account.user.0,
                account.name,
                account.expires.map(unix_secs),
                signed_in_at,
                refused_at,
                refusal,
                signed_out_at,
                now,
            ],
        )
        .map_err(query_failed)?;
        let stored = fetch_account(&tx, account.service)?;
        tx.commit().map_err(query_failed)?;
        info!("account saved");
        Ok(stored)
    }

    /// Every listing, by entry then service.
    pub fn listings(&self) -> Result<Vec<Listing>, StoreError> {
        let _span = info_span!("store.listings").entered();
        let mut stmt = self
            .conn
            .prepare(&format!("{SELECT_LISTING} ORDER BY entry_id, service"))
            .map_err(query_failed)?;
        let rows = stmt.query_map([], RawListing::read).map_err(query_failed)?;
        let listings = rows
            .map(|row| row.map_err(query_failed)?.parse())
            .collect::<Result<Vec<_>, _>>()?;
        debug!(count = listings.len(), "listings loaded");
        Ok(listings)
    }

    /// Writes how a show stands with a service, replacing the row for the
    /// pair. The base describes what the list holds for the media the show
    /// was tied to, and the park what that media's row refused, so tying
    /// it to another media, or to none, starts it over: the base group,
    /// the repeat offset and the parked group go back to nothing. The same
    /// media keeps them, so a show linked again to where it was, or
    /// reported gone from there, carries on from its last settle. Linking
    /// fills a total the entry lacks from the media's episode count, in
    /// the same transaction, so the show can complete on its last episode;
    /// a total the person already set stands.
    pub fn set_tie(
        &mut self,
        entry: EntryId,
        service: Service,
        tie: &Tie,
    ) -> Result<Tied, StoreError> {
        let _span = info_span!(
            "store.set_tie",
            entry = %entry,
            service = service.tag(),
            tie = tie_tag(tie)
        )
        .entered();
        let tx = self.conn.transaction().map_err(query_failed)?;
        let current = fetch(&tx, entry)?;
        let media = tie_media(tie);
        let media_id = media.map(|media| media.id.0);
        tx.execute(
            "UPDATE listings SET base_user = NULL, base_listed = NULL, base_status = NULL, \
             base_progress = NULL, base_repeat = NULL, based_at = NULL, repeat_offset = 0, \
             parked_status = NULL, parked_progress = NULL, parked_repeat = NULL, \
             parked = NULL, parked_at = NULL \
             WHERE entry_id = ?1 AND service = ?2 AND media_id IS NOT ?3",
            params![entry.0, service.tag(), media_id],
        )
        .map_err(query_failed)?;
        let (found, by) = match tie {
            Tie::Unpicked { found } => (Some(*found), None),
            Tie::Linked { by, .. } => (None, Some(by.tag())),
            Tie::Gone { .. } | Tie::Off => (None, None),
        };
        tx.execute(
            "INSERT INTO listings (entry_id, service, tie, found, media_id, media_title, \
             media_episodes, mal_id, linked_by, tied_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10) \
             ON CONFLICT(entry_id, service) DO UPDATE SET tie = excluded.tie, \
             found = excluded.found, media_id = excluded.media_id, \
             media_title = excluded.media_title, media_episodes = excluded.media_episodes, \
             mal_id = excluded.mal_id, linked_by = excluded.linked_by, \
             tied_at = excluded.tied_at",
            params![
                entry.0,
                service.tag(),
                tie_tag(tie),
                found,
                media_id,
                media.map(|media| media.title.as_str()),
                media.and_then(|media| media.episodes),
                media.and_then(|media| media.mal_id),
                by,
                unix_now(),
            ],
        )
        .map_err(query_failed)?;
        let filled = match tie {
            Tie::Linked {
                media:
                    Media {
                        episodes: Some(episodes),
                        ..
                    },
                ..
            } if current.total.is_none() => Some(set_column(&tx, entry, "total", *episodes)?),
            _ => None,
        };
        let listing = fetch_listing(&tx, entry, service)?;
        tx.commit().map_err(query_failed)?;
        info!(total_filled = filled.is_some(), "tie set");
        Ok(Tied {
            listing,
            entry: filled,
        })
    }

    /// Sets a show aside after the service rejected a write, keeping what
    /// was wanted and why it was refused. A show never looked up on the
    /// service has no row to set aside.
    pub fn park(
        &mut self,
        entry: EntryId,
        service: Service,
        parked: &Parked,
    ) -> Result<Listing, StoreError> {
        let _span = info_span!("store.park", entry = %entry, service = service.tag()).entered();
        let tx = self.conn.transaction().map_err(query_failed)?;
        let changed = tx
            .execute(
                "UPDATE listings SET parked_status = ?1, parked_progress = ?2, \
                 parked_repeat = ?3, parked = ?4, parked_at = ?5 \
                 WHERE entry_id = ?6 AND service = ?7",
                params![
                    parked.wanted.status.tag(),
                    parked.wanted.progress,
                    parked.wanted.repeat,
                    parked.message,
                    unix_secs(parked.at),
                    entry.0,
                    service.tag(),
                ],
            )
            .map_err(query_failed)?;
        if changed == 0 {
            return Err(StoreError::NotListed { entry, service });
        }
        let listing = fetch_listing(&tx, entry, service)?;
        tx.commit().map_err(query_failed)?;
        info!("show parked");
        Ok(listing)
    }

    /// Takes a show out of the park. The group goes back to NULL rather
    /// than the row going: the row is the show's standing, and nothing
    /// here deletes.
    pub fn unpark(&mut self, entry: EntryId, service: Service) -> Result<Listing, StoreError> {
        let _span = info_span!("store.unpark", entry = %entry, service = service.tag()).entered();
        let tx = self.conn.transaction().map_err(query_failed)?;
        let changed = tx
            .execute(
                "UPDATE listings SET parked_status = NULL, parked_progress = NULL, \
                 parked_repeat = NULL, parked = NULL, parked_at = NULL \
                 WHERE entry_id = ?1 AND service = ?2",
                params![entry.0, service.tag()],
            )
            .map_err(query_failed)?;
        if changed == 0 {
            return Err(StoreError::NotListed { entry, service });
        }
        let listing = fetch_listing(&tx, entry, service)?;
        tx.commit().map_err(query_failed)?;
        info!("show unparked");
        Ok(listing)
    }

    /// Writes what a report settled into, in one transaction: the fields
    /// the library takes from the list, the base the next merge measures
    /// against, and the row History reads the take from. The base moves
    /// here and nowhere else, after the list confirmed what it holds, so a
    /// crash or a duplicate send re-asks the same question. A show tied
    /// elsewhere since the job was built is left alone, with `None` back:
    /// its report describes a media it no longer stands with. The parked
    /// group goes, since the list has answered. Every value is absolute, so
    /// writing the same settlement twice changes nothing the second time
    /// and adds no second row. `rewatched` stays as it was: a repeat taken
    /// from the list moves the listing's offset, never the entry's count.
    pub(crate) fn settle(
        &mut self,
        entry: EntryId,
        service: Service,
        media: MediaId,
        settlement: &Settlement,
    ) -> Result<Option<Settled>, StoreError> {
        let _span = info_span!(
            "store.settle",
            entry = %entry,
            service = service.tag(),
            media = media.0
        )
        .entered();
        let tx = self.conn.transaction().map_err(query_failed)?;
        let listing = fetch_listing(&tx, entry, service)?;
        if !matches!(&listing.tie, Tie::Linked { media: linked, .. } if linked.id == media) {
            info!("show tied elsewhere since; nothing settled");
            return Ok(None);
        }
        let before = fetch(&tx, entry)?;
        let mut taken = before.clone();
        if let Some(status) = settlement.take_status {
            (taken.status, taken.rewatching) = status.columns();
        }
        if let Some(progress) = settlement.take_progress {
            taken.progress = progress;
        }
        let stored = if taken == before {
            before.clone()
        } else {
            write_standing(&tx, &taken)?
        };
        let now = unix_now();
        let base = &settlement.base;
        tx.execute(
            "UPDATE listings SET base_user = ?1, base_listed = ?2, base_status = ?3, \
             base_progress = ?4, base_repeat = ?5, based_at = ?6, repeat_offset = ?7, \
             parked_status = NULL, parked_progress = NULL, parked_repeat = NULL, \
             parked = NULL, parked_at = NULL \
             WHERE entry_id = ?8 AND service = ?9",
            params![
                base.user.0,
                base.listed,
                base.state.status.tag(),
                base.state.progress,
                base.state.repeat,
                now,
                settlement.repeat_offset,
                entry.0,
                service.tag(),
            ],
        )
        .map_err(query_failed)?;
        let (status_before, status) = (ListStatus::of(&before), ListStatus::of(&stored));
        let update = if stored.progress != before.progress || status != status_before {
            tx.execute(
                "INSERT INTO updates (entry_id, service, progress_before, progress, \
                 status_before, status, at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    entry.0,
                    service.tag(),
                    before.progress,
                    stored.progress,
                    status_before.tag(),
                    status.tag(),
                    now,
                ],
            )
            .map_err(query_failed)?;
            Some(fetch_update(&tx, ListUpdateId(tx.last_insert_rowid()))?)
        } else {
            None
        };
        let listing = fetch_listing(&tx, entry, service)?;
        tx.commit().map_err(query_failed)?;
        info!(
            progress = stored.progress,
            status = status.tag(),
            taken = update.is_some(),
            "settled"
        );
        Ok(Some(Settled {
            entry: stored,
            listing,
            update,
        }))
    }

    /// Every value the library took from a list, oldest first.
    pub fn updates(&self) -> Result<Vec<ListUpdate>, StoreError> {
        let _span = info_span!("store.updates").entered();
        let mut stmt = self
            .conn
            .prepare(&format!("{SELECT_UPDATE} ORDER BY id"))
            .map_err(query_failed)?;
        let rows = stmt.query_map([], RawUpdate::read).map_err(query_failed)?;
        let updates = rows
            .map(|row| row.map_err(query_failed)?.parse())
            .collect::<Result<Vec<_>, _>>()?;
        debug!(count = updates.len(), "updates loaded");
        Ok(updates)
    }
}

fn fetch_account(conn: &Connection, service: Service) -> Result<Account, StoreError> {
    conn.query_row(
        &format!("{SELECT_ACCOUNT} WHERE service = ?1"),
        [service.tag()],
        RawAccount::read,
    )
    .map_err(query_failed)?
    .parse()
}

fn fetch_listing(
    conn: &Connection,
    entry: EntryId,
    service: Service,
) -> Result<Listing, StoreError> {
    conn.query_row(
        &format!("{SELECT_LISTING} WHERE entry_id = ?1 AND service = ?2"),
        params![entry.0, service.tag()],
        RawListing::read,
    )
    .optional()
    .map_err(query_failed)?
    .ok_or(StoreError::NotListed { entry, service })?
    .parse()
}

/// The values taken from a list, newest first, for one entry or all of
/// them, written at or after `since` or at any time: the ones
/// [`Store::history`] lays beside a page of watches. One statement serves
/// every case; the table holds one row per value taken, so no index is
/// missed.
pub(super) fn updates_since(
    conn: &Connection,
    entry: Option<EntryId>,
    since: Option<i64>,
) -> Result<Vec<ListUpdate>, StoreError> {
    let mut stmt = conn
        .prepare(&format!(
            "{SELECT_UPDATE} WHERE (?1 IS NULL OR entry_id = ?1) AND (?2 IS NULL OR at >= ?2) \
             ORDER BY id DESC"
        ))
        .map_err(query_failed)?;
    let rows = stmt
        .query_map(params![entry.map(|id| id.0), since], RawUpdate::read)
        .map_err(query_failed)?;
    rows.map(|row| row.map_err(query_failed)?.parse()).collect()
}

fn fetch_update(conn: &Connection, id: ListUpdateId) -> Result<ListUpdate, StoreError> {
    conn.query_row(
        &format!("{SELECT_UPDATE} WHERE id = ?1"),
        [id.0],
        RawUpdate::read,
    )
    .map_err(query_failed)?
    .parse()
}

fn tie_tag(tie: &Tie) -> &'static str {
    match tie {
        Tie::Unpicked { .. } => TIE_UNPICKED,
        Tie::Linked { .. } => TIE_LINKED,
        Tie::Gone { .. } => TIE_GONE,
        Tie::Off => TIE_OFF,
    }
}

/// The media a tie carries: the one a show is linked to, or was.
fn tie_media(tie: &Tie) -> Option<&Media> {
    match tie {
        Tie::Linked { media, .. } | Tie::Gone { media } => Some(media),
        Tie::Unpicked { .. } | Tie::Off => None,
    }
}

/// An account row as SQLite hands it over, before the domain checks in
/// `parse`.
struct RawAccount {
    service: String,
    user_id: i64,
    name: String,
    expires_at: Option<i64>,
    refused_at: Option<i64>,
    refusal: Option<String>,
    signed_out_at: Option<i64>,
}

impl RawAccount {
    // Positional, so `SELECT_ACCOUNT` only ever appends a column.
    fn read(row: &Row<'_>) -> rusqlite::Result<RawAccount> {
        Ok(RawAccount {
            service: row.get(0)?,
            user_id: row.get(1)?,
            name: row.get(2)?,
            expires_at: row.get(3)?,
            refused_at: row.get(4)?,
            refusal: row.get(5)?,
            signed_out_at: row.get(6)?,
        })
    }

    /// The standing is read from the marks: signed out when that mark is
    /// set, else refused when that one is, else in.
    fn parse(self) -> Result<Account, StoreError> {
        let invalid = |field| StoreError::InvalidAccount {
            service: self.service.clone(),
            field,
        };
        let service = Service::from_tag(&self.service).ok_or_else(|| invalid("service"))?;
        let signed = match (self.signed_out_at, self.refused_at) {
            (Some(_), _) => Signed::Out,
            (None, Some(_)) => Signed::Refused {
                message: self.refusal.ok_or_else(|| invalid("refusal"))?,
            },
            (None, None) => Signed::In,
        };
        Ok(Account {
            service,
            user: UserId(u64::try_from(self.user_id).map_err(|_| invalid("user_id"))?),
            name: self.name,
            expires: self
                .expires_at
                .map(|at| unix_time(at).ok_or_else(|| invalid("expires_at")))
                .transpose()?,
            signed,
        })
    }
}

/// A listing row as SQLite hands it over, before the domain checks in
/// `parse`.
struct RawListing {
    entry_id: i64,
    service: String,
    tie: String,
    found: Option<i64>,
    media_id: Option<i64>,
    media_title: Option<String>,
    media_episodes: Option<i64>,
    mal_id: Option<i64>,
    linked_by: Option<String>,
    base_user: Option<i64>,
    base_listed: Option<bool>,
    base_status: Option<String>,
    base_progress: Option<i64>,
    base_repeat: Option<i64>,
    based_at: Option<i64>,
    repeat_offset: i64,
    parked_status: Option<String>,
    parked_progress: Option<i64>,
    parked_repeat: Option<i64>,
    parked: Option<String>,
    parked_at: Option<i64>,
}

impl RawListing {
    // Positional, so `SELECT_LISTING` only ever appends a column.
    fn read(row: &Row<'_>) -> rusqlite::Result<RawListing> {
        Ok(RawListing {
            entry_id: row.get(0)?,
            service: row.get(1)?,
            tie: row.get(2)?,
            found: row.get(3)?,
            media_id: row.get(4)?,
            media_title: row.get(5)?,
            media_episodes: row.get(6)?,
            mal_id: row.get(7)?,
            linked_by: row.get(8)?,
            base_user: row.get(9)?,
            base_listed: row.get(10)?,
            base_status: row.get(11)?,
            base_progress: row.get(12)?,
            base_repeat: row.get(13)?,
            based_at: row.get(14)?,
            repeat_offset: row.get(15)?,
            parked_status: row.get(16)?,
            parked_progress: row.get(17)?,
            parked_repeat: row.get(18)?,
            parked: row.get(19)?,
            parked_at: row.get(20)?,
        })
    }

    fn parse(self) -> Result<Listing, StoreError> {
        let entry = EntryId(self.entry_id);
        let invalid = |field| StoreError::InvalidListing {
            entry,
            service: self.service.clone(),
            field,
        };
        Ok(Listing {
            entry,
            service: Service::from_tag(&self.service).ok_or_else(|| invalid("service"))?,
            tie: read_tie(&self).map_err(invalid)?,
            base: read_base(&self).map_err(invalid)?,
            repeat_offset: self.repeat_offset,
            parked: read_parked(&self).map_err(invalid)?,
        })
    }
}

/// The media columns as one value, for a tie that carries one.
fn read_media(raw: &RawListing) -> Result<Media, &'static str> {
    let id = raw
        .media_id
        .and_then(|id| u64::try_from(id).ok())
        .ok_or("media")?;
    Ok(Media {
        id: MediaId(id),
        title: raw.media_title.clone().ok_or("media")?,
        episodes: raw
            .media_episodes
            .map(u32::try_from)
            .transpose()
            .map_err(|_| "media_episodes")?,
        mal_id: raw
            .mal_id
            .map(u64::try_from)
            .transpose()
            .map_err(|_| "mal_id")?,
    })
}

/// The tie is its tag with the columns that kind needs: how many were
/// found, a media and who linked it, a media alone, or nothing.
fn read_tie(raw: &RawListing) -> Result<Tie, &'static str> {
    Ok(match raw.tie.as_str() {
        TIE_UNPICKED => Tie::Unpicked {
            found: raw
                .found
                .and_then(|found| u32::try_from(found).ok())
                .ok_or("found")?,
        },
        TIE_LINKED => Tie::Linked {
            media: read_media(raw)?,
            by: raw
                .linked_by
                .as_deref()
                .and_then(LinkedBy::from_tag)
                .ok_or("linked_by")?,
        },
        TIE_GONE => Tie::Gone {
            media: read_media(raw)?,
        },
        TIE_OFF => Tie::Off,
        _ => return Err("tie"),
    })
}

/// The base group reads as one value: all NULL is no base, all set is the
/// base, anything else is a broken row.
fn read_base(raw: &RawListing) -> Result<Option<ListBase>, &'static str> {
    match (
        raw.base_user,
        raw.base_listed,
        raw.base_status.as_deref(),
        raw.base_progress,
        raw.base_repeat,
        raw.based_at,
    ) {
        (None, None, None, None, None, None) => Ok(None),
        (Some(user), Some(listed), Some(status), Some(progress), Some(repeat), Some(_)) => {
            Ok(Some(ListBase {
                user: UserId(u64::try_from(user).map_err(|_| "base_user")?),
                listed,
                state: list_state(status, progress, repeat).ok_or("base")?,
            }))
        }
        _ => Err("base"),
    }
}

/// The parked group reads as one value on the same terms as the base.
fn read_parked(raw: &RawListing) -> Result<Option<Parked>, &'static str> {
    match (
        raw.parked_status.as_deref(),
        raw.parked_progress,
        raw.parked_repeat,
        raw.parked.as_deref(),
        raw.parked_at,
    ) {
        (None, None, None, None, None) => Ok(None),
        (Some(status), Some(progress), Some(repeat), Some(message), Some(at)) => Ok(Some(Parked {
            wanted: list_state(status, progress, repeat).ok_or("parked")?,
            message: message.to_owned(),
            at: unix_time(at).ok_or("parked_at")?,
        })),
        _ => Err("parked"),
    }
}

/// Three columns as one list state; `None` when any is out of range.
fn list_state(status: &str, progress: i64, repeat: i64) -> Option<ListState> {
    Some(ListState {
        status: ListStatus::from_tag(status)?,
        progress: u32::try_from(progress).ok()?,
        repeat: u32::try_from(repeat).ok()?,
    })
}

/// An update row as SQLite hands it over, before the domain checks in
/// `parse`.
struct RawUpdate {
    id: i64,
    entry_id: i64,
    service: String,
    progress_before: i64,
    progress: i64,
    status_before: String,
    status: String,
    at: i64,
}

impl RawUpdate {
    // Positional, so `SELECT_UPDATE` only ever appends a column.
    fn read(row: &Row<'_>) -> rusqlite::Result<RawUpdate> {
        Ok(RawUpdate {
            id: row.get(0)?,
            entry_id: row.get(1)?,
            service: row.get(2)?,
            progress_before: row.get(3)?,
            progress: row.get(4)?,
            status_before: row.get(5)?,
            status: row.get(6)?,
            at: row.get(7)?,
        })
    }

    fn parse(self) -> Result<ListUpdate, StoreError> {
        let id = ListUpdateId(self.id);
        let invalid = |field| StoreError::InvalidUpdate { id, field };
        let count = |value: i64, field| u32::try_from(value).map_err(|_| invalid(field));
        Ok(ListUpdate {
            id,
            entry: EntryId(self.entry_id),
            service: Service::from_tag(&self.service).ok_or_else(|| invalid("service"))?,
            progress_before: count(self.progress_before, "progress_before")?,
            progress: count(self.progress, "progress")?,
            status_before: ListStatus::from_tag(&self.status_before)
                .ok_or_else(|| invalid("status_before"))?,
            status: ListStatus::from_tag(&self.status).ok_or_else(|| invalid("status"))?,
            at: unix_time(self.at).ok_or_else(|| invalid("at"))?,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use super::super::tests::{entry, open, open_tmp};
    use super::*;
    use crate::{NewEntry, WatchStatus};

    fn account(signed: Signed) -> Account {
        Account {
            service: Service::AniList,
            user: UserId(7),
            name: "someone".into(),
            expires: Some(UNIX_EPOCH + Duration::from_secs(1_800_000_000)),
            signed,
        }
    }

    fn media(id: u64) -> Media {
        Media {
            id: MediaId(id),
            title: format!("Show {id}"),
            episodes: Some(12),
            mal_id: Some(id + 1000),
        }
    }

    fn linked(id: u64) -> Tie {
        Tie::Linked {
            media: media(id),
            by: LinkedBy::Title,
        }
    }

    fn parked() -> Parked {
        Parked {
            wanted: ListState {
                status: ListStatus::Completed,
                progress: 12,
                repeat: 1,
            },
            message: "Validation error".into(),
            at: UNIX_EPOCH + Duration::from_secs(1_700_000_000),
        }
    }

    /// A base as `Store::settle` will write it, for a user 7 who has the
    /// show listed.
    const BASE: &str = "UPDATE listings SET base_user = 7, base_listed = 1, \
                        base_status = 'watching', base_progress = 3, base_repeat = 1, \
                        based_at = 100, repeat_offset = 2";

    fn based() -> Option<ListBase> {
        Some(ListBase {
            user: UserId(7),
            listed: true,
            state: ListState {
                status: ListStatus::Watching,
                progress: 3,
                repeat: 1,
            },
        })
    }

    #[test]
    fn an_account_round_trips_each_standing_and_survives_reopen() {
        let (_tmp, dir) = open_tmp();
        let mut store = open(&dir);
        assert_eq!(store.accounts().unwrap(), vec![]);
        // Through every standing and back in, so each mark is seen set and
        // then cleared.
        for signed in [
            Signed::In,
            Signed::Refused {
                message: "Invalid token".into(),
            },
            Signed::Out,
            Signed::In,
            Signed::Out,
        ] {
            let saved = store.save_account(&account(signed.clone())).unwrap();
            assert_eq!(saved, account(signed));
            assert_eq!(store.accounts().unwrap(), vec![saved]);
        }
        drop(store);
        assert_eq!(open(&dir).accounts().unwrap(), vec![account(Signed::Out)]);
    }

    #[test]
    fn saving_an_account_again_replaces_its_row() {
        let (_tmp, dir) = open_tmp();
        let mut store = open(&dir);
        store.save_account(&account(Signed::In)).unwrap();
        store.execute_raw("UPDATE accounts SET signed_in_at = 100");
        let renamed = Account {
            user: UserId(8),
            name: "someone".into(),
            expires: None,
            ..account(Signed::Out)
        };
        assert_eq!(store.save_account(&renamed).unwrap(), renamed);
        assert_eq!(store.accounts().unwrap(), vec![renamed]);
        let (rows, signed_in_at): (i64, i64) = store
            .conn
            .query_row(
                "SELECT COUNT(*), MAX(signed_in_at) FROM accounts",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(rows, 1);
        // A sign-out keeps when the account signed in; only a sign-in moves
        // it.
        assert_eq!(signed_in_at, 100);
        store.save_account(&account(Signed::In)).unwrap();
        let signed_in_at: i64 = store
            .conn
            .query_row("SELECT signed_in_at FROM accounts", [], |row| row.get(0))
            .unwrap();
        assert!(signed_in_at > 100);
    }

    #[test]
    fn garbage_account_rows_surface_as_invalid() {
        let (_tmp, dir) = open_tmp();
        let mut store = open(&dir);
        store.save_account(&account(Signed::In)).unwrap();
        for (broken, field) in [
            (
                "UPDATE accounts SET refused_at = 5, refusal = NULL",
                "refusal",
            ),
            (
                "UPDATE accounts SET refused_at = NULL, user_id = -1",
                "user_id",
            ),
            (
                "UPDATE accounts SET user_id = 7, expires_at = -1",
                "expires_at",
            ),
            (
                "UPDATE accounts SET expires_at = NULL, service = 'mal'",
                "service",
            ),
        ] {
            store.execute_raw(broken);
            assert!(
                matches!(
                    store.accounts(),
                    Err(StoreError::InvalidAccount { field: got, .. }) if got == field
                ),
                "{broken}"
            );
        }
    }

    #[test]
    fn a_listing_round_trips_every_tie_and_survives_reopen() {
        let (_tmp, dir) = open_tmp();
        let mut store = open(&dir);
        assert_eq!(store.listings().unwrap(), vec![]);
        let mut expected = Vec::new();
        for tie in [
            Tie::Unpicked { found: 3 },
            linked(5),
            Tie::Gone { media: media(6) },
            Tie::Off,
        ] {
            let id = store.add(entry("Show")).unwrap().id;
            let tied = store.set_tie(id, Service::AniList, &tie).unwrap();
            let listing = Listing {
                entry: id,
                service: Service::AniList,
                tie,
                base: None,
                repeat_offset: 0,
                parked: None,
            };
            assert_eq!(tied.listing, listing);
            expected.push(listing);
        }
        assert_eq!(store.listings().unwrap(), expected);
        drop(store);
        assert_eq!(open(&dir).listings().unwrap(), expected);
    }

    #[test]
    fn linking_fills_a_missing_total_and_leaves_a_known_one_alone() {
        let (_tmp, dir) = open_tmp();
        let mut store = open(&dir);
        let open_ended = store.add(entry("Open")).unwrap().id;
        let tied = store
            .set_tie(open_ended, Service::AniList, &linked(5))
            .unwrap();
        let filled = tied.entry.expect("the total was filled");
        assert_eq!((filled.id, filled.total), (open_ended, Some(12)));
        assert_eq!(store.entries().unwrap(), vec![filled]);

        let known = store
            .add(NewEntry {
                total: Some(24),
                ..entry("Known")
            })
            .unwrap();
        let tied = store
            .set_tie(known.id, Service::AniList, &linked(6))
            .unwrap();
        assert_eq!(tied.entry, None);
        assert_eq!(store.entries().unwrap()[1], known);

        // A media the service has not counted, or one the show is only
        // reported gone from, fills nothing.
        let uncounted = store.add(entry("Uncounted")).unwrap().id;
        let tie = Tie::Linked {
            media: Media {
                episodes: None,
                ..media(7)
            },
            by: LinkedBy::Person,
        };
        assert_eq!(
            store
                .set_tie(uncounted, Service::AniList, &tie)
                .unwrap()
                .entry,
            None
        );
        let gone = store.add(entry("Gone")).unwrap().id;
        assert_eq!(
            store
                .set_tie(gone, Service::AniList, &Tie::Gone { media: media(8) })
                .unwrap()
                .entry,
            None
        );
        let totals: Vec<Option<u32>> = store
            .entries()
            .unwrap()
            .iter()
            .map(|entry| entry.total)
            .collect();
        assert_eq!(totals, vec![Some(12), Some(24), None, None]);
    }

    #[test]
    fn tying_to_another_media_starts_the_base_and_the_park_over_and_the_same_one_keeps_them() {
        let (_tmp, dir) = open_tmp();
        let mut store = open(&dir);
        let id = store.add(entry("Show")).unwrap().id;
        store.set_tie(id, Service::AniList, &linked(5)).unwrap();
        store.execute_raw(BASE);
        store.park(id, Service::AniList, &parked()).unwrap();

        let again = Tie::Linked {
            media: media(5),
            by: LinkedBy::Person,
        };
        let same = store.set_tie(id, Service::AniList, &again).unwrap().listing;
        assert_eq!(
            (same.tie, same.base, same.repeat_offset, same.parked),
            (again, based(), 2, Some(parked()))
        );
        let gone = store
            .set_tie(id, Service::AniList, &Tie::Gone { media: media(5) })
            .unwrap()
            .listing;
        assert_eq!(
            (gone.base, gone.repeat_offset, gone.parked),
            (based(), 2, Some(parked()))
        );
        let other = store
            .set_tie(id, Service::AniList, &linked(6))
            .unwrap()
            .listing;
        assert_eq!(
            (other.base, other.repeat_offset, other.parked),
            (None, 0, None)
        );

        store.execute_raw(BASE);
        store.park(id, Service::AniList, &parked()).unwrap();
        let off = store
            .set_tie(id, Service::AniList, &Tie::Off)
            .unwrap()
            .listing;
        assert_eq!(off.tie, Tie::Off);
        assert_eq!((off.base, off.repeat_offset, &off.parked), (None, 0, &None));
        // Every tie replaced the one row.
        assert_eq!(store.listings().unwrap(), vec![off]);
    }

    #[test]
    fn tying_a_show_that_is_not_there_is_not_found() {
        let (_tmp, dir) = open_tmp();
        let mut store = open(&dir);
        assert!(matches!(
            store.set_tie(EntryId(999), Service::AniList, &Tie::Off),
            Err(StoreError::NotFound { .. })
        ));
        assert_eq!(store.listings().unwrap(), vec![]);
    }

    #[test]
    fn parking_and_unparking_round_trip_and_keep_the_row() {
        let (_tmp, dir) = open_tmp();
        let mut store = open(&dir);
        let id = store.add(entry("Show")).unwrap().id;
        let tied = store
            .set_tie(id, Service::AniList, &linked(5))
            .unwrap()
            .listing;

        let set_aside = store.park(id, Service::AniList, &parked()).unwrap();
        assert_eq!(
            set_aside,
            Listing {
                parked: Some(parked()),
                ..tied.clone()
            }
        );
        assert_eq!(store.listings().unwrap(), vec![set_aside]);

        let back = store.unpark(id, Service::AniList).unwrap();
        assert_eq!(back, tied);
        assert_eq!(store.listings().unwrap(), vec![back]);
    }

    fn settlement(
        take_status: Option<ListStatus>,
        take_progress: Option<u32>,
        base_state: ListState,
    ) -> Settlement {
        Settlement {
            take_status,
            take_progress,
            base: ListBase {
                user: UserId(7),
                listed: true,
                state: base_state,
            },
            repeat_offset: 2,
        }
    }

    fn updates(store: &Store) -> i64 {
        store
            .conn
            .query_row("SELECT COUNT(*) FROM updates", [], |row| row.get(0))
            .unwrap()
    }

    #[test]
    fn settling_takes_the_lists_fields_and_writes_one_updates_row() {
        let (_tmp, dir) = open_tmp();
        let mut store = open(&dir);
        let id = store
            .add(NewEntry {
                progress: 3,
                ..entry("Show")
            })
            .unwrap()
            .id;
        store.set_tie(id, Service::AniList, &linked(5)).unwrap();
        let held = ListState {
            status: ListStatus::Completed,
            progress: 12,
            repeat: 3,
        };
        let taken = settlement(Some(ListStatus::Completed), Some(12), held);

        let settled = store
            .settle(id, Service::AniList, MediaId(5), &taken)
            .unwrap()
            .expect("the show is still linked to media 5");
        assert_eq!(
            (
                settled.entry.status,
                settled.entry.progress,
                settled.entry.rewatching,
                settled.entry.rewatched
            ),
            (WatchStatus::Completed, 12, false, 0)
        );
        assert_eq!(settled.listing.base, Some(taken.base));
        assert_eq!(settled.listing.repeat_offset, 2);
        let update = settled.update.expect("progress and status were taken");
        assert_eq!(
            (
                update.entry,
                update.service,
                update.progress_before,
                update.progress,
                update.status_before,
                update.status
            ),
            (
                id,
                Service::AniList,
                3,
                12,
                ListStatus::Watching,
                ListStatus::Completed
            )
        );
        assert!(update.at > UNIX_EPOCH);
        assert_eq!(store.entries().unwrap(), vec![settled.entry.clone()]);
        assert_eq!(store.listings().unwrap(), vec![settled.listing.clone()]);
        assert_eq!(updates(&store), 1);
        assert_eq!(store.updates().unwrap(), vec![update]);

        // The same report again: nothing moves, and no second row.
        let again = store
            .settle(id, Service::AniList, MediaId(5), &taken)
            .unwrap()
            .unwrap();
        assert_eq!(
            again,
            Settled {
                update: None,
                ..settled
            }
        );
        assert_eq!(updates(&store), 1);
    }

    #[test]
    fn settling_a_rewatch_taken_from_the_list_moves_the_flag_not_the_count() {
        let (_tmp, dir) = open_tmp();
        let mut store = open(&dir);
        let id = store
            .add(NewEntry {
                status: WatchStatus::Completed,
                progress: 12,
                total: Some(12),
                ..entry("Show")
            })
            .unwrap()
            .id;
        store.set_tie(id, Service::AniList, &linked(5)).unwrap();
        let held = ListState {
            status: ListStatus::Rewatching,
            progress: 2,
            repeat: 1,
        };
        let settled = store
            .settle(
                id,
                Service::AniList,
                MediaId(5),
                &settlement(Some(ListStatus::Rewatching), Some(2), held),
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            (
                settled.entry.status,
                settled.entry.rewatching,
                settled.entry.progress,
                settled.entry.rewatched
            ),
            (WatchStatus::Completed, true, 2, 0)
        );
        let update = settled.update.unwrap();
        assert_eq!(
            (update.status_before, update.status),
            (ListStatus::Completed, ListStatus::Rewatching)
        );
        // A base alone, with nothing taken, writes no row.
        let base_only = store
            .settle(
                id,
                Service::AniList,
                MediaId(5),
                &settlement(None, None, held),
            )
            .unwrap()
            .unwrap();
        assert_eq!(base_only.update, None);
        assert_eq!(base_only.entry, settled.entry);
        assert_eq!(updates(&store), 1);
    }

    #[test]
    fn settling_a_show_tied_elsewhere_since_writes_nothing() {
        let (_tmp, dir) = open_tmp();
        let mut store = open(&dir);
        let id = store.add(entry("Show")).unwrap().id;
        let tied = store
            .set_tie(id, Service::AniList, &linked(6))
            .unwrap()
            .listing;
        let held = ListState {
            status: ListStatus::Completed,
            progress: 12,
            repeat: 0,
        };
        let stale = settlement(Some(ListStatus::Completed), Some(12), held);
        assert_eq!(
            store
                .settle(id, Service::AniList, MediaId(5), &stale)
                .unwrap(),
            None
        );
        let gone = store
            .set_tie(id, Service::AniList, &Tie::Gone { media: media(5) })
            .unwrap()
            .listing;
        assert_eq!(
            store
                .settle(id, Service::AniList, MediaId(5), &stale)
                .unwrap(),
            None
        );
        assert_eq!(store.entries().unwrap()[0].progress, 0);
        assert_eq!(
            store.listings().unwrap(),
            vec![Listing {
                tie: gone.tie,
                ..tied
            }]
        );
        assert_eq!(updates(&store), 0);
        assert!(matches!(
            store.settle(EntryId(999), Service::AniList, MediaId(5), &stale),
            Err(StoreError::NotListed { .. })
        ));
    }

    #[test]
    fn settling_takes_a_show_out_of_the_park() {
        let (_tmp, dir) = open_tmp();
        let mut store = open(&dir);
        let id = store.add(entry("Show")).unwrap().id;
        store.set_tie(id, Service::AniList, &linked(5)).unwrap();
        store.park(id, Service::AniList, &parked()).unwrap();
        let unlisted = Settlement {
            base: ListBase {
                listed: false,
                ..settlement(None, None, parked().wanted).base
            },
            repeat_offset: -1,
            ..settlement(None, None, parked().wanted)
        };
        let settled = store
            .settle(id, Service::AniList, MediaId(5), &unlisted)
            .unwrap()
            .unwrap();
        assert_eq!(settled.listing.parked, None);
        assert_eq!(settled.listing.base, Some(unlisted.base));
        assert_eq!(settled.listing.repeat_offset, -1);
        assert_eq!(settled.update, None);
    }

    #[test]
    fn garbage_update_rows_surface_as_invalid() {
        let (_tmp, dir) = open_tmp();
        let mut store = open(&dir);
        let id = store.add(entry("Show")).unwrap().id;
        store.set_tie(id, Service::AniList, &linked(5)).unwrap();
        let held = ListState {
            status: ListStatus::Watching,
            progress: 1,
            repeat: 0,
        };
        let update = store
            .settle(
                id,
                Service::AniList,
                MediaId(5),
                &settlement(None, Some(1), held),
            )
            .unwrap()
            .unwrap()
            .update
            .unwrap();
        for (broken, field) in [
            ("UPDATE updates SET service = 'mal'", "service"),
            (
                "UPDATE updates SET service = 'anilist', progress_before = -1",
                "progress_before",
            ),
            (
                "UPDATE updates SET progress_before = 0, progress = -1",
                "progress",
            ),
            (
                "UPDATE updates SET progress = 1, status_before = 'bogus'",
                "status_before",
            ),
            (
                "UPDATE updates SET status_before = 'watching', status = 'bogus'",
                "status",
            ),
            ("UPDATE updates SET status = 'watching', at = -1", "at"),
        ] {
            store.execute_raw(broken);
            assert!(
                matches!(
                    fetch_update(&store.conn, update.id),
                    Err(StoreError::InvalidUpdate { id, field: got }) if id == update.id && got == field
                ),
                "{broken}"
            );
        }
    }

    #[test]
    fn parking_a_show_never_looked_up_is_not_listed() {
        let (_tmp, dir) = open_tmp();
        let mut store = open(&dir);
        let id = store.add(entry("Show")).unwrap().id;
        assert!(matches!(
            store.park(id, Service::AniList, &parked()),
            Err(StoreError::NotListed { entry, service: Service::AniList }) if entry == id
        ));
        assert!(matches!(
            store.unpark(id, Service::AniList),
            Err(StoreError::NotListed { entry, service: Service::AniList }) if entry == id
        ));
        assert_eq!(store.listings().unwrap(), vec![]);
    }

    /// Each step breaks the row one way and mends the last, so every check
    /// in `parse` is reached with the ones before it passing.
    #[test]
    fn garbage_listing_rows_surface_as_invalid() {
        let (_tmp, dir) = open_tmp();
        let mut store = open(&dir);
        let id = store.add(entry("Show")).unwrap().id;
        store.set_tie(id, Service::AniList, &linked(5)).unwrap();
        for (broken, field) in [
            ("UPDATE listings SET base_user = 7", "base"),
            (
                "UPDATE listings SET base_user = NULL, parked = 'Refused'",
                "parked",
            ),
            (
                "UPDATE listings SET parked = NULL, parked_at = -1, parked_status = 'watching', \
                 parked_progress = 1, parked_repeat = 0, parked = 'Refused'",
                "parked_at",
            ),
            (
                "UPDATE listings SET parked = NULL, parked_at = NULL, parked_status = NULL, \
                 parked_progress = NULL, parked_repeat = NULL, tie = 'bogus'",
                "tie",
            ),
            (
                "UPDATE listings SET tie = 'linked', media_id = NULL",
                "media",
            ),
            (
                "UPDATE listings SET media_id = 5, linked_by = NULL",
                "linked_by",
            ),
            (
                "UPDATE listings SET linked_by = 'title', tie = 'unpicked'",
                "found",
            ),
            (
                "UPDATE listings SET tie = 'gone', media_title = NULL",
                "media",
            ),
            (
                "UPDATE listings SET media_title = 'Show', service = 'mal'",
                "service",
            ),
        ] {
            store.execute_raw(broken);
            assert!(
                matches!(
                    store.listings(),
                    Err(StoreError::InvalidListing { entry, field: got, .. })
                        if entry == id && got == field
                ),
                "{broken}"
            );
        }
    }
}
