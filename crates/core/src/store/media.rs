//! The `media` table: what a service says about a show beyond pairing, one
//! row per service and media, replaced in place each time it is fetched
//! again. Titles and genres are kept one per line.

use rusqlite::{OptionalExtension, Row, params};
use tracing::{info, info_span};

use super::{Store, StoreError, query_failed, unix_now, unix_time};
use crate::{Airing, DescribedAt, Format, MediaId, Season, Service, ShowInfo};

const SELECT_MEDIA: &str = "SELECT titles, format, season, season_year, status, episodes, \
                            genres, studio, description, cover_file FROM media";

/// A show's info as stored: what the service said, and the cover file's
/// name under the covers folder when one was kept.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredInfo {
    pub info: ShowInfo,
    pub cover_file: Option<String>,
}

impl Store {
    /// Keeps what a service said about `media`. A fetch that brought no
    /// cover keeps the file the last one did, so a failed download never
    /// takes a picture away.
    pub fn describe(
        &mut self,
        service: Service,
        media: MediaId,
        info: &ShowInfo,
        cover_file: Option<&str>,
    ) -> Result<DescribedAt, StoreError> {
        let _span =
            info_span!("store.describe", service = service.tag(), media = media.0).entered();
        let at = unix_now();
        self.conn
            .execute(
                "INSERT INTO media (service, media_id, titles, format, season, season_year, \
                 status, episodes, genres, studio, description, cover_file, described_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13) \
                 ON CONFLICT(service, media_id) DO UPDATE SET titles = excluded.titles, \
                 format = excluded.format, season = excluded.season, \
                 season_year = excluded.season_year, status = excluded.status, \
                 episodes = excluded.episodes, genres = excluded.genres, \
                 studio = excluded.studio, description = excluded.description, \
                 cover_file = COALESCE(excluded.cover_file, media.cover_file), \
                 described_at = excluded.described_at",
                params![
                    service.tag(),
                    i64::try_from(media.0).unwrap_or(i64::MAX),
                    info.titles.join("\n"),
                    info.format.map(Format::tag),
                    info.season.map(Season::tag),
                    info.season_year,
                    info.status.map(Airing::tag),
                    info.episodes,
                    info.genres.join("\n"),
                    info.studio,
                    info.description,
                    cover_file,
                    at,
                ],
            )
            .map_err(query_failed)?;
        info!("show info kept");
        Ok(DescribedAt {
            service,
            media,
            at: unix_time(at).unwrap_or(std::time::UNIX_EPOCH),
        })
    }

    /// When each show was last described, which is all the app state keeps
    /// of it. A row whose service or stamp cannot be read is left out, so it
    /// is fetched again.
    pub fn described(&self) -> Result<Vec<DescribedAt>, StoreError> {
        let _span = info_span!("store.described").entered();
        let mut stmt = self
            .conn
            .prepare("SELECT service, media_id, described_at FROM media ORDER BY service, media_id")
            .map_err(query_failed)?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })
            .map_err(query_failed)?;
        let mut described = Vec::new();
        for row in rows {
            let (service, media, at) = row.map_err(query_failed)?;
            let (Some(service), Ok(media), Some(at)) = (
                Service::from_tag(&service),
                u64::try_from(media),
                unix_time(at),
            ) else {
                continue;
            };
            described.push(DescribedAt {
                service,
                media: MediaId(media),
                at,
            });
        }
        Ok(described)
    }

    /// What was kept about `media`, if anything was.
    pub fn show_info(
        &self,
        service: Service,
        media: MediaId,
    ) -> Result<Option<StoredInfo>, StoreError> {
        let _span =
            info_span!("store.show_info", service = service.tag(), media = media.0).entered();
        self.conn
            .query_row(
                &format!("{SELECT_MEDIA} WHERE service = ?1 AND media_id = ?2"),
                params![service.tag(), i64::try_from(media.0).unwrap_or(i64::MAX)],
                read_info,
            )
            .optional()
            .map_err(query_failed)
    }
}

fn lines(text: String) -> Vec<String> {
    text.lines()
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

/// A tag read back, or `None` for one this build does not know.
fn tagged<T>(tag: Option<String>, from_tag: fn(&str) -> Option<T>) -> Option<T> {
    tag.as_deref().and_then(from_tag)
}

fn read_info(row: &Row<'_>) -> rusqlite::Result<StoredInfo> {
    Ok(StoredInfo {
        info: ShowInfo {
            titles: lines(row.get(0)?),
            format: tagged(row.get(1)?, Format::from_tag),
            season: tagged(row.get(2)?, Season::from_tag),
            season_year: row.get(3)?,
            status: tagged(row.get(4)?, Airing::from_tag),
            episodes: row.get(5)?,
            genres: lines(row.get(6)?),
            studio: row.get(7)?,
            description: row.get(8)?,
        },
        cover_file: row.get(9)?,
    })
}

#[cfg(test)]
mod tests {
    use super::super::tests::{open, open_tmp};
    use super::*;

    fn frieren() -> ShowInfo {
        ShowInfo {
            titles: vec![
                "Sousou no Frieren".to_owned(),
                "Frieren: Beyond Journey's End".to_owned(),
                "葬送のフリーレン".to_owned(),
            ],
            format: Some(Format::Tv),
            season: Some(Season::Fall),
            season_year: Some(2023),
            status: Some(Airing::Finished),
            episodes: Some(28),
            genres: vec!["Adventure".to_owned(), "Drama".to_owned()],
            studio: Some("Madhouse".to_owned()),
            description: Some("After the party that defeated the Demon King disbands.".to_owned()),
        }
    }

    #[test]
    fn show_info_is_kept_and_read_back_whole() {
        let (_tmp, dir) = open_tmp();
        let mut store = open(&dir);
        assert_eq!(
            store.show_info(Service::AniList, MediaId(154_587)).unwrap(),
            None
        );
        let at = store
            .describe(
                Service::AniList,
                MediaId(154_587),
                &frieren(),
                Some("anilist-154587.jpg"),
            )
            .unwrap();
        let kept = store
            .show_info(Service::AniList, MediaId(154_587))
            .unwrap()
            .unwrap();
        assert_eq!(kept.info, frieren());
        assert_eq!(kept.cover_file.as_deref(), Some("anilist-154587.jpg"));
        assert_eq!(store.described().unwrap(), vec![at]);
    }

    #[test]
    fn a_fetch_without_a_cover_keeps_the_last_one() {
        let (_tmp, dir) = open_tmp();
        let mut store = open(&dir);
        store
            .describe(
                Service::AniList,
                MediaId(1),
                &frieren(),
                Some("anilist-1.jpg"),
            )
            .unwrap();
        let later = ShowInfo {
            episodes: Some(29),
            ..frieren()
        };
        store
            .describe(Service::AniList, MediaId(1), &later, None)
            .unwrap();
        let kept = store
            .show_info(Service::AniList, MediaId(1))
            .unwrap()
            .unwrap();
        assert_eq!(kept.info.episodes, Some(29));
        assert_eq!(kept.cover_file.as_deref(), Some("anilist-1.jpg"));
        assert_eq!(store.described().unwrap().len(), 1);
    }
}
