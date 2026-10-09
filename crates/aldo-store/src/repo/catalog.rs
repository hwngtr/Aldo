//! Release identity cache, plus the permanent caches for Discogs responses and
//! resolved torrent metainfo.
//!
//! The Discogs cache is the reason this table is not optional: `/database/search`
//! needs a token and allows 60 requests per minute, so 200,000 releases is
//! 55 hours of API time. A release is fetched once and never again.

use aldo_core::audio::{ByteSize, DurationMs};
use aldo_core::ids::{
    ArtistId, DiscogsArtistId, DiscogsReleaseId, LabelId, MusicBrainzId, ReleaseId, ReleaseTrackId,
};
use aldo_core::release::{
    ArtistCredit, ArtistRef, MediaFormat, ReleaseCredit, ReleaseIdentifier, ReleaseIdentity,
    ReleaseTrack, ReleaseType, TrackKind, TrackPosition,
};
use sqlx::Row;
use sqlx::SqlitePool;

use crate::error::{
    StoreError, bool_from_stored, bytes_from_stored, millis_from_stored, narrow_id, narrow_u8,
    narrow_u16, narrow_u32, now_ms, widen_bytes, widen_millis,
};

/// A torrent file listing held in the permanent metainfo cache.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedMetainfo {
    /// The bencoded `info` dictionary, exactly as it was resolved.
    pub payload: Vec<u8>,
    /// The torrent's display name.
    pub name: String,
    pub total_bytes: ByteSize,
    pub flac_count: u32,
}

