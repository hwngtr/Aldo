//! `mud search`: ask each provider what it has for a query.
//!
//! Two providers answer different questions. Discogs says what the release *is*
//! and is what tagging will match against. SoulSeek says which files exist right
//! now, which is the only place a download can come from.

use std::process::ExitCode;

use mud_catalog::{FanoutRecord, record_discogs};
use mud_core::provider::DiscoveryProvider;
use mud_core::search::{RawQuery, SearchFilters, SearchQuery};
use mud_soulseek::{SoulSeekConfig, SoulSeekProvider, rank_sources};
use mud_store::RateBudget;
use mud_store::repo::{CandidateStore, CatalogRow, FanoutStatus, LOCAL_USER, SessionStore};

use crate::cli::SearchArgs;
use crate::settings::{Settings, SettingsError};

/// Keep the interactive result list short enough to scan.
const MAX_SOULSEEK_RESULTS: usize = 10;

pub async fn run(settings: &Settings, args: &SearchArgs) -> ExitCode {
    match search(settings, args).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("mud: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn search(settings: &Settings, args: &SearchArgs) -> Result<(), SearchError> {
    let query = parse_query(&args.query)?;
    let store = settings.open_store().await?;
    let sessions = SessionStore::new(store.pool().clone());
    let now = mud_store::now_ms();

    println!("Query: {}", describe(&query));

    // Recorded before either provider is asked, so a search that could not be
    // made still shows up in `mud sessions`.
    let session_id = sessions.create(LOCAL_USER, &query, now).await?;

    let discogs = settings.discogs(&store)?;
    let record = record_discogs(
        discogs.as_ref(),
        &sessions,
        session_id,
        &query,
        args.limit,
        now,
    )
    .await?;
    if let Some(release) = record.matches.first() {
        sessions
            .set_selected_discogs_release(session_id, release.discogs_release_id)
            .await?;
    }
    print_discogs(&record);
    let discogs_count = record.result_count();

    let soulseek_count = match settings.soulseek()? {
        None => {
            println!();
            println!("Server: not configured: set MUD_SLSK_USERNAME and MUD_SLSK_PASSWORD");
            0
        }
        Some(config) => soulseek(&sessions, session_id, &query, config, &store, now).await?,
    };

    let total = discogs_count.saturating_add(soulseek_count);
    sessions.set_result_count(session_id, total).await?;

    Ok(())
}

/// Connects, searches, records the fan-out, and prints what came back.
async fn soulseek(
    sessions: &SessionStore,
    session_id: mud_core::ids::SessionId,
    query: &SearchQuery,
    config: SoulSeekConfig,
    store: &mud_store::Store,
    now: i64,
) -> Result<u32, SearchError> {
    // One search is one unit of the budget. Exceeding it is a thirty-minute
    // ban, so the refusal is reported rather than retried.
    let budgets = mud_store::repo::BudgetStore::new(store.pool().clone());
    budgets
        .ensure("soulseek", RateBudget::SOULSEEK_SEARCH, now)
        .await?;
    if let Err(wait) = budgets.acquire("soulseek", now).await? {
        println!();
        println!("Server: rate limited, retry in {wait:?}");
        return Ok(0);
    }

    let shared = config.shares_anything();
    let provider = match SoulSeekProvider::connect(config).await {
        Ok(provider) => provider,
        Err(error) => {
            println!();
            println!("Server: {error}");
            return Ok(0);
        }
    };

    let fanout = sessions
        .begin_fanout(
            session_id,
            mud_core::ProviderId::SoulSeek,
            &query.soulseek_query(),
            now,
        )
        .await?;
    let started = std::time::Instant::now();

    let outcome = provider.search(query).await;
    let latency_ms = u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX);

    match outcome {
        Ok(mut found) => {
            rank_sources(&mut found.candidates);

            let candidate_store = CandidateStore::new(store.pool().clone());
            candidate_store
                .record_rejections(session_id, fanout, &found.rejected)
                .await?;

            for candidate in &mut found.candidates {
                let candidate_id = candidate_store
                    .insert_or_get(session_id, candidate.dedupe_key, candidate, now)
                    .await?;
                candidate_store
                    .absorb_source(candidate_id, fanout, &candidate.locator)
                    .await?;
                candidate_store
                    .insert_files(candidate_id, candidate)
                    .await?;
                candidate.candidate_id = candidate_id;
            }

            let daily_indexes: Vec<(u8, mud_core::ids::CandidateId)> = found
                .candidates
                .iter()
                .take(MAX_SOULSEEK_RESULTS)
                .enumerate()
                .map(|(index, candidate)| {
                    (
                        u8::try_from(index + 1).unwrap_or(u8::MAX),
                        candidate.candidate_id,
                    )
                })
                .collect();
            let utc_day = mud_store::now_ms().div_euclid(86_400_000);
            candidate_store
                .replace_daily_indexes(utc_day, &daily_indexes)
                .await?;

            let count = u32::try_from(found.candidates.len()).unwrap_or(u32::MAX);
            sessions
                .finish_fanout(fanout, FanoutStatus::Done, count, latency_ms, None)
                .await?;
            print_soulseek(&found, shared);
            Ok(count)
        }
        Err(error) => {
            let detail = error.to_string();
            sessions
                .finish_fanout(fanout, FanoutStatus::Error, 0, latency_ms, Some(&detail))
                .await?;
            println!();
            println!("Server: {detail}");
            Ok(0)
        }
    }
}

fn parse_query(text: &str) -> Result<SearchQuery, SearchError> {
    SearchQuery::parse(&RawQuery::new(text), SearchFilters::default())
        .ok_or(SearchError::BlankQuery)
}

