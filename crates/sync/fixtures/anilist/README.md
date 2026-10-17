# AniList fixtures

Every file here but one is a real exchange with AniList, captured on
2026-10-17 by the probe on Probe AniList's list writes, batched reads and
errors (RYU-108). Each request was the document in
`crates/sync/src/anilist.rs`, sent as the throwaway account RyuujiApp (user
7749611). AniList's rate limit was 30 a minute at the time, not the 90 its
documentation names. The probe script was not committed. The ticket records
each request and what came back.

Each file is one answer as the wire sees it:

- `status`: the HTTP status.
- `headers`: the headers the wire reads (`Retry-After`,
  `X-RateLimit-Remaining`) plus `X-RateLimit-Limit` for the record. Names
  match without regard to case.
- `body`: the JSON body, or a string holding the raw text of a body that was
  not JSON.

| File | Answers | Origin |
| --- | --- | --- |
| `viewer.json` | `Viewer { id name }` | captured |
| `search.json` | `Page.media` for "Sousou no Frieren": six hits, with null English titles and MAL ids, an unknown episode count and synonyms in many scripts | captured |
| `read_listed.json` | `Media.mediaListEntry` present | captured |
| `read_unlisted.json` | `Media.mediaListEntry` null, for a show with no episode count | captured |
| `save.json` | `SaveMediaListEntry` after a progress write | captured |
| `unauthorized_401.json` | HTTP 401: a call that needs the token, sent without one | captured |
| `invalid_token_400.json` | HTTP 400 with `Invalid token` and `data` null: a token AniList does not accept | captured |
| `throttled_429.json` | HTTP 429 with `Retry-After` and a spent budget | captured |
| `not_found_404.json` | HTTP 404 with `Media` null and an error with `status` 404 | captured |
| `validation_400.json` | HTTP 400 with a `validation` map: a save with a negative progress | captured |
| `server_error_500.json` | HTTP 500 with an HTML body | made up; AniList could not be made to answer one |
| `describe.json` | `Media` with titles, season, status, genres, studio, a description and the cover's address | captured; the synopsis's words are replaced, since they are not ours to copy, and its `<br>` breaks and source line are as captured |
