//! `mud download`: fetch the files a search result points at.
//!
//! A download moves files from the candidate's provider into the staging
//! directory, tracks transfer progress through `AcquisitionStore`, and on
//! completion moves the completed album into the music library.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use mud_core::audio::ByteSize;
use mud_core::candidate::{CandidateFile, Locator};
use mud_core::ids::DiscogsReleaseId;
use mud_core::ids::{AcquisitionId, AssetId};
use mud_core::release::{TrackKind, TrackPosition};
use mud_soulseek::{DownloadProgress, SoulSeekProvider};
use mud_store::repo::{
    AcquisitionStatus, AcquisitionStore, AssetState, CandidateStore, Engine, LOCAL_USER,
    LoadedCandidate,
};
use std::process::Command as ProcessCommand;

use crate::cli::DownloadArgs;
use crate::settings::{Settings, SettingsError};

pub async fn run(settings: &Settings, args: &DownloadArgs) -> ExitCode {
    match download(settings, args).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("mud: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn download(settings: &Settings, args: &DownloadArgs) -> Result<(), DownloadError> {
    let store = settings.open_store().await?;
    let candidates = CandidateStore::new(store.pool().clone());
    let now = mud_store::now_ms();
    let utc_day = now.div_euclid(86_400_000);
    let candidate_id = match candidates
        .resolve_daily_index(utc_day, args.result_index)
        .await
    {
        Ok(candidate_id) => candidate_id,
        Err(mud_store::StoreError::NotFound { .. }) => {
            return Err(DownloadError::DailyIndexNotFound(args.result_index));
        }
        Err(error) => return Err(DownloadError::Store(error)),
    };

    let candidate = match candidates.load(candidate_id).await {
        Ok(c) => c,
        Err(mud_store::StoreError::NotFound { .. }) => {
            return Err(DownloadError::CandidateNotFound(candidate_id.get()));
        }
        Err(e) => return Err(DownloadError::Store(e)),
    };

    let selected_release = match args.discogs_release {
        Some(raw_id) => Some(raw_id),
        None => candidates
            .selected_discogs_release(candidate_id)
            .await?
            .map(|raw_id| {
                u32::try_from(raw_id).map_err(|_| DownloadError::CorruptDiscogsReleaseId(raw_id))
            })
            .transpose()?,
    };

    let tag_metadata = if let Some(raw_id) = selected_release {
        let discogs_id = DiscogsReleaseId(raw_id);
        let catalog = mud_store::repo::CatalogStore::new(store.pool().clone());
        let release = catalog.get_by_discogs_id(discogs_id).await?;
        let tracks: Vec<_> = catalog
            .tracks(release.release_id)
            .await?
            .into_iter()
            .filter(|track| track.kind == TrackKind::Track)
            .collect();
        if tracks.len() != candidate.files.len() {
            return Err(DownloadError::TrackCountMismatch {
                files: candidate.files.len(),
                tracks: tracks.len(),
            });
        }
        let artists = catalog.album_artists(release.release_id).await?;
        let artist = artists
            .iter()
            .filter(|credit| credit.is_album_artist)
            .map(|credit| {
                format!(
                    "{}{}",
                    credit.artist.name,
                    credit.join_phrase.as_deref().unwrap_or("")
                )
            })
            .collect::<String>();
        Some(TagMetadata {
            discogs_release_id: raw_id,
            album: release.title,
            artist: if artist.is_empty() {
                candidate.display_artist.clone()
            } else {
                artist
            },
            year: release.year,
            tracks: tracks
                .into_iter()
                .map(|track| (track.title, track.position))
                .collect(),
        })
    } else {
        println!("No matching Discogs release was stored; downloading without metadata.");
        None
    };

    let slsk = match &candidate.locator {
        Locator::SoulSeek(slsk) => slsk,
        Locator::Torrent(_) => {
            return Err(DownloadError::UnsupportedEngine("BitTorrent"));
        }
    };

    let config = settings
        .soulseek()?
        .ok_or(DownloadError::SoulSeekNotConfigured)?;

    let total_bytes = ByteSize::new(candidate.files.iter().map(|f| f.size.as_u64()).sum());
    print_header(args.result_index, &candidate, total_bytes, &slsk.peer);

    let staging_dir = settings
        .library_root
        .join(".staging")
        .join(candidate_id.get().to_string());
    tokio::fs::create_dir_all(&staging_dir).await?;

    let acquisitions = AcquisitionStore::new(store.pool().clone());
    let acquisition_id = acquisitions
        .create(candidate_id, LOCAL_USER, Engine::SoulSeek, total_bytes, now)
        .await?;

    let mut assets = acquisitions.assets(acquisition_id).await?;
    if assets.is_empty() {
        let _ = acquisitions
            .expect_assets(acquisition_id, &candidate.files)
            .await?;
        assets = acquisitions.assets(acquisition_id).await?;
    }

    acquisitions
        .set_status(acquisition_id, AcquisitionStatus::Downloading)
        .await?;

    println!("Connecting to server as {}...", config.username);
    let provider = SoulSeekProvider::connect(config).await?;

    download_all_files(
        &candidate,
        &slsk.peer,
        &staging_dir,
        &provider,
        &acquisitions,
        acquisition_id,
        &assets,
    )
    .await?;

    finalize_download(
        settings,
        &candidate,
        &staging_dir,
        &acquisitions,
        acquisition_id,
        total_bytes,
        tag_metadata.as_ref(),
    )
    .await
}

fn print_header(id: i64, candidate: &LoadedCandidate, total_bytes: ByteSize, peer: &str) {
    let artist = if candidate.display_artist.is_empty() {
        "Unknown Artist"
    } else {
        &candidate.display_artist
    };
    let album = if candidate.display_album.is_empty() {
        "Unknown Album"
    } else {
        &candidate.display_album
    };

    println!(
        "Candidate #{id}: {artist} - {album} ({} files, {total_bytes}) from peer '{peer}'",
        candidate.files.len()
    );
}

async fn download_all_files(
    candidate: &LoadedCandidate,
    peer: &str,
    staging_dir: &Path,
    provider: &SoulSeekProvider,
    acquisitions: &AcquisitionStore,
    acquisition_id: AcquisitionId,
    assets: &[mud_store::repo::AssetRow],
) -> Result<(), DownloadError> {
    let mut completed_bytes: u64 = 0;
    let total_files = candidate.files.len();

    for (index, file) in candidate.files.iter().enumerate() {
        let base_filename = extract_filename(&file.path);
        let staged_file_path = staging_dir.join(&base_filename);
        let asset_id = assets.get(index).map(|a| a.asset_id);

        if is_file_complete(&staged_file_path, file.size.as_u64()).await? {
            println!(
                "  [{}/{}] {} (already complete, skipping)",
                index + 1,
                total_files,
                base_filename
            );
            if let Some(id) = asset_id {
                acquisitions
                    .mark_asset_complete(id, AssetState::Complete, None)
                    .await?;
            }
            completed_bytes = completed_bytes.saturating_add(file.size.as_u64());
            acquisitions
                .update_progress(acquisition_id, ByteSize::new(completed_bytes), 0)
                .await?;
            continue;
        }

        println!(
            "  [{}/{}] {} ({})...",
            index + 1,
            total_files,
            base_filename,
            file.size
        );

        fetch_file(
            provider,
            peer,
            file,
            &base_filename,
            staging_dir,
            acquisitions,
            acquisition_id,
            asset_id,
        )
        .await?;

        completed_bytes = completed_bytes.saturating_add(file.size.as_u64());
        acquisitions
            .update_progress(acquisition_id, ByteSize::new(completed_bytes), 0)
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

#[allow(clippy::too_many_arguments)]
async fn fetch_file(
    provider: &SoulSeekProvider,
    peer: &str,
    file: &CandidateFile,
    base_filename: &str,
    staging_dir: &Path,
    acquisitions: &AcquisitionStore,
    acquisition_id: AcquisitionId,
    asset_id: Option<AssetId>,
) -> Result<(), DownloadError> {
    let (progress_tx, mut progress_rx) = tokio::sync::mpsc::channel(32);
    let file_size_bytes = file.size.as_u64();

    let progress_task = tokio::spawn(async move {
        while let Some(prog) = progress_rx.recv().await {
            match prog {
                DownloadProgress::Queued => {
                    print!("\r    waiting in peer queue...                 ");
                    let _ = std::io::stdout().flush();
                }
                DownloadProgress::InProgress {
                    bytes_downloaded,
                    total_bytes,
                    speed_bps,
                } => {
                    let total = if total_bytes > 0 {
                        total_bytes
                    } else {
                        file_size_bytes
                    };
                    let completed = bytes_downloaded.min(total);
                    let pct = if total > 0 {
                        (completed as f64 / total as f64) * 100.0
                    } else {
                        0.0
                    };
                    let bar = progress_bar(completed, total);
                    let speed_str = format_speed(speed_bps);
                    print!(
                        "\r    {bar} {:5.1}% ({}/{}) @ {}       ",
                        pct,
                        ByteSize::new(bytes_downloaded),
                        ByteSize::new(total),
                        speed_str
                    );
                    let _ = std::io::stdout().flush();
                }
                DownloadProgress::Completed => {
                    println!("\r    100% completed.                           ");
                    break;
                }
                DownloadProgress::Failed(reason) => {
                    println!("\r    failed: {reason}                          ");
                    break;
                }
            }
        }
    });

    match provider
        .download(
            peer,
            &file.path,
            file.size.as_u64(),
            staging_dir,
            progress_tx,
        )
        .await
    {
        Ok(()) => {
            let _ = progress_task.await;
            if let Some(id) = asset_id {
                acquisitions
                    .mark_asset_complete(id, AssetState::Complete, None)
                    .await?;
            }
            Ok(())
        }
        Err(e) => {
            let _ = progress_task.await;
            acquisitions
                .set_status(acquisition_id, AcquisitionStatus::Partial)
                .await?;
            Err(DownloadError::FileFailed {
                filename: base_filename.to_owned(),
                message: e.to_string(),
            })
        }
    }
}

async fn finalize_download(
    settings: &Settings,
    candidate: &LoadedCandidate,
    staging_dir: &Path,
    acquisitions: &AcquisitionStore,
    acquisition_id: AcquisitionId,
    total_bytes: ByteSize,
    tag_metadata: Option<&TagMetadata>,
) -> Result<(), DownloadError> {
    let target_dir = target_directory(
        &settings.library_root,
        &candidate.display_artist,
        &candidate.display_album,
    );
    tokio::fs::create_dir_all(&target_dir).await?;

    for (index, file) in candidate.files.iter().enumerate() {
        let base_filename = extract_filename(&file.path);
        let staged_file = staging_dir.join(&base_filename);
        let target_file = target_dir.join(&base_filename);
        if staged_file.exists() && tokio::fs::rename(&staged_file, &target_file).await.is_err() {
            tokio::fs::copy(&staged_file, &target_file).await?;
            let _ = tokio::fs::remove_file(&staged_file).await;
        }
        if let Some(metadata) = tag_metadata {
            let track_index =
                track_index_for_filename(&base_filename, &metadata.tracks).unwrap_or(index);
            let (track_number, disc_number) = match metadata.tracks.get(track_index).map(|x| &x.1) {
                Some(TrackPosition::DiscTrack { disc, track }) => {
                    (u32::from(*track), u32::from(*disc))
                }
                _ => (u32::try_from(track_index + 1).unwrap_or(u32::MAX), 1),
            };
            write_flac_tags(
                &target_file,
                &metadata.album,
                &metadata.artist,
                metadata.year,
                &metadata.tracks[track_index].0,
                track_number,
                u32::try_from(metadata.tracks.len()).unwrap_or(u32::MAX),
                disc_number,
                metadata.discogs_release_id,
            )?;
        }
    }

    let _ = tokio::fs::remove_dir(staging_dir).await;

    acquisitions
        .set_status(acquisition_id, AcquisitionStatus::Downloaded)
        .await?;
    acquisitions
        .update_progress(acquisition_id, total_bytes, 0)
        .await?;

    println!(
        "\nDownload complete: {} files saved to {}{}",
        candidate.files.len(),
        target_dir.display(),
        if tag_metadata.is_some() {
            " (Discogs tags written)"
        } else {
            ""
        }
    );

    Ok(())
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

#[derive(Debug)]
struct TagMetadata {
    discogs_release_id: u32,
    album: String,
    artist: String,
    year: Option<u16>,
    tracks: Vec<(String, TrackPosition)>,
}

fn write_flac_tags(
    path: &Path,
    album: &str,
    artist: &str,
    year: Option<u16>,
    title: &str,
    track: u32,
    track_total: u32,
    disc: u32,
    discogs_release_id: u32,
) -> Result<(), DownloadError> {
    let mut command = ProcessCommand::new("metaflac");
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
        ("TITLE", title.to_owned()),
        ("ALBUM", album.to_owned()),
        ("ARTIST", artist.to_owned()),
        ("ALBUMARTIST", artist.to_owned()),
        ("TRACKNUMBER", track.to_string()),
        ("TRACKTOTAL", track_total.to_string()),
        ("DISCNUMBER", disc.to_string()),
        ("DISCOGS_RELEASE_ID", discogs_release_id.to_string()),
    ] {
        command.arg(format!("--set-tag={key}={value}"));
    }
    if let Some(year) = year {
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
        .filter(|s| !s.is_empty())
        .unwrap_or(path)
        .to_owned()
}

fn format_speed(speed_bps: u32) -> String {
    if speed_bps >= 1_000_000 {
        format!("{:.1} MiB/s", f64::from(speed_bps) / 1_048_576.0)
    } else if speed_bps >= 1_000 {
        format!("{:.1} KiB/s", f64::from(speed_bps) / 1_024.0)
    } else {
        format!("{speed_bps} B/s")
    }
}

/// A compact inline bar for the active file. The terminal line updates in
/// place, so a multi-file download stays readable without drawing a UI.
fn progress_bar(completed: u64, total: u64) -> String {
    const WIDTH: usize = 20;
    let filled = if total == 0 {
        0
    } else {
        let cells = (u128::from(completed.min(total)) * WIDTH as u128) / u128::from(total);
        usize::try_from(cells).unwrap_or(WIDTH)
    };

    format!("[{}{}]", "=".repeat(filled), "-".repeat(WIDTH - filled))
}

fn sanitize_path_component(name: &str) -> String {
    let sanitized: String = name
        .chars()
        .map(|c| match c {
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

fn target_directory(library_root: &Path, artist: &str, album: &str) -> PathBuf {
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

#[derive(Debug, thiserror::Error)]
enum DownloadError {
    #[error(
        "result index #{0} is unavailable; run 'mud search' to refresh today's results (indexes expire at UTC midnight)"
    )]
    DailyIndexNotFound(i64),

    #[error("candidate #{0} was not found; run 'mud search' first")]
    CandidateNotFound(i64),

    #[error("server is not configured: set MUD_SLSK_USERNAME and MUD_SLSK_PASSWORD")]
    SoulSeekNotConfigured,

    #[error("{0} downloads are not yet implemented")]
    UnsupportedEngine(&'static str),

    #[error(
        "Discogs release has {tracks} tracks, but the selected SoulSeek folder has {files} files"
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
    Store(#[from] mud_store::StoreError),

    #[error(transparent)]
    Settings(#[from] SettingsError),

    #[error(transparent)]
    Provider(#[from] mud_core::error::ProviderError),
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
    fn formats_speed_appropriately() {
        assert_eq!(format_speed(500), "500 B/s");
        assert_eq!(format_speed(1_500), "1.5 KiB/s");
        assert_eq!(format_speed(2_500_000), "2.4 MiB/s");
    }

    #[test]
    fn target_directory_sanitizes_unfriendly_characters() {
        let dir = target_directory(Path::new("/lib"), "AC/DC", "Who Made Who?");
        assert_eq!(dir, PathBuf::from("/lib/AC_DC/Who Made Who_"));
    }
}
