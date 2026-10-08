//! Candidate persistence and cross-source deduplication.
//!
//! Deduplication is enforced by a unique index on `(session_id, dedupe_key)`,
//! so two providers reporting the same album converge on one row even if they
//! finish concurrently. `absorb_source` then attaches the additional locator.

use mud_core::DedupeKey;
use mud_core::audio::{BitDepth, ByteSize, DurationMs, SampleRate};
use mud_core::candidate::{Candidate, CandidateFile, Locator, RejectionCounts, SoulSeekLocator};
use mud_core::ids::{CandidateId, FanoutId, LocatorId, SessionId};
use sqlx::Row;
use sqlx::SqlitePool;

use crate::error::{
    StoreError, bool_from_stored, bytes_from_stored, digest_from_stored, millis_from_stored,
    narrow_id, narrow_u8, narrow_u16, narrow_u32, ordinal_from_index, widen_bytes, widen_millis,
};

#[derive(Debug, Clone)]
pub struct CandidateRow {
    pub candidate_id: CandidateId,
    pub dedupe_key: DedupeKey,
    pub display_artist: String,
    pub display_album: String,
    pub display_year: Option<u16>,
    pub track_count: u16,
    pub disc_count: u8,
    pub total_bytes: ByteSize,
    pub provider_count: u32,
    pub seeders: Option<u32>,
    pub best_sample_rate_hz: Option<SampleRate>,
    pub best_bit_depth: Option<BitDepth>,
}

/// A candidate read back from the store, ready to download: its identity, the
/// source to fetch it from, and the files that source holds.
#[derive(Debug, Clone)]
pub struct LoadedCandidate {
    pub candidate_id: CandidateId,
    pub display_artist: String,
    pub display_album: String,
    pub locator: Locator,
    pub files: Vec<CandidateFile>,
}