/// A release with only its most useful fields, for populating a result list.
#[derive(Debug, Clone)]
pub struct CatalogRow {
    pub release_id: ReleaseId,
    pub discogs_release_id: DiscogsReleaseId,
    pub title: String,
    pub artist: Option<String>,
    pub year: Option<u16>,
    pub release_types: Vec<ReleaseType>,
    pub track_count: u16,
    pub cover_art_url: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CatalogStore {
    pool: SqlitePool,
}

impl CatalogStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn upsert_release(
        &self,
        release: &ReleaseIdentity,
        now_ms: i64,
    ) -> Result<ReleaseId, StoreError> {
        sqlx::query(
            "INSERT INTO release
                (discogs_release_id, discogs_master_id, musicbrainz_release_id, title,
                 release_types, media, status, year, country, disc_count, duration_ms,
                 cover_art_url, popularity, genres, styles, fetched_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT (discogs_release_id) DO UPDATE SET
                 discogs_master_id = excluded.discogs_master_id,
                 musicbrainz_release_id = excluded.musicbrainz_release_id,
                 title = excluded.title,
                 release_types = excluded.release_types,
                 media = excluded.media,
                 status = excluded.status,
                 year = excluded.year,
                 country = excluded.country,
                 disc_count = excluded.disc_count,
                 duration_ms = excluded.duration_ms,
                 cover_art_url = excluded.cover_art_url,
                 popularity = excluded.popularity,
                 genres = excluded.genres,
                 styles = excluded.styles,
                 fetched_at = excluded.fetched_at",
        )
        .bind(i64::from(release.discogs_release_id))
        .bind(release.discogs_master_id.map(i64::from))
        .bind(
            release
                .musicbrainz_release_id
                .as_ref()
                .map(MusicBrainzId::as_str),
        )
        .bind(&release.title)
        .bind(
            release
                .release_types
                .iter()
                .map(|t| t.as_vorbis_value())
                .collect::<Vec<_>>()
                .join(";"),
        )
        .bind(release.media.map(MediaFormat::as_stored_str))
        .bind(release.status.as_stored_str())
        .bind(release.year.map(i64::from))
        .bind(release.country.as_deref())
        .bind(i32::from(release.disc_count))
        .bind(
            release
                .duration
                .map(DurationMs::as_millis)
                .map(|ms| widen_millis(ms, "release.duration_ms"))
                .transpose()?,
        )
        .bind(release.cover_art_url.as_deref())
        .bind(release.popularity.map(i64::from))
        .bind(release.genres.join(";"))
        .bind(release.styles.join(";"))
        .bind(now_ms)
        .execute(&self.pool)
        .await?;

        let raw: i64 =
            sqlx::query_scalar("SELECT release_id FROM release WHERE discogs_release_id = ?")
                .bind(i64::from(release.discogs_release_id))
                .fetch_one(&self.pool)
                .await?;

        let release_id = narrow_id::<ReleaseId>(raw, "release_id")?;

        self.replace_tracklist(release_id, &release.tracklist)
            .await?;
        // Artists first: credits reference them, and the tracklist must exist
        // before a credit can resolve its scope to a local track.
        self.replace_artists(release_id, &release.artists).await?;
        self.replace_credits(release_id, &release.credits).await?;
        self.replace_identifiers(release_id, &release.identifiers)
            .await?;

        Ok(release_id)
    }

    /// Replaces the tracklist wholesale. A re-fetch of a release is authoritative,
    /// so merging would leave stale tracks behind.
    async fn replace_tracklist(
        &self,
        release_id: ReleaseId,
        tracks: &[ReleaseTrack],
    ) -> Result<(), StoreError> {
        sqlx::query("DELETE FROM release_track WHERE release_id = ?")
            .bind(release_id.get())
            .execute(&self.pool)
            .await?;

        // Inserted in two passes: a track may point at an index entry that is
        // part of the same batch.
        for track in tracks {
            sqlx::query(
                "INSERT INTO release_track
                    (release_id, position_raw, disc_number, track_number, title, kind, duration_ms)
                 VALUES (?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(release_id.get())
            .bind(position_raw(&track.position))
            .bind(i32::from(track.position.disc_number()))
            .bind(track.position.track_number().map(i32::from))
            .bind(&track.title)
            .bind(track.kind.as_stored_str())
            .bind(
                track
                    .duration
                    .map(DurationMs::as_millis)
                    .map(|ms| widen_millis(ms, "release_track.duration_ms"))
                    .transpose()?,
            )
            .execute(&self.pool)
            .await?;
        }

        for (track, parent_position) in tracks
            .iter()
            .filter_map(|track| Some((track, track.parent_position.as_deref()?)))
        {
            let raw_self = self
                .track_id_at(release_id, &position_raw(&track.position))
                .await?;
            let raw_parent = self.track_id_at(release_id, parent_position).await?;

            sqlx::query("UPDATE release_track SET parent_track_id = ? WHERE track_id = ?")
                .bind(raw_parent.get())
                .bind(raw_self.get())
                .execute(&self.pool)
                .await?;
        }

        Ok(())
    }

    async fn track_id_at(
        &self,
        release_id: ReleaseId,
        position: &str,
    ) -> Result<ReleaseTrackId, StoreError> {
        let raw = sqlx::query_scalar::<_, i64>(
            "SELECT track_id FROM release_track
             WHERE release_id = ? AND position_raw = ? ORDER BY track_id LIMIT 1",
        )
        .bind(release_id.get())
        .bind(position)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| StoreError::not_found("release_track"))?;
        narrow_id::<ReleaseTrackId>(raw, "release_track.track_id")
    }

    /// Stores every distinct artist credited on the release and records the
    /// album-artist list. Runs before credits, which reference the same rows.
    async fn replace_artists(
        &self,
        release_id: ReleaseId,
        artists: &[ArtistCredit],
    ) -> Result<(), StoreError> {
        sqlx::query("DELETE FROM release_artist WHERE release_id = ?")
            .bind(release_id.get())
            .execute(&self.pool)
            .await?;

        for credit in artists {
            let artist_id = self.upsert_artist(&credit.artist).await?;

            sqlx::query(
                "INSERT INTO release_artist
                    (release_id, artist_id, position, join_phrase, anv, is_album_artist)
                 VALUES (?, ?, ?, ?, ?, ?)
                 ON CONFLICT (release_id, artist_id) DO UPDATE SET
                     position = excluded.position,
                     anv = excluded.anv,
                     is_album_artist = excluded.is_album_artist",
            )
            .bind(release_id.get())
            .bind(artist_id.get())
            .bind(i64::from(credit.position))
            .bind(credit.join_phrase.as_deref())
            .bind(credit.artist.name_variation.as_deref())
            .bind(credit.is_album_artist)
            .execute(&self.pool)
            .await?;
        }
        Ok(())
    }

    /// Inserts an artist if it is new, returning its local id.
    ///
    /// The Discogs id is the identity when there is one. A credit that carries
    /// no id is matched on its canonical name instead, because SQLite permits
    /// many `NULL`s under a `UNIQUE` constraint and so cannot deduplicate them.
    pub async fn upsert_artist(&self, reference: &ArtistRef) -> Result<ArtistId, StoreError> {
        if let Some(discogs_id) = reference.discogs_artist_id {
            sqlx::query(
                "INSERT INTO artist (discogs_artist_id, canonical_name, anv)
                 VALUES (?, ?, ?)
                 ON CONFLICT (discogs_artist_id) DO UPDATE SET
                     canonical_name = excluded.canonical_name,
                     anv = COALESCE(excluded.anv, artist.anv)",
            )
            .bind(discogs_id.get())
            .bind(&reference.name)
            .bind(reference.name_variation.as_deref())
            .execute(&self.pool)
            .await?;

            let raw = sqlx::query_scalar::<_, i64>(
                "SELECT artist_id FROM artist WHERE discogs_artist_id = ?",
            )
            .bind(discogs_id.get())
            .fetch_one(&self.pool)
            .await?;
            return narrow_id::<ArtistId>(raw, "artist.artist_id");
        }

        if let Some(raw) = sqlx::query_scalar::<_, i64>(
            "SELECT artist_id FROM artist
             WHERE canonical_name = ? AND discogs_artist_id IS NULL LIMIT 1",
        )
        .bind(&reference.name)
        .fetch_optional(&self.pool)
        .await?
        {
            return narrow_id::<ArtistId>(raw, "artist.artist_id");
        }

        let raw = sqlx::query_scalar::<_, i64>(
            "INSERT INTO artist (canonical_name, anv) VALUES (?, ?) RETURNING artist_id",
        )
        .bind(&reference.name)
        .bind(reference.name_variation.as_deref())
        .fetch_one(&self.pool)
        .await?;
        narrow_id::<ArtistId>(raw, "artist.artist_id")
    }

    async fn replace_credits(
        &self,
        release_id: ReleaseId,
        credits: &[ReleaseCredit],
    ) -> Result<(), StoreError> {
        sqlx::query("DELETE FROM release_credit WHERE release_id = ?")
            .bind(release_id.get())
            .execute(&self.pool)
            .await?;

        for credit in credits {
            let artist_id = self.upsert_artist(&credit.artist).await?;

            // A credit scoped to a position this release does not have is
            // dropped to release level rather than pointed at a track that is
            // not there, which the foreign key would refuse.
            let track_id = match credit.track_position.as_deref() {
                Some(position) => self
                    .track_id_at(release_id, position)
                    .await
                    .map(Some)
                    .or_else(|error| match error {
                        StoreError::NotFound { .. } => Ok(None),
                        other => Err(other),
                    })?,
                None => None,
            };

            sqlx::query(
                "INSERT INTO release_credit
                    (release_id, release_track_id, artist_id, role, anv, tracks_scope_raw)
                 VALUES (?, ?, ?, ?, ?, ?)",
            )
            .bind(release_id.get())
            .bind(track_id.map(i64::from))
            .bind(artist_id.get())
            .bind(&credit.role)
            .bind(credit.artist.name_variation.as_deref())
            .bind(credit.tracks_scope_raw.as_deref())
            .execute(&self.pool)
            .await?;
        }
        Ok(())
    }

    async fn replace_identifiers(
        &self,
        release_id: ReleaseId,
        identifiers: &[ReleaseIdentifier],
    ) -> Result<(), StoreError> {
        sqlx::query("DELETE FROM release_identifier WHERE release_id = ?")
            .bind(release_id.get())
            .execute(&self.pool)
            .await?;

        for identifier in identifiers {
            let label_id = match (identifier.label, identifier.discogs_label_id) {
                (Some(label), _) => Some(label.get()),
                (None, Some(discogs_label_id)) => Some(
                    self.upsert_label(&identifier.label_name, Some(discogs_label_id))
                        .await?
                        .get(),
                ),
                (None, None) => None,
            };

            sqlx::query(
                "INSERT OR REPLACE INTO release_identifier
                    (release_id, label_id, discogs_label_id, label_name, catalog_number, barcode)
                 VALUES (?, ?, ?, ?, ?, ?)",
            )
            .bind(release_id.get())
            .bind(label_id)
            .bind(identifier.discogs_label_id.map(i64::from))
            .bind(&identifier.label_name)
            .bind(identifier.catalog_number.as_deref())
            .bind(identifier.barcode.as_deref())
            .execute(&self.pool)
            .await?;
        }
        Ok(())
    }

    pub async fn upsert_label(
        &self,
        name: &str,
        discogs_label_id: Option<aldo_core::ids::DiscogsLabelId>,
    ) -> Result<LabelId, StoreError> {
        sqlx::query(
            "INSERT INTO label (name, discogs_label_id) VALUES (?, ?)
             ON CONFLICT (discogs_label_id) DO UPDATE SET name = excluded.name",
        )
        .bind(name)
        .bind(discogs_label_id.map(i64::from))
        .execute(&self.pool)
        .await?;

        let row = match discogs_label_id {
            Some(id) => {
                sqlx::query_scalar::<_, i64>(
                    "SELECT label_id FROM label WHERE discogs_label_id = ?",
                )
                .bind(i64::from(id))
                .fetch_one(&self.pool)
                .await
            }
            None => {
                sqlx::query_scalar::<_, i64>("SELECT label_id FROM label WHERE name = ?")
                    .bind(name)
                    .fetch_one(&self.pool)
                    .await
            }
        };
        narrow_id::<LabelId>(row?, "label_id")
    }

    pub async fn get(&self, release_id: ReleaseId) -> Result<CatalogRow, StoreError> {
        let row = sqlx::query(
            "SELECT release_id, discogs_release_id, title, year, release_types,
                    cover_art_url,
                    (SELECT COUNT(*) FROM release_track t
                     WHERE t.release_id = release.release_id AND t.kind = 'track') AS track_count,
                    (SELECT a.canonical_name FROM release_artist ra
                     JOIN artist a ON a.artist_id = ra.artist_id
                     WHERE ra.release_id = release.release_id AND ra.is_album_artist = 1
                     ORDER BY ra.position LIMIT 1) AS album_artist
             FROM release WHERE release_id = ?",
        )
        .bind(release_id.get())
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| StoreError::not_found("release"))?;

        let raw_id: i64 = row.try_get("release_id")?;
        let raw_discogs: i64 = row.try_get("discogs_release_id")?;
        let year: Option<i64> = row.try_get("year")?;
        let types: String = row.try_get("release_types")?;

        Ok(CatalogRow {
            release_id: narrow_id::<ReleaseId>(raw_id, "release.release_id")?,
            discogs_release_id: narrow_id::<DiscogsReleaseId>(raw_discogs, "discogs_release_id")?,
            title: row.try_get("title")?,
            artist: row.try_get("album_artist")?,
            year: year.map(|y| narrow_u16(y, "release.year")).transpose()?,
            // An unrecognised stored type is corruption, not a type to drop:
            // filtering it away would report a release with a release type
            // silently missing and nothing to say so.
            release_types: types
                .split(';')
                .filter(|s| !s.is_empty())
                .map(|text| {
                    ReleaseType::from_vorbis_value(text)
                        .ok_or_else(|| StoreError::corrupt("release.release_types", text))
                })
                .collect::<Result<_, StoreError>>()?,
            track_count: narrow_u16(row.try_get("track_count")?, "release_track")?,
            cover_art_url: row.try_get("cover_art_url")?,
        })
    }

    /// Loads a stored release by its public Discogs ID.
    pub async fn get_by_discogs_id(
        &self,
        discogs_release_id: DiscogsReleaseId,
    ) -> Result<CatalogRow, StoreError> {
        let raw: Option<i64> =
            sqlx::query_scalar("SELECT release_id FROM release WHERE discogs_release_id = ?")
                .bind(discogs_release_id.get())
                .fetch_optional(&self.pool)
                .await?;
        let raw = raw.ok_or_else(|| StoreError::not_found("Discogs release"))?;
        self.get(narrow_id::<ReleaseId>(raw, "release.release_id")?)
            .await
    }

    pub async fn tracks(&self, release_id: ReleaseId) -> Result<Vec<ReleaseTrack>, StoreError> {
        let rows = sqlx::query(
            "SELECT t.track_id, t.position_raw, t.title, t.kind, t.duration_ms,
                    p.position_raw AS parent_position
             FROM release_track t
             LEFT JOIN release_track p ON p.track_id = t.parent_track_id
             WHERE t.release_id = ?
             ORDER BY t.disc_number, t.track_number, t.track_id",
        )
        .bind(release_id.get())
        .fetch_all(&self.pool)
        .await?;

        rows.iter()
            .map(|row| {
                let kind_text: String = row.try_get("kind")?;
                let raw_id: i64 = row.try_get("track_id")?;
                let duration: Option<i64> = row.try_get("duration_ms")?;
                let position: String = row.try_get("position_raw")?;

                Ok(ReleaseTrack {
                    track_id: narrow_id::<ReleaseTrackId>(raw_id, "track_id")?,
                    release_id,
                    position: TrackPosition::parse(&position),
                    title: row.try_get("title")?,
                    kind: TrackKind::from_stored_str(&kind_text)
                        .ok_or_else(|| StoreError::corrupt("release_track.kind", &kind_text))?,
                    duration: duration
                        .map(|d| millis_from_stored(d, "release_track.duration_ms"))
                        .transpose()?
                        .map(DurationMs::from_millis),
                    parent_position: row.try_get("parent_position")?,
                })
            })
            .collect::<Result<_, StoreError>>()
    }

    pub async fn album_artists(
        &self,
        release_id: ReleaseId,
    ) -> Result<Vec<ArtistCredit>, StoreError> {
        let rows = sqlx::query(
            "SELECT a.artist_id, a.discogs_artist_id, a.canonical_name, ra.anv,
                    ra.join_phrase, ra.position, ra.is_album_artist
             FROM release_artist ra JOIN artist a ON a.artist_id = ra.artist_id
             WHERE ra.release_id = ? ORDER BY ra.position",
        )
        .bind(release_id.get())
        .fetch_all(&self.pool)
        .await?;

        rows.iter()
            .map(|row| {
                let raw_artist: i64 = row.try_get("artist_id")?;
                let raw_discogs: Option<i64> = row.try_get("discogs_artist_id")?;
                Ok(ArtistCredit {
                    artist: ArtistRef {
                        artist: narrow_id::<ArtistId>(raw_artist, "artist_id")?,
                        discogs_artist_id: raw_discogs
                            .map(|d| narrow_id::<DiscogsArtistId>(d, "artist.discogs_artist_id"))
                            .transpose()?,
                        name: row.try_get("canonical_name")?,
                        name_variation: row.try_get("anv")?,
                    },
                    join_phrase: row.try_get("join_phrase")?,
                    position: narrow_u8(row.try_get("position")?, "release_artist.position")?,
                    is_album_artist: bool_from_stored(
                        row.try_get("is_album_artist")?,
                        "release_artist.is_album_artist",
                    )?,
                })
            })
            .collect::<Result<_, StoreError>>()
    }

    // --- Discogs response cache -------------------------------------------

    pub async fn cached_discogs_payload(
        &self,
        release: DiscogsReleaseId,
    ) -> Result<Option<String>, StoreError> {
        sqlx::query_scalar("SELECT payload_json FROM discogs_lookup WHERE discogs_release_id = ?")
            .bind(release.get())
            .fetch_optional(&self.pool)
            .await
            .map_err(Into::into)
    }

    pub async fn cache_discogs_payload(
        &self,
        release: DiscogsReleaseId,
        payload: &str,
        now_ms: i64,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO discogs_lookup (discogs_release_id, payload_json, fetched_at)
             VALUES (?, ?, ?)
             ON CONFLICT (discogs_release_id) DO UPDATE SET
                 payload_json = excluded.payload_json, fetched_at = excluded.fetched_at",
        )
        .bind(release.get())
        .bind(payload)
        .bind(now_ms)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    // --- Torrent metainfo cache -------------------------------------------

    pub async fn cache_metainfo(
        &self,
        infohash: aldo_core::InfoHash,
        payload: &[u8],
        name: &str,
        total_bytes: ByteSize,
        flac_count: u32,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO torrent_metainfo (infohash, payload_b64, name, total_bytes, flac_count, resolved_at)
             VALUES (?, ?, ?, ?, ?, ?)
             ON CONFLICT (infohash) DO NOTHING",
        )
        .bind(infohash.to_hex())
        .bind(payload)
        .bind(name)
        .bind(widen_bytes(
            total_bytes.as_u64(),
            "torrent_metainfo.total_bytes",
        )?)
        .bind(i64::from(flac_count))
        .bind(now_ms())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn cached_metainfo(
        &self,
        infohash: aldo_core::InfoHash,
    ) -> Result<Option<CachedMetainfo>, StoreError> {
        let row = sqlx::query(
            "SELECT payload_b64, name, total_bytes, flac_count
             FROM torrent_metainfo WHERE infohash = ?",
        )
        .bind(infohash.to_hex())
        .fetch_optional(&self.pool)
        .await?;

        row.map(|row| {
            let payload: Vec<u8> = row.try_get("payload_b64")?;
            let name: String = row.try_get("name")?;
            let total_bytes: i64 = row.try_get("total_bytes")?;
            let flac_count: i64 = row.try_get("flac_count")?;
            Ok(CachedMetainfo {
                payload,
                name,
                total_bytes: ByteSize::new(bytes_from_stored(
                    total_bytes,
                    "torrent_metainfo.total_bytes",
                )?),
                flac_count: narrow_u32(flac_count, "torrent_metainfo.flac_count")?,
            })
        })
        .transpose()
    }

    /// How many Discogs releases are cached, for diagnostics.
    pub async fn discogs_cache_size(&self) -> Result<u32, StoreError> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM discogs_lookup")
            .fetch_one(&self.pool)
            .await?;
        narrow_u32(count, "discogs_lookup")
    }
}

