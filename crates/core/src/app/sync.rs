//! The reducer's list side: what the core does with each report the sync
//! worker sends back, the commands the picker and a Library row give it,
//! and the watched-ahead rule that asks a list before a file ahead of the
//! library is refused for good. Every store write goes through `commit`
//! and memory takes what the store returned, as on the library side.
//! Nothing here decides what is pending: `AppState::work` derives that
//! from the library and the listings after each change.

use std::fs;
use std::time::SystemTime;

use super::{Ryuuji, commit, upsert};
use crate::{
    Account, Asked, Attempt, AutoLink, Candidate, Cover, Described, EntryId, Link, LinkedBy,
    ListState, Listing, Media, MediaId, Notice, Parked, Picker, Remote, Report, Service, Settled,
    ShowInfo, Signed, Tie, Tied, UserId, auto_link, settlement,
};

/// The folder under the data directory the covers are kept in.
const COVERS: &str = "covers";

impl Ryuuji {
    pub(super) fn synced(&mut self, report: Report) {
        match report {
            Report::Checking(service) => {
                self.state.sync.attempt = Some(Attempt::Checking(service));
            }
            Report::SignedIn(account) => self.signed_in(account),
            Report::NotSignedIn { service, message } => {
                self.state.sync.attempt = Some(Attempt::Failed { service, message });
            }
            Report::SignedOut(service) => self.signed_out(service),
            Report::Refused { service, message } => self.refused(service, message),
            Report::Found {
                service,
                entry,
                query,
                candidates,
            } => self.found(service, entry, query, candidates),
            Report::Reconciled {
                service,
                user,
                entry,
                media,
                asked,
                remote,
            } => self.reconciled(service, user, entry, media, &asked, &remote),
            Report::Rejected {
                service,
                entry,
                media,
                wanted,
                message,
            } => self.rejected(service, entry, media, wanted, message),
            Report::Gone {
                service,
                entry,
                media,
            } => self.gone(service, entry, media),
            Report::Described {
                service,
                media,
                info,
                cover,
            } => self.described(service, media, &info, cover.as_ref()),
        }
    }

    /// Keeps what a service said about a show, and its cover as a file under
    /// the data folder. A cover that cannot be written is left out with a
    /// warning, since the page has a placeholder for it; the info is stored
    /// either way.
    fn described(
        &mut self,
        service: Service,
        media: MediaId,
        info: &ShowInfo,
        cover: Option<&Cover>,
    ) {
        let file = cover.and_then(|cover| self.keep_cover(service, media, cover));
        let outcome = self.store.describe(service, media, info, file.as_deref());
        if let Some(at) = commit(&mut self.state.notices, "show info", outcome) {
            let described = &mut self.state.sync.described;
            match described
                .iter_mut()
                .find(|known| known.service == service && known.media == media)
            {
                Some(known) => *known = at,
                None => described.push(at),
            }
        }
    }

    /// Writes a cover into the covers folder by way of a temporary file, so
    /// a crash never leaves half a picture under the name the page reads.
    /// Says the file's name, or nothing when it could not be written.
    fn keep_cover(&self, service: Service, media: MediaId, cover: &Cover) -> Option<String> {
        let extension: String = cover
            .extension
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .take(5)
            .collect::<String>()
            .to_ascii_lowercase();
        let extension = if extension.is_empty() {
            "jpg".to_owned()
        } else {
            extension
        };
        let name = format!("{}-{}.{extension}", service.tag(), media.0);
        let folder = self.dir.root().join(COVERS);
        let written = fs::create_dir_all(&folder).and_then(|()| {
            let partial = folder.join(format!("{name}.partial"));
            fs::write(&partial, &cover.bytes)?;
            fs::rename(&partial, folder.join(&name))
        });
        match written {
            Ok(()) => Some(name),
            Err(err) => {
                tracing::warn!(error = %err, cover = %name, "cover not kept");
                None
            }
        }
    }

    /// What was kept about a show on `service`, with its cover's path when
    /// the file is there. Read from the store on each call rather than held
    /// in the app state, which the shell clones on every dispatch.
    pub fn show_info(
        &self,
        service: Service,
        media: MediaId,
    ) -> Result<Option<Described>, crate::StoreError> {
        Ok(self.store.show_info(service, media)?.map(|kept| Described {
            info: kept.info,
            cover: kept
                .cover_file
                .map(|file| self.dir.root().join(COVERS).join(file))
                .filter(|path| path.is_file()),
        }))
    }

    /// Keeps the account the token turned out to belong to. The attempt is
    /// over either way: a row that could not be written is its own notice,
    /// and the card must not go on saying it is checking.
    fn signed_in(&mut self, account: Account) {
        self.state.sync.attempt = None;
        let Some(stored) = self.save_account(Account {
            signed: Signed::In,
            ..account
        }) else {
            return;
        };
        tracing::info!(
            service = stored.service.tag(),
            user = stored.user.0,
            "signed in"
        );
        clear_refusal(&mut self.state.notices, stored.service);
    }

    /// Marks the account signed out. Its ties and bases stay, so signing
    /// back in as the same person carries on from them.
    fn signed_out(&mut self, service: Service) {
        let Some(account) = self.state.sync.account(service).cloned() else {
            return;
        };
        if self
            .save_account(Account {
                signed: Signed::Out,
                ..account
            })
            .is_none()
        {
            return;
        }
        tracing::info!(service = service.tag(), "signed out");
        clear_refusal(&mut self.state.notices, service);
    }

    /// Marks the account refused, so nothing goes out until a new token,
    /// and says so once. Only an account signed in can be refused: the
    /// worker reports the refusal for every job it had in hand, so a
    /// second report, or one from a job that met the empty vault after a
    /// sign-out, is stale and changes nothing.
    fn refused(&mut self, service: Service, message: String) {
        let Some(account) = self.state.sync.signed_in(service).cloned() else {
            return;
        };
        if self
            .save_account(Account {
                signed: Signed::Refused { message },
                ..account
            })
            .is_none()
        {
            return;
        }
        tracing::warn!(service = service.tag(), "token refused");
        self.state.notices.push(Notice::SignInRefused { service });
    }

    fn save_account(&mut self, account: Account) -> Option<Account> {
        let outcome = self.store.save_account(&account);
        let stored = commit(&mut self.state.notices, "account", outcome)?;
        upsert_account(&mut self.state.sync.accounts, stored.clone());
        Some(stored)
    }