#[derive(Debug, Clone)]
pub struct CandidateStore {
    pool: SqlitePool,
}
impl CandidateStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Inserts a candidate, or returns the existing row when one with the same
    /// dedupe key already exists in this session.
    pub async fn insert_or_get(
        &self,
        session_id: SessionId,
        dedupe_key: DedupeKey,
        candidate: &Candidate,
        now_ms: i64,
    ) -> Result<CandidateId, StoreError> {
        sqlx::query(
            "INSERT INTO candidate
                (session_id, dedupe_key, display_artist, display_album, display_year,
                 track_count, disc_count, total_bytes, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT (session_id, dedupe_key) DO NOTHING",
        )
        .bind(session_id.get())
        .bind(dedupe_key.as_sha256().to_hex())
        .bind(&candidate.display.artist)
        .bind(&candidate.display.album)
        .bind(candidate.display.year.map(i64::from))
        .bind(i64::from(candidate.display.track_count))
        .bind(i32::from(candidate.display.disc_count))
        .bind(widen_bytes(
            candidate.total_bytes.as_u64(),
            "candidate.total_bytes",
        )?)
        .bind(now_ms)
        .execute(&self.pool)
        .await?;

        let raw: i64 = sqlx::query_scalar(
            "SELECT candidate_id FROM candidate WHERE session_id = ? AND dedupe_key = ?",
        )
        .bind(session_id.get())
        .bind(dedupe_key.as_sha256().to_hex())
        .fetch_one(&self.pool)
        .await?;
        narrow_id::<CandidateId>(raw, "candidate_id")
    }

    /// Attaches a locator as an additional source, promoting it to primary if
    /// the current primary has no capacity.
    ///
    /// A locator already recorded for this candidate is reused rather than
    /// duplicated: the same peer offering the same file is one source, however
    /// many times it is reported.
    pub async fn absorb_source(
        &self,
        candidate_id: CandidateId,
        fanout_id: FanoutId,
        locator: &Locator,
    ) -> Result<LocatorId, StoreError> {
        let locator_id = match self.existing_locator(candidate_id, locator).await? {
            Some(existing) => existing,
            None => self.insert_locator(candidate_id, locator).await?,
        };

        // Promoting demotes the previous primary; only one row may be primary.
        // The flag is bound as a `bool`, so the column can only ever receive 0
        // or 1, and its CHECK makes that a property of the table too.
        if locator.is_available() {
            sqlx::query("UPDATE candidate_locator SET is_primary = ? WHERE candidate_id = ?")
                .bind(false)
                .bind(candidate_id.get())
                .execute(&self.pool)
                .await?;
            sqlx::query("UPDATE candidate_locator SET is_primary = ? WHERE locator_id = ?")
                .bind(true)
                .bind(locator_id.get())
                .execute(&self.pool)
                .await?;
        } else {
            sqlx::query(
                "UPDATE candidate_locator SET is_primary = ?
                 WHERE locator_id = (
                     SELECT MIN(locator_id) FROM candidate_locator WHERE candidate_id = ?
                 )",
            )
            .bind(true)
            .bind(candidate_id.get())
            .execute(&self.pool)
            .await?;
        }

        sqlx::query(
            "INSERT OR IGNORE INTO candidate_source (candidate_id, fanout_id, locator_id)
             VALUES (?, ?, ?)",
        )
        .bind(candidate_id.get())
        .bind(fanout_id.get())
        .bind(locator_id.get())
        .execute(&self.pool)
        .await?;

        Ok(locator_id)
    }

    /// Finds a locator already recorded for this candidate from the same source.
    async fn existing_locator(
        &self,
        candidate_id: CandidateId,
        locator: &Locator,
    ) -> Result<Option<LocatorId>, StoreError> {
        let row: Option<i64> = match locator {
            Locator::SoulSeek(slsk) => {
                sqlx::query_scalar(
                    "SELECT locator_id FROM candidate_locator
                 WHERE candidate_id = ? AND kind = 'soulseek'
                   AND slsk_peer = ? AND slsk_remote_path = ?",
                )
                .bind(candidate_id.get())
                .bind(&slsk.peer)
                .bind(&slsk.remote_path)
                .fetch_optional(&self.pool)
                .await?
            }
            Locator::Torrent(torrent) => {
                sqlx::query_scalar(
                    "SELECT locator_id FROM candidate_locator
                 WHERE candidate_id = ? AND kind = 'torrent'
                   AND infohash = ? AND file_index IS ?",
                )
                .bind(candidate_id.get())
                .bind(torrent.infohash.to_hex())
                .bind(torrent.file_index.map(i64::from))
                .fetch_optional(&self.pool)
                .await?
            }
        };

        row.map(|raw| narrow_id::<LocatorId>(raw, "locator_id"))
            .transpose()
    }

    /// Both locator shapes write to the same table, so one statement covers
    /// both and the `kind` column records which columns are meaningful.
    async fn insert_locator(
        &self,
        candidate_id: CandidateId,
        locator: &Locator,
    ) -> Result<LocatorId, StoreError> {
        let soulseek = match locator {
            Locator::SoulSeek(slsk) => Some(slsk),
            Locator::Torrent(_) => None,
        };
        let torrent = match locator {
            Locator::SoulSeek(_) => None,
            Locator::Torrent(torrent) => Some(torrent),
        };

        let raw: i64 = sqlx::query_scalar(
            "INSERT INTO candidate_locator
                (candidate_id, kind, is_primary,
                 slsk_peer, slsk_remote_path, slsk_size, slsk_slot_free,
                 slsk_queue_len, slsk_speed_bps, sample_rate_hz, bit_depth, duration_ms,
                 infohash, file_index, tracker, seeders, leechers)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) RETURNING locator_id",
        )
        .bind(candidate_id.get())
        .bind(match locator {
            Locator::SoulSeek(_) => "soulseek",
            Locator::Torrent(_) => "torrent",
        })
        .bind(false)
        .bind(soulseek.map(|s| s.peer.as_str()))
        .bind(soulseek.map(|s| s.remote_path.as_str()))
        .bind(
            soulseek
                .map(|s| widen_bytes(s.size.as_u64(), "candidate_locator.slsk_size"))
                .transpose()?,
        )
        .bind(soulseek.map(|s| s.has_free_slot))
        .bind(soulseek.and_then(|s| s.queue_length.map(i64::from)))
        .bind(soulseek.map(|s| i64::from(s.upload_speed_bps)))
        .bind(soulseek.and_then(|s| s.sample_rate.map(|rate| i64::from(rate.hz()))))
        .bind(soulseek.and_then(|s| s.bit_depth.map(|depth| i64::from(depth.bits()))))
        .bind(
            soulseek
                .and_then(|s| s.duration.map(DurationMs::as_millis))
                .map(|ms| widen_millis(ms, "candidate_locator.duration_ms"))
                .transpose()?,
        )
        .bind(torrent.map(|t| t.infohash.to_hex()))
        .bind(torrent.and_then(|t| t.file_index.map(i64::from)))
        .bind(torrent.and_then(|t| t.tracker.as_deref()))
        .bind(torrent.and_then(|t| t.seeders.map(i64::from)))
        .bind(torrent.and_then(|t| t.leechers.map(i64::from)))
        .fetch_one(&self.pool)
        .await?;

        narrow_id::<LocatorId>(raw, "locator_id")
    }

    pub async fn insert_files(
        &self,
        candidate_id: CandidateId,
        candidate: &Candidate,
    ) -> Result<(), StoreError> {
        for (index, file) in candidate.files.iter().enumerate() {
            let ordinal = ordinal_from_index(index, "candidate_file.ordinal")?;
            sqlx::query(
                "INSERT OR IGNORE INTO candidate_file (candidate_id, ordinal, path, bytes)
                 VALUES (?, ?, ?, ?)",
            )
            .bind(candidate_id.get())
            .bind(i64::from(ordinal))
            .bind(&file.path)
            .bind(widen_bytes(file.size.as_u64(), "candidate_file.bytes")?)
            .execute(&self.pool)
            .await?;
        }
        Ok(())
    }

    pub async fn list(&self, session_id: SessionId) -> Result<Vec<CandidateRow>, StoreError> {
        let rows = sqlx::query(
            "SELECT c.candidate_id, c.dedupe_key, c.display_artist, c.display_album,
                    c.display_year, c.track_count, c.disc_count, c.total_bytes,
                    (SELECT COUNT(*) FROM candidate_source WHERE candidate_id = c.candidate_id)
                        AS provider_count,
                    (SELECT MAX(l.seeders) FROM candidate_locator l
                     WHERE l.candidate_id = c.candidate_id) AS seeders,
                    (SELECT MAX(l.sample_rate_hz) FROM candidate_locator l
                     WHERE l.candidate_id = c.candidate_id) AS sample_rate,
                    (SELECT MAX(l.bit_depth) FROM candidate_locator l
                     WHERE l.candidate_id = c.candidate_id) AS bit_depth
             FROM candidate c
             WHERE c.session_id = ?
             ORDER BY provider_count DESC, c.candidate_id",
        )
        .bind(session_id.get())
        .fetch_all(&self.pool)
        .await?;

        rows.iter()
            .map(|row| {
                let raw_key: String = row.try_get("dedupe_key")?;
                let raw_id: i64 = row.try_get("candidate_id")?;
                let year: Option<i64> = row.try_get("display_year")?;
                let seeders: Option<i64> = row.try_get("seeders")?;

                Ok(CandidateRow {
                    candidate_id: narrow_id::<CandidateId>(raw_id, "candidate.candidate_id")?,
                    dedupe_key: DedupeKey::from_sha256(digest_from_stored::<mud_core::Sha256>(
                        &raw_key,
                        "candidate.dedupe_key",
                    )?),
                    display_artist: row.try_get("display_artist")?,
                    display_album: row.try_get("display_album")?,
                    display_year: year
                        .map(|y| narrow_u16(y, "candidate.display_year"))
                        .transpose()?,
                    track_count: narrow_u16(row.try_get("track_count")?, "candidate.track_count")?,
                    disc_count: narrow_u8(row.try_get("disc_count")?, "candidate.disc_count")?,
                    total_bytes: ByteSize::new(bytes_from_stored(
                        row.try_get("total_bytes")?,
                        "candidate.total_bytes",
                    )?),
                    provider_count: narrow_u32(row.try_get("provider_count")?, "candidate_source")?,
                    seeders: seeders
                        .map(|s| narrow_u32(s, "candidate_locator.seeders"))
                        .transpose()?,
                    best_sample_rate_hz: row
                        .try_get::<Option<i64>, _>("sample_rate")?
                        .map(|hz| {
                            let raw = narrow_u32(hz, "candidate_locator.sample_rate_hz")?;
                            SampleRate::new(raw).ok_or_else(|| {
                                StoreError::corrupt(
                                    "candidate_locator.sample_rate_hz",
                                    format!("{raw} Hz is not a sample rate"),
                                )
                            })
                        })
                        .transpose()?,
                    best_bit_depth: row
                        .try_get::<Option<i64>, _>("bit_depth")?
                        .map(|bits| {
                            let raw = narrow_u8(bits, "candidate_locator.bit_depth")?;
                            BitDepth::new(raw).ok_or_else(|| {
                                StoreError::corrupt(
                                    "candidate_locator.bit_depth",
                                    format!("{raw} bits is not a bit depth"),
                                )
                            })
                        })
                        .transpose()?,
                })
            })
            .collect::<Result<_, StoreError>>()
    }

    /// Records why a raw hit was discarded.
    pub async fn record_rejections(
        &self,
        session_id: SessionId,
        fanout_id: FanoutId,
        counts: &RejectionCounts,
    ) -> Result<(), StoreError> {
        let total = counts.total();
        if total == 0 {
            return Ok(());
        }
        sqlx::query(
            "UPDATE provider_fanout
             SET error = COALESCE(error, '') || ? , result_count = result_count + ?
             WHERE fanout_id = ? AND session_id = ?",
        )
        .bind(format!(
            "rejected not_flac={} bit_depth={} too_large={} unavailable={} unparsable={}; ",
            counts.not_flac,
            counts.bit_depth_too_low,
            counts.too_large,
            counts.no_available_source,
            counts.unparsable
        ))
        .bind(i64::from(total))
        .bind(fanout_id.get())
        .bind(session_id.get())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Replaces the user-facing indexes for one UTC day. Candidate ids stay
    /// stable internally; the CLI index is valid only until the UTC day ends.
    pub async fn replace_daily_indexes(
        &self,
        utc_day: i64,
        indexes: &[(u8, CandidateId)],
    ) -> Result<(), StoreError> {
        let mut transaction = self.pool.begin().await?;
        sqlx::query("DELETE FROM daily_result_index WHERE utc_day <= ?")
            .bind(utc_day)
            .execute(&mut *transaction)
            .await?;

        for (result_index, candidate_id) in indexes {
            sqlx::query(
                "INSERT INTO daily_result_index (utc_day, result_index, candidate_id)
                 VALUES (?, ?, ?)",
            )
            .bind(utc_day)
            .bind(i64::from(*result_index))
            .bind(candidate_id.get())
            .execute(&mut *transaction)
            .await?;
        }

        transaction.commit().await?;
        Ok(())
    }

    /// Resolves a numbered CLI result for the current UTC day.
    pub async fn resolve_daily_index(
        &self,
        utc_day: i64,
        result_index: i64,
    ) -> Result<CandidateId, StoreError> {
        let raw: Option<i64> = sqlx::query_scalar(
            "SELECT candidate_id FROM daily_result_index
             WHERE utc_day = ? AND result_index = ?",
        )
        .bind(utc_day)
        .bind(result_index)
        .fetch_optional(&self.pool)
        .await?;

        raw.map(|id| narrow_id::<CandidateId>(id, "daily_result_index.candidate_id"))
            .transpose()?
            .ok_or_else(|| StoreError::not_found("today's result index"))
    }

    /// Reads a candidate back with its primary source and its file list.
    ///
    /// # Errors
    /// Returns [`StoreError::NotFound`] for an unknown id, and `Corrupt` when a
    /// stored value cannot be read back into its typed form.
    pub async fn load(&self, candidate_id: CandidateId) -> Result<LoadedCandidate, StoreError> {
        let row = sqlx::query(
            "SELECT display_artist, display_album FROM candidate WHERE candidate_id = ?",
        )
        .bind(candidate_id.get())
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| StoreError::not_found("candidate"))?;

        let locator = self.primary_locator(candidate_id).await?;

        let files = sqlx::query(
            "SELECT path, bytes FROM candidate_file WHERE candidate_id = ? ORDER BY ordinal",
        )
        .bind(candidate_id.get())
        .fetch_all(&self.pool)
        .await?;

        let files = files
            .iter()
            .map(|file| {
                Ok(CandidateFile {
                    path: file.try_get("path")?,
                    size: ByteSize::new(bytes_from_stored(
                        file.try_get("bytes")?,
                        "candidate_file.bytes",
                    )?),
                })
            })
            .collect::<Result<Vec<_>, StoreError>>()?;

        Ok(LoadedCandidate {
            candidate_id,
            display_artist: row.try_get("display_artist")?,
            display_album: row.try_get("display_album")?,
            locator,
            files,
        })
    }

    pub async fn selected_discogs_release(
        &self,
        candidate_id: CandidateId,
    ) -> Result<Option<i64>, StoreError> {
        sqlx::query_scalar(
            "SELECT s.selected_discogs_release_id
             FROM candidate c
             JOIN search_session s ON s.session_id = c.session_id
             WHERE c.candidate_id = ?",
        )
        .bind(candidate_id.get())
        .fetch_optional(&self.pool)
        .await
        .map_err(StoreError::from)
    }

    /// Reads the primary locator back into the domain type.
    async fn primary_locator(&self, candidate_id: CandidateId) -> Result<Locator, StoreError> {
        let row = sqlx::query(
            "SELECT kind, slsk_peer, slsk_remote_path, slsk_size, slsk_slot_free,
                    slsk_queue_len, slsk_speed_bps, sample_rate_hz, bit_depth, duration_ms,
                    infohash, file_index, tracker, seeders, leechers
             FROM candidate_locator
             WHERE candidate_id = ? AND is_primary = 1
             ORDER BY locator_id LIMIT 1",
        )
        .bind(candidate_id.get())
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| StoreError::not_found("candidate_locator"))?;

        let kind: String = row.try_get("kind")?;
        match kind.as_str() {
            "soulseek" => {
                let sample_rate_hz: Option<i64> = row.try_get("sample_rate_hz")?;
                let bit_depth: Option<i64> = row.try_get("bit_depth")?;
                let duration_ms: Option<i64> = row.try_get("duration_ms")?;

                Ok(Locator::SoulSeek(SoulSeekLocator {
                    peer: row.try_get("slsk_peer")?,
                    remote_path: row.try_get("slsk_remote_path")?,
                    size: ByteSize::new(bytes_from_stored(
                        row.try_get("slsk_size")?,
                        "candidate_locator.slsk_size",
                    )?),
                    has_free_slot: bool_from_stored(
                        row.try_get("slsk_slot_free")?,
                        "candidate_locator.slsk_slot_free",
                    )?,
                    queue_length: row
                        .try_get::<Option<i64>, _>("slsk_queue_len")?
                        .map(|raw| narrow_u32(raw, "candidate_locator.slsk_queue_len"))
                        .transpose()?,
                    upload_speed_bps: narrow_u32(
                        row.try_get("slsk_speed_bps")?,
                        "candidate_locator.slsk_speed_bps",
                    )?,
                    sample_rate: sample_rate_hz
                        .map(|hz| {
                            let raw = narrow_u32(hz, "candidate_locator.sample_rate_hz")?;
                            SampleRate::new(raw).ok_or_else(|| {
                                StoreError::corrupt(
                                    "candidate_locator.sample_rate_hz",
                                    format!("{raw} Hz is not a sample rate"),
                                )
                            })
                        })
                        .transpose()?,
                    bit_depth: bit_depth
                        .map(|bits| {
                            let raw = narrow_u8(bits, "candidate_locator.bit_depth")?;
                            BitDepth::new(raw).ok_or_else(|| {
                                StoreError::corrupt(
                                    "candidate_locator.bit_depth",
                                    format!("{raw} bits is not a bit depth"),
                                )
                            })
                        })
                        .transpose()?,
                    duration: duration_ms
                        .map(|ms| millis_from_stored(ms, "candidate_locator.duration_ms"))
                        .transpose()?
                        .map(DurationMs::from_millis),
                }))
            }
            "torrent" => {
                let infohash: String = row.try_get("infohash")?;
                let file_index: Option<i64> = row.try_get("file_index")?;
                let seeders: Option<i64> = row.try_get("seeders")?;
                let leechers: Option<i64> = row.try_get("leechers")?;

                Ok(Locator::Torrent(mud_core::TorrentLocator {
                    infohash: digest_from_stored::<mud_core::InfoHash>(
                        &infohash,
                        "candidate_locator.infohash",
                    )?,
                    file_index: file_index
                        .map(|index| narrow_u32(index, "candidate_locator.file_index"))
                        .transpose()?,
                    tracker: row.try_get("tracker")?,
                    seeders: seeders
                        .map(|s| narrow_u32(s, "candidate_locator.seeders"))
                        .transpose()?,
                    leechers: leechers
                        .map(|l| narrow_u32(l, "candidate_locator.leechers"))
                        .transpose()?,
                }))
            }
            other => Err(StoreError::corrupt("candidate_locator.kind", other)),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures;
    use mud_core::audio::ByteSize;
    use mud_core::candidate::{
        CandidateDisplay, CandidateFile, CandidateSource, SoulSeekLocator, TorrentLocator,
    };
    use mud_core::ids::UserId;
    use mud_core::{InfoHash, ProviderId};

    fn soulseek(peer: &str, available: bool) -> Locator {
        Locator::SoulSeek(SoulSeekLocator {
            peer: peer.into(),
            remote_path: "Artist/Album/01.flac".into(),
            size: ByteSize::new(40_000_000),
            has_free_slot: available,
            queue_length: if available { None } else { Some(3) },
            upload_speed_bps: 1_500_000,
            sample_rate: SampleRate::new(44_100),
            bit_depth: BitDepth::new(16),
            duration: Some(DurationMs::from_millis(254_000)),
        })
    }

    /// `variant` distinguishes distinct torrents of the same album: the same
    /// info hash is one torrent, so a locator is deduplicated on it.
    fn torrent(seeders: u32, variant: u8) -> Locator {
        Locator::Torrent(TorrentLocator {
            infohash: InfoHash([variant; 20]),
            file_index: None,
            tracker: Some("http://tracker/announce".into()),
            seeders: Some(seeders),
            leechers: Some(2),
        })
    }

    fn candidate(artist: &str, album: &str, year: Option<u16>, tracks: u16) -> Candidate {
        Candidate {
            candidate_id: CandidateId(0),
            dedupe_key: DedupeKey::from_release_parts(artist, album, year, tracks, 1),
            display: CandidateDisplay {
                artist: artist.into(),
                album: album.into(),
                year,
                track_count: tracks,
                disc_count: 1,
            },
            locator: soulseek("peer-a", true),
            sources: vec![],
            files: vec![CandidateFile {
                path: "Artist/Album/01.flac".into(),
                size: ByteSize::new(40_000_000),
            }],
            total_bytes: ByteSize::new(40_000_000),
        }
    }

    async fn fixture() -> (CandidateStore, SessionId) {
        let (store, user) = fixtures::store_with_a_user().await;
        let session = fixtures::seed_session(store.pool(), user).await;

        (CandidateStore::new(store.pool().clone()), session)
    }

    async fn seed_fanout(pool: &SqlitePool, session_id: SessionId) -> FanoutId {
        crate::repo::session::insert_fanout(
            pool,
            session_id,
            None,
            ProviderId::SoulSeek,
            "q",
            crate::repo::session::FanoutStatus::Running,
            0,
        )
        .await
        .expect("fanout")
    }

    #[tokio::test]
    async fn two_providers_reporting_one_album_collapse_to_one_candidate() {
        let (candidates, session_id) = fixture().await;
        let pool = candidates.pool.clone();

        let first = candidate("Radiohead", "OK Computer", Some(1997), 12);
        let second = candidate("radiohead", "ok   computer", Some(1997), 12);

        let a = candidates
            .insert_or_get(session_id, first.dedupe_key, &first, 0)
            .await
            .expect("first inserted");
        let b = candidates
            .insert_or_get(session_id, second.dedupe_key, &second, 0)
            .await
            .expect("second inserted");

        assert_eq!(a, b, "same release produced two candidates");
        assert_eq!(candidates.list(session_id).await.expect("listed").len(), 1);

        let fanout_a = seed_fanout(&pool, session_id).await;
        candidates
            .absorb_source(a, fanout_a, &soulseek("peer-a", true))
            .await
            .expect("source absorbed");
        candidates
            .absorb_source(a, fanout_a, &torrent(12, 0xaa))
            .await
            .expect("source absorbed");

        let listed = candidates.list(session_id).await.expect("listed");
        assert_eq!(listed[0].provider_count, 2);
        assert_eq!(listed[0].seeders, Some(12));
    }

    #[tokio::test]
    async fn different_releases_stay_separate() {
        let (candidates, session_id) = fixture().await;

        let ok_computer = candidate("Radiohead", "OK Computer", Some(1997), 12);
        let kid_a = candidate("Radiohead", "Kid A", Some(2000), 10);

        let a = candidates
            .insert_or_get(session_id, ok_computer.dedupe_key, &ok_computer, 0)
            .await
            .expect("inserted");
        let b = candidates
            .insert_or_get(session_id, kid_a.dedupe_key, &kid_a, 0)
            .await
            .expect("inserted");

        assert_ne!(a, b);
        assert_eq!(candidates.list(session_id).await.expect("listed").len(), 2);
    }

    #[tokio::test]
    async fn the_same_release_in_two_sessions_stays_two_candidates() {
        let (candidates, session_id) = fixture().await;
        let pool = candidates.pool.clone();
        let same = candidate("Radiohead", "OK Computer", Some(1997), 12);

        let first = candidates
            .insert_or_get(session_id, same.dedupe_key, &same, 0)
            .await
            .expect("inserted");

        let second_session = fixtures::seed_session(&pool, UserId(1)).await;

        let second = candidates
            .insert_or_get(second_session, same.dedupe_key, &same, 0)
            .await
            .expect("inserted");

        assert_ne!(first, second, "sessions shared a candidate row");
    }

    #[tokio::test]
    async fn an_available_source_becomes_primary_over_an_unavailable_one() {
        let (candidates, session_id) = fixture().await;
        let fanout = seed_fanout(&candidates.pool.clone(), session_id).await;
        let subject = candidate("Artist", "Album", None, 10);

        let candidate_id = candidates
            .insert_or_get(session_id, subject.dedupe_key, &subject, 0)
            .await
            .expect("inserted");

        candidates
            .absorb_source(candidate_id, fanout, &torrent(0, 0x01))
            .await
            .expect("dead source absorbed");
        candidates
            .absorb_source(candidate_id, fanout, &torrent(9, 0x02))
            .await
            .expect("live source absorbed");

        let primary: i64 =
            sqlx::query_scalar("SELECT locator_id FROM candidate_locator WHERE is_primary = 1")
                .fetch_one(&candidates.pool)
                .await
                .expect("one primary");

        let seeders: Option<i64> =
            sqlx::query_scalar("SELECT seeders FROM candidate_locator WHERE locator_id = ?")
                .bind(primary)
                .fetch_one(&candidates.pool)
                .await
                .expect("primary row");

        assert_eq!(seeders, Some(9), "primary locator has no seeders");
    }

    #[tokio::test]
    async fn exactly_one_locator_is_ever_primary() {
        let (candidates, session_id) = fixture().await;
        let fanout = seed_fanout(&candidates.pool.clone(), session_id).await;
        let subject = candidate("Artist", "Album", None, 10);
        let candidate_id = candidates
            .insert_or_get(session_id, subject.dedupe_key, &subject, 0)
            .await
            .expect("inserted");

        for peer in ["a", "b", "c"] {
            candidates
                .absorb_source(candidate_id, fanout, &soulseek(peer, true))
                .await
                .expect("absorbed");
        }

        let primaries: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM candidate_locator WHERE is_primary = 1 AND candidate_id = ?",
        )
        .bind(candidate_id.get())
        .fetch_one(&candidates.pool)
        .await
        .expect("count");

        assert_eq!(primaries, 1);
    }

    #[tokio::test]
    async fn only_one_source_row_per_fanout_locator_pair() {
        let (candidates, session_id) = fixture().await;
        let fanout = seed_fanout(&candidates.pool.clone(), session_id).await;
        let subject = candidate("Artist", "Album", None, 10);
        let candidate_id = candidates
            .insert_or_get(session_id, subject.dedupe_key, &subject, 0)
            .await
            .expect("inserted");

        let locator = soulseek("peer-a", true);
        let first = candidates
            .absorb_source(candidate_id, fanout, &locator)
            .await
            .expect("first");
        candidates
            .absorb_source(candidate_id, fanout, &locator)
            .await
            .expect("repeat");

        let rows: Vec<i64> = sqlx::query_scalar(
            "SELECT locator_id FROM candidate_source WHERE fanout_id = ? ORDER BY locator_id",
        )
        .bind(fanout.get())
        .fetch_all(&candidates.pool)
        .await
        .expect("rows");

        assert_eq!(rows, vec![first.get()]);
    }

    #[tokio::test]
    async fn stores_the_file_list() {
        let (candidates, session_id) = fixture().await;
        let mut subject = candidate("Artist", "Album", None, 2);
        subject.files.push(CandidateFile {
            path: "Artist/Album/02.flac".into(),
            size: ByteSize::new(41_000_000),
        });

        let candidate_id = candidates
            .insert_or_get(session_id, subject.dedupe_key, &subject, 0)
            .await
            .expect("inserted");
        candidates
            .insert_files(candidate_id, &subject)
            .await
            .expect("files");

        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM candidate_file WHERE candidate_id = ?")
                .bind(candidate_id.get())
                .fetch_one(&candidates.pool)
                .await
                .expect("count");
        assert_eq!(count, 2);
    }

    #[tokio::test]
    async fn records_rejection_reasons_on_the_fanout() {
        let (candidates, session_id) = fixture().await;
        let fanout = seed_fanout(&candidates.pool.clone(), session_id).await;

        let counts = RejectionCounts {
            not_flac: 40,
            bit_depth_too_low: 3,
            too_large: 1,
            no_available_source: 12,
            unparsable: 0,
        };
        candidates
            .record_rejections(session_id, fanout, &counts)
            .await
            .expect("recorded");

        let error: String =
            sqlx::query_scalar("SELECT error FROM provider_fanout WHERE fanout_id = ?")
                .bind(fanout.get())
                .fetch_one(&candidates.pool)
                .await
                .expect("error column");

        assert!(error.contains("not_flac=40"), "{error}");
        assert!(error.contains("unavailable=12"), "{error}");
    }

    #[tokio::test]
    async fn a_candidate_absorbs_sources_in_memory() {
        let mut subject = candidate("Artist", "Album", None, 1);
        subject.locator = torrent(0, 0x03);
        subject.sources = vec![CandidateSource {
            fanout_id: FanoutId(1),
            provider: ProviderId::TorrentMeta,
            locator: torrent(0, 0x03),
        }];

        subject.absorb(CandidateSource {
            fanout_id: FanoutId(2),
            provider: ProviderId::TorrentMeta,
            locator: torrent(5, 0x04),
        });

        assert_eq!(subject.provider_count(), 2);
        assert_eq!(subject.locator, torrent(5, 0x04));
    }
}
#[cfg(test)]
mod load_tests {
    use super::*;
    use crate::fixtures;
    use mud_core::ProviderId;
    use mud_core::audio::ByteSize;
    use mud_core::candidate::{Candidate, CandidateDisplay, CandidateFile, SoulSeekLocator};
    use mud_core::ids::CandidateId;

