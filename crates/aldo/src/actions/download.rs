//! What a download does, independent of how it is shown.
//!
//! The CLI prints each step; the TUI draws bars. Both call [`plan`], [`metadata`]
//! and [`execute`], so a download moves the same files, records the same
//! acquisition and writes the same tags either way. Rendering lives in the
//! callers, and every step is reported as a [`DownloadEvent`].

use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;

use aldo_core::audio::ByteSize;
use aldo_core::candidate::{CandidateFile, Locator};
use aldo_core::ids::{AcquisitionId, AssetId, CandidateId, DiscogsReleaseId};
use aldo_core::release::{TrackKind, TrackPosition};
use aldo_soulseek::{DownloadProgress, SoulSeekProvider};
use aldo_store::repo::{
    AcquisitionStatus, AcquisitionStore, AssetState, CandidateStore, CatalogStore, Engine,
    LOCAL_USER, LoadedCandidate,
};
use tokio::sync::mpsc;

use crate::settings::{Settings, SettingsError};

/// What will be downloaded, resolved before any network access so a front end
/// can show it and ask before committing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadPlan {
    pub candidate_id: CandidateId,
    pub artist: String,
    pub album: String,
    pub locator: Locator,
    pub files: Vec<CandidateFile>,
    pub total_bytes: ByteSize,
}

impl DownloadPlan {
    /// The peer to fetch from, or `None` for an engine that is not wired up.
    #[must_use]
    pub fn peer(&self) -> Option<&str> {
        match &self.locator {
            Locator::SoulSeek(locator) => Some(&locator.peer),
            Locator::Torrent(_) => None,
        }
    }

    /// `Artist - Album`, with placeholders when the peer's folder had no name.
    #[must_use]
    pub fn title(&self) -> String {
        format!(
            "{} - {}",
            or_unknown(&self.artist, "Unknown Artist"),
            or_unknown(&self.album, "Unknown Album")
        )
    }
}

fn or_unknown<'a>(value: &'a str, fallback: &'a str) -> &'a str {
    if value.is_empty() { fallback } else { value }
}

/// Everything a download needs, resolved before anything is transferred.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prepared {
    pub plan: DownloadPlan,
    /// The tags to write, or `None` to download untagged.
    pub tags: Option<TagMetadata>,
    /// A reason the download will be untagged, when it is not a refusal.
    pub warning: Option<String>,
}

/// Records which Discogs pressing a candidate should be tagged with.
///
/// A search can match several pressings of one release; this is how a front end
/// records the user's choice before the download resolves its tags.
///
/// # Errors
/// Fails when the store cannot be opened or the candidate is gone.
pub async fn choose_release(
    settings: &Settings,
    candidate_id: CandidateId,
    release_id: u32,
) -> Result<(), DownloadError> {
    let store = settings.open_store().await?;
    let candidates = CandidateStore::new(store.pool().clone());

    candidates
        .set_selected_discogs_release(candidate_id, DiscogsReleaseId(release_id))
        .await?;
    Ok(())
}

/// Resolves a plan and its metadata for a front end that will ask before
/// starting.
///
/// A release whose track count does not match the folder is a *warning* here and
/// a refusal in the CLI: the CLI must not silently tag a partial folder, while
/// the interface can show the mismatch and let the user continue untagged.
///
/// # Errors
/// Fails for the same reasons as [`plan`] and [`metadata`], except for the
/// track-count mismatch.
pub async fn prepare(
    settings: &Settings,
    candidate_id: CandidateId,
    discogs_release: Option<u32>,
) -> Result<Prepared, DownloadError> {
    let plan = plan(settings, candidate_id).await?;

    match metadata(settings, &plan, discogs_release, false).await {
        Ok(tags) => Ok(Prepared {
            plan,
            tags,
            warning: None,
        }),
        Err(DownloadError::TrackCountMismatch { files, tracks }) => Ok(Prepared {
            plan,
            tags: None,
            warning: Some(format!(
                "the matched release lists {tracks} tracks, but this folder has {files} files; \
                 it will download without tags"
            )),
        }),
        Err(error) => Err(error),
    }
}