    /// Takes what a search turned up. The picker's own query fills the
    /// picker, while it waits: an answer once it is filled is the
    /// background search's, which asked the same title. A search ties a
    /// show that has not been looked up yet; one that has been by now, by
    /// the person's pick or an earlier answer, is left as it stands. One
    /// answer can be both, when the picker asked a never-looked-up show's
    /// own title: it fills the picker and ties the show, so the background
    /// search is not asked after it only to fill the picker again.
    fn found(
        &mut self,
        service: Service,
        entry: EntryId,
        query: String,
        candidates: Vec<Candidate>,
    ) {
        let looked_up = self.state.sync.listing(entry, service).is_some();
        if let Some(picker) = self.state.sync.picker.as_mut()
            && picker.entry == entry
            && picker.service == service
            && picker.query == query
            && picker.found.is_none()
        {
            if looked_up {
                picker.found = Some(candidates);
                return;
            }
            picker.found = Some(candidates.clone());
        }
        if looked_up {
            return;
        }
        let Some(show) = self.state.entry(entry) else {
            return;
        };
        let tie = match auto_link(show, &candidates) {
            AutoLink::Link(media) => Tie::Linked {
                media,
                by: LinkedBy::Title,
            },
            AutoLink::Pick { found } => Tie::Unpicked { found },
        };
        self.tie(entry, service, &tie);
    }

    /// Settles what a reconcile found into the library and the base. A
    /// report from another user's list describes nothing this one holds,
    /// and one about a media the show no longer stands with is left by the
    /// store. Where the library took progress or status from the list the
    /// gates run again: a file refused as not the next episode may be it
    /// now, which is what the watched-ahead check asked.
    fn reconciled(
        &mut self,
        service: Service,
        user: UserId,
        entry: EntryId,
        media: MediaId,
        asked: &Asked,
        remote: &Remote,
    ) {
        if !self
            .state
            .sync
            .signed_in(service)
            .is_some_and(|account| account.user == user)
        {
            tracing::debug!(
                entry = entry.as_i64(),
                service = service.tag(),
                user = user.0,
                "report for an account not signed in; ignored"
            );
            return;
        }
        let show = self.state.entry(entry);
        let (Some(show), Some(listing)) = (show, self.state.sync.listing(entry, service)) else {
            return;
        };
        let settled = settlement(asked, remote, user, show, listing);
        let outcome = self.store.settle(entry, service, media, &settled);
        let Some(Some(Settled {
            entry: taken,
            listing,
            update,
        })) = commit(&mut self.state.notices, "settlement", outcome)
        else {
            return;
        };
        clear_rejection(&mut self.state.notices, service, &taken.title);
        upsert(&mut self.state.library, taken);
        upsert_listing(&mut self.state.sync.listings, listing);
        self.state
            .sync
            .checks
            .retain(|check| *check != (service, entry));
        if update.is_some() {
            self.record_now();
        }
    }

    /// Parks the show with what the service refused, when the refusal is
    /// about the media it still stands with, and says so: in place of a
    /// notice still standing, since a second refusal only follows a second
    /// send, and that is worth saying again.
    fn rejected(
        &mut self,
        service: Service,
        entry: EntryId,
        media: MediaId,
        wanted: ListState,
        message: String,
    ) {
        if self.linked_media(entry, service, media).is_none() {
            return;
        }
        let parked = Parked {
            wanted,
            message: message.clone(),
            at: SystemTime::now(),
        };
        let outcome = self.store.park(entry, service, &parked);
        let Some(listing) = commit(&mut self.state.notices, "parked show", outcome) else {
            return;
        };
        tracing::warn!(
            entry = entry.as_i64(),
            service = service.tag(),
            message = %message,
            "write rejected; show parked"
        );
        upsert_listing(&mut self.state.sync.listings, listing);
        let Some(show) = self.title_of(entry) else {
            return;
        };
        raise(
            &mut self.state.notices,
            Notice::ListRejected {
                service,
                show,
                message,
            },
        );
    }

    /// Marks the media gone from the service, when the report is about the
    /// media the show still stands with, and says so once: the tie is no
    /// longer linked after the first, so a second report finds nothing to
    /// mark.
    fn gone(&mut self, service: Service, entry: EntryId, media: MediaId) {
        let Some(media) = self.linked_media(entry, service, media) else {
            return;
        };
        if self.tie(entry, service, &Tie::Gone { media }).is_none() {
            return;
        }
        let Some(show) = self.title_of(entry) else {
            return;
        };
        raise(&mut self.state.notices, Notice::ListGone { service, show });
    }

    pub(super) fn search_list(&mut self, entry: EntryId, service: Service, query: String) {
        self.state.sync.picker = Some(Picker {
            entry,
            service,
            query,
            found: None,
        });
    }

    /// Ties the show to the person's pick. The picker closes whether or not
    /// the write took: a refused one is said by its notice, and the pick
    /// can be made again. A notice about the media the show stood with
    /// before goes with it: gone from the service, or refusing a write,
    /// which the store forgets with the park when the media changes.
    pub(super) fn link_show(&mut self, entry: EntryId, service: Service, media: Media) {
        self.state.sync.picker = None;
        let tie = Tie::Linked {
            media,
            by: LinkedBy::Person,
        };
        let Some(listing) = self.tie(entry, service, &tie) else {
            return;
        };
        if let Some(show) = self.title_of(entry) {
            clear_gone(&mut self.state.notices, service, &show);
            if listing.parked.is_none() {
                clear_rejection(&mut self.state.notices, service, &show);
            }
        }
    }

    pub(super) fn stop_syncing(&mut self, entry: EntryId, service: Service) {
        self.state.sync.picker = None;
        if self.tie(entry, service, &Tie::Off).is_none() {
            return;
        }
        if let Some(show) = self.title_of(entry) {
            clear_gone(&mut self.state.notices, service, &show);
        }
    }

    /// Takes a show out of the park, so its change is sent again. The
    /// notice goes with the park: refused again, it is said again.
    pub(super) fn retry_sync(&mut self, entry: EntryId, service: Service) {
        let outcome = self.store.unpark(entry, service);
        let Some(listing) = commit(&mut self.state.notices, "parked show", outcome) else {
            return;
        };
        upsert_listing(&mut self.state.sync.listings, listing);
        if let Some(show) = self.title_of(entry) {
            clear_rejection(&mut self.state.notices, service, &show);
        }
    }