fn describe(query: &SearchQuery) -> String {
    let artist = query.artist.as_deref().unwrap_or("-");
    let album = query.album.as_deref().unwrap_or("-");
    match query.year {
        Some(year) => format!("{artist} / {album} ({year})"),
        None => format!("{artist} / {album}"),
    }
}

fn print_discogs(record: &FanoutRecord) {
    println!("Discogs: {}", record.detail);

    if record.answered() && !record.matches.is_empty() {
        for row in &record.matches {
            println!("  {}", render_release(row));
        }
    }
}

fn print_soulseek(outcome: &mud_core::provider::ProviderOutcome, sharing: bool) {
    let rejected = outcome.rejected.total();
    let shown = outcome.candidates.len().min(MAX_SOULSEEK_RESULTS);
    println!();
    println!(
        "Server: {} folders, {} files{}{}",
        outcome.candidates.len(),
        outcome.raw_hits,
        if rejected > 0 {
            format!(" ({rejected} rejected as not FLAC)")
        } else {
            String::new()
        },
        if shown < outcome.candidates.len() {
            format!("; showing {shown} fastest")
        } else {
            String::new()
        }
    );

    if mission_hint(outcome.candidates.len(), sharing).is_some() {
        println!(
            "  {}",
            mission_hint(outcome.candidates.len(), sharing).unwrap_or_default()
        );
    }

    for (index, candidate) in outcome
        .candidates
        .iter()
        .take(MAX_SOULSEEK_RESULTS)
        .enumerate()
    {
        println!("  {}", render_candidate(candidate, index + 1));
    }
}

/// A hint when a SoulSeek search comes back thin, since the usual cause is
/// configuration rather than the query.
fn mission_hint(found: usize, sharing: bool) -> Option<String> {
    if !sharing {
        return Some(
            "sharing nothing: peers queue this client last. Run \
             `mud --slsk-share <dir> search \"artist - album\"`"
                .to_owned(),
        );
    }
    if found == 0 {
        return Some(
            "nothing found: results come only from peers online right now, and an \
             open port (2234) helps"
                .to_owned(),
        );
    }
    None
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
fn render_candidate(candidate: &mud_core::candidate::Candidate, result_index: usize) -> String {
    let locator = match &candidate.locator {
        mud_core::candidate::Locator::SoulSeek(locator) => locator,
        // The provider only ever returns SoulSeek locators, but a torrent
        // locator is representable, and rendering it as if it were a peer
        // would be a lie.
        mud_core::candidate::Locator::Torrent(_) => {
            return format!("{} | not a SoulSeek result", folder_of(candidate));
        }
    };

    let qualities = match (locator.bit_depth, locator.sample_rate) {
        (Some(depth), Some(rate)) => {
            format!("{depth} {} kHz", rate.hz() / 1000)
        }
        _ => "quality unknown".to_owned(),
    };

    format!(
        "#{result_index:<2}  {}  {}  {} files  {}  {}  {} advertised  {}",
        folder_of(candidate),
        locator.peer,
        candidate.display.track_count,
        candidate.total_bytes,
        qualities,
        format_speed(locator.upload_speed_bps),
        if locator.has_free_slot {
            "free slot"
        } else {
            "queued"
        },
    )
}

fn format_speed(bytes_per_second: u32) -> String {
    if bytes_per_second >= 1_000_000 {
        format!("{:.1} MiB/s", f64::from(bytes_per_second) / 1_048_576.0)
    } else if bytes_per_second >= 1_000 {
        format!("{:.1} KiB/s", f64::from(bytes_per_second) / 1_024.0)
    } else {
        format!("{bytes_per_second} B/s")
    }
}

fn folder_of(candidate: &mud_core::candidate::Candidate) -> String {
    if candidate.display.artist.is_empty() && candidate.display.album.is_empty() {
        return "(share root)".to_owned();
    }
    if candidate.display.artist.is_empty() {
        return candidate.display.album.clone();
    }
    format!("{}/{}", candidate.display.artist, candidate.display.album)
}

#[derive(Debug, thiserror::Error)]
enum SearchError {
    #[error("the query was blank")]
    BlankQuery,

    #[error(transparent)]
    Settings(#[from] SettingsError),

    #[error(transparent)]
    Store(#[from] mud_store::StoreError),

    #[error("the Discogs search could not be recorded: {0}")]
    Catalog(#[from] mud_catalog::CatalogSearchError),
}

#[cfg(test)]
mod tests {
    use super::*;

    use mud_core::ReleaseId;
    use mud_core::audio::{BitDepth, ByteSize, SampleRate};
    use mud_core::candidate::{Candidate, CandidateDisplay, DedupeKey, Locator, SoulSeekLocator};
    use mud_core::ids::{CandidateId, DiscogsReleaseId};

    fn release(title: &str, artist: Option<&str>, year: Option<u16>, tracks: u16) -> CatalogRow {
        CatalogRow {
            release_id: ReleaseId(1),
            discogs_release_id: DiscogsReleaseId(1),
            title: title.to_owned(),
            artist: artist.map(str::to_owned),
            year,
            release_types: vec![mud_core::ReleaseType::Album],
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
            parse_query("   ").expect_err("blank"),
            SearchError::BlankQuery
        ));
    }

    #[test]
    fn a_quoted_query_keeps_its_parts() {
        let query = parse_query("Radiohead - OK Computer - 1997").expect("parses");
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
    fn sharing_nothing_is_called_out_even_when_results_arrived() {
        assert!(mission_hint(3, false).is_some_and(|hint| hint.contains("--slsk-share")));
        assert!(mission_hint(3, true).is_none());
    }

    #[test]
    fn an_empty_result_that_shared_is_explained_by_the_network_not_the_config() {
        let hint = mission_hint(0, true).expect("a hint");
        assert!(hint.contains("peers online"), "{hint}");
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