fn position_raw(position: &TrackPosition) -> String {
    match position {
        TrackPosition::DiscTrack { disc, track } if *disc == 1 => track.to_string(),
        TrackPosition::DiscTrack { disc, track } => format!("{disc}-{track}"),
        TrackPosition::Side { disc, side } if *disc == 1 => side.to_string(),
        TrackPosition::Side { disc, side } => format!("{disc}{side}"),
        TrackPosition::Unparsed { raw } => raw.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Store;
    use aldo_core::ids::{DiscogsLabelId, DiscogsMasterId, ReleaseTrackId};
    use aldo_core::release::{ArtistRef, ReleaseCredit, ReleaseStatus, Side};

    fn track(position: &str, title: &str) -> ReleaseTrack {
        ReleaseTrack {
            track_id: ReleaseTrackId(0),
            release_id: ReleaseId(0),
            position: TrackPosition::parse(position),
            title: title.into(),
            kind: TrackKind::Track,
            duration: Some(DurationMs::from_millis(250_000)),
            parent_position: None,
        }
    }

    fn release(id: u32) -> ReleaseIdentity {
        ReleaseIdentity {
            release_id: ReleaseId(0),
            discogs_release_id: DiscogsReleaseId(id),
            master_id: None,
            discogs_master_id: Some(DiscogsMasterId(7)),
            musicbrainz_release_id: None,
            title: "OK Computer".into(),
            artists: vec![],
            release_types: vec![ReleaseType::Album],
            media: Some(MediaFormat::Cd),
            status: ReleaseStatus::Accepted,
            year: Some(1997),
            country: Some("GB".into()),
            disc_count: 1,
            duration: None,
            tracklist: vec![track("1", "Airbag"), track("2", "Paranoid Android")],
            credits: vec![],
            identifiers: vec![],
            genres: vec!["Alternative".into()],
            styles: vec!["Britpop".into()],
            cover_art_url: Some("https://img/cover.jpg".into()),
            popularity: Some(1234),
        }
    }

    async fn store() -> CatalogStore {
        let store = Store::open_temporary().await.expect("store opens");
        CatalogStore::new(store.pool().clone())
    }

    #[tokio::test]
    async fn stores_and_reads_a_release() {
        let catalog = store().await;
        let subject = release(1);

        let release_id = catalog.upsert_release(&subject, 0).await.expect("stored");

        let row = catalog.get(release_id).await.expect("read back");
        assert_eq!(row.discogs_release_id, DiscogsReleaseId(1));
        assert_eq!(row.title, "OK Computer");
        assert_eq!(row.year, Some(1997));
        assert_eq!(row.track_count, 2);
        assert_eq!(row.release_types, vec![ReleaseType::Album]);
        assert_eq!(row.cover_art_url.as_deref(), Some("https://img/cover.jpg"));
    }

    fn credited_artist(name: &str, discogs_id: u32, anv: Option<&str>) -> ArtistCredit {
        ArtistCredit {
            artist: ArtistRef::from_discogs_credit(
                ArtistId(0),
                Some(DiscogsArtistId(discogs_id)),
                name,
                anv,
            ),
            join_phrase: None,
            position: 0,
            is_album_artist: true,
        }
    }

    #[tokio::test]
    async fn a_mapped_release_persists_its_artists() {
        // Without this the album artist is unwritable and every result row
        // renders nameless: `CatalogRow.artist` reads through `release_artist`.
        let catalog = store().await;
        let mut subject = release(1);
        subject.artists = vec![credited_artist("Radiohead", 3840, None)];

        let release_id = catalog.upsert_release(&subject, 0).await.expect("stored");

        let row = catalog.get(release_id).await.expect("read back");
        assert_eq!(row.artist.as_deref(), Some("Radiohead"));

        let artists = catalog.album_artists(release_id).await.expect("artists");
        assert_eq!(artists.len(), 1);
        assert_eq!(artists[0].artist.name, "Radiohead");
        assert_eq!(
            artists[0].artist.discogs_artist_id,
            Some(DiscogsArtistId(3840))
        );
        assert!(artists[0].is_album_artist);
    }

    #[tokio::test]
    async fn an_artist_is_stored_once_however_often_it_is_credited() {
        let catalog = store().await;
        let mut subject = release(1);
        subject.artists = vec![credited_artist("Radiohead", 3840, None)];
        catalog.upsert_release(&subject, 0).await.expect("stored");
        catalog
            .upsert_release(&subject, 1)
            .await
            .expect("stored again");

        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM artist")
            .fetch_one(&catalog.pool)
            .await
            .expect("count");
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn a_credit_with_no_discogs_id_is_deduplicated_on_its_name() {
        // SQLite allows many NULLs under a UNIQUE constraint, so the name is the
        // only identity a nameless-id artist can have.
        let catalog = store().await;
        let nameless = |name: &str| ArtistRef::from_discogs_credit(ArtistId(0), None, name, None);

        let first = catalog
            .upsert_artist(&nameless("Various"))
            .await
            .expect("first");
        let second = catalog
            .upsert_artist(&nameless("Various"))
            .await
            .expect("second");
        let other = catalog
            .upsert_artist(&nameless("Someone Else"))
            .await
            .expect("third");

        assert_eq!(first, second, "the same name produced two artist rows");
        assert_ne!(first, other);

        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM artist")
            .fetch_one(&catalog.pool)
            .await
            .expect("count");
        assert_eq!(count, 2);
    }

    fn credit(role: &str, track_position: Option<&str>) -> ReleaseCredit {
        ReleaseCredit {
            credit_id: None,
            release_id: ReleaseId(0),
            track_position: track_position.map(str::to_owned),
            artist: ArtistRef::from_discogs_credit(
                ArtistId(0),
                Some(DiscogsArtistId(9)),
                "Nigel Godrich",
                None,
            ),
            role: role.to_owned(),
            tracks_scope_raw: None,
        }
    }

    #[tokio::test]
    async fn a_credit_scoped_to_a_track_lands_on_that_track() {
        let catalog = store().await;
        let mut subject = release(1);
        subject.artists = vec![credited_artist("Radiohead", 3840, None)];
        subject.credits = vec![credit("Producer", Some("2"))];

        let release_id = catalog.upsert_release(&subject, 0).await.expect("stored");

        let scoped: Option<i64> =
            sqlx::query_scalar("SELECT release_track_id FROM release_credit WHERE release_id = ?")
                .bind(release_id.get())
                .fetch_one(&catalog.pool)
                .await
                .expect("credit");

        let expected: i64 = sqlx::query_scalar(
            "SELECT track_id FROM release_track WHERE release_id = ? AND position_raw = '2'",
        )
        .bind(release_id.get())
        .fetch_one(&catalog.pool)
        .await
        .expect("track");

        assert_eq!(scoped, Some(expected));
    }

    #[tokio::test]
    async fn a_credit_naming_a_position_the_release_lacks_falls_back_to_release_level() {
        // Discogs scopes credits with free text such as `"3, 7-9"`. Pointing the
        // foreign key at a track that is not there would abort the whole upsert,
        // so the credit degrades to a release-level one instead.
        let catalog = store().await;
        let mut subject = release(1);
        subject.credits = vec![credit("Producer", Some("9-9"))];

        let release_id = catalog
            .upsert_release(&subject, 0)
            .await
            .expect("an unresolvable scope must not fail the release");

        let scoped: Option<i64> =
            sqlx::query_scalar("SELECT release_track_id FROM release_credit WHERE release_id = ?")
                .bind(release_id.get())
                .fetch_one(&catalog.pool)
                .await
                .expect("credit");

        assert_eq!(scoped, None);
    }

    #[tokio::test]
    async fn re_upserting_the_same_release_does_not_duplicate_it() {
        let catalog = store().await;
        let subject = release(1);

        let first = catalog.upsert_release(&subject, 0).await.expect("stored");
        let second = catalog
            .upsert_release(&subject, 1)
            .await
            .expect("stored again");

        assert_eq!(first, second);
    }

    #[tokio::test]
    async fn re_upserting_replaces_the_tracklist() {
        let catalog = store().await;
        let mut subject = release(1);
        let release_id = catalog.upsert_release(&subject, 0).await.expect("stored");

        // The upstream release gained a bonus track.
        subject.tracklist.push(track("3", "Substitute"));
        catalog
            .upsert_release(&subject, 1)
            .await
            .expect("stored again");

        let tracks = catalog.tracks(release_id).await.expect("tracks");
        assert_eq!(tracks.len(), 3);
        assert!(tracks.iter().any(|t| t.title == "Substitute"));
    }

    #[tokio::test]
    async fn does_not_leave_a_stale_track_after_a_shorter_refetch() {
        let catalog = store().await;
        let mut subject = release(1);
        let release_id = catalog.upsert_release(&subject, 0).await.expect("stored");

        subject.tracklist.truncate(1);
        catalog
            .upsert_release(&subject, 1)
            .await
            .expect("stored again");

        let tracks = catalog.tracks(release_id).await.expect("tracks");
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].title, "Airbag");
    }

    #[tokio::test]
    async fn track_positions_round_trip_through_storage() {
        let catalog = store().await;
        let mut subject = release(2);
        subject.disc_count = 2;
        subject.tracklist = vec![
            track("1", "Disc One"),
            track("2-1", "Disc Two"),
            track("A", "Side A"),
            track("Bonus", "Unnumbered"),
        ];
        let release_id = catalog.upsert_release(&subject, 0).await.expect("stored");

        let tracks = catalog.tracks(release_id).await.expect("tracks");
        let by_title = |name: &str| {
            tracks
                .iter()
                .find(|t| t.title == name)
                .map(|t| t.position.clone())
                .expect("track present")
        };

        assert_eq!(
            by_title("Disc One"),
            TrackPosition::DiscTrack { disc: 1, track: 1 }
        );
        assert_eq!(
            by_title("Disc Two"),
            TrackPosition::DiscTrack { disc: 2, track: 1 }
        );
        assert_eq!(
            by_title("Side A"),
            TrackPosition::Side {
                disc: 1,
                side: Side::A
            }
        );
        assert_eq!(
            by_title("Unnumbered"),
            TrackPosition::Unparsed {
                raw: "Bonus".into()
            }
        );
    }

    #[tokio::test]
    async fn counts_only_playable_tracks() {
        let catalog = store().await;
        let mut subject = release(3);
        subject.tracklist = vec![
            track("1", "Real"),
            ReleaseTrack {
                track_id: ReleaseTrackId(0),
                release_id: ReleaseId(0),
                position: TrackPosition::parse("2"),
                title: "Bonus Tracks".into(),
                kind: TrackKind::Heading,
                duration: None,
                parent_position: None,
            },
            track("3", "Also real"),
        ];
        let release_id = catalog.upsert_release(&subject, 0).await.expect("stored");

        assert_eq!(catalog.get(release_id).await.expect("row").track_count, 2);
    }

    #[tokio::test]
    async fn caches_a_discogs_payload_permanently() {
        /// Whether a release is cached is answered by one mechanism: the payload
        /// read. A second "does it exist" query could disagree with it.
        async fn is_cached(catalog: &CatalogStore, id: DiscogsReleaseId) -> bool {
            catalog
                .cached_discogs_payload(id)
                .await
                .expect("read")
                .is_some()
        }

        let catalog = store().await;
        assert!(!is_cached(&catalog, DiscogsReleaseId(5)).await);

        catalog
            .cache_discogs_payload(DiscogsReleaseId(5), "{\"id\":5}", 0)
            .await
            .expect("cached");

        assert!(is_cached(&catalog, DiscogsReleaseId(5)).await);
        assert_eq!(
            catalog
                .cached_discogs_payload(DiscogsReleaseId(5))
                .await
                .expect("read")
                .as_deref(),
            Some("{\"id\":5}")
        );
        assert_eq!(catalog.discogs_cache_size().await.expect("size"), 1);
    }

    #[tokio::test]
    async fn an_uncached_release_returns_nothing_rather_than_a_default() {
        let catalog = store().await;
        assert!(
            catalog
                .cached_discogs_payload(DiscogsReleaseId(99))
                .await
                .expect("read")
                .is_none()
        );
    }

    #[tokio::test]
    async fn caches_resolved_metainfo_by_infohash() {
        let catalog = store().await;
        let infohash = aldo_core::InfoHash([3; 20]);

        assert!(
            catalog
                .cached_metainfo(infohash)
                .await
                .expect("read")
                .is_none()
        );

        catalog
            .cache_metainfo(
                infohash,
                b"d4:infod",
                "Album",
                ByteSize::new(42_000_000),
                12,
            )
            .await
            .expect("cached");

        let cached = catalog
            .cached_metainfo(infohash)
            .await
            .expect("read")
            .expect("present");
        assert_eq!(cached.payload, b"d4:infod");
        assert_eq!(cached.name, "Album");
        assert_eq!(cached.total_bytes, ByteSize::new(42_000_000));
        assert_eq!(cached.flac_count, 12);
    }

    #[tokio::test]
    async fn re_caching_the_same_infohash_keeps_the_first_listing() {
        let catalog = store().await;
        let infohash = aldo_core::InfoHash([4; 20]);

        catalog
            .cache_metainfo(infohash, b"first", "Album", ByteSize::new(1), 1)
            .await
            .expect("cached");
        catalog
            .cache_metainfo(infohash, b"second", "Other", ByteSize::new(2), 2)
            .await
            .expect("cached again");

        let cached = catalog
            .cached_metainfo(infohash)
            .await
            .expect("read")
            .expect("present");
        assert_eq!(cached.payload, b"first");
    }

    #[tokio::test]
    async fn a_missing_release_is_reported_not_defaulted() {
        let catalog = store().await;
        assert!(matches!(
            catalog.get(ReleaseId(4_242)).await,
            Err(StoreError::NotFound { table: "release" })
        ));
    }

    #[tokio::test]
    async fn an_unrecognised_stored_release_type_is_reported_not_dropped() {
        let catalog = store().await;
        let release_id = catalog
            .upsert_release(&release(1), 0)
            .await
            .expect("stored");

        sqlx::query("UPDATE release SET release_types = 'album;hologram' WHERE release_id = ?")
            .bind(release_id.get())
            .execute(&catalog.pool)
            .await
            .expect("row updated");

        assert!(matches!(
            catalog.get(release_id).await,
            Err(StoreError::Corrupt {
                column: "release.release_types",
                ..
            })
        ));
    }

    #[tokio::test]
    async fn a_label_is_returned_as_a_typed_identifier() {
        let catalog = store().await;
        let label_id = catalog
            .upsert_label("Parlophone", Some(DiscogsLabelId(9)))
            .await
            .expect("stored");

        let artist: i64 = sqlx::query_scalar(
            "INSERT INTO artist (canonical_name) VALUES ('Radiohead') RETURNING artist_id",
        )
        .fetch_one(&catalog.pool)
        .await
        .expect("artist inserted");

        let release_id = catalog
            .upsert_release(&release(1), 0)
            .await
            .expect("stored");
        sqlx::query(
            "INSERT INTO release_artist (release_id, artist_id, position, is_album_artist)
             VALUES (?, ?, 0, 1)",
        )
        .bind(release_id.get())
        .bind(artist)
        .execute(&catalog.pool)
        .await
        .expect("credit inserted");

        let credits = catalog.album_artists(release_id).await.expect("read back");
        assert!(credits[0].is_album_artist);
        assert_eq!(
            catalog
                .upsert_label("Parlophone", Some(DiscogsLabelId(9)))
                .await
                .expect("stored again"),
            label_id
        );
    }

    #[tokio::test]
    async fn a_flag_column_holds_only_zero_or_one() {
        let catalog = store().await;
        let release_id = catalog
            .upsert_release(&release(1), 0)
            .await
            .expect("stored");
        let artist: i64 = sqlx::query_scalar(
            "INSERT INTO artist (canonical_name) VALUES ('Nirvana') RETURNING artist_id",
        )
        .fetch_one(&catalog.pool)
        .await
        .expect("artist inserted");

        let refused = sqlx::query(
            "INSERT INTO release_artist (release_id, artist_id, position, is_album_artist)
             VALUES (?, ?, 0, 2)",
        )
        .bind(release_id.get())
        .bind(artist)
        .execute(&catalog.pool)
        .await;

        assert!(refused.is_err(), "2 was accepted as a flag");
    }
}
