//! AniList's GraphQL API as Ryuuji speaks it: the documents, the variables
//! each call sends, what is read from each answer, and how an answer that
//! is not the one asked for is told apart. Every call is a POST of
//! `{"query", "variables"}` to one URL, signed with the person's token.
//! The token itself is a JWT, and its `exp` claim is read here too.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use base64::prelude::BASE64_URL_SAFE_NO_PAD;
use ryuuji_core::{
    Candidate, ListEntryId, ListState, ListStatus, Media, MediaId, Remote, UserId, Write,
};
use serde_json::{Map, Value, json};

use crate::http::{Request, Response};
use crate::service::{Failure, Wire};

pub(crate) const URL: &str = "https://graphql.anilist.co";
/// The pause when a 429 names no `Retry-After`.
const DEFAULT_RETRY: Duration = Duration::from_secs(60);

const VIEWER: &str = "query { Viewer { id name } }";
const FIND: &str = "query ($search: String) { Page(perPage: 10) { media(search: $search, type: ANIME, sort: SEARCH_MATCH) { id idMal title { romaji english native } synonyms episodes format seasonYear } } }";
const READ: &str = "query ($id: Int) { Media(id: $id, type: ANIME) { id episodes mediaListEntry { id status progress repeat } } }";
const SAVE: &str = "mutation ($id: Int, $mediaId: Int, $status: MediaListStatus, $progress: Int, $repeat: Int) { SaveMediaListEntry(id: $id, mediaId: $mediaId, status: $status, progress: $progress, repeat: $repeat) { id status progress repeat } }";

/// AniList holds nothing between calls: the token is the vault's and goes
/// on each request. A service with a refresh token will hold it here.
pub(crate) struct Session;

impl Session {
    pub(crate) fn viewer(&self, wire: Wire<'_>) -> Result<(UserId, String), Failure> {
        let response = wire(&viewer())?;
        parse_viewer(classify(&response)?)
    }

    pub(crate) fn find(&self, wire: Wire<'_>, title: &str) -> Result<Vec<Candidate>, Failure> {
        let response = wire(&find(title))?;
        parse_find(classify(&response)?)
    }

    pub(crate) fn read(&self, wire: Wire<'_>, media: MediaId) -> Result<Remote, Failure> {
        let response = wire(&read(media))?;
        parse_read(classify(&response)?)
    }

    pub(crate) fn save(
        &self,
        wire: Wire<'_>,
        media: MediaId,
        id: Option<ListEntryId>,
        write: Write,
    ) -> Result<Remote, Failure> {
        let response = wire(&save(media, id, write))?;
        parse_save(classify(&response)?)
    }
}

pub(crate) fn viewer() -> Request {
    request(VIEWER, json!({}))
}

pub(crate) fn find(title: &str) -> Request {
    request(FIND, json!({ "search": title }))
}

pub(crate) fn read(media: MediaId) -> Request {
    request(READ, json!({ "id": media.0 }))
}

/// `mediaId` always, `id` when the read returned one, and only the fields
/// the write sets: a field left out of the variables is a field AniList
/// leaves as it is.
pub(crate) fn save(media: MediaId, id: Option<ListEntryId>, write: Write) -> Request {
    let mut variables = Map::new();
    variables.insert("mediaId".into(), media.0.into());
    if let Some(id) = id {
        variables.insert("id".into(), id.0.into());
    }
    let (status, progress, repeat) = match write {
        Write::Create(state) => (Some(state.status), Some(state.progress), Some(state.repeat)),
        Write::Change(patch) => (patch.status, patch.progress, patch.repeat),
    };
    if let Some(status) = status {
        variables.insert("status".into(), status_tag(status).into());
    }
    if let Some(progress) = progress {
        variables.insert("progress".into(), progress.into());
    }
    if let Some(repeat) = repeat {
        variables.insert("repeat".into(), repeat.into());
    }
    request(SAVE, Value::Object(variables))
}

fn request(query: &str, variables: Value) -> Request {
    Request {
        url: URL,
        body: json!({ "query": query, "variables": variables }),
        signed: true,
    }
}