    fn source_candidate(peer: &str) -> Candidate {
        Candidate {
            candidate_id: CandidateId(0),
            dedupe_key: DedupeKey::for_source(peer, "Radiohead/OK Computer"),
            display: CandidateDisplay {
                artist: "Radiohead".into(),
                album: "OK Computer".into(),
                year: Some(1997),
                track_count: 2,
                disc_count: 1,
            },
            locator: Locator::SoulSeek(SoulSeekLocator {
                peer: peer.into(),
                remote_path: "Radiohead/OK Computer".into(),
                size: ByteSize::new(82_000_000),
                has_free_slot: true,
                queue_length: None,
                upload_speed_bps: 2_000_000,
                sample_rate: SampleRate::new(96_000),
                bit_depth: BitDepth::new(24),
                duration: Some(DurationMs::from_millis(254_000)),
            }),
            sources: Vec::new(),
            files: vec![
                CandidateFile {
                    path: "Radiohead/OK Computer/01 - Airbag.flac".into(),
                    size: ByteSize::new(40_000_000),
                },
                CandidateFile {
                    path: "Radiohead/OK Computer/02 - Paranoid Android.flac".into(),
                    size: ByteSize::new(42_000_000),
                },
            ],
            total_bytes: ByteSize::new(82_000_000),
        }
    }