/// The metadata a completed download will be tagged with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagMetadata {
    pub discogs_release_id: u32,
    pub album: String,
    pub artist: String,
    pub year: Option<u16>,
    pub tracks: Vec<(String, TrackPosition)>,
}

/// What a finished download did.
#[derive(Debug, Clone)]
pub struct DownloadReport {
    pub files: usize,
    pub target_dir: PathBuf,
    pub tagged: bool,
}

/// One step of a download, for a front end that draws progress.
#[derive(Debug, Clone)]
pub enum DownloadEvent {
    /// The client is logging in, which can take a few seconds.
    Connecting {
        username: String,
    },
    /// A file already in staging at the right size was reused.
    FileSkipped {
        index: usize,
        total: usize,
        name: String,
    },
    FileStarted {
        index: usize,
        total: usize,
        name: String,
        size: ByteSize,
    },
    /// A progress tick: bytes for the file currently transferring.
    Progress {
        bytes_done: u64,
        bytes_total: u64,
        speed_bps: u32,
        queued: bool,
    },
    FileDone {
        index: usize,
    },
    Finished(DownloadReport),
}

/// Resolves a 1-based result index from today's search into a candidate.
///
/// # Errors
/// Returns [`DownloadError::DailyIndexNotFound`] when the index has expired at
/// UTC midnight, and `CandidateNotFound` when the row is missing.
pub async fn resolve_daily_index(
    settings: &Settings,
    result_index: i64,
) -> Result<CandidateId, DownloadError> {
    let store = settings.open_store().await?;
    let candidates = CandidateStore::new(store.pool().clone());
    let utc_day = aldo_store::now_ms().div_euclid(86_400_000);

    match candidates.resolve_daily_index(utc_day, result_index).await {
        Ok(candidate_id) => Ok(candidate_id),
        Err(aldo_store::StoreError::NotFound { .. }) => {
            Err(DownloadError::DailyIndexNotFound(result_index))
        }
        Err(error) => Err(DownloadError::Store(error)),
    }
}

/// Loads a candidate into a plan. No network access, so `--list` needs nothing
/// else.
///
/// # Errors
/// Fails when the store cannot be opened or the candidate is not there.
pub async fn plan(
    settings: &Settings,
    candidate_id: CandidateId,
) -> Result<DownloadPlan, DownloadError> {
    let store = settings.open_store().await?;
    let candidates = CandidateStore::new(store.pool().clone());

    let candidate = match candidates.load(candidate_id).await {
        Ok(candidate) => candidate,
        Err(aldo_store::StoreError::NotFound { .. }) => {
            return Err(DownloadError::CandidateNotFound(candidate_id.get()));
        }
        Err(error) => return Err(DownloadError::Store(error)),
    };

    Ok(plan_of(&candidate))
}

fn plan_of(candidate: &LoadedCandidate) -> DownloadPlan {
    let total_bytes = ByteSize::new(candidate.files.iter().map(|file| file.size.as_u64()).sum());

    DownloadPlan {
        candidate_id: candidate.candidate_id,
        artist: candidate.display_artist.clone(),
        album: candidate.display_album.clone(),
        locator: candidate.locator.clone(),
        files: candidate.files.clone(),
        total_bytes,
    }
}

