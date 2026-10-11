//! What a search does, independent of how it is shown.
//!
//! The CLI prints it; the TUI lists it. Both call [`run`], so a search records
//! the same rows, reserves the same budget and applies the same ranking either
//! way. Rendering lives in the callers.

use std::time::Duration;

use aldo_catalog::{FanoutRecord, record_discogs};
use aldo_core::candidate::Candidate;
use aldo_core::error::ProviderError;
use aldo_core::ids::{CandidateId, SessionId};
use aldo_core::provider::DiscoveryProvider as _;
use aldo_core::search::{RawQuery, SearchFilters, SearchQuery};
use aldo_soulseek::{SoulSeekConfig, SoulSeekProvider, rank_sources_for_query};
use aldo_store::RateBudget;
use aldo_store::repo::{CandidateStore, FanoutStatus, LOCAL_USER, SessionStore};
use tokio::sync::mpsc;

use crate::settings::{Settings, SettingsError};

/// Keep the interactive result list short enough to scan.
pub const MAX_SOULSEEK_RESULTS: usize = 10;

/// How far along a search is, for a front end that can show progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchPhase {
    Discogs,
    SoulSeek,
}

/// What a search produced, ready to be rendered or acted on.
#[derive(Debug)]
pub struct SearchRun {
    pub query: SearchQuery,
    pub discogs: FanoutRecord,
    pub soulseek: SoulSeekReport,
}

/// What became of the SoulSeek half of a search.
///
/// "Was never asked", "was refused" and "answered with nothing" are three
/// different facts, so they are three different values rather than an empty
/// result with a side-channel reason.
#[derive(Debug)]
pub enum SoulSeekReport {
    /// No account is configured, so the network was never asked.
    NotConfigured,
    /// The server rejected the login: the password is wrong, or the name is
    /// already taken by someone else. Distinct from "not configured" because
    /// the fix is to check the account, not to set one.
    Refused,
    /// The rate budget refused the request, which was therefore never sent.
    RateLimited { retry_after: Duration },
    /// The network could not be reached, or the login was refused.
    Unreachable { message: String },
    /// The search ran. Zero candidates is still an answer.
    Answered(SoulSeekFound),
}

/// A SoulSeek search that reached the network.
#[derive(Debug)]
pub struct SoulSeekFound {
    pub candidates: Vec<Candidate>,
    /// Hits seen before filtering, used to explain an empty result.
    pub raw_hits: u32,
    pub rejected: u32,
    pub shares_anything: bool,
    pub shared_directories: Vec<String>,
    pub search_timeout: Duration,
    pub listen_port: u16,
}

impl SoulSeekReport {
    /// The candidates this report carries, or an empty slice.
    ///
    /// Lets a renderer treat "answered with nothing", "was refused" and "was
    /// never asked" uniformly when all it wants is the folders.
    #[must_use]
    pub fn sources(&self) -> &[Candidate] {
        match self {
            Self::Answered(found) => &found.candidates,
            Self::NotConfigured
            | Self::Refused
            | Self::RateLimited { .. }
            | Self::Unreachable { .. } => &[],
        }
    }
}

impl SoulSeekFound {
    /// Why a thin search came back thin, when the usual cause is configuration
    /// rather than the query.
    ///
    /// Shared so the CLI and the interface explain an empty result the same
    /// way. An empty answer from SoulSeek is not proof that a release does not
    /// exist: results come only from peers online at that moment.
    #[must_use]
    pub fn hint(&self) -> Option<String> {
        if !self.shares_anything {
            return Some(
                "sharing nothing: peers queue this client last. Run \
                 `aldo --slsk-share <dir> search \"artist - album\"`"
                    .to_owned(),
            );
        }
        if !self.candidates.is_empty() {
            return None;
        }
        if self.rejected > 0 {
            return Some(format!(
                "connected and received {} file hits, but all were rejected by \
                 Aldo's lossless-file filters ({} rejected); try a broader query",
                self.raw_hits, self.rejected
            ));
        }
        Some(format!(
            "no peer files returned after {}s; sharing {}. Results come only from \
             peers online right now. Retry with `--slsk-timeout {}`; forwarding port \
             {} can improve availability",
            self.search_timeout.as_secs(),
            self.shared_directories.join(", "),
            self.search_timeout.as_secs().saturating_mul(2).max(20),
            self.listen_port
        ))
    }
}