/// The `data` of an answer that is the one asked for, or how it was not.
/// The `errors` array is read whatever the HTTP status, since AniList puts
/// its reasons there: a 200 can carry a validation error, and a 401 or a
/// 404 says which in its message.
pub(crate) fn classify(response: &Response) -> Result<&Value, Failure> {
    let errors: Vec<&Value> = response
        .body
        .as_ref()
        .and_then(|body| body.get("errors"))
        .and_then(Value::as_array)
        .map(|errors| errors.iter().collect())
        .unwrap_or_default();
    let messages: Vec<&str> = errors
        .iter()
        .filter_map(|error| error.get("message")?.as_str())
        .collect();
    let with_status = |code: u64| {
        errors
            .iter()
            .any(|error| error.get("status").and_then(Value::as_u64) == Some(code))
    };
    let message = |fallback: String| {
        errors
            .iter()
            .find_map(|error| validation(error))
            .or_else(|| messages.first().map(|message| (*message).to_owned()))
            .unwrap_or(fallback)
    };
    if response.status == 401 || messages.iter().any(|m| m.contains("Invalid token")) {
        return Err(Failure::Refused(message(
            "AniList refused the token".to_owned(),
        )));
    }
    if response.status == 429 {
        return Err(Failure::Throttled(
            response.received + response.retry_after.unwrap_or(DEFAULT_RETRY),
        ));
    }
    if response.status == 404 || with_status(404) {
        return Err(Failure::NotFound);
    }
    if (400..500).contains(&response.status) {
        return Err(Failure::Rejected(message(format!(
            "AniList answered {}",
            response.status
        ))));
    }
    if response.status >= 500 {
        return Err(Failure::Transient);
    }
    let Some(body) = &response.body else {
        return Err(Failure::Transient);
    };
    match body.get("data") {
        Some(data) if !data.is_null() => Ok(data),
        _ => Err(Failure::Rejected(message(
            "AniList answered with no data".to_owned(),
        ))),
    }
}

/// What a validation error says about each field, as one sentence: the
/// error's own `message` is only "validation".
fn validation(error: &Value) -> Option<String> {
    let fields = error.get("validation")?.as_object()?;
    let sentences: Vec<&str> = fields
        .values()
        .filter_map(Value::as_array)
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    (!sentences.is_empty()).then(|| sentences.join(" "))
}

pub(crate) fn parse_viewer(data: &Value) -> Result<(UserId, String), Failure> {
    let viewer = present(data, "Viewer")?;
    let id = viewer
        .get("id")
        .and_then(Value::as_u64)
        .ok_or_else(|| shape("Viewer.id"))?;
    let name = viewer
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| shape("Viewer.name"))?;
    Ok((UserId(id), name.to_owned()))
}

pub(crate) fn parse_find(data: &Value) -> Result<Vec<Candidate>, Failure> {
    present(data, "Page")?
        .get("media")
        .and_then(Value::as_array)
        .ok_or_else(|| shape("Page.media"))?
        .iter()
        .map(candidate)
        .collect()
}

/// A search hit: its titles are the romaji, English and native titles and
/// the synonyms, whichever AniList has, and the first of those names it.
fn candidate(media: &Value) -> Result<Candidate, Failure> {
    let id = media
        .get("id")
        .and_then(Value::as_u64)
        .ok_or_else(|| shape("media.id"))?;
    let title = media.get("title");
    let named = |key: &str| title?.get(key).and_then(Value::as_str).map(str::to_owned);
    let mut titles: Vec<String> = ["romaji", "english", "native"]
        .into_iter()
        .filter_map(named)
        .collect();
    titles.extend(
        media
            .get("synonyms")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned),
    );
    let shown = titles
        .first()
        .cloned()
        .ok_or_else(|| shape("media.title"))?;
    Ok(Candidate {
        media: Media {
            id: MediaId(id),
            title: shown,
            episodes: media
                .get("episodes")
                .and_then(Value::as_u64)
                .and_then(|n| u32::try_from(n).ok()),
            mal_id: media.get("idMal").and_then(Value::as_u64),
        },
        titles,
        format: media
            .get("format")
            .and_then(Value::as_str)
            .map(str::to_owned),
        year: media
            .get("seasonYear")
            .and_then(Value::as_u64)
            .and_then(|year| u16::try_from(year).ok()),
    })
}

