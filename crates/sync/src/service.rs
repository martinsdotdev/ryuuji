//! The services as one type. Exactly one exists today, and a trait with one
//! implementation would hide nothing, so each call is a closed `match`:
//! MyAnimeList and Kitsu each add a variant, a module and an arm, and the
//! compiler names every call they have to answer. How a call can fail is
//! also settled here, since the engine and the reports need the kinds told
//! apart the same way for every service.

use std::fmt;
use std::time::Instant;

use ryuuji_core::{Candidate, ListEntryId, MediaId, Remote, Service, ShowInfo, UserId, Write};

use crate::anilist;
use crate::http::{Request, Response};

/// What a call goes out through: the worker's transport with the token
/// looked up, or a script in a test.
pub(crate) type Wire<'a> = &'a mut dyn FnMut(&Request) -> Result<Response, Failure>;

pub(crate) enum Session {
    AniList(anilist::Session),
}

impl Session {
    pub(crate) fn of(service: Service) -> Session {
        match service {
            Service::AniList => Session::AniList(anilist::Session),
        }
    }

    /// Whose token this is.
    pub(crate) fn viewer(&self, wire: Wire<'_>) -> Result<(UserId, String), Failure> {
        match self {
            Session::AniList(session) => session.viewer(wire),
        }
    }

    /// The shows a title turns up.
    pub(crate) fn find(&self, wire: Wire<'_>, title: &str) -> Result<Vec<Candidate>, Failure> {
        match self {
            Session::AniList(session) => session.find(wire, title),
        }
    }

    /// What the person's list holds for a show.
    pub(crate) fn read(&self, wire: Wire<'_>, media: MediaId) -> Result<Remote, Failure> {
        match self {
            Session::AniList(session) => session.read(wire, media),
        }
    }

    /// What the service says about a show, and its cover's address.
    pub(crate) fn describe(
        &self,
        wire: Wire<'_>,
        media: MediaId,
    ) -> Result<(ShowInfo, Option<String>), Failure> {
        match self {
            Session::AniList(session) => session.describe(wire, media),
        }
    }

    /// Writes to the list row, `id` where the read returned one, and
    /// returns the row as the service then holds it.
    pub(crate) fn save(
        &self,
        wire: Wire<'_>,
        media: MediaId,
        id: Option<ListEntryId>,
        write: Write,
    ) -> Result<Remote, Failure> {
        match self {
            Session::AniList(session) => session.save(wire, media, id, write),
        }
    }
}

/// How a call failed, told apart by what follows from it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Failure {
    /// The token does not work; the lane stops until a new one is signed in.
    Refused(String),
    /// The service asked for a pause until then.
    Throttled(Instant),
    /// The media is no longer on the service.
    NotFound,
    /// The service would not take the write, or answered in a shape that
    /// could not be read; the show is parked with the message.
    Rejected(String),
    /// The service had a problem of its own; the job backs off.
    Transient,
    /// The service could not be reached; the lane pauses.
    Offline,
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Failure::Refused(message) | Failure::Rejected(message) => f.write_str(message),
            Failure::Throttled(_) => {
                f.write_str("the service asked for a pause before the next request")
            }
            Failure::NotFound => f.write_str("the show is no longer on the service"),
            Failure::Transient => f.write_str("the service had a problem answering"),
            Failure::Offline => f.write_str("the service could not be reached"),
        }
    }
}