    /// The watched-ahead rule. A file past the next episode of a linked
    /// show may be right where the library is behind: the person watched on
    /// elsewhere, and the list knows. Before the refusal is left to stand,
    /// every signed-in service the show is linked on is asked to read its
    /// list, once per file, so a file played on does not ask again with
    /// every event. The read is the reconcile `work` gives at once for a
    /// checked show; a list ahead is taken by the settle, and the gates run
    /// again.
    pub(super) fn ask_lists_ahead(&mut self) {
        let Some(last) = &self.state.last_match else {
            return;
        };
        let Link::Exact(id) = last.link else {
            return;
        };
        let Some(range) = &last.episode else {
            return;
        };
        let Some(show) = self.state.entry(id) else {
            return;
        };
        if !show
            .progress
            .checked_add(1)
            .is_some_and(|next| *range.start() > next)
        {
            return;
        }
        let (progress, end) = (show.progress, *range.end());
        let sync = &self.state.sync;
        let linked: Vec<Service> = sync
            .signed_in_accounts()
            .map(|account| account.service)
            .filter(|&service| {
                sync.listing(id, service)
                    .is_some_and(|listing| listing.tie.linked().is_some())
            })
            .collect();
        for service in linked {
            if !self.asked_ahead.insert((service, id, end)) {
                continue;
            }
            tracing::info!(
                entry = id.as_i64(),
                service = service.tag(),
                episode = end,
                progress,
                "file ahead of the library; asking the list"
            );
            if !self.state.sync.checks.contains(&(service, id)) {
                self.state.sync.checks.push((service, id));
            }
        }
    }

    /// Writes how a show stands with a service and takes the row back, and
    /// the entry with it when linking filled in its total.
    fn tie(&mut self, entry: EntryId, service: Service, tie: &Tie) -> Option<Listing> {
        let outcome = self.store.set_tie(entry, service, tie);
        let Tied {
            listing,
            entry: filled,
        } = commit(&mut self.state.notices, "listing", outcome)?;
        if let Some(filled) = filled {
            upsert(&mut self.state.library, filled);
        }
        upsert_listing(&mut self.state.sync.listings, listing.clone());
        Some(listing)
    }

    /// The media the show is linked to on `service`, when that is `media`:
    /// what a report about a media has to be about to apply to the show.
    fn linked_media(&self, entry: EntryId, service: Service, media: MediaId) -> Option<Media> {
        match &self.state.sync.listing(entry, service)?.tie {
            Tie::Linked { media: linked, .. } if linked.id == media => Some(linked.clone()),
            _ => None,
        }
    }

    fn title_of(&self, entry: EntryId) -> Option<String> {
        self.state
            .library
            .iter()
            .find(|show| show.id == entry)
            .map(|show| show.title.clone())
    }
}

/// Says `notice` in place of one of its kind about the same show, where one
/// stands, else at the back of the queue.
fn raise(notices: &mut Vec<Notice>, notice: Notice) {
    match notices
        .iter()
        .position(|standing| same_show(standing, &notice))
    {
        Some(at) => notices[at] = notice,
        None => notices.push(notice),
    }
}

/// Whether two list notices are of one kind about one show on one service.
fn same_show(a: &Notice, b: &Notice) -> bool {
    match (a, b) {
        (
            Notice::ListRejected { service, show, .. },
            Notice::ListRejected {
                service: other,
                show: title,
                ..
            },
        )
        | (
            Notice::ListGone { service, show },
            Notice::ListGone {
                service: other,
                show: title,
            },
        ) => service == other && show == title,
        _ => false,
    }
}

fn clear_refusal(notices: &mut Vec<Notice>, service: Service) {
    notices
        .retain(|notice| !matches!(notice, Notice::SignInRefused { service: s } if *s == service));
}

fn clear_rejection(notices: &mut Vec<Notice>, service: Service, show: &str) {
    notices.retain(|notice| {
        !matches!(notice, Notice::ListRejected { service: s, show: t, .. } if *s == service && t == show)
    });
}

fn clear_gone(notices: &mut Vec<Notice>, service: Service, show: &str) {
    notices.retain(|notice| {
        !matches!(notice, Notice::ListGone { service: s, show: t } if *s == service && t == show)
    });
}

fn upsert_account(accounts: &mut Vec<Account>, account: Account) {
    match accounts
        .iter_mut()
        .find(|existing| existing.service == account.service)
    {
        Some(existing) => *existing = account,
        None => accounts.push(account),
    }
}