/// Parses user input into a query, or reports it as blank.
///
/// # Errors
/// Returns [`SearchRunError::BlankQuery`] when the text has no content.
pub fn parse(text: &str) -> Result<SearchQuery, SearchRunError> {
    SearchQuery::parse(&RawQuery::new(text), SearchFilters::default())
        .ok_or(SearchRunError::BlankQuery)
}

/// Runs one search against every configured provider and records it.
///
/// `progress` receives a message before each provider is asked, so a front end
/// can say what it is waiting on. Passing `None` runs silently.
///
/// # Errors
/// Fails only for a store or configuration failure. A provider that failed is
/// captured in the returned report rather than returned as an error, because a
/// search that failed is still a search that happened.
pub async fn run(
    settings: &Settings,
    query: &SearchQuery,
    limit: u32,
    progress: Option<&mpsc::UnboundedSender<SearchPhase>>,
) -> Result<SearchRun, SearchRunError> {
    let store = settings.open_store().await?;
    let sessions = SessionStore::new(store.pool().clone());
    let now = aldo_store::now_ms();

    // Recorded before either provider is asked, so a search that could not be
    // made still shows up in `aldo sessions`.
    let session_id = sessions.create(LOCAL_USER, query, now).await?;

    notify(progress, SearchPhase::Discogs);
    let discogs = settings.discogs(&store)?;
    let record = record_discogs(discogs.as_ref(), &sessions, session_id, query, limit, now).await?;
    if let Some(release) = record.matches.first() {
        sessions
            .set_selected_discogs_release(session_id, release.discogs_release_id)
            .await?;
    }
    let discogs_count = record.result_count();

    notify(progress, SearchPhase::SoulSeek);
    let soulseek = match settings.soulseek()? {
        None => SoulSeekReport::NotConfigured,
        Some(config) => soulseek(&sessions, session_id, query, config, &store, now).await?,
    };
    let soulseek_count = match &soulseek {
        SoulSeekReport::Answered(found) => {
            u32::try_from(found.candidates.len()).unwrap_or(u32::MAX)
        }
        _ => 0,
    };

    let result_count = discogs_count.saturating_add(soulseek_count);
    sessions.set_result_count(session_id, result_count).await?;

    Ok(SearchRun {
        query: query.clone(),
        discogs: record,
        soulseek,
    })
}

/// Connects, searches, records the fan-out, and returns what came back.
async fn soulseek(
    sessions: &SessionStore,
    session_id: SessionId,
    query: &SearchQuery,
    config: SoulSeekConfig,
    store: &aldo_store::Store,
    now: i64,
) -> Result<SoulSeekReport, SearchRunError> {
    // One search is one unit of the budget. Exceeding it is a thirty-minute
    // ban, so the refusal is reported rather than retried.
    let budgets = aldo_store::repo::BudgetStore::new(store.pool().clone());
    budgets
        .ensure("soulseek", RateBudget::SOULSEEK_SEARCH, now)
        .await?;
    if let Err(wait) = budgets.acquire("soulseek", now).await? {
        return Ok(SoulSeekReport::RateLimited { retry_after: wait });
    }

    // Read the config facts before it is moved into the connection, because an
    // empty result is usually explained by one of them.
    let shares_anything = config.shares_anything();
    let shared_directories = config.shared_directories.clone();
    let search_timeout = config.search_timeout;
    let listen_port = config.listen_port;

    let provider = match SoulSeekProvider::connect(config).await {
        Ok(provider) => provider,
        // A refused login is not a network fault, so it is reported as its own
        // fact rather than folded into "could not reach".
        Err(ProviderError::Unauthorised { .. }) => return Ok(SoulSeekReport::Refused),
        Err(error) => {
            return Ok(SoulSeekReport::Unreachable {
                message: error.to_string(),
            });
        }
    };

    let fanout = sessions
        .begin_fanout(
            session_id,
            aldo_core::ProviderId::SoulSeek,
            &query.soulseek_query(),
            now,
        )
        .await?;
    let started = std::time::Instant::now();

    let outcome = provider.search(query).await;
    let latency_ms = u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX);

    match outcome {
        Ok(mut found) => {
            rank_sources_for_query(&mut found.candidates, query);

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

            let daily_indexes: Vec<(u8, CandidateId)> = found
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
            let utc_day = aldo_store::now_ms().div_euclid(86_400_000);
            candidate_store
                .replace_daily_indexes(utc_day, &daily_indexes)
                .await?;

            let count = u32::try_from(found.candidates.len()).unwrap_or(u32::MAX);
            sessions
                .finish_fanout(fanout, FanoutStatus::Done, count, latency_ms, None)
                .await?;

            Ok(SoulSeekReport::Answered(SoulSeekFound {
                candidates: found.candidates,
                raw_hits: found.raw_hits,
                rejected: found.rejected.total(),
                shares_anything,
                shared_directories,
                search_timeout,
                listen_port,
            }))
        }
        Err(error) => {
            let detail = error.to_string();
            sessions
                .finish_fanout(fanout, FanoutStatus::Error, 0, latency_ms, Some(&detail))
                .await?;
            Ok(SoulSeekReport::Unreachable { message: detail })
        }
    }
}

