//! Acquisitions, assets, and their lifecycle.

use aldo_core::audio::ByteSize;
use aldo_core::candidate::CandidateFile;
use aldo_core::ids::{AcquisitionId, AssetId, CandidateId, InfoHash, Sha256, UserId};
use sqlx::Row;
use sqlx::SqlitePool;

use crate::error::{
    StoreError, bytes_from_stored, digest_from_stored, narrow_id, narrow_u32, now_ms,
    ordinal_from_index, widen_bytes,
};

/// A non-negative, finite ratio. `qb_ratio` is how much a torrent has downloaded
/// relative to its size, so a negative or infinite value is meaningless and is
/// refused here rather than stored and clamped by a SQL constraint.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ratio(f64);

impl Ratio {
    /// # Errors
    /// Returns `None` for a negative, infinite, or NaN value.
    pub fn new(value: f64) -> Option<Self> {
        (value.is_finite() && value >= 0.0).then_some(Self(value))
    }

    #[must_use]
    pub fn as_f64(self) -> f64 {
        self.0
    }
}

/// A fraction of a whole, so 0.0 through 1.0. qbittorrent reports download
/// progress this way; anything outside the closed interval is a different
/// quantity, not a progress value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UnitInterval(f64);

impl UnitInterval {
    /// # Errors
    /// Returns `None` for a value outside `0.0..=1.0`, or for a non-finite one.
    pub fn new(value: f64) -> Option<Self> {
        (value.is_finite() && (0.0..=1.0).contains(&value)).then_some(Self(value))
    }

    #[must_use]
    pub fn as_f64(self) -> f64 {
        self.0
    }
}

stored_enum!(
    /// How bytes are moved.
    Engine,
    "acquisition.engine" => {
        /// A direct peer connection over the SoulSeek protocol.
        SoulSeek => "soulseek",
        /// qbittorrent-nox, driven over its WebUI HTTP API.
        Qbittorrent => "qbittorrent",
    }
);

stored_enum!(
    /// Acquisition lifecycle. Only `Verified` means the files are final and
    /// tagged.
    AcquisitionStatus,
    "acquisition.status" => {
        Queued => "queued",
        Downloading => "downloading",
        Partial => "partial",
        Downloaded => "downloaded",
        Analyzed => "analyzed",
        Matched => "matched",
        Tagged => "tagged",
        Failed => "failed",
        Cancelled => "cancelled",
    }
);

impl AcquisitionStatus {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Tagged | Self::Failed | Self::Cancelled)
    }
}

stored_enum!(
    /// Where one asset has got to.
    AssetState,
    "asset.state" => {
        Expected => "expected",
        Partial => "partial",
        Complete => "complete",
        Verified => "verified",
    }
);

#[derive(Debug, Clone)]
pub struct AcquisitionRow {
    pub acquisition_id: AcquisitionId,
    pub candidate_id: CandidateId,
    pub engine: Engine,
    pub status: AcquisitionStatus,
    /// The qbittorrent task hash: a v1 info hash, never an arbitrary string.
    pub qb_hash: Option<InfoHash>,
    pub bytes_done: ByteSize,
    pub bytes_total: ByteSize,
    pub speed_bps: u32,
    pub asset_count: u32,
    pub complete_assets: u32,
}

#[derive(Debug, Clone)]
pub struct AssetRow {
    pub asset_id: AssetId,
    pub acquisition_id: AcquisitionId,
    pub ordinal: u32,
    pub rel_path: String,
    pub bytes: ByteSize,
    pub state: AssetState,
    /// The SHA-256 of the audio stream, for deduplicating an identical rip.
    pub sha256_audio: Option<Sha256>,
}

#[derive(Debug, Clone)]
pub struct AcquisitionStore {
    pool: SqlitePool,
}

