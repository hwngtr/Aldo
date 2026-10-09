//! Response shapes.
//!
//! The store's row types are an internal detail: column order, presence of local
//! ids, and cache bookkeeping must not become part of the wire contract. Each
//! route returns a type declared here.

use aldo_core::audio::{BitDepth, DurationMs, SampleRate};
use aldo_core::ids::{CandidateId, DiscogsReleaseId, ReleaseId, SessionId};
use aldo_core::search::SearchFilters;
use serde::{Deserialize, Serialize};

use aldo_store::repo::{AcquisitionRow, BudgetSnapshot, CandidateRow, CatalogRow, SessionRow};

/// A past search, without the cached filter JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionView {
    pub session_id: SessionId,
    pub query: String,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub year: Option<u16>,
    /// How many results the search left, across every provider.
    pub result_count: u32,
    pub created_at_ms: i64,
}

impl From<SessionRow> for SessionView {
    fn from(row: SessionRow) -> Self {
        Self {
            session_id: row.session_id,
            query: row.query.phrase,
            artist: row.query.artist,
            album: row.query.album,
            year: row.query.year,
            result_count: row.result_count,
            created_at_ms: row.created_at,
        }
    }
}

/// A search result row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateView {
    pub candidate_id: CandidateId,
    pub artist: String,
    pub album: String,
    pub year: Option<u16>,
    pub track_count: u16,
    pub disc_count: u8,
    pub total_bytes: u64,
    /// How many providers agreed on this release.
    pub provider_count: u32,
    pub seeders: Option<u32>,
    pub sample_rate_hz: Option<u32>,
    pub bit_depth: Option<u8>,
}

impl From<CandidateRow> for CandidateView {
    fn from(row: CandidateRow) -> Self {
        Self {
            candidate_id: row.candidate_id,
            artist: row.display_artist,
            album: row.display_album,
            year: row.display_year,
            track_count: row.track_count,
            disc_count: row.disc_count,
            total_bytes: row.total_bytes.as_u64(),
            provider_count: row.provider_count,
            seeders: row.seeders,
            sample_rate_hz: row.best_sample_rate_hz.map(SampleRate::hz),
            bit_depth: row.best_bit_depth.map(BitDepth::bits),
        }
    }
}

/// An in-flight download.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcquisitionView {
    pub acquisition_id: aldo_core::ids::AcquisitionId,
    pub candidate_id: CandidateId,
    pub engine: String,
    pub status: String,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub speed_bps: u32,
    pub asset_count: u32,
    pub complete_assets: u32,
}

impl From<AcquisitionRow> for AcquisitionView {
    fn from(row: AcquisitionRow) -> Self {
        Self {
            acquisition_id: row.acquisition_id,
            candidate_id: row.candidate_id,
            engine: row.engine.as_str().to_owned(),
            status: row.status.as_str().to_owned(),
            bytes_done: row.bytes_done.as_u64(),
            bytes_total: row.bytes_total.as_u64(),
            speed_bps: row.speed_bps,
            asset_count: row.asset_count,
            complete_assets: row.complete_assets,
        }
    }
}

/// Remaining allowance for one upstream.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetView {
    pub provider: String,
    pub capacity: u32,
    pub available: u32,
    pub retry_after_ms: u64,
}

impl BudgetView {
    #[must_use]
    pub fn new(provider: &str, snapshot: BudgetSnapshot) -> Self {
        Self {
            provider: provider.to_owned(),
            capacity: snapshot.capacity,
            available: snapshot.available,
            retry_after_ms: DurationMs::from_std(snapshot.retry_after).as_millis(),
        }
    }
}

/// The body of `POST /search`.
#[derive(Debug, Clone, Deserialize)]
pub struct SearchRequest {
    pub query: String,
    #[serde(default)]
    pub filters: SearchFilters,
    /// How many release bodies to fetch. Clamped by the catalog service to its
    /// own ceiling, because each fetch spends rate budget.
    #[serde(default)]
    pub release_limit: Option<u32>,
}

impl SearchRequest {
    #[must_use]
    pub fn parsed(&self) -> Option<aldo_core::SearchQuery> {
        aldo_core::SearchQuery::parse(&aldo_core::RawQuery::new(&self.query), self.filters)
    }

    #[must_use]
    pub fn release_limit(&self) -> u32 {
        self.release_limit
            .unwrap_or(aldo_catalog::DEFAULT_RELEASE_LIMIT)
    }
}

/// One release Discogs matched.
///
/// These are catalog identities, not downloaded files: nothing here has a
/// source yet, which is why the route calls them matches rather than candidates.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseMatchView {
    pub release_id: ReleaseId,
    pub discogs_release_id: DiscogsReleaseId,
    pub title: String,
    pub artist: Option<String>,
    pub year: Option<u16>,
    /// Vorbis spellings, so the shell can render them without a translation.
    pub release_types: Vec<String>,
    pub track_count: u16,
    pub cover_art_url: Option<String>,
}

impl From<CatalogRow> for ReleaseMatchView {
    fn from(row: CatalogRow) -> Self {
        Self {
            release_id: row.release_id,
            discogs_release_id: row.discogs_release_id,
            title: row.title,
            artist: row.artist,
            year: row.year,
            release_types: row
                .release_types
                .iter()
                .map(|kind| kind.as_vorbis_value().to_owned())
                .collect(),
            track_count: row.track_count,
            cover_art_url: row.cover_art_url,
        }
    }
}

