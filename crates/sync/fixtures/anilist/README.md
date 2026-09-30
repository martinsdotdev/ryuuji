# AniList fixtures

Every file here was written by hand from AniList's GraphQL schema, not
captured from the service. The shapes are what the schema promises for the
documents in `crates/sync/src/anilist.rs`; the ids, names and counts are
made up. The probe ticket replaces each file with a capture of a real
exchange, and any parser these fixtures pinned wrongly is fixed then.

Each file is one answer as the wire sees it:

- `status`: the HTTP status.
- `headers`: the headers the wire reads (`Retry-After`,
  `X-RateLimit-Remaining`) plus `X-RateLimit-Limit` for the record. Names
  match without regard to case.
- `body`: the JSON body, or a string holding the raw text of a body that was
  not JSON.

| File | Answers | Origin |
| --- | --- | --- |
| `viewer.json` | `Viewer { id name }` | schema |
| `search.json` | `Page.media` with a full hit, one with null English title, synonyms and counts, and one with only a native title | schema |
| `read_listed.json` | `Media.mediaListEntry` present | schema |
| `read_unlisted.json` | `Media.mediaListEntry` null | schema |
| `save.json` | `SaveMediaListEntry` after a progress write | schema |
| `unauthorized_401.json` | HTTP 401 with an `errors` entry | schema; the message is a guess |
| `invalid_token_200.json` | HTTP 200 carrying `Invalid token` in `errors` | schema; the shape AniList is reported to use for a bad token |
| `throttled_429.json` | HTTP 429 with `Retry-After` and a spent budget | AniList's rate-limit documentation |
| `not_found_404.json` | HTTP 404 with `Media` null and an error with `status` 404 | schema |
| `validation_200.json` | HTTP 200 with `data` null and a `validation` map in the error | schema; the field text is a guess |
| `server_error_500.json` | HTTP 500 with an HTML body | made up |
| `describe.json` | `Media` with titles, season, status, genres, studio, a description carrying `<br>` and `<i>`, and the cover's address | schema; the description and address are made up |