fn notify(progress: Option<&mpsc::UnboundedSender<SearchPhase>>, phase: SearchPhase) {
    if let Some(sender) = progress {
        let _ = sender.send(phase);
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SearchRunError {
    #[error("the query was blank")]
    BlankQuery,

    #[error(transparent)]
    Settings(#[from] SettingsError),

    #[error(transparent)]
    Store(#[from] aldo_store::StoreError),

    #[error("the Discogs search could not be recorded: {0}")]
    Catalog(#[from] aldo_catalog::CatalogSearchError),
}

#[cfg(test)]
mod tests {
    use super::*;

    use aldo_core::audio::{BitDepth, ByteSize, SampleRate};
    use aldo_core::candidate::{CandidateDisplay, DedupeKey, Locator, SoulSeekLocator};

    fn found(
        candidates: Vec<Candidate>,
        raw_hits: u32,
        rejected: u32,
        shared: bool,
    ) -> SoulSeekFound {
        SoulSeekFound {
            candidates,
            raw_hits,
            rejected,
            shares_anything: shared,
            shared_directories: vec!["/home/you/Music".to_owned()],
            search_timeout: Duration::from_secs(8),
            listen_port: 2234,
        }
    }

    fn one_source() -> Candidate {
        Candidate {
            candidate_id: CandidateId(1),
            dedupe_key: DedupeKey::from_release_parts("A", "B", None, 1, 1),
            display: CandidateDisplay {
                artist: "A".to_owned(),
                album: "B".to_owned(),
                year: None,
                track_count: 1,
                disc_count: 1,
            },
            locator: Locator::SoulSeek(SoulSeekLocator {
                peer: "p".to_owned(),
                remote_path: "x".to_owned(),
                size: ByteSize::new(1),
                has_free_slot: true,
                queue_length: None,
                upload_speed_bps: 1_024,
                sample_rate: SampleRate::new(44_100),
                bit_depth: BitDepth::new(16),
                duration: None,
            }),
            sources: Vec::new(),
            files: Vec::new(),
            total_bytes: ByteSize::new(0),
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
    fn a_structured_query_is_understood() {
        let query = parse("Radiohead - OK Computer - 1997").expect("parses");
        assert_eq!(query.artist.as_deref(), Some("Radiohead"));
        assert_eq!(query.album.as_deref(), Some("OK Computer"));
        assert_eq!(query.year, Some(1997));
    }

    #[test]
    fn sharing_nothing_is_called_out_even_when_results_arrived() {
        assert!(
            found(vec![one_source()], 3, 0, false)
                .hint()
                .is_some_and(|hint| hint.contains("--slsk-share"))
        );
        assert!(found(vec![one_source()], 3, 0, true).hint().is_none());
    }

    #[test]
    fn an_empty_result_that_shared_is_explained_by_the_network_not_the_config() {
        let hint = found(Vec::new(), 0, 0, true).hint().expect("a hint");
        assert!(hint.contains("peer files returned"), "{hint}");
        assert!(hint.contains("--slsk-timeout 20"), "{hint}");

        let mut long = found(Vec::new(), 0, 0, true);
        long.search_timeout = Duration::from_secs(30);
        let hint = long.hint().expect("a hint");
        assert!(hint.contains("--slsk-timeout 60"), "{hint}");
    }

    #[test]
    fn filtered_hits_are_distinguished_from_an_empty_network_search() {
        let hint = found(Vec::new(), 4, 4, true).hint().expect("a hint");
        assert!(hint.contains("received 4 file hits"), "{hint}");
        assert!(hint.contains("4 rejected"), "{hint}");
        assert!(!hint.contains("--slsk-timeout"), "{hint}");
    }

    #[test]
    fn a_report_that_was_never_asked_carries_no_sources() {
        assert_eq!(SoulSeekReport::NotConfigured.sources(), []);
        assert_eq!(SoulSeekReport::Refused.sources(), []);
        assert_eq!(
            SoulSeekReport::RateLimited {
                retry_after: Duration::from_secs(5)
            }
            .sources(),
            []
        );
    }
}