/// Resolves the Discogs metadata to tag with, or `None` to download untagged.
///
/// # Errors
/// Fails before any transfer when the selected release's track count does not
/// match the folder, because tagging a partial folder writes wrong metadata.
pub async fn metadata(
    settings: &Settings,
    plan: &DownloadPlan,
    discogs_release: Option<u32>,
    force: bool,
) -> Result<Option<TagMetadata>, DownloadError> {
    if force {
        return Ok(None);
    }

    let store = settings.open_store().await?;
    let candidates = CandidateStore::new(store.pool().clone());

    let raw_id = match discogs_release {
        Some(raw_id) => Some(i64::from(raw_id)),
        None => {
            candidates
                .selected_discogs_release(plan.candidate_id)
                .await?
        }
    };
    let Some(raw_id) = raw_id else {
        return Ok(None);
    };
    let raw_id =
        u32::try_from(raw_id).map_err(|_| DownloadError::CorruptDiscogsReleaseId(raw_id))?;

    let catalog = CatalogStore::new(store.pool().clone());
    let release = catalog.get_by_discogs_id(DiscogsReleaseId(raw_id)).await?;
    let tracks: Vec<_> = catalog
        .tracks(release.release_id)
        .await?
        .into_iter()
        .filter(|track| track.kind == TrackKind::Track)
        .collect();

    if tracks.len() != plan.files.len() {
        return Err(DownloadError::TrackCountMismatch {
            files: plan.files.len(),
            tracks: tracks.len(),
        });
    }

    let credits = catalog.album_artists(release.release_id).await?;
    let mut artist = String::new();
    for credit in credits.iter().filter(|credit| credit.is_album_artist) {
        artist.push_str(&credit.artist.name);
        if let Some(phrase) = &credit.join_phrase {
            artist.push_str(phrase);
        }
    }

    Ok(Some(TagMetadata {
        discogs_release_id: raw_id,
        album: release.title,
        artist: if artist.is_empty() {
            plan.artist.clone()
        } else {
            artist
        },
        year: release.year,
        tracks: tracks
            .into_iter()
            .map(|track| (track.title, track.position))
            .collect(),
    }))
}

/// Moves the files, tags them, and records the acquisition.
///
/// # Errors
/// Fails when SoulSeek is not configured, the engine is not supported, or a
/// transfer fails. A failed transfer leaves the acquisition `partial`, so the
/// next attempt reuses the files that did arrive.
pub async fn execute(
    settings: &Settings,
    plan: &DownloadPlan,
    tags: Option<&TagMetadata>,
    events: Option<&mpsc::UnboundedSender<DownloadEvent>>,
) -> Result<DownloadReport, DownloadError> {
    let Some(peer) = plan.peer() else {
        return Err(DownloadError::UnsupportedEngine("BitTorrent"));
    };
    let peer = peer.to_owned();

    let config = settings
        .soulseek()?
        .ok_or(DownloadError::SoulSeekNotConfigured)?;

    let store = settings.open_store().await?;
    let staging_dir = settings
        .library_root
        .join(".staging")
        .join(plan.candidate_id.get().to_string());
    tokio::fs::create_dir_all(&staging_dir).await?;

    let acquisitions = AcquisitionStore::new(store.pool().clone());
    let now = aldo_store::now_ms();
    let acquisition_id = acquisitions
        .create(
            plan.candidate_id,
            LOCAL_USER,
            Engine::SoulSeek,
            plan.total_bytes,
            now,
        )
        .await?;

    let mut assets = acquisitions.assets(acquisition_id).await?;
    if assets.is_empty() {
        let _ = acquisitions
            .expect_assets(acquisition_id, &plan.files)
            .await?;
        assets = acquisitions.assets(acquisition_id).await?;
    }
    acquisitions
        .set_status(acquisition_id, AcquisitionStatus::Downloading)
        .await?;

    emit(
        events,
        DownloadEvent::Connecting {
            username: config.username.clone(),
        },
    );
    let provider = SoulSeekProvider::connect(config).await?;

    let run = Run {
        plan,
        peer: &peer,
        staging_dir: &staging_dir,
        provider: &provider,
        acquisitions: &acquisitions,
        acquisition_id,
        assets: &assets,
        events,
    };
    fetch_all(&run).await?;

    finalize(
        settings,
        plan,
        &staging_dir,
        &acquisitions,
        acquisition_id,
        tags,
        events,
    )
    .await
}

/// Everything one download needs, bundled so no helper takes nine arguments.
struct Run<'a> {
    plan: &'a DownloadPlan,
    peer: &'a str,
    staging_dir: &'a Path,
    provider: &'a SoulSeekProvider,
    acquisitions: &'a AcquisitionStore,
    acquisition_id: AcquisitionId,
    assets: &'a [aldo_store::repo::AssetRow],
    events: Option<&'a mpsc::UnboundedSender<DownloadEvent>>,
}