fn upsert_listing(listings: &mut Vec<Listing>, listing: Listing) {
    match listings
        .iter_mut()
        .find(|existing| existing.entry == listing.entry && existing.service == listing.service)
    {
        Some(existing) => *existing = listing,
        None => listings.push(listing),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::super::tests::{
        entry, later, open_tmp, outcome, stored_writes, watch_past_threshold,
    };
    use super::*;
    use crate::{
        Command, DataDir, Decline, Format, Job, LinkState, ListEntryId, ListStatus, NewEntry,
        RecordOutcome, Store, Task, WatchStatus, When, Work, Write, merge, normalize_title,
    };

    const ANILIST: Service = Service::AniList;

    fn account(signed: Signed) -> Account {
        Account {
            service: ANILIST,
            user: UserId(7),
            name: "someone".into(),
            expires: None,
            signed,
        }
    }

    fn media(id: u64, title: &str, episodes: Option<u32>) -> Media {
        Media {
            id: MediaId(id),
            title: title.into(),
            episodes,
            mal_id: None,
        }
    }

    fn candidate(id: u64, title: &str, episodes: Option<u32>) -> Candidate {
        Candidate {
            media: media(id, title, episodes),
            titles: vec![],
            format: Some(Format::Tv),
            year: None,
        }
    }

    fn state(status: ListStatus, progress: u32, repeat: u32) -> ListState {
        ListState {
            status,
            progress,
            repeat,
        }
    }

    /// "Show" as the service lists it: the one candidate most tests have.
    fn show() -> Candidate {
        candidate(5, "Show", Some(12))
    }

    /// A list service with no network: rows by media, a catalogue for
    /// searches, and per job the same steps the worker takes. A search
    /// answers from the catalogue. A reconcile reads the row, merges,
    /// applies the write, and reports the row as it then stands.
    struct FakeList {
        rows: HashMap<MediaId, ListState>,
        catalogue: Vec<Candidate>,
        /// Writes applied, so a round can be shown to have written nothing.
        writes: u32,
    }

    impl FakeList {
        fn new(catalogue: Vec<Candidate>) -> FakeList {
            FakeList {
                rows: HashMap::new(),
                catalogue,
                writes: 0,
            }
        }

        /// What the list holds for `media`, set as a site edit would.
        fn holds(&mut self, media: u64, state: ListState) {
            self.rows.insert(MediaId(media), state);
        }

        fn row(&self, media: u64) -> Option<ListState> {
            self.rows.get(&MediaId(media)).copied()
        }

        fn read(&self, media: MediaId) -> Remote {
            match self.rows.get(&media) {
                Some(&state) => Remote::Listed {
                    id: Some(ListEntryId(1000 + media.0)),
                    state,
                },
                None => Remote::Unlisted,
            }
        }

        /// What the catalogue says about `media`.
        fn describe(&self, media: MediaId) -> ShowInfo {
            self.catalogue
                .iter()
                .find(|candidate| candidate.media.id == media)
                .map(|candidate| ShowInfo {
                    titles: candidate.titles.clone(),
                    format: candidate.format,
                    season_year: candidate.year,
                    episodes: candidate.media.episodes,
                    ..ShowInfo::default()
                })
                .unwrap_or_default()
        }

        fn search(&self, query: &str) -> Vec<Candidate> {
            let query = normalize_title(query);
            self.catalogue
                .iter()
                .filter(|candidate| {
                    std::iter::once(&candidate.media.title)
                        .chain(&candidate.titles)
                        .any(|name| normalize_title(name).contains(&query))
                })
                .cloned()
                .collect()
        }

        /// The report for every job in `work`, with each reconcile's write
        /// applied: what one posting of the work gets back.
        fn reports(&mut self, work: &Work) -> Vec<Report> {
            let mut reports = Vec::new();
            for service in &work.services {
                for job in &service.jobs {
                    reports.push(match &job.task {
                        Task::Find { title } => Report::Found {
                            service: service.service,
                            entry: job.entry,
                            query: title.clone(),
                            candidates: self.search(title),
                        },
                        Task::Reconcile {
                            media,
                            wanted,
                            base,
                        } => {
                            let merged = merge(*wanted, *base, &self.read(*media));
                            match merged.write {
                                Some(Write::Create(state)) => {
                                    self.rows.insert(*media, state);
                                    self.writes += 1;
                                }
                                Some(Write::Change(patch)) => {
                                    let row = self.rows.get_mut(media).expect("a listed row");
                                    *row = patch.apply(*row);
                                    self.writes += 1;
                                }
                                None => {}
                            }
                            Report::Reconciled {
                                service: service.service,
                                user: service.user,
                                entry: job.entry,
                                media: *media,
                                asked: Asked {
                                    wanted: *wanted,
                                    base: merged.base,
                                },
                                remote: self.read(*media),
                            }
                        }
                        Task::Describe { media } => Report::Described {
                            service: service.service,
                            media: *media,
                            info: self.describe(*media),
                            cover: None,
                        },
                    });
                }
            }
            reports
        }

        /// One round: answers every job `work()` asks for.
        fn answer(&mut self, app: &mut Ryuuji) {
            for report in self.reports(&app.state().work()) {
                app.dispatch(Command::Synced(report));
            }
        }
    }

    fn signed_in(app: &mut Ryuuji) {
        app.dispatch(Command::Synced(Report::SignedIn(account(Signed::In))));
    }

    /// An app signed in to AniList with "Show" at `status` and `progress`,
    /// and the show's id.
    fn app_with_show(dir: &DataDir, status: WatchStatus, progress: u32) -> (Ryuuji, EntryId) {
        let mut app = Ryuuji::open(dir).unwrap();
        signed_in(&mut app);
        app.dispatch(Command::AddEntry(NewEntry {
            status,
            progress,
            ..entry("Show")
        }));
        let id = app.state().library[0].id;
        (app, id)
    }

    /// Two rounds: the search ties the show, the reconcile settles its
    /// first merge, and the show is in step.
    fn linked(app: &mut Ryuuji, list: &mut FakeList) {
        list.answer(app);
        list.answer(app);
        assert!(jobs(app).is_empty(), "{:?}", jobs(app));
    }

    /// The searches and reconciles the work holds. Fetching show info is
    /// beside the point of these tests and has its own.
    fn jobs(app: &Ryuuji) -> Vec<Job> {
        app.state()
            .work()
            .services
            .into_iter()
            .flat_map(|service| service.jobs)
            .filter(|job| !matches!(job.task, Task::Describe { .. }))
            .collect()
    }

    fn listing(app: &Ryuuji, id: EntryId) -> Listing {
        app.state().sync.listing(id, ANILIST).unwrap().clone()
    }

    /// `(progress before, progress, status before, status)` of every value
    /// taken from the list, oldest first.
    fn stored_updates(dir: &DataDir) -> Vec<(u32, u32, ListStatus, ListStatus)> {
        Store::open(dir)
            .unwrap()
            .store
            .updates()
            .unwrap()
            .iter()
            .map(|update| {
                (
                    update.progress_before,
                    update.progress,
                    update.status_before,
                    update.status,
                )
            })
            .collect()
    }

    #[test]
    fn a_description_keeps_the_info_and_the_cover_file() {
        let (_tmp, dir) = open_tmp();
        let mut app = Ryuuji::open(&dir).unwrap();
        let info = ShowInfo {
            titles: vec!["Sousou no Frieren".into()],
            episodes: Some(28),
            ..ShowInfo::default()
        };
        app.dispatch(Command::Synced(Report::Described {
            service: ANILIST,
            media: MediaId(154_587),
            info: info.clone(),
            cover: Some(Cover {
                bytes: b"not a real jpeg".to_vec(),
                extension: ".JPG?x".into(),
            }),
        }));
        let described = app.show_info(ANILIST, MediaId(154_587)).unwrap().unwrap();
        assert_eq!(described.info, info);
        let cover = described.cover.unwrap();
        assert_eq!(cover, dir.root().join("covers").join("anilist-154587.jpgx"));
        assert_eq!(fs::read(&cover).unwrap(), b"not a real jpeg");
        assert_eq!(
            app.state()
                .sync
                .described
                .iter()
                .map(|known| (known.service, known.media))
                .collect::<Vec<_>>(),
            vec![(ANILIST, MediaId(154_587))]
        );
        assert!(
            !dir.root()
                .join("covers")
                .join("anilist-154587.jpgx.partial")
                .exists()
        );

        // Fetched again with no cover: the file stays, and so does one row.
        app.dispatch(Command::Synced(Report::Described {
            service: ANILIST,
            media: MediaId(154_587),
            info: ShowInfo {
                episodes: Some(29),
                ..info
            },
            cover: None,
        }));
        let again = app.show_info(ANILIST, MediaId(154_587)).unwrap().unwrap();
        assert_eq!(again.info.episodes, Some(29));
        assert_eq!(again.cover, Some(cover));
        assert_eq!(app.state().sync.described.len(), 1);
        drop(app);
        assert_eq!(Ryuuji::open(&dir).unwrap().state().sync.described.len(), 1);
    }

    #[test]
    fn a_sign_in_is_checked_then_kept_or_its_failure_shown() {
        let (_tmp, dir) = open_tmp();
        let mut app = Ryuuji::open(&dir).unwrap();
        app.dispatch(Command::Synced(Report::Checking(ANILIST)));
        assert_eq!(app.state().sync.attempt, Some(Attempt::Checking(ANILIST)));
        app.dispatch(Command::Synced(Report::NotSignedIn {
            service: ANILIST,
            message: "Invalid token".into(),
        }));
        assert_eq!(
            app.state().sync.attempt,
            Some(Attempt::Failed {
                service: ANILIST,
                message: "Invalid token".into()
            })
        );
        assert_eq!(app.state().sync.accounts, vec![]);

        app.dispatch(Command::Synced(Report::Checking(ANILIST)));
        signed_in(&mut app);
        assert_eq!(app.state().sync.attempt, None);
        assert_eq!(app.state().sync.accounts, vec![account(Signed::In)]);
        assert!(app.state().notices.is_empty());
        drop(app);
        assert_eq!(
            Ryuuji::open(&dir).unwrap().state().sync.accounts,
            vec![account(Signed::In)]
        );
    }

    #[test]
    fn a_refused_token_is_said_once_until_the_account_signs_in_again() {
        let (_tmp, dir) = open_tmp();
        let mut app = Ryuuji::open(&dir).unwrap();
        signed_in(&mut app);
        let refused = || {
            Command::Synced(Report::Refused {
                service: ANILIST,
                message: "Invalid token".into(),
            })
        };
        app.dispatch(refused());
        assert_eq!(
            app.state().sync.accounts,
            vec![account(Signed::Refused {
                message: "Invalid token".into()
            })]
        );
        assert_eq!(
            app.state().notices,
            vec![Notice::SignInRefused { service: ANILIST }]
        );
        app.dispatch(refused());
        assert_eq!(app.state().notices.len(), 1);
        // A failed save queued behind it, so the sign-in can be seen to take
        // the refusal alone, where it stands.
        app.dispatch(Command::SetProgress {
            id: EntryId(9999),
            progress: 1,
        });
        assert_eq!(app.state().notices.len(), 2);

        signed_in(&mut app);
        assert_eq!(app.state().sync.accounts, vec![account(Signed::In)]);
        assert!(matches!(
            app.state().notices.as_slice(),
            [Notice::SaveFailed { .. }]
        ));
        // Refused again after that, it is a new refusal and is said again.
        app.dispatch(refused());
        assert!(matches!(
            app.state().notices.as_slice(),
            [Notice::SaveFailed { .. }, Notice::SignInRefused { .. }]
        ));
    }

    /// The worker's refusal for a job that ran on after the sign-out, with
    /// the token already forgotten, is stale: the account stays signed out
    /// and nothing is said.
    #[test]
    fn a_refusal_after_a_sign_out_is_stale_and_changes_nothing() {
        let (_tmp, dir) = open_tmp();
        let mut app = Ryuuji::open(&dir).unwrap();
        signed_in(&mut app);
        app.dispatch(Command::Synced(Report::SignedOut(ANILIST)));
        app.dispatch(Command::Synced(Report::Refused {
            service: ANILIST,
            message: "No AniList token is kept on this computer; paste one in Settings.".into(),
        }));
        assert_eq!(app.state().sync.accounts, vec![account(Signed::Out)]);
        assert!(app.state().notices.is_empty());
        drop(app);
        assert_eq!(
            Ryuuji::open(&dir).unwrap().state().sync.accounts,
            vec![account(Signed::Out)]
        );
    }

    #[test]
    fn signing_out_keeps_the_account_and_the_ties_made_under_it() {
        let (_tmp, dir) = open_tmp();
        let (mut app, id) = app_with_show(&dir, WatchStatus::Watching, 3);
        let mut list = FakeList::new(vec![show()]);
        list.holds(5, state(ListStatus::Watching, 3, 0));
        linked(&mut app, &mut list);
        let tied = listing(&app, id);
        assert!(tied.base.is_some());

        app.dispatch(Command::Synced(Report::SignedOut(ANILIST)));
        assert_eq!(app.state().sync.accounts, vec![account(Signed::Out)]);
        assert_eq!(app.state().work(), Work { services: vec![] });
        assert_eq!(listing(&app, id), tied);

        // Back in as the same person, the show is still in step.
        signed_in(&mut app);
        assert!(jobs(&app).is_empty());
    }

    /// User 7's list held two rewatches Ryuuji never saw, so the offset was
    /// 2. User 8 signs in with the show on no list: their first merge holds
    /// it, and the change that creates their row carries Ryuuji's own
    /// count, not 7's.
    #[test]
    fn signing_in_as_another_user_sends_ryuujis_own_repeat_count() {
        let (_tmp, dir) = open_tmp();
        let (mut app, id) = app_with_show(&dir, WatchStatus::Watching, 3);
        let mut list = FakeList::new(vec![show()]);
        list.holds(5, state(ListStatus::Watching, 3, 2));
        linked(&mut app, &mut list);
        assert_eq!(listing(&app, id).repeat_offset, 2);

        app.dispatch(Command::Synced(Report::SignedOut(ANILIST)));
        app.dispatch(Command::Synced(Report::SignedIn(Account {
            user: UserId(8),
            name: "someone".into(),
            ..account(Signed::In)
        })));
        let mut theirs = FakeList::new(vec![show()]);
        assert_eq!(jobs(&app).len(), 1);
        theirs.answer(&mut app);
        assert!(jobs(&app).is_empty());
        assert_eq!(theirs.row(5), None);
        assert_eq!(listing(&app, id).repeat_offset, 0);

        app.dispatch(Command::SetProgress { id, progress: 4 });
        theirs.answer(&mut app);
        assert_eq!(theirs.row(5), Some(state(ListStatus::Watching, 4, 0)));
        assert!(jobs(&app).is_empty());
    }

    #[test]
    fn one_fitting_candidate_links_and_fills_the_total_and_a_tie_waits_for_a_pick() {
        let (_tmp, dir) = open_tmp();
        let mut app = Ryuuji::open(&dir).unwrap();
        signed_in(&mut app);
        app.dispatch(Command::AddEntry(NewEntry {
            total: None,
            ..entry("Show")
        }));
        app.dispatch(Command::AddEntry(NewEntry {
            total: None,
            ..entry("Fruits Basket")
        }));
        let (show_id, fruits) = (app.state().library[0].id, app.state().library[1].id);
        let mut list = FakeList::new(vec![
            show(),
            candidate(1, "Fruits Basket", Some(26)),
            Candidate {
                titles: vec!["Fruits Basket".into()],
                ..candidate(2, "Fruits Basket (2019)", Some(25))
            },
        ]);

        list.answer(&mut app);
        assert_eq!(
            listing(&app, show_id).tie,
            Tie::Linked {
                media: show().media,
                by: LinkedBy::Title
            }
        );
        assert_eq!(app.state().library[0].total, Some(12));
        assert_eq!(listing(&app, fruits).tie, Tie::Unpicked { found: 2 });
        assert_eq!(app.state().library[1].total, None);
        // Both are looked up now, so neither is searched for again.
        assert!(
            jobs(&app)
                .iter()
                .all(|job| matches!(job.task, Task::Reconcile { .. }))
        );
        assert!(app.state().notices.is_empty());
    }

    #[test]
    fn a_search_answer_for_a_show_tied_meanwhile_changes_nothing() {
        let (_tmp, dir) = open_tmp();
        let (mut app, id) = app_with_show(&dir, WatchStatus::Watching, 3);
        let mut list = FakeList::new(vec![show()]);
        // The background search answers after the person has picked.
        let late = list.reports(&app.state().work());
        app.dispatch(Command::LinkShow {
            entry: id,
            service: ANILIST,
            media: media(6, "Show (2019)", Some(12)),
        });
        let before = app.state().clone();
        for report in late {
            app.dispatch(Command::Synced(report));
        }
        assert_eq!(app.state(), &before);
    }

    /// Done predicate 2.
    #[test]
    fn a_recording_raises_the_lists_progress_and_undo_lowers_it() {
        let (_tmp, dir) = open_tmp();
        let (mut app, _) = app_with_show(&dir, WatchStatus::PlanToWatch, 0);
        let mut list = FakeList::new(vec![show()]);
        linked(&mut app, &mut list);
        // Not on the list: it is added when it next changes here.
        assert_eq!(list.row(5), None);

        watch_past_threshold(&mut app, "Show - 01.mkv");
        let RecordOutcome::Recorded(row) = outcome(&app) else {
            panic!("not recorded: {:?}", outcome(&app));
        };
        assert_eq!(jobs(&app).len(), 1);
        list.answer(&mut app);
        assert_eq!(list.row(5), Some(state(ListStatus::Watching, 1, 0)));
        assert!(jobs(&app).is_empty());

        app.dispatch(Command::UndoRecording(row));
        list.answer(&mut app);
        assert_eq!(list.row(5), Some(state(ListStatus::PlanToWatch, 0, 0)));
        assert_eq!(list.writes, 2);
        assert_eq!(stored_updates(&dir), vec![]);
        assert!(jobs(&app).is_empty());
    }

    /// Done predicate 3: the library at 3, AniList at 5, the file episode 6.
    #[test]
    fn an_episode_ahead_of_the_library_records_once_the_list_says_it_is_next() {
        let (_tmp, dir) = open_tmp();
        let (mut app, id) = app_with_show(&dir, WatchStatus::Watching, 3);
        let mut list = FakeList::new(vec![show()]);
        list.holds(5, state(ListStatus::Watching, 3, 0));
        linked(&mut app, &mut list);
        // Watched on elsewhere.
        list.holds(5, state(ListStatus::Watching, 5, 0));

        watch_past_threshold(&mut app, "Show - 06.mkv");
        assert_eq!(outcome(&app), RecordOutcome::Declined(Decline::NotNext));
        assert_eq!(app.state().sync.checks, vec![(ANILIST, id)]);
        let asked = jobs(&app);
        assert_eq!(asked.len(), 1);
        assert_eq!(asked[0].when, When::Now);

        list.answer(&mut app);
        assert_eq!(app.state().sync.checks, vec![]);
        assert_eq!(app.state().library[0].progress, 6);
        assert!(matches!(outcome(&app), RecordOutcome::Recorded(_)));
        assert_eq!(
            stored_updates(&dir),
            vec![(3, 5, ListStatus::Watching, ListStatus::Watching)]
        );
        assert_eq!(stored_writes(&dir), vec![(6..=6, 5, 6)]);
        assert_eq!(list.writes, 0);

        list.answer(&mut app);
        assert_eq!(list.row(5), Some(state(ListStatus::Watching, 6, 0)));
        assert_eq!(list.writes, 1);
        assert!(jobs(&app).is_empty());

        // A file further ahead asks once more. The list has nothing new, so
        // the refusal stands, and playing on does not ask again.
        watch_past_threshold(&mut app, "Show - 09.mkv");
        assert_eq!(app.state().sync.checks, vec![(ANILIST, id)]);
        list.answer(&mut app);
        assert_eq!(app.state().sync.checks, vec![]);
        assert_eq!(outcome(&app), RecordOutcome::Declined(Decline::NotNext));
        assert_eq!(app.state().library[0].progress, 6);
        app.dispatch(Command::Playback(later("Show - 09.mkv", 800)));
        assert_eq!(app.state().sync.checks, vec![]);
        assert_eq!(stored_updates(&dir).len(), 1);
    }

    #[test]
    fn the_check_is_asked_only_for_a_linked_show_and_a_file_ahead() {
        let (_tmp, dir) = open_tmp();
        let (mut app, id) = app_with_show(&dir, WatchStatus::Watching, 4);
        // Not looked up yet: there is no list to read.
        watch_past_threshold(&mut app, "Show - 06.mkv");
        assert_eq!(outcome(&app), RecordOutcome::Declined(Decline::NotNext));
        assert_eq!(app.state().sync.checks, vec![]);

        let mut list = FakeList::new(vec![show()]);
        list.holds(5, state(ListStatus::Watching, 4, 0));
        linked(&mut app, &mut list);
        // Behind the library: no list can make episode 3 the next one.
        watch_past_threshold(&mut app, "Show - 03.mkv");
        assert_eq!(outcome(&app), RecordOutcome::Declined(Decline::NotNext));
        assert_eq!(app.state().sync.checks, vec![]);
        watch_past_threshold(&mut app, "Show - 07.mkv");
        assert_eq!(app.state().sync.checks, vec![(ANILIST, id)]);

        // Signed out, a check is not work.
        app.dispatch(Command::Synced(Report::SignedOut(ANILIST)));
        assert_eq!(app.state().work(), Work { services: vec![] });
    }

    /// Done predicate 4.
    #[test]
    fn a_refused_account_gives_no_work_until_it_signs_in_again() {
        let (_tmp, dir) = open_tmp();
        let (mut app, id) = app_with_show(&dir, WatchStatus::Watching, 3);
        let mut list = FakeList::new(vec![show()]);
        list.holds(5, state(ListStatus::Watching, 3, 0));
        linked(&mut app, &mut list);
        app.dispatch(Command::SetProgress { id, progress: 4 });
        let pending = jobs(&app);
        assert_eq!(pending.len(), 1);

        app.dispatch(Command::Synced(Report::Refused {
            service: ANILIST,
            message: "Invalid token".into(),
        }));
        assert_eq!(app.state().work(), Work { services: vec![] });
        list.answer(&mut app);
        assert_eq!(list.row(5), Some(state(ListStatus::Watching, 3, 0)));

        signed_in(&mut app);
        assert_eq!(jobs(&app), pending);
        list.answer(&mut app);
        assert_eq!(list.row(5), Some(state(ListStatus::Watching, 4, 0)));
        assert!(jobs(&app).is_empty());
    }

    /// Done predicate 5: the write lands, the report does not.
    #[test]
    fn a_reconcile_whose_report_was_lost_is_asked_again_and_converges_once() {
        let (_tmp, dir) = open_tmp();
        let (mut app, id) = app_with_show(&dir, WatchStatus::Watching, 3);
        let mut list = FakeList::new(vec![show()]);
        list.holds(5, state(ListStatus::Watching, 3, 0));
        linked(&mut app, &mut list);
        // A site edit and a local one, then the round whose report is lost.
        list.holds(5, state(ListStatus::OnHold, 3, 0));
        app.dispatch(Command::SetProgress { id, progress: 5 });
        let lost = list.reports(&app.state().work());
        assert_eq!(lost.len(), 1);
        assert_eq!(list.row(5), Some(state(ListStatus::OnHold, 5, 0)));
        assert_eq!(list.writes, 1);
        assert_eq!(jobs(&app).len(), 1);

        list.answer(&mut app);
        assert_eq!(list.writes, 1);
        assert_eq!(app.state().library[0].status, WatchStatus::OnHold);
        assert_eq!(app.state().library[0].progress, 5);
        assert_eq!(
            stored_updates(&dir),
            vec![(5, 5, ListStatus::Watching, ListStatus::OnHold)]
        );
        assert!(jobs(&app).is_empty());
        list.answer(&mut app);
        assert_eq!(stored_updates(&dir).len(), 1);
        assert_eq!(list.writes, 1);
    }

    #[test]
    fn a_report_for_another_user_or_a_show_relinked_since_changes_nothing() {
        let (_tmp, dir) = open_tmp();
        let (mut app, id) = app_with_show(&dir, WatchStatus::Watching, 3);
        let mut list = FakeList::new(vec![show()]);
        list.holds(5, state(ListStatus::Watching, 3, 0));
        linked(&mut app, &mut list);
        app.dispatch(Command::SetProgress { id, progress: 4 });
        let reconciled = |user, media| {
            Command::Synced(Report::Reconciled {
                service: ANILIST,
                user: UserId(user),
                entry: id,
                media: MediaId(media),
                asked: Asked {
                    wanted: state(ListStatus::Watching, 4, 0),
                    base: state(ListStatus::Watching, 3, 0),
                },
                remote: Remote::Listed {
                    id: Some(ListEntryId(1005)),
                    state: state(ListStatus::Watching, 4, 0),
                },
            })
        };

        let before = app.state().clone();
        app.dispatch(reconciled(8, 5));
        assert_eq!(app.state(), &before);

        app.dispatch(Command::LinkShow {
            entry: id,
            service: ANILIST,
            media: media(6, "Show (2019)", Some(12)),
        });
        let relinked = app.state().clone();
        app.dispatch(reconciled(7, 5));
        assert_eq!(app.state(), &relinked);
        assert!(app.state().notices.is_empty());

        // The one that is about this user and this media settles.
        app.dispatch(Command::LinkShow {
            entry: id,
            service: ANILIST,
            media: show().media,
        });
        app.dispatch(reconciled(7, 5));
        assert_eq!(
            listing(&app, id).base.map(|base| base.state),
            Some(state(ListStatus::Watching, 4, 0))
        );
        assert!(jobs(&app).is_empty());
    }

    #[test]
    fn a_rejected_write_parks_the_show_with_one_notice_until_it_is_retried() {
        let (_tmp, dir) = open_tmp();
        let (mut app, id) = app_with_show(&dir, WatchStatus::Watching, 3);
        let mut list = FakeList::new(vec![show()]);
        list.holds(5, state(ListStatus::Watching, 3, 0));
        linked(&mut app, &mut list);
        app.dispatch(Command::SetProgress { id, progress: 4 });
        let rejected = |media, progress| {
            Command::Synced(Report::Rejected {
                service: ANILIST,
                entry: id,
                media: MediaId(media),
                wanted: state(ListStatus::Watching, progress, 0),
                message: "Validation error".into(),
            })
        };
        let notice = Notice::ListRejected {
            service: ANILIST,
            show: "Show".into(),
            message: "Validation error".into(),
        };

        app.dispatch(rejected(5, 4));
        let parked = listing(&app, id).parked.expect("parked");
        assert_eq!(
            (parked.wanted, parked.message.as_str()),
            (state(ListStatus::Watching, 4, 0), "Validation error")
        );
        assert_eq!(app.state().notices, vec![notice.clone()]);
        assert!(jobs(&app).is_empty());
        // Said again only in its place, and a refusal about another media
        // is not this show's.
        app.dispatch(rejected(5, 4));
        app.dispatch(rejected(6, 4));
        assert_eq!(app.state().notices, vec![notice.clone()]);

        // Wanting something else sends it again; back at what was refused,
        // it waits.
        app.dispatch(Command::SetProgress { id, progress: 5 });
        assert_eq!(jobs(&app).len(), 1);
        app.dispatch(Command::SetProgress { id, progress: 4 });
        assert!(jobs(&app).is_empty());

        app.dispatch(Command::RetrySync {
            entry: id,
            service: ANILIST,
        });
        assert_eq!(listing(&app, id).parked, None);
        assert!(app.state().notices.is_empty());
        assert_eq!(jobs(&app).len(), 1);
        list.answer(&mut app);
        assert_eq!(list.row(5), Some(state(ListStatus::Watching, 4, 0)));

        // A settle takes a show out of the park too, and its notice with it.
        app.dispatch(rejected(5, 4));
        assert_eq!(app.state().notices, vec![notice]);
        app.dispatch(Command::SetProgress { id, progress: 5 });
        list.answer(&mut app);
        assert_eq!(listing(&app, id).parked, None);
        assert!(app.state().notices.is_empty());
    }

    /// Parked on media 5's refusal and linked to media 6 by the person: the
    /// park was about 5, so the show is asked for a first merge with 6 and
    /// the notice goes. Linked again to 5 itself, the park stands.
    #[test]
    fn relinking_a_parked_show_to_another_media_unparks_it_and_asks_a_first_merge() {
        let (_tmp, dir) = open_tmp();
        let (mut app, id) = app_with_show(&dir, WatchStatus::Watching, 3);
        let mut list = FakeList::new(vec![show()]);
        list.holds(5, state(ListStatus::Watching, 3, 0));
        linked(&mut app, &mut list);
        app.dispatch(Command::SetProgress { id, progress: 4 });
        app.dispatch(Command::Synced(Report::Rejected {
            service: ANILIST,
            entry: id,
            media: MediaId(5),
            wanted: state(ListStatus::Watching, 4, 0),
            message: "The progress may not be greater than 12.".into(),
        }));
        assert!(listing(&app, id).parked.is_some());
        assert_eq!(app.state().notices.len(), 1);
        assert!(jobs(&app).is_empty());

        app.dispatch(Command::LinkShow {
            entry: id,
            service: ANILIST,
            media: show().media,
        });
        assert!(listing(&app, id).parked.is_some());
        assert_eq!(app.state().notices.len(), 1);
        assert!(jobs(&app).is_empty());

        app.dispatch(Command::LinkShow {
            entry: id,
            service: ANILIST,
            media: media(6, "Show (2019)", Some(26)),
        });
        let relinked = listing(&app, id);
        assert_eq!((relinked.parked, relinked.base), (None, None));
        assert!(app.state().notices.is_empty());
        assert_eq!(
            jobs(&app),
            vec![Job {
                entry: id,
                task: Task::Reconcile {
                    media: MediaId(6),
                    wanted: state(ListStatus::Watching, 4, 0),
                    base: None,
                },
                when: When::Settled,
            }]
        );
        assert_eq!(
            app.state()
                .sync
                .link_state(&app.state().library[0], ANILIST),
            Some(LinkState::Merging {
                show: "Show (2019)".into()
            })
        );
    }

    #[test]
    fn a_media_gone_from_the_service_marks_the_show_and_says_so_once() {
        let (_tmp, dir) = open_tmp();
        let (mut app, id) = app_with_show(&dir, WatchStatus::Watching, 3);
        let mut list = FakeList::new(vec![show()]);
        list.holds(5, state(ListStatus::Watching, 3, 0));
        linked(&mut app, &mut list);
        let gone = |media| {
            Command::Synced(Report::Gone {
                service: ANILIST,
                entry: id,
                media: MediaId(media),
            })
        };
        let notice = Notice::ListGone {
            service: ANILIST,
            show: "Show".into(),
        };

        // A stale report about another media is not this show's.
        app.dispatch(gone(6));
        assert!(matches!(listing(&app, id).tie, Tie::Linked { .. }));
        assert!(app.state().notices.is_empty());

        app.dispatch(gone(5));
        assert_eq!(
            listing(&app, id).tie,
            Tie::Gone {
                media: show().media
            }
        );
        assert_eq!(app.state().notices, vec![notice.clone()]);
        app.dispatch(gone(5));
        assert_eq!(app.state().notices, vec![notice]);
        assert!(jobs(&app).is_empty());
        assert_eq!(
            app.state()
                .sync
                .link_state(&app.state().library[0], ANILIST),
            Some(LinkState::Gone {
                show: "Show".into()
            })
        );

        let picked = media(6, "Show (2019)", Some(12));
        app.dispatch(Command::LinkShow {
            entry: id,
            service: ANILIST,
            media: picked.clone(),
        });
        assert_eq!(
            listing(&app, id).tie,
            Tie::Linked {
                media: picked,
                by: LinkedBy::Person
            }
        );
        assert!(app.state().notices.is_empty());
        // A first merge with the new media.
        assert_eq!(jobs(&app).len(), 1);
    }

    #[test]
    fn the_picker_asks_its_query_at_once_and_links_by_the_persons_pick() {
        let (_tmp, dir) = open_tmp();
        let mut app = Ryuuji::open(&dir).unwrap();
        signed_in(&mut app);
        app.dispatch(Command::AddEntry(NewEntry {
            total: None,
            ..entry("Furuba")
        }));
        let id = app.state().library[0].id;
        let found = vec![
            candidate(1, "Fruits Basket", Some(26)),
            candidate(2, "Fruits Basket (2019)", Some(25)),
        ];
        let mut list = FakeList::new(found.clone());
        list.answer(&mut app);
        assert_eq!(listing(&app, id).tie, Tie::Unpicked { found: 0 });

        let search = || Command::SearchList {
            entry: id,
            service: ANILIST,
            query: "Fruits Basket".into(),
        };
        let picker = |found| {
            Some(Picker {
                entry: id,
                service: ANILIST,
                query: "Fruits Basket".into(),
                found,
            })
        };
        app.dispatch(search());
        assert_eq!(app.state().sync.picker, picker(None));
        assert_eq!(
            jobs(&app),
            vec![Job {
                entry: id,
                task: Task::Find {
                    title: "Fruits Basket".into()
                },
                when: When::Now,
            }]
        );

        list.answer(&mut app);
        assert_eq!(app.state().sync.picker, picker(Some(found.clone())));
        assert_eq!(listing(&app, id).tie, Tie::Unpicked { found: 0 });
        assert!(jobs(&app).is_empty());

        app.dispatch(Command::LinkShow {
            entry: id,
            service: ANILIST,
            media: found[1].media.clone(),
        });
        assert_eq!(app.state().sync.picker, None);
        assert_eq!(
            listing(&app, id).tie,
            Tie::Linked {
                media: found[1].media.clone(),
                by: LinkedBy::Person
            }
        );
        assert_eq!(app.state().library[0].total, Some(25));
        assert_eq!(jobs(&app).len(), 1);

        app.dispatch(search());
        app.dispatch(Command::ClosePicker);
        assert_eq!(app.state().sync.picker, None);
        app.dispatch(search());
        app.dispatch(Command::StopSyncing {
            entry: id,
            service: ANILIST,
        });
        assert_eq!(app.state().sync.picker, None);
        assert_eq!(listing(&app, id).tie, Tie::Off);
        assert!(jobs(&app).is_empty());
    }

    /// The picker opened over a show never looked up, asking the show's own
    /// title: the one answer fills the picker and ties the show, so no
    /// background search follows it to be swallowed by the picker, and an
    /// answer once the picker is filled is not the picker's.
    #[test]
    fn a_picker_asking_the_shows_own_title_fills_and_ties_from_the_one_answer() {
        let (_tmp, dir) = open_tmp();
        let (mut app, id) = app_with_show(&dir, WatchStatus::Watching, 3);
        let mut list = FakeList::new(vec![show()]);
        app.dispatch(Command::SearchList {
            entry: id,
            service: ANILIST,
            query: "Show".into(),
        });
        assert_eq!(jobs(&app).len(), 1);
        list.answer(&mut app);
        let picker = app.state().sync.picker.clone().expect("the picker is open");
        assert_eq!(picker.found, Some(vec![show()]));
        assert_eq!(
            listing(&app, id).tie,
            Tie::Linked {
                media: show().media,
                by: LinkedBy::Title
            }
        );
        assert!(
            jobs(&app)
                .iter()
                .all(|job| matches!(job.task, Task::Reconcile { .. }))
        );

        app.dispatch(Command::Synced(Report::Found {
            service: ANILIST,
            entry: id,
            query: "Show".into(),
            candidates: vec![],
        }));
        assert_eq!(app.state().sync.picker, Some(picker));
        assert!(matches!(listing(&app, id).tie, Tie::Linked { .. }));
    }
}
