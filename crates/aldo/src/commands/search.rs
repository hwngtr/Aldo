//! `aldo search`: ask each provider what it has for a query, then print it.
//!
//! The operation itself lives in [`crate::actions::search`], because the TUI runs
//! the same search and must not diverge from it. This module only decides how the
//! result reads on a terminal.

use std::process::ExitCode;

use aldo_catalog::FanoutRecord;
use aldo_core::candidate::Candidate;
use aldo_store::repo::CatalogRow;

use crate::actions::search::{
    MAX_SOULSEEK_RESULTS, SearchRun, SearchRunError, SoulSeekFound, SoulSeekReport, parse,
};
use crate::cli::SearchArgs;
use crate::format::{folder as folder_of, quality, speed};
use crate::settings::Settings;

pub async fn run(settings: &Settings, args: &SearchArgs) -> ExitCode {
    match search(settings, args).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("aldo: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn search(settings: &Settings, args: &SearchArgs) -> Result<(), SearchRunError> {
    let query = parse(&args.query)?;
    crate::outln!("Query: {}", describe(&query));

    let run = crate::actions::search::run(settings, &query, args.limit, None).await?;
    print_run(&run);

    Ok(())
}

/// Prints a whole search the way the CLI reads it.
fn print_run(run: &SearchRun) {
    print_discogs(&run.discogs);
    if run.query.artist.is_none() && !run.discogs.matches.is_empty() {
        crate::outln!("  Tip: use `Artist - Album` when you want a less ambiguous Discogs match.");
    }

    match &run.soulseek {
        SoulSeekReport::NotConfigured => {
            crate::outln!();
            crate::outln!("Server: not configured: run `aldo login`");
        }
        SoulSeekReport::Refused => {
            crate::outln!();
            crate::outln!(
                "Server: sign-in rejected: the username or password is wrong, or the \
                 name is already taken. Check the account, then run `aldo login`."
            );
        }
        SoulSeekReport::RateLimited { retry_after } => {
            crate::outln!();
            crate::outln!("Server: rate limited, retry in {retry_after:?}");
        }
        SoulSeekReport::Unreachable { message } => {
            crate::outln!();
            crate::outln!("Server: {message}");
        }
        SoulSeekReport::Answered(found) => print_soulseek(found),
    }
}

fn describe(query: &aldo_core::search::SearchQuery) -> String {
    let artist = query.artist.as_deref().unwrap_or("-");
    let album = query.album.as_deref().unwrap_or("-");
    match query.year {
        Some(year) => format!("{artist} / {album} ({year})"),
        None => format!("{artist} / {album}"),
    }
}

fn print_discogs(record: &FanoutRecord) {
    crate::outln!("Discogs: {}", record.detail);

    if record.answered() && !record.matches.is_empty() {
        for row in &record.matches {
            crate::outln!("  {}", render_release(row));
        }
    }
}

fn print_soulseek(found: &SoulSeekFound) {
    let shown = found.candidates.len().min(MAX_SOULSEEK_RESULTS);
    crate::outln!();
    crate::outln!(
        "Server: connected; {} folders, {} files{}{}",
        found.candidates.len(),
        found.raw_hits,
        if found.rejected > 0 {
            format!(" ({} rejected as not FLAC)", found.rejected)
        } else {
            String::new()
        },
        if shown < found.candidates.len() {
            format!("; showing {shown} fastest")
        } else {
            String::new()
        }
    );

    if let Some(hint) = found.hint() {
        crate::outln!("  {hint}");
    }

    for (index, candidate) in found
        .candidates
        .iter()
        .take(MAX_SOULSEEK_RESULTS)
        .enumerate()
    {
        crate::outln!("  {}", render_candidate(candidate, index + 1));
    }
}

/// One Discogs line: `Title - Artist (Year) | album | 12 tracks`.
fn render_release(row: &CatalogRow) -> String {
    let artist = row.artist.as_deref().unwrap_or("unknown artist");
    let year = row
        .year
        .map_or_else(|| "----".to_owned(), |year| year.to_string());

    let kinds = if row.release_types.is_empty() {
        "release".to_owned()
    } else {
        row.release_types
            .iter()
            .map(|kind| kind.as_vorbis_value())
            .collect::<Vec<_>>()
            .join("/")
    };

    format!(
        "Discogs #{}  {} - {} ({}) | {} | {} tracks",
        row.discogs_release_id, row.title, artist, year, kinds, row.track_count
    )
}

/// One SoulSeek line includes the peer's claimed transfer speed and slot state.
fn render_candidate(candidate: &Candidate, result_index: usize) -> String {
    let locator = match &candidate.locator {
        aldo_core::candidate::Locator::SoulSeek(locator) => locator,
        // The provider only ever returns SoulSeek locators, but a torrent
        // locator is representable, and rendering it as if it were a peer
        // would be a lie.
        aldo_core::candidate::Locator::Torrent(_) => {
            return format!("{} | not a SoulSeek result", folder_of(candidate));
        }
    };

    format!(
        "#{result_index:<2}  {}  {}  {} files  {}  {}  {} advertised  {}",
        folder_of(candidate),
        locator.peer,
        candidate.display.track_count,
        candidate.total_bytes,
        quality(locator.bit_depth, locator.sample_rate),
        speed(locator.upload_speed_bps),
        if locator.has_free_slot {
            "free slot"
        } else {
            "queued"
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    use aldo_core::audio::{BitDepth, ByteSize, SampleRate};
    use aldo_core::candidate::{CandidateDisplay, DedupeKey, Locator, SoulSeekLocator};
    use aldo_core::ids::{CandidateId, DiscogsReleaseId, ReleaseId};

    fn release(title: &str, artist: Option<&str>, year: Option<u16>, tracks: u16) -> CatalogRow {
        CatalogRow {
            release_id: ReleaseId(1),
            discogs_release_id: DiscogsReleaseId(1),
            title: title.to_owned(),
            artist: artist.map(str::to_owned),
            year,
            release_types: vec![aldo_core::ReleaseType::Album],
            track_count: tracks,
            cover_art_url: None,
        }
    }

    fn candidate(
        artist: &str,
        album: &str,
        peer: &str,
        depth: Option<u8>,
        rate: Option<u32>,
        free: bool,
    ) -> Candidate {
        Candidate {
            candidate_id: CandidateId(0),
            dedupe_key: DedupeKey::from_release_parts(artist, album, None, 1, 1),
            display: CandidateDisplay {
                artist: artist.to_owned(),
                album: album.to_owned(),
                year: None,
                track_count: 12,
                disc_count: 1,
            },
            locator: Locator::SoulSeek(SoulSeekLocator {
                peer: peer.to_owned(),
                remote_path: "x".to_owned(),
                size: ByteSize::new(1),
                has_free_slot: free,
                queue_length: None,
                upload_speed_bps: 1_024,
                sample_rate: rate.and_then(SampleRate::new),
                bit_depth: depth.and_then(BitDepth::new),
                duration: None,
            }),
            sources: Vec::new(),
            files: Vec::new(),
            total_bytes: ByteSize::new(358_000_000),
        }
    }
    #[test]
    fn a_blank_query_is_reported() {
        assert!(matches!(
            parse("   ").expect_err("blank"),
            SearchRunError::BlankQuery
        ));
    }

    #[test]
    fn a_quoted_query_keeps_its_parts() {
        let query = parse("Radiohead - OK Computer - 1997").expect("parses");
        assert_eq!(query.artist.as_deref(), Some("Radiohead"));
        assert_eq!(query.album.as_deref(), Some("OK Computer"));
        assert_eq!(query.year, Some(1997));
        assert_eq!(describe(&query), "Radiohead / OK Computer (1997)");
    }

    #[test]
    fn a_release_line_reads_as_title_artist_year_kind_tracks() {
        assert_eq!(
            render_release(&release("OK Computer", Some("Radiohead"), Some(1997), 12)),
            "Discogs #1  OK Computer - Radiohead (1997) | album | 12 tracks"
        );
    }

    #[test]
    fn a_release_with_no_artist_says_so_rather_than_printing_nothing() {
        assert_eq!(
            render_release(&release("Various", None, None, 3)),
            "Discogs #1  Various - unknown artist (----) | album | 3 tracks"
        );
    }

    #[test]
    fn a_candidate_line_carries_the_folder_the_peer_and_the_quality() {
        let rendered = render_candidate(
            &candidate(
                "Radiohead",
                "OK Computer",
                "alice",
                Some(16),
                Some(44_100),
                true,
            ),
            1,
        );

        assert_eq!(
            rendered,
            "#1   Radiohead/OK Computer  alice  12 files  341.4 MiB  16-bit 44 kHz  1.0 KiB/s advertised  free slot"
        );
    }

    #[test]
    fn a_candidate_with_no_claims_says_the_quality_is_unknown() {
        let rendered = render_candidate(&candidate("A", "B", "bob", None, None, false), 1);
        assert!(rendered.contains("quality unknown"), "{rendered}");
        assert!(rendered.ends_with("queued"), "{rendered}");
    }

    #[test]
    fn a_file_at_a_share_root_is_labelled_rather_than_left_blank() {
        assert_eq!(
            render_candidate(&candidate("", "", "alice", Some(16), Some(44_100), true), 1),
            "#1   (share root)  alice  12 files  341.4 MiB  16-bit 44 kHz  1.0 KiB/s advertised  free slot"
        );
    }

    #[test]
    fn candidate_line_shows_the_daily_index_not_the_database_id() {
        let mut c = candidate(
            "Radiohead",
            "OK Computer",
            "alice",
            Some(16),
            Some(44_100),
            true,
        );
        c.candidate_id = CandidateId(42);
        let rendered = render_candidate(&c, 1);
        assert!(rendered.starts_with("#1   "), "{rendered}");
        assert!(!rendered.starts_with("#42"), "{rendered}");
    }
}