/// What a provider did for one search.
///
/// "Found nothing" and "was never asked" must not look alike: the first is an
/// answer, the second is setup work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderState {
    /// The provider answered. Zero results is still an answer.
    Done,
    /// The rate budget refused the request, which was therefore never sent.
    BudgetDenied,
    /// Credentials are missing, so the search could not be attempted.
    NotConfigured,
    /// The provider was asked and did not answer usefully.
    Failed,
}

/// One provider's contribution to a search, for display.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderStatusView {
    pub provider: String,
    pub state: ProviderState,
    pub result_count: u32,
    pub latency_ms: u32,
    /// Why, when the state is not `done`.
    pub detail: Option<String>,
    /// Milliseconds until the budget refills, when it refused.
    pub retry_after_ms: Option<u64>,
}

/// The body of a search response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResponse {
    pub session_id: SessionId,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub year: Option<u16>,
    pub matches: Vec<ReleaseMatchView>,
    pub providers: Vec<ProviderStatusView>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use aldo_core::audio::ByteSize;
    use aldo_core::ids::UserId;
    use aldo_core::search::SearchQuery;

    #[test]
    fn a_search_request_keeps_its_filters() {
        let request: SearchRequest =
            serde_json::from_str(r#"{"query":"A - B","filters":{"lossless_only":true}}"#)
                .expect("parsed");
        let parsed = request.parsed().expect("parses");

        assert!(parsed.filters.lossless_only);
        assert_eq!(parsed.album.as_deref(), Some("B"));
    }

    #[test]
    fn a_search_request_defaults_to_lossless_only() {
        let request: SearchRequest = serde_json::from_str(r#"{"query":"A - B"}"#).expect("parsed");
        assert!(request.parsed().expect("parses").filters.lossless_only);
    }

    #[test]
    fn an_unknown_filter_field_is_refused_rather_than_ignored() {
        assert!(
            serde_json::from_str::<SearchRequest>(r#"{"query":"A","filters":{"nope":true}}"#)
                .is_err(),
            "a misspelled filter was silently dropped"
        );
    }

    #[test]
    fn a_blank_query_parses_to_nothing() {
        let request = SearchRequest {
            query: "   ".to_owned(),
            filters: SearchFilters::default(),
            release_limit: None,
        };
        assert!(request.parsed().is_none());
    }

    #[test]
    fn session_view_exposes_the_query_without_the_cached_filter_json() {
        let row = SessionRow {
            session_id: SessionId(4),
            user_id: UserId(1),
            query: SearchQuery {
                phrase: "Radiohead - OK Computer".into(),
                artist: Some("Radiohead".into()),
                album: Some("OK Computer".into()),
                year: Some(1997),
                filters: SearchFilters::default(),
            },
            result_count: 3,
            created_at: 1_700_000_000_000,
        };

        let view = SessionView::from(row);
        assert_eq!(view.album.as_deref(), Some("OK Computer"));
        assert_eq!(view.year, Some(1997));
        assert_eq!(view.result_count, 3);

        let encoded = serde_json::to_string(&view).expect("encoded");
        assert!(!encoded.contains("lossless_only"), "{encoded}");
    }

    #[test]
    fn budget_view_converts_a_snapshot_without_losing_the_wait() {
        use std::time::Duration;

        let view = BudgetView::new(
            "soulseek",
            BudgetSnapshot {
                capacity: 34,
                available: 21,
                retry_after: Duration::from_millis(2_500),
            },
        );

        assert_eq!(view.provider, "soulseek");
        assert_eq!(view.capacity, 34);
        assert_eq!(view.available, 21);
        assert_eq!(view.retry_after_ms, 2_500);
    }

    #[test]
    fn a_candidate_view_does_not_leak_store_internals() {
        // Derived from the real serialisation, so it cannot drift from the type.
        let row = CandidateRow {
            candidate_id: CandidateId(1),
            dedupe_key: aldo_core::DedupeKey::from_release_parts("A", "B", None, 1, 1),
            display_artist: "A".into(),
            display_album: "B".into(),
            display_year: None,
            track_count: 1,
            disc_count: 1,
            total_bytes: ByteSize::new(10),
            provider_count: 2,
            seeders: Some(3),
            best_sample_rate_hz: SampleRate::new(44_100),
            best_bit_depth: BitDepth::new(16),
        };

        let encoded = serde_json::to_value(CandidateView::from(row)).expect("encoded");
        let keys: Vec<&str> = encoded
            .as_object()
            .expect("object")
            .keys()
            .map(String::as_str)
            .collect();

        assert!(keys.contains(&"provider_count"));
        for internal in ["dedupe_key", "release_id", "session_id", "source_count"] {
            assert!(!keys.contains(&internal), "{internal} leaked into the API");
        }
    }

    #[test]
    fn an_acquisition_view_reports_progress_the_shell_can_render() {
        use aldo_core::ids::{AcquisitionId, InfoHash};
        use aldo_store::repo::{AcquisitionStatus, Engine};

        let row = AcquisitionRow {
            acquisition_id: AcquisitionId(7),
            candidate_id: CandidateId(1),
            engine: Engine::Qbittorrent,
            status: AcquisitionStatus::Downloading,
            qb_hash: Some(InfoHash::from_hex(&"ab".repeat(20)).expect("valid hash")),
            bytes_done: ByteSize::new(50),
            bytes_total: ByteSize::new(100),
            speed_bps: 1_024,
            asset_count: 12,
            complete_assets: 6,
        };

        let encoded = serde_json::to_value(AcquisitionView::from(row)).expect("encoded");
        assert_eq!(encoded["bytes_done"], 50);
        assert_eq!(encoded["complete_assets"], 6);

        // The internal qbittorrent task hash is not part of the contract.
        assert!(encoded.get("qb_hash").is_none());
    }
}