async fn fetch_all(run: &Run<'_>) -> Result<(), DownloadError> {
    let total = run.plan.files.len();
    let mut completed_bytes: u64 = 0;

    for (offset, file) in run.plan.files.iter().enumerate() {
        let index = offset + 1;
        let base_filename = extract_filename(&file.path);
        let staged_file_path = run.staging_dir.join(&base_filename);
        let asset_id = run.assets.get(offset).map(|asset| asset.asset_id);

        if is_file_complete(&staged_file_path, file.size.as_u64()).await? {
            emit(
                run.events,
                DownloadEvent::FileSkipped {
                    index,
                    total,
                    name: base_filename,
                },
            );
            if let Some(id) = asset_id {
                run.acquisitions
                    .mark_asset_complete(id, AssetState::Complete, None)
                    .await?;
            }
            completed_bytes = completed_bytes.saturating_add(file.size.as_u64());
            run.acquisitions
                .update_progress(run.acquisition_id, ByteSize::new(completed_bytes), 0)
                .await?;
            continue;
        }

        emit(
            run.events,
            DownloadEvent::FileStarted {
                index,
                total,
                name: base_filename.clone(),
                size: file.size,
            },
        );
        fetch_file(run, file, &base_filename, asset_id).await?;

        emit(run.events, DownloadEvent::FileDone { index });
        completed_bytes = completed_bytes.saturating_add(file.size.as_u64());
        run.acquisitions
            .update_progress(run.acquisition_id, ByteSize::new(completed_bytes), 0)
            .await?;
    }

    Ok(())
}

async fn is_file_complete(path: &Path, expected_size: u64) -> Result<bool, std::io::Error> {
    if !path.exists() {
        return Ok(false);
    }
    let meta = tokio::fs::metadata(path).await?;
    Ok(meta.len() == expected_size)
}

/// Streams one file, translating the client's progress into events.
async fn fetch_file(
    run: &Run<'_>,
    file: &CandidateFile,
    base_filename: &str,
    asset_id: Option<AssetId>,
) -> Result<(), DownloadError> {
    let (progress_tx, mut progress_rx) = mpsc::channel(32);
    let events = run.events.cloned();
    let file_size = file.size.as_u64();

    // The client reports bytes; the event carries the file's ordinal so a bar
    // knows which row it belongs to without re-deriving it.
    let forwarder = tokio::spawn(async move {
        while let Some(progress) = progress_rx.recv().await {
            let Some(events) = events.as_ref() else {
                continue;
            };
            let event = match progress {
                DownloadProgress::Queued => DownloadEvent::Progress {
                    bytes_done: 0,
                    bytes_total: file_size,
                    speed_bps: 0,
                    queued: true,
                },
                DownloadProgress::InProgress {
                    bytes_downloaded,
                    total_bytes,
                    speed_bps,
                } => DownloadEvent::Progress {
                    bytes_done: bytes_downloaded,
                    bytes_total: if total_bytes > 0 {
                        total_bytes
                    } else {
                        file_size
                    },
                    speed_bps,
                    queued: false,
                },
                // Completed and Failed are reported by the transfer's own
                // result, so the last tick would only duplicate them.
                DownloadProgress::Completed | DownloadProgress::Failed(_) => continue,
            };
            let _ = events.send(event);
        }
    });

    let outcome = run
        .provider
        .download(
            run.peer,
            &file.path,
            file.size.as_u64(),
            run.staging_dir,
            progress_tx,
        )
        .await;

    let _ = forwarder.await;

    match outcome {
        Ok(()) => {
            if let Some(id) = asset_id {
                run.acquisitions
                    .mark_asset_complete(id, AssetState::Complete, None)
                    .await?;
            }
            Ok(())
        }
        Err(error) => {
            run.acquisitions
                .set_status(run.acquisition_id, AcquisitionStatus::Partial)
                .await?;
            Err(DownloadError::FileFailed {
                filename: base_filename.to_owned(),
                message: error.to_string(),
            })
        }
    }
}