impl AcquisitionStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn create(
        &self,
        candidate_id: CandidateId,
        user_id: UserId,
        engine: Engine,
        bytes_total: ByteSize,
        now_ms: i64,
    ) -> Result<AcquisitionId, StoreError> {
        sqlx::query(
            "INSERT INTO acquisition (candidate_id, user_id, engine, status, bytes_total, created_at)
             VALUES (?, ?, ?, ?, ?, ?)
             ON CONFLICT (candidate_id, engine) DO NOTHING",
        )
        .bind(candidate_id.get())
        .bind(user_id.get())
        .bind(engine.as_str())
        .bind(AcquisitionStatus::Queued.as_str())
        .bind(widen_bytes(bytes_total.as_u64(), "acquisition.bytes_total")?)
        .bind(now_ms)
        .execute(&self.pool)
        .await?;

        let raw: i64 = sqlx::query_scalar(
            "SELECT acquisition_id FROM acquisition WHERE candidate_id = ? AND engine = ?",
        )
        .bind(candidate_id.get())
        .bind(engine.as_str())
        .fetch_one(&self.pool)
        .await?;
        narrow_id::<AcquisitionId>(raw, "acquisition_id")
    }

    /// Inserts the expected asset list before any bytes arrive. A torrent is a
    /// directory of files, not a file, so the list comes from the metainfo.
    ///
    /// `files` is a slice of [`CandidateFile`] rather than `&[(String, u64)]`:
    /// a bare tuple makes the order of the path and the byte count a guess, and
    /// leaves the size as a number with no unit.
    pub async fn expect_assets(
        &self,
        acquisition_id: AcquisitionId,
        files: &[CandidateFile],
    ) -> Result<Vec<AssetId>, StoreError> {
        let mut ids = Vec::with_capacity(files.len());
        for (index, file) in files.iter().enumerate() {
            let ordinal = ordinal_from_index(index, "asset.ordinal")?;
            // RETURNING, not `last_insert_rowid()`: the pool may hand a
            // follow-up query a different connection.
            let raw: i64 = sqlx::query_scalar(
                "INSERT INTO asset (acquisition_id, ordinal, rel_path, bytes, state)
                 VALUES (?, ?, ?, ?, ?) RETURNING asset_id",
            )
            .bind(acquisition_id.get())
            .bind(i64::from(ordinal))
            .bind(&file.path)
            .bind(widen_bytes(file.size.as_u64(), "asset.bytes")?)
            .bind(AssetState::Expected.as_str())
            .fetch_one(&self.pool)
            .await?;

            ids.push(narrow_id::<AssetId>(raw, "asset_id")?);
        }
        Ok(ids)
    }

    pub async fn set_status(
        &self,
        acquisition_id: AcquisitionId,
        status: AcquisitionStatus,
    ) -> Result<(), StoreError> {
        sqlx::query("UPDATE acquisition SET status = ? WHERE acquisition_id = ?")
            .bind(status.as_str())
            .bind(acquisition_id.get())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn attach_qbittorrent(
        &self,
        acquisition_id: AcquisitionId,
        qb_hash: InfoHash,
        savepath: &str,
        ratio: Ratio,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "UPDATE acquisition SET qb_hash = ?, qb_savepath = ?, qb_ratio = ? WHERE acquisition_id = ?",
        )
        .bind(qb_hash.to_hex())
        .bind(savepath)
        .bind(ratio.as_f64())
        .bind(acquisition_id.get())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn record_qb_event(
        &self,
        acquisition_id: AcquisitionId,
        kind: &str,
        qb_state: &str,
        progress: UnitInterval,
        now_ms: i64,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO qbittorrent_event (acquisition_id, kind, qb_state, progress, received_at)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(acquisition_id.get())
        .bind(kind)
        .bind(qb_state)
        .bind(progress.as_f64())
        .bind(now_ms)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn update_progress(
        &self,
        acquisition_id: AcquisitionId,
        bytes_done: ByteSize,
        speed_bps: u32,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "UPDATE acquisition SET bytes_done = ?, speed_bps = ?, started_at = COALESCE(started_at, ?)
             WHERE acquisition_id = ?",
        )
        .bind(widen_bytes(bytes_done.as_u64(), "acquisition.bytes_done")?)
        .bind(i64::from(speed_bps))
        .bind(now_ms())
        .bind(acquisition_id.get())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn mark_asset_complete(
        &self,
        asset_id: AssetId,
        state: AssetState,
        sha256_audio: Option<Sha256>,
    ) -> Result<(), StoreError> {
        sqlx::query("UPDATE asset SET state = ?, sha256_audio = ? WHERE asset_id = ?")
            .bind(state.as_str())
            .bind(sha256_audio.map(Sha256::to_hex))
            .bind(asset_id.get())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn link_asset_to_track(
        &self,
        asset_id: AssetId,
        release_track_id: aldo_core::ids::ReleaseTrackId,
    ) -> Result<(), StoreError> {
        sqlx::query("UPDATE asset SET release_track_id = ? WHERE asset_id = ?")
            .bind(release_track_id.get())
            .bind(asset_id.get())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn get(&self, acquisition_id: AcquisitionId) -> Result<AcquisitionRow, StoreError> {
        let row = sqlx::query(
            "SELECT a.acquisition_id, a.candidate_id, a.engine, a.status, a.qb_hash,
                    a.bytes_done, a.bytes_total, a.speed_bps,
                    (SELECT COUNT(*) FROM asset WHERE acquisition_id = a.acquisition_id) AS asset_count,
                    (SELECT COUNT(*) FROM asset
                     WHERE acquisition_id = a.acquisition_id AND state IN ('complete', 'verified'))
                        AS complete_assets
             FROM acquisition a WHERE a.acquisition_id = ?",
        )
        .bind(acquisition_id.get())
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| StoreError::not_found("acquisition"))?;

        let engine_text: String = row.try_get("engine")?;
        let status_text: String = row.try_get("status")?;
        let raw_id: i64 = row.try_get("acquisition_id")?;
        let raw_candidate: i64 = row.try_get("candidate_id")?;
        let raw_hash: Option<String> = row.try_get("qb_hash")?;

        Ok(AcquisitionRow {
            acquisition_id: narrow_id::<AcquisitionId>(raw_id, "acquisition_id")?,
            candidate_id: narrow_id::<CandidateId>(raw_candidate, "candidate_id")?,
            engine: Engine::parse(&engine_text)?,
            status: AcquisitionStatus::parse(&status_text)?,
            qb_hash: raw_hash
                .map(|hex| digest_from_stored::<InfoHash>(&hex, "acquisition.qb_hash"))
                .transpose()?,
            bytes_done: ByteSize::new(bytes_from_stored(
                row.try_get("bytes_done")?,
                "acquisition.bytes_done",
            )?),
            bytes_total: ByteSize::new(bytes_from_stored(
                row.try_get("bytes_total")?,
                "acquisition.bytes_total",
            )?),
            speed_bps: narrow_u32(row.try_get("speed_bps")?, "acquisition.speed_bps")?,
            asset_count: narrow_u32(row.try_get("asset_count")?, "acquisition.asset_count")?,
            complete_assets: narrow_u32(
                row.try_get("complete_assets")?,
                "acquisition.complete_assets",
            )?,
        })
    }

    pub async fn assets(&self, acquisition_id: AcquisitionId) -> Result<Vec<AssetRow>, StoreError> {
        let rows = sqlx::query(
            "SELECT asset_id, acquisition_id, ordinal, rel_path, bytes, state, sha256_audio
             FROM asset WHERE acquisition_id = ? ORDER BY ordinal",
        )
        .bind(acquisition_id.get())
        .fetch_all(&self.pool)
        .await?;

        rows.iter().map(asset_row).collect()
    }

    /// Finds a completed asset by its audio digest, for deduplicating the same
    /// rip downloaded twice.
    pub async fn find_by_audio_digest(
        &self,
        digest: &Sha256,
    ) -> Result<Option<AssetRow>, StoreError> {
        let row = sqlx::query(
            "SELECT asset_id, acquisition_id, ordinal, rel_path, bytes, state, sha256_audio
             FROM asset WHERE sha256_audio = ? LIMIT 1",
        )
        .bind(digest.to_hex())
        .fetch_optional(&self.pool)
        .await?;

        row.as_ref().map(asset_row).transpose()
    }

    pub async fn list_active(&self, user_id: UserId) -> Result<Vec<AcquisitionRow>, StoreError> {
        let ids: Vec<i64> = sqlx::query_scalar(
            "SELECT acquisition_id FROM acquisition
             WHERE user_id = ? AND status NOT IN ('tagged', 'failed', 'cancelled')
             ORDER BY created_at",
        )
        .bind(user_id.get())
        .fetch_all(&self.pool)
        .await?;

        let mut rows = Vec::with_capacity(ids.len());
        for raw in ids {
            let id = narrow_id::<AcquisitionId>(raw, "acquisition_id")?;
            rows.push(self.get(id).await?);
        }
        Ok(rows)
    }
}

/// Maps one `asset` row. Shared by `assets` and `find_by_audio_digest` so both
/// narrow the same columns the same way.
fn asset_row(row: &sqlx::sqlite::SqliteRow) -> Result<AssetRow, StoreError> {
    let state_text: String = row.try_get("state")?;
    let raw_id: i64 = row.try_get("asset_id")?;
    let raw_acquisition: i64 = row.try_get("acquisition_id")?;
    let raw_digest: Option<String> = row.try_get("sha256_audio")?;

    Ok(AssetRow {
        asset_id: narrow_id::<AssetId>(raw_id, "asset_id")?,
        acquisition_id: narrow_id::<AcquisitionId>(raw_acquisition, "acquisition_id")?,
        ordinal: narrow_u32(row.try_get("ordinal")?, "asset.ordinal")?,
        rel_path: row.try_get("rel_path")?,
        bytes: ByteSize::new(bytes_from_stored(row.try_get("bytes")?, "asset.bytes")?),
        state: AssetState::parse(&state_text)?,
        sha256_audio: raw_digest
            .map(|hex| digest_from_stored::<Sha256>(&hex, "asset.sha256_audio"))
            .transpose()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures;

    /// The store keeps its pool private, so a raw count query in a test needs a
    /// handle to it.
    fn candidates_pool(store: &AcquisitionStore) -> &SqlitePool {
        &store.pool
    }

    async fn fixture() -> (AcquisitionStore, CandidateId, UserId) {
        let (store, user) = fixtures::store_with_a_user().await;
        let session = fixtures::seed_session(store.pool(), user).await;
        let candidate = fixtures::seed_candidate(store.pool(), session).await;

        (AcquisitionStore::new(store.pool().clone()), candidate, user)
    }

    fn sample_files() -> Vec<CandidateFile> {
        vec![
            CandidateFile {
                path: "Artist/Album/01 - One.flac".into(),
                size: ByteSize::new(40_000_000),
            },
            CandidateFile {
                path: "Artist/Album/02 - Two.flac".into(),
                size: ByteSize::new(42_000_000),
            },
        ]
    }

    fn infohash(byte: u8) -> InfoHash {
        InfoHash([byte; 20])
    }

    #[tokio::test]
    async fn creates_an_acquisition_with_an_expected_file_list() {
        let (store, candidate, user) = fixture().await;
        let acquisition_id = store
            .create(
                candidate,
                user,
                Engine::Qbittorrent,
                ByteSize::new(82_000_000),
                0,
            )
            .await
            .expect("created");

        let asset_ids = store
            .expect_assets(acquisition_id, &sample_files())
            .await
            .expect("assets expected");
        assert_eq!(asset_ids.len(), 2);

        let row = store.get(acquisition_id).await.expect("read back");
        assert_eq!(row.status, AcquisitionStatus::Queued);
        assert_eq!(row.asset_count, 2);
        assert_eq!(row.complete_assets, 0);
        assert_eq!(row.bytes_total, ByteSize::new(82_000_000));
        assert!(!row.status.is_terminal());
    }

    #[tokio::test]
    async fn creating_twice_returns_the_same_acquisition() {
        let (store, candidate, user) = fixture().await;

        let first = store
            .create(candidate, user, Engine::SoulSeek, ByteSize::new(100), 0)
            .await
            .expect("created");
        let second = store
            .create(candidate, user, Engine::SoulSeek, ByteSize::new(100), 0)
            .await
            .expect("created again");

        assert_eq!(first, second);
    }

    #[tokio::test]
    async fn the_same_candidate_can_use_two_engines() {
        let (store, candidate, user) = fixture().await;

        let soulseek = store
            .create(candidate, user, Engine::SoulSeek, ByteSize::new(100), 0)
            .await
            .expect("created");
        let torrent = store
            .create(candidate, user, Engine::Qbittorrent, ByteSize::new(100), 0)
            .await
            .expect("created");

        assert_ne!(soulseek, torrent);
    }

    #[tokio::test]
    async fn tracks_download_progress() {
        let (store, candidate, user) = fixture().await;
        let id = store
            .create(
                candidate,
                user,
                Engine::Qbittorrent,
                ByteSize::new(82_000_000),
                0,
            )
            .await
            .expect("created");

        store
            .update_progress(id, ByteSize::new(41_000_000), 2_500_000)
            .await
            .expect("progress");

        let row = store.get(id).await.expect("read back");
        assert_eq!(row.bytes_done, ByteSize::new(41_000_000));
        assert_eq!(row.speed_bps, 2_500_000);
    }

    #[tokio::test]
    async fn counts_only_completed_assets() {
        let (store, candidate, user) = fixture().await;
        let id = store
            .create(
                candidate,
                user,
                Engine::Qbittorrent,
                ByteSize::new(82_000_000),
                0,
            )
            .await
            .expect("created");
        let assets = store
            .expect_assets(id, &sample_files())
            .await
            .expect("assets");

        store
            .mark_asset_complete(assets[0], AssetState::Complete, None)
            .await
            .expect("completed");

        assert_eq!(store.get(id).await.expect("row").complete_assets, 1);

        store
            .mark_asset_complete(assets[1], AssetState::Verified, Some(Sha256([6; 32])))
            .await
            .expect("verified");

        assert_eq!(store.get(id).await.expect("row").complete_assets, 2);
    }

    #[tokio::test]
    async fn a_partial_asset_does_not_count_as_complete() {
        let (store, candidate, user) = fixture().await;
        let id = store
            .create(
                candidate,
                user,
                Engine::Qbittorrent,
                ByteSize::new(82_000_000),
                0,
            )
            .await
            .expect("created");
        let assets = store
            .expect_assets(id, &sample_files())
            .await
            .expect("assets");

        store
            .mark_asset_complete(assets[0], AssetState::Partial, None)
            .await
            .expect("marked");

        assert_eq!(store.get(id).await.expect("row").complete_assets, 0);
    }

    #[tokio::test]
    async fn finds_a_previous_download_by_audio_digest() {
        let (store, candidate, user) = fixture().await;
        let id = store
            .create(
                candidate,
                user,
                Engine::SoulSeek,
                ByteSize::new(82_000_000),
                0,
            )
            .await
            .expect("created");
        let assets = store
            .expect_assets(id, &sample_files())
            .await
            .expect("assets");

        let digest = Sha256([9; 32]);
        store
            .mark_asset_complete(assets[1], AssetState::Verified, Some(digest))
            .await
            .expect("verified");

        let found = store
            .find_by_audio_digest(&digest)
            .await
            .expect("query ran")
            .expect("digest found");
        assert_eq!(found.asset_id, assets[1]);
        assert_eq!(found.sha256_audio, Some(digest));

        assert!(
            store
                .find_by_audio_digest(&Sha256([8; 32]))
                .await
                .expect("query ran")
                .is_none(),
            "unknown digest returned a row"
        );
    }

    #[tokio::test]
    async fn active_acquisitions_exclude_finished_ones() {
        let (store, candidate, user) = fixture().await;
        let active = store
            .create(candidate, user, Engine::SoulSeek, ByteSize::new(100), 0)
            .await
            .expect("created");
        let finished = store
            .create(candidate, user, Engine::Qbittorrent, ByteSize::new(100), 0)
            .await
            .expect("created");
        store
            .set_status(finished, AcquisitionStatus::Tagged)
            .await
            .expect("tagged");

        let listed = store.list_active(user).await.expect("listed");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].acquisition_id, active);
    }

    #[tokio::test]
    async fn records_qbittorrent_progress_events() {
        let (store, candidate, user) = fixture().await;
        let id = store
            .create(candidate, user, Engine::Qbittorrent, ByteSize::new(100), 0)
            .await
            .expect("created");
        let hash = infohash(0xab);
        store
            .attach_qbittorrent(id, hash, "/staging/1", Ratio::new(0.0).expect("ratio"))
            .await
            .expect("attached");

        for progress in [0.5, 1.0] {
            store
                .record_qb_event(
                    id,
                    "torrent_finished",
                    "downloading",
                    UnitInterval::new(progress).expect("in range"),
                    0,
                )
                .await
                .expect("event recorded");
        }

        let row = store.get(id).await.expect("row");
        assert_eq!(row.qb_hash, Some(hash));

        let events: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM qbittorrent_event WHERE acquisition_id = ?")
                .bind(id.get())
                .fetch_one(candidates_pool(&store))
                .await
                .expect("count");
        assert_eq!(events, 2);
    }

    #[tokio::test]
    async fn refuses_an_out_of_range_qb_progress() {
        let (store, candidate, user) = fixture().await;
        let id = store
            .create(candidate, user, Engine::Qbittorrent, ByteSize::new(100), 0)
            .await
            .expect("created");

        // The type already refuses the value, so the schema check is only
        // reachable by a statement this crate does not own. It still has to hold.
        let result = sqlx::query(
            "INSERT INTO qbittorrent_event (acquisition_id, kind, qb_state, progress, received_at)
             VALUES (?, 'torrent_finished', 'downloading', 1.5, 0)",
        )
        .bind(id.get())
        .execute(candidates_pool(&store))
        .await;

        assert!(result.is_err(), "progress above 1.0 was accepted");
    }

    #[test]
    fn a_progress_value_outside_the_unit_interval_cannot_be_constructed() {
        for progress in [1.5, -0.1, f64::NAN, f64::INFINITY] {
            assert!(
                UnitInterval::new(progress).is_none(),
                "{progress} was accepted as a fraction"
            );
        }
        assert!((UnitInterval::new(1.0).expect("valid").as_f64() - 1.0).abs() < f64::EPSILON);
        // A seeding ratio above 1.0 is normal, so it is a different type.
        assert!(Ratio::new(2.5).is_some());
        assert!(Ratio::new(-0.5).is_none());
        assert!(Ratio::new(f64::NAN).is_none());
    }

    #[tokio::test]
    async fn an_asset_ordinal_that_the_schema_refuses_is_rejected() {
        let (store, candidate, user) = fixture().await;
        let id = store
            .create(candidate, user, Engine::Qbittorrent, ByteSize::new(100), 0)
            .await
            .expect("created");

        let result = sqlx::query(
            "INSERT INTO asset (acquisition_id, ordinal, rel_path, bytes, state)
             VALUES (?, -1, 'x.flac', 1, 'expected')",
        )
        .bind(id.get())
        .execute(candidates_pool(&store))
        .await;

        assert!(result.is_err(), "a negative ordinal was accepted");
    }

    #[tokio::test]
    async fn a_missing_acquisition_is_reported_not_defaulted() {
        let (store, _, _) = fixture().await;
        assert!(matches!(
            store.get(AcquisitionId(9_999)).await,
            Err(StoreError::NotFound {
                table: "acquisition"
            })
        ));
    }

    #[test]
    fn enums_round_trip_through_their_stored_forms() {
        for engine in [Engine::SoulSeek, Engine::Qbittorrent] {
            assert_eq!(Engine::parse(engine.as_str()).ok(), Some(engine));
        }
        for status in [
            AcquisitionStatus::Queued,
            AcquisitionStatus::Downloading,
            AcquisitionStatus::Partial,
            AcquisitionStatus::Downloaded,
            AcquisitionStatus::Analyzed,
            AcquisitionStatus::Matched,
            AcquisitionStatus::Tagged,
            AcquisitionStatus::Failed,
            AcquisitionStatus::Cancelled,
        ] {
            assert_eq!(AcquisitionStatus::parse(status.as_str()).ok(), Some(status));
        }
        for state in [
            AssetState::Expected,
            AssetState::Partial,
            AssetState::Complete,
            AssetState::Verified,
        ] {
            assert_eq!(AssetState::parse(state.as_str()).ok(), Some(state));
        }
    }

    #[test]
    fn an_unknown_stored_value_is_reported_not_guessed() {
        assert!(matches!(
            Engine::parse("aria2"),
            Err(StoreError::Corrupt {
                column: "acquisition.engine",
                ..
            })
        ));
        assert!(matches!(
            AcquisitionStatus::parse("paused"),
            Err(StoreError::Corrupt {
                column: "acquisition.status",
                ..
            })
        ));
        assert!(matches!(
            AssetState::parse("corrupt"),
            Err(StoreError::Corrupt {
                column: "asset.state",
                ..
            })
        ));
    }

    #[tokio::test]
    async fn a_stored_hash_that_is_not_a_hash_is_reported_rather_than_kept() {
        let (store, candidate, user) = fixture().await;
        let id = store
            .create(candidate, user, Engine::Qbittorrent, ByteSize::new(100), 0)
            .await
            .expect("created");

        // The column is length-checked, so this is unreachable through the
        // typed write path; the read still has to reject a bad digest.
        sqlx::query(
            "INSERT INTO asset (acquisition_id, ordinal, rel_path, bytes, state, sha256_audio)
             VALUES (?, 0, 'x.flac', 1, 'verified', ?)",
        )
        .bind(id.get())
        .bind("z".repeat(64))
        .execute(candidates_pool(&store))
        .await
        .expect("row inserted");

        assert!(matches!(
            store.assets(id).await,
            Err(StoreError::Corrupt {
                column: "asset.sha256_audio",
                ..
            })
        ));
    }
}