pub(crate) fn parse_read(data: &Value) -> Result<Remote, Failure> {
    match present(data, "Media")?.get("mediaListEntry") {
        None | Some(Value::Null) => Ok(Remote::Unlisted),
        Some(entry) => listed(entry),
    }
}

pub(crate) fn parse_save(data: &Value) -> Result<Remote, Failure> {
    listed(present(data, "SaveMediaListEntry")?)
}

fn listed(entry: &Value) -> Result<Remote, Failure> {
    let id = entry
        .get("id")
        .and_then(Value::as_u64)
        .ok_or_else(|| shape("mediaListEntry.id"))?;
    let status = entry
        .get("status")
        .and_then(Value::as_str)
        .ok_or_else(|| shape("mediaListEntry.status"))?;
    Ok(Remote::Listed {
        id: Some(ListEntryId(id)),
        state: ListState {
            status: status_of(status)?,
            progress: count(entry, "progress")?,
            repeat: count(entry, "repeat")?,
        },
    })
}

/// A count AniList may leave null, which reads as none.
fn count(entry: &Value, key: &str) -> Result<u32, Failure> {
    match entry.get(key) {
        None | Some(Value::Null) => Ok(0),
        Some(value) => value
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .ok_or_else(|| shape(&format!("mediaListEntry.{key}"))),
    }
}

/// A field of `data` that is there and not null.
fn present<'a>(data: &'a Value, field: &str) -> Result<&'a Value, Failure> {
    data.get(field)
        .filter(|value| !value.is_null())
        .ok_or_else(|| shape(field))
}

fn shape(field: &str) -> Failure {
    Failure::Rejected(format!("AniList answered without {field}"))
}

fn status_tag(status: ListStatus) -> &'static str {
    match status {
        ListStatus::Watching => "CURRENT",
        ListStatus::Rewatching => "REPEATING",
        ListStatus::Completed => "COMPLETED",
        ListStatus::OnHold => "PAUSED",
        ListStatus::Dropped => "DROPPED",
        ListStatus::PlanToWatch => "PLANNING",
    }
}

fn status_of(tag: &str) -> Result<ListStatus, Failure> {
    ListStatus::ALL
        .into_iter()
        .find(|status| status_tag(*status) == tag)
        .ok_or_else(|| Failure::Rejected(format!("AniList answered with the status {tag}")))
}

/// The payload of a JWT, decoded and not verified: the signature is the
/// service's to check, and nothing here decides on a claim beyond a date.
pub(crate) fn jwt_payload(token: &str) -> Option<Value> {
    let payload = token.split('.').nth(1)?;
    let bytes = BASE64_URL_SAFE_NO_PAD.decode(payload).ok()?;
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    value.is_object().then_some(value)
}