async fn finalize(
    settings: &Settings,
    plan: &DownloadPlan,
    staging_dir: &Path,
    acquisitions: &AcquisitionStore,
    acquisition_id: AcquisitionId,
    tags: Option<&TagMetadata>,
    events: Option<&mpsc::UnboundedSender<DownloadEvent>>,
) -> Result<DownloadReport, DownloadError> {
    let target_dir = target_directory(&settings.library_root, &plan.artist, &plan.album);
    tokio::fs::create_dir_all(&target_dir).await?;

    for (offset, file) in plan.files.iter().enumerate() {
        let base_filename = extract_filename(&file.path);
        let staged_file = staging_dir.join(&base_filename);
        let target_file = target_dir.join(&base_filename);

        if staged_file.exists() && tokio::fs::rename(&staged_file, &target_file).await.is_err() {
            tokio::fs::copy(&staged_file, &target_file).await?;
            let _ = tokio::fs::remove_file(&staged_file).await;
        }

        if let Some(metadata) = tags {
            let track_index =
                track_index_for_filename(&base_filename, &metadata.tracks).unwrap_or(offset);
            let (track_number, disc_number) =
                match metadata.tracks.get(track_index).map(|track| &track.1) {
                    Some(TrackPosition::DiscTrack { disc, track }) => {
                        (u32::from(*track), u32::from(*disc))
                    }
                    _ => (u32::try_from(track_index + 1).unwrap_or(u32::MAX), 1),
                };
            write_flac_tags(
                &target_file,
                &TrackTags {
                    album: &metadata.album,
                    artist: &metadata.artist,
                    year: metadata.year,
                    title: &metadata.tracks[track_index].0,
                    track: track_number,
                    track_total: u32::try_from(metadata.tracks.len()).unwrap_or(u32::MAX),
                    disc: disc_number,
                    discogs_release_id: metadata.discogs_release_id,
                },
            )?;
        }
    }

    let _ = tokio::fs::remove_dir(staging_dir).await;

    acquisitions
        .set_status(acquisition_id, AcquisitionStatus::Downloaded)
        .await?;
    acquisitions
        .update_progress(acquisition_id, plan.total_bytes, 0)
        .await?;

    let report = DownloadReport {
        files: plan.files.len(),
        target_dir,
        tagged: tags.is_some(),
    };
    emit(events, DownloadEvent::Finished(report.clone()));
    Ok(report)
}

fn track_index_for_filename(filename: &str, tracks: &[(String, TrackPosition)]) -> Option<usize> {
    let track_number = filename
        .split(" - ")
        .find_map(|part| part.trim().parse::<u8>().ok())?;
    tracks.iter().position(|(_, position)| {
        matches!(
            position,
            TrackPosition::DiscTrack { disc: 1, track } if *track == track_number
        )
    })
}

/// The tags one file gets, bundled so the writer takes one argument.
struct TrackTags<'a> {
    album: &'a str,
    artist: &'a str,
    year: Option<u16>,
    title: &'a str,
    track: u32,
    track_total: u32,
    disc: u32,
    discogs_release_id: u32,
}