    #[tokio::test]
    async fn a_stored_candidate_round_trips_through_load() {
        let (store, user) = fixtures::store_with_a_user().await;
        let session = fixtures::seed_session(store.pool(), user).await;
        let candidates = CandidateStore::new(store.pool().clone());

        let source = source_candidate("alice");
        let candidate_id = candidates
            .insert_or_get(session, source.dedupe_key, &source, 0)
            .await
            .expect("inserted");

        let fanout = crate::repo::session::insert_fanout(
            store.pool(),
            session,
            None,
            ProviderId::SoulSeek,
            "q",
            crate::repo::session::FanoutStatus::Running,
            0,
        )
        .await
        .expect("fanout");

        candidates
            .absorb_source(candidate_id, fanout, &source.locator)
            .await
            .expect("source");
        candidates
            .insert_files(candidate_id, &source)
            .await
            .expect("files");

        let loaded = candidates.load(candidate_id).await.expect("loaded");

        assert_eq!(loaded.candidate_id, candidate_id);
        assert_eq!(loaded.display_artist, "Radiohead");
        assert_eq!(loaded.display_album, "OK Computer");
        assert_eq!(loaded.files.len(), 2);

        match &loaded.locator {
            Locator::SoulSeek(locator) => {
                assert_eq!(locator.peer, "alice");
                assert_eq!(locator.sample_rate, SampleRate::new(96_000));
                assert_eq!(locator.bit_depth, BitDepth::new(24));
                assert!(locator.has_free_slot);
            }
            Locator::Torrent(_) => panic!("a soulseek source loaded as a torrent"),
        }
    }

    #[tokio::test]
    async fn loading_an_unknown_candidate_is_reported_not_defaulted() {
        let (store, user) = fixtures::store_with_a_user().await;
        let candidates = CandidateStore::new(store.pool().clone());

        assert!(matches!(
            candidates.load(CandidateId(9_999)).await,
            Err(StoreError::NotFound { table: "candidate" })
        ));
        let _ = user;
    }
}