/// When the token stops working, from its `exp` claim.
pub(crate) fn expiry(token: &str) -> Option<SystemTime> {
    let exp = jwt_payload(token)?.get("exp")?.as_u64()?;
    Some(UNIX_EPOCH + Duration::from_secs(exp))
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use ryuuji_core::Patch;

    use super::*;

    const VIEWER_OK: &str = include_str!("../fixtures/anilist/viewer.json");
    const SEARCH: &str = include_str!("../fixtures/anilist/search.json");
    const READ_LISTED: &str = include_str!("../fixtures/anilist/read_listed.json");
    const READ_UNLISTED: &str = include_str!("../fixtures/anilist/read_unlisted.json");
    const SAVED: &str = include_str!("../fixtures/anilist/save.json");
    const UNAUTHORIZED: &str = include_str!("../fixtures/anilist/unauthorized_401.json");
    const INVALID_TOKEN: &str = include_str!("../fixtures/anilist/invalid_token_200.json");
    const THROTTLED: &str = include_str!("../fixtures/anilist/throttled_429.json");
    const NOT_FOUND: &str = include_str!("../fixtures/anilist/not_found_404.json");
    const VALIDATION: &str = include_str!("../fixtures/anilist/validation_200.json");
    const SERVER_ERROR: &str = include_str!("../fixtures/anilist/server_error_500.json");

    fn state(status: ListStatus, progress: u32, repeat: u32) -> ListState {
        ListState {
            status,
            progress,
            repeat,
        }
    }

    fn data(text: &str) -> Value {
        classify(&Response::fixture(text)).unwrap().clone()
    }

    #[test]
    fn every_document_goes_out_as_a_post_of_query_and_variables() {
        assert_eq!(
            viewer().body.to_string(),
            r#"{"query":"query { Viewer { id name } }","variables":{}}"#
        );
        assert_eq!(
            find("Sousou no Frieren").body.to_string(),
            r#"{"query":"query ($search: String) { Page(perPage: 10) { media(search: $search, type: ANIME, sort: SEARCH_MATCH) { id idMal title { romaji english native } synonyms episodes format seasonYear } } }","variables":{"search":"Sousou no Frieren"}}"#
        );
        assert_eq!(
            read(MediaId(154587)).body.to_string(),
            r#"{"query":"query ($id: Int) { Media(id: $id, type: ANIME) { id episodes mediaListEntry { id status progress repeat } } }","variables":{"id":154587}}"#
        );
        for request in [viewer(), find("x"), read(MediaId(1))] {
            assert_eq!(request.url, "https://graphql.anilist.co");
            assert!(request.signed);
        }
    }

    #[test]
    fn a_save_sends_only_the_fields_the_write_sets_and_the_id_the_read_returned() {
        let mutation = r#""query":"mutation ($id: Int, $mediaId: Int, $status: MediaListStatus, $progress: Int, $repeat: Int) { SaveMediaListEntry(id: $id, mediaId: $mediaId, status: $status, progress: $progress, repeat: $repeat) { id status progress repeat } }""#;
        let progress_only = save(
            MediaId(154587),
            Some(ListEntryId(381_234_567)),
            Write::Change(Patch {
                progress: Some(4),
                ..Patch::default()
            }),
        );
        assert_eq!(
            progress_only.body.to_string(),
            format!(
                r#"{{{mutation},"variables":{{"id":381234567,"mediaId":154587,"progress":4}}}}"#
            )
        );
        let status_and_repeat = save(
            MediaId(154587),
            Some(ListEntryId(381_234_567)),
            Write::Change(Patch {
                status: Some(ListStatus::Completed),
                progress: None,
                repeat: Some(2),
            }),
        );
        assert_eq!(
            status_and_repeat.body.to_string(),
            format!(
                r#"{{{mutation},"variables":{{"id":381234567,"mediaId":154587,"repeat":2,"status":"COMPLETED"}}}}"#
            )
        );
        let created = save(
            MediaId(154587),
            None,
            Write::Create(state(ListStatus::Watching, 1, 0)),
        );
        assert_eq!(
            created.body.to_string(),
            format!(
                r#"{{{mutation},"variables":{{"mediaId":154587,"progress":1,"repeat":0,"status":"CURRENT"}}}}"#
            )
        );
        assert!(created.signed);
    }

    #[test]
    fn the_viewer_is_the_user_and_their_name() {
        assert_eq!(
            parse_viewer(&data(VIEWER_OK)),
            Ok((UserId(7), "someone".to_owned()))
        );
        assert_eq!(
            parse_viewer(&json!({ "Viewer": null })),
            Err(shape("Viewer"))
        );
        assert_eq!(
            parse_viewer(&json!({ "Viewer": { "id": 1 } })),
            Err(shape("Viewer.name"))
        );
    }

    #[test]
    fn a_search_reads_every_title_and_leaves_out_what_is_null() {
        let found = parse_find(&data(SEARCH)).unwrap();
        assert_eq!(found.len(), 3);
        assert_eq!(
            found[0],
            Candidate {
                media: Media {
                    id: MediaId(154587),
                    title: "Sousou no Frieren".into(),
                    episodes: Some(28),
                    mal_id: Some(52991),
                },
                titles: vec![
                    "Sousou no Frieren".into(),
                    "Frieren: Beyond Journey's End".into(),
                    "葬送のフリーレン".into(),
                    "Frieren at the Funeral".into(),
                    "Frieren".into(),
                ],
                format: Some("TV".into()),
                year: Some(2023),
            }
        );
        // No English title, no synonyms, and an airing show with no count.
        assert_eq!(
            found[1],
            Candidate {
                media: Media {
                    id: MediaId(182255),
                    title: "Sousou no Frieren 2nd Season".into(),
                    episodes: None,
                    mal_id: None,
                },
                titles: vec![
                    "Sousou no Frieren 2nd Season".into(),
                    "葬送のフリーレン 第2期".into()
                ],
                format: Some("TV".into()),
                year: None,
            }
        );
        // Only a native title, and no format.
        assert_eq!(found[2].media.title, "フリーレン");
        assert_eq!(found[2].titles, vec!["フリーレン".to_owned()]);
        assert_eq!(found[2].format, None);
        assert_eq!(parse_find(&json!({ "Page": { "media": [] } })), Ok(vec![]));
        assert_eq!(
            parse_find(&json!({ "Page": { "media": [{ "id": 1, "title": {} }] } })),
            Err(shape("media.title"))
        );
    }

    #[test]
    fn a_read_is_listed_with_its_row_or_unlisted() {
        assert_eq!(
            parse_read(&data(READ_LISTED)),
            Ok(Remote::Listed {
                id: Some(ListEntryId(381_234_567)),
                state: state(ListStatus::Watching, 3, 0),
            })
        );
        assert_eq!(parse_read(&data(READ_UNLISTED)), Ok(Remote::Unlisted));
        assert_eq!(parse_read(&json!({ "Media": null })), Err(shape("Media")));
        // Counts AniList leaves null read as none.
        let nulls = json!({ "Media": { "id": 1, "mediaListEntry": { "id": 9, "status": "PLANNING", "progress": null } } });
        assert_eq!(
            parse_read(&nulls),
            Ok(Remote::Listed {
                id: Some(ListEntryId(9)),
                state: state(ListStatus::PlanToWatch, 0, 0),
            })
        );
    }

    #[test]
    fn a_save_answer_is_the_row_as_saved() {
        assert_eq!(
            parse_save(&data(SAVED)),
            Ok(Remote::Listed {
                id: Some(ListEntryId(381_234_567)),
                state: state(ListStatus::Watching, 4, 0),
            })
        );
        assert_eq!(
            parse_save(&json!({ "SaveMediaListEntry": null })),
            Err(shape("SaveMediaListEntry"))
        );
    }

    #[test]
    fn every_status_maps_both_ways_and_an_unknown_one_is_rejected_by_name() {
        for (status, tag) in [
            (ListStatus::Watching, "CURRENT"),
            (ListStatus::Rewatching, "REPEATING"),
            (ListStatus::Completed, "COMPLETED"),
            (ListStatus::OnHold, "PAUSED"),
            (ListStatus::Dropped, "DROPPED"),
            (ListStatus::PlanToWatch, "PLANNING"),
        ] {
            assert_eq!(status_tag(status), tag);
            assert_eq!(status_of(tag), Ok(status));
        }
        assert_eq!(ListStatus::ALL.len(), 6);
        assert_eq!(
            status_of("WATCHING"),
            Err(Failure::Rejected(
                "AniList answered with the status WATCHING".into()
            ))
        );
        let unknown = json!({ "SaveMediaListEntry": { "id": 1, "status": "BINGED", "progress": 1, "repeat": 0 } });
        assert!(matches!(parse_save(&unknown), Err(Failure::Rejected(m)) if m.contains("BINGED")));
    }

    #[test]
    fn every_fixture_classifies_by_its_status_and_its_errors() {
        for text in [VIEWER_OK, SEARCH, READ_LISTED, READ_UNLISTED, SAVED] {
            let response = Response::fixture(text);
            assert!(classify(&response).is_ok(), "{text}");
            assert_eq!(response.rate_remaining, Some(89));
        }
        assert_eq!(
            classify(&Response::fixture(UNAUTHORIZED)),
            Err(Failure::Refused("Unauthorized.".into()))
        );
        assert_eq!(
            classify(&Response::fixture(INVALID_TOKEN)),
            Err(Failure::Refused("Invalid token".into()))
        );
        let throttled = Response::fixture(THROTTLED);
        assert_eq!(throttled.rate_remaining, Some(0));
        assert_eq!(
            classify(&throttled),
            Err(Failure::Throttled(
                throttled.received + Duration::from_secs(30)
            ))
        );
        assert_eq!(
            classify(&Response::fixture(NOT_FOUND)),
            Err(Failure::NotFound)
        );
        assert_eq!(
            classify(&Response::fixture(VALIDATION)),
            Err(Failure::Rejected(
                "The progress may not be greater than 28.".into()
            ))
        );
        assert_eq!(
            classify(&Response::fixture(SERVER_ERROR)),
            Err(Failure::Transient)
        );
    }

    #[test]
    fn the_signals_are_read_whatever_the_status_carries_them() {
        let received = Instant::now();
        let response = |status: u16, body: Option<Value>| Response {
            status,
            retry_after: None,
            rate_remaining: None,
            body,
            received,
        };
        // A 429 with no Retry-After waits the default minute.
        assert_eq!(
            classify(&response(429, None)),
            Err(Failure::Throttled(received + DEFAULT_RETRY))
        );
        // A 401 with an unreadable body is still a refusal.
        assert_eq!(
            classify(&response(401, None)),
            Err(Failure::Refused("AniList refused the token".into()))
        );
        // A 404 said only in the errors array is still not found.
        let gone = json!({ "data": { "Media": null }, "errors": [{ "message": "Not Found.", "status": 404 }] });
        assert_eq!(classify(&response(200, Some(gone))), Err(Failure::NotFound));
        // Another 4xx is a rejection with its message, or its status.
        let bad = json!({ "data": null, "errors": [{ "message": "Cannot query field \"foo\"", "status": 400 }] });
        assert_eq!(
            classify(&response(400, Some(bad))),
            Err(Failure::Rejected("Cannot query field \"foo\"".into()))
        );
        assert_eq!(
            classify(&response(403, None)),
            Err(Failure::Rejected("AniList answered 403".into()))
        );
        // A 200 whose body is not JSON, or has no data, cannot be read.
        assert_eq!(classify(&response(200, None)), Err(Failure::Transient));
        assert_eq!(
            classify(&response(200, Some(json!({ "data": null })))),
            Err(Failure::Rejected("AniList answered with no data".into()))
        );
        assert_eq!(classify(&response(503, None)), Err(Failure::Transient));
        // Errors beside data still hand the data over.
        let partial = json!({ "data": { "Viewer": { "id": 1, "name": "a" } }, "errors": [{ "message": "deprecated" }] });
        assert_eq!(
            classify(&response(200, Some(partial))).map(|data| data["Viewer"]["id"].clone()),
            Ok(json!(1))
        );
    }

    #[test]
    fn the_jwt_payload_is_read_unverified_and_the_expiry_from_it() {
        let token = crate::tests::jwt(r#"{"exp":1790121600,"sub":"7"}"#);
        assert_eq!(
            jwt_payload(&token).unwrap()["sub"],
            Value::String("7".into())
        );
        assert_eq!(
            expiry(&token),
            Some(UNIX_EPOCH + Duration::from_secs(1_790_121_600))
        );
        assert_eq!(expiry(&crate::tests::jwt(r#"{"sub":"1"}"#)), None);
        assert_eq!(jwt_payload("no dots"), None);
        assert_eq!(jwt_payload("a.!!!.c"), None);
        assert_eq!(expiry("a.bm90IGpzb24.c"), None);
    }
}