/// Replaces the Vorbis comments through `metaflac`, which rewrites only the
/// metadata blocks and never the audio frames.
fn write_flac_tags(path: &Path, tags: &TrackTags<'_>) -> Result<(), DownloadError> {
    let mut command = ProcessCommand::new("metaflac");
    command.env_clear();
    for key in [
        "TITLE",
        "ALBUM",
        "ARTIST",
        "ALBUMARTIST",
        "TRACKNUMBER",
        "TRACKTOTAL",
        "DISCNUMBER",
        "DATE",
        "YEAR",
        "DISCOGS_RELEASE_ID",
    ] {
        command.arg(format!("--remove-tag={key}"));
    }
    for (key, value) in [
        ("TITLE", tags.title.to_owned()),
        ("ALBUM", tags.album.to_owned()),
        ("ARTIST", tags.artist.to_owned()),
        ("ALBUMARTIST", tags.artist.to_owned()),
        ("TRACKNUMBER", tags.track.to_string()),
        ("TRACKTOTAL", tags.track_total.to_string()),
        ("DISCNUMBER", tags.disc.to_string()),
        ("DISCOGS_RELEASE_ID", tags.discogs_release_id.to_string()),
    ] {
        command.arg(format!("--set-tag={key}={value}"));
    }
    if let Some(year) = tags.year {
        command.arg(format!("--set-tag=DATE={year}"));
        command.arg(format!("--set-tag=YEAR={year}"));
    }

    let output = command.arg(path).output().map_err(|error| {
        DownloadError::Tag(format!(
            "could not run metaflac ({error}); install the FLAC tools"
        ))
    })?;
    if !output.status.success() {
        return Err(DownloadError::Tag(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }
    Ok(())
}

fn extract_filename(path: &str) -> String {
    path.split(['/', '\\'])
        .next_back()
        .filter(|name| !name.is_empty())
        .unwrap_or(path)
        .to_owned()
}

fn sanitize_path_component(name: &str) -> String {
    let sanitized: String = name
        .chars()
        .map(|ch| match ch {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\0' => '_',
            other => other,
        })
        .collect();
    let trimmed = sanitized.trim();
    if trimmed.is_empty() {
        "Unknown".to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// Where a plan's files will end up, with each component made filename-safe.
///
/// Public so a confirmation can state the exact destination the download will
/// use, rather than approximating it.
#[must_use]
pub fn target_directory(library_root: &Path, artist: &str, album: &str) -> PathBuf {
    let artist_part = sanitize_path_component(if artist.is_empty() {
        "Unknown Artist"
    } else {
        artist
    });
    let album_part = sanitize_path_component(if album.is_empty() {
        "Unknown Album"
    } else {
        album
    });
    library_root.join(artist_part).join(album_part)
}

fn emit(events: Option<&mpsc::UnboundedSender<DownloadEvent>>, event: DownloadEvent) {
    if let Some(sender) = events {
        let _ = sender.send(event);
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DownloadError {
    #[error(
        "result index #{0} is unavailable; run 'aldo search' to refresh today's results (indexes expire at UTC midnight)"
    )]
    DailyIndexNotFound(i64),

    #[error("candidate #{0} was not found; run 'aldo search' first")]
    CandidateNotFound(i64),

    #[error("server is not configured: run `aldo login`")]
    SoulSeekNotConfigured,

    #[error("{0} downloads are not yet implemented")]
    UnsupportedEngine(&'static str),

    #[error(
        "the selected server folder has {files} files, but the matched Discogs release has {tracks} tracks; use `aldo download <index> --force` to download without Discogs metadata"
    )]
    TrackCountMismatch { files: usize, tracks: usize },

    #[error("failed to write FLAC tags: {0}")]
    Tag(String),

    #[error("stored Discogs release ID {0} is outside the supported range")]
    CorruptDiscogsReleaseId(i64),

    #[error("failed to download {filename}: {message}")]
    FileFailed { filename: String, message: String },

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Store(#[from] aldo_store::StoreError),

    #[error(transparent)]
    Settings(#[from] SettingsError),

    #[error(transparent)]
    Provider(#[from] aldo_core::error::ProviderError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_basename_from_windows_and_unix_paths() {
        assert_eq!(extract_filename("a/b/c.flac"), "c.flac");
        assert_eq!(extract_filename("a\\b\\c.flac"), "c.flac");
        assert_eq!(extract_filename("c.flac"), "c.flac");
    }

    #[test]
    fn matches_discogs_tracks_by_the_number_in_the_filename() {
        let tracks = vec![
            (
                "Don't Go Near the Water".to_owned(),
                TrackPosition::DiscTrack { disc: 1, track: 1 },
            ),
            (
                "Long Promised Road".to_owned(),
                TrackPosition::DiscTrack { disc: 1, track: 2 },
            ),
        ];
        assert_eq!(
            track_index_for_filename(
                "The Beach Boys - Surf's Up - 01 - Don't Go Near the Water.flac",
                &tracks
            ),
            Some(0)
        );
    }

    #[test]
    fn target_directory_sanitizes_unfriendly_characters() {
        let dir = target_directory(Path::new("/lib"), "AC/DC", "Who Made Who?");
        assert_eq!(dir, PathBuf::from("/lib/AC_DC/Who Made Who_"));
    }
}
