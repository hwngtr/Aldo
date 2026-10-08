//! Recording a Discogs search as a fan-out.
//!
//! Shared by the CLI and the optional HTTP API so that both explain an empty
//! result the same way. A fan-out row is written even when the provider was
//! never asked, because "could not search" and "searched and found nothing"
//! must stay distinguishable after the fact.

use std::time::{Duration, Instant};

use mud_core::ProviderId;
use mud_core::ids::SessionId;
use mud_core::search::SearchQuery;
use mud_store::repo::{CatalogRow, FanoutStatus, SessionStore};

use crate::catalog_service::{CatalogSearchError, CatalogSearchOutcome, CatalogService};

/// Why a fan-out ended as it did.
///
/// The store keeps a coarser vocabulary than the caller needs, so the reason is
/// stated here rather than inferred from the status or, worse, from the text of
/// the detail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FanoutReason {
    /// The provider answered. Zero results is still an answer.
    Answered,
    /// No credentials, so the request was never sent.
    NotConfigured,
    /// The rate budget refused, so the request was never sent.
    BudgetDenied,
    /// The provider was asked and did not answer usefully.
    Failed,
}

/// What a fan-out produced, ready to be shown and already recorded.
#[derive(Debug, Clone)]
pub struct FanoutRecord {
    pub matches: Vec<CatalogRow>,
    pub reason: FanoutReason,
    pub status: FanoutStatus,
    /// Written to the fan-out row's error column. `None` on success, because a
    /// successful search has nothing to record as an error.
    pub error: Option<String>,
    /// Said to the user whatever the state.
    pub detail: String,
    pub retry_after: Option<Duration>,
}

impl FanoutRecord {
    fn done(found: CatalogSearchOutcome) -> Self {
        let detail = format!(
            "{} fetched, {} cached, {} reported",
            found.fetched, found.cached, found.raw_hits
        );
        Self {
            matches: found.matches,
            reason: FanoutReason::Answered,
            status: FanoutStatus::Done,
            error: None,
            detail,
            retry_after: None,
        }
    }

    fn not_configured() -> Self {
        let detail = "not configured: set MUD_DISCOGS_TOKEN".to_owned();
        Self {
            matches: Vec::new(),
            reason: FanoutReason::NotConfigured,
            status: FanoutStatus::Error,
            error: Some(detail.clone()),
            detail,
            retry_after: None,
        }
    }

    fn rate_limited(retry_after: Duration) -> Self {
        Self {
            matches: Vec::new(),
            reason: FanoutReason::BudgetDenied,
            status: FanoutStatus::BudgetDenied,
            error: Some("the Discogs rate budget refused the request".to_owned()),
            detail: format!("rate limited, retry in {retry_after:?}"),
            retry_after: Some(retry_after),
        }
    }

    fn failed(detail: String) -> Self {
        Self {
            matches: Vec::new(),
            reason: FanoutReason::Failed,
            status: FanoutStatus::Error,
            error: Some(detail.clone()),
            detail,
            retry_after: None,
        }
    }

    /// Whether the provider answered at all.
    #[must_use]
    pub fn answered(&self) -> bool {
        self.reason == FanoutReason::Answered
    }

    #[must_use]
    pub fn result_count(&self) -> u32 {
        u32::try_from(self.matches.len()).unwrap_or(u32::MAX)
    }
}

/// Runs the Discogs fan-out for an existing session and records it.
///
/// `service` is `None` when no token is configured, which is not a failure: the
/// request is simply never sent, and that is what gets recorded.
///
/// # Errors
/// Returns a catalog error only for a store failure while recording. A provider
/// failure is captured in the record rather than returned, because a search
/// that failed is still a search that happened.
pub async fn discogs(
    service: Option<&CatalogService>,
    sessions: &SessionStore,
    session_id: SessionId,
    query: &SearchQuery,
    limit: u32,
    now_ms: i64,
) -> Result<FanoutRecord, CatalogSearchError> {
    let fanout_id = sessions
        .begin_fanout(
            session_id,
            ProviderId::Discogs,
            &query.soulseek_query(),
            now_ms,
        )
        .await?;
    let started = Instant::now();

    let record = match service {
        None => FanoutRecord::not_configured(),
        Some(service) => match service.search(query, limit, now_ms).await {
            Ok(found) => FanoutRecord::done(found),
            Err(CatalogSearchError::RateLimited { retry_after }) => {
                FanoutRecord::rate_limited(retry_after)
            }
            Err(error) => {
                let detail = error.to_string();
                tracing::warn!(provider = "discogs", %detail, "search failed");
                FanoutRecord::failed(detail)
            }
        },
    };

    let latency_ms = u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX);
    sessions
        .finish_fanout(
            fanout_id,
            record.status,
            record.result_count(),
            latency_ms,
            record.error.as_deref(),
        )
        .await?;

    Ok(record)
}

#[cfg(test)]
mod tests {
    use super::*;

    use mud_store::Store;
    use mud_store::repo::LOCAL_USER;

    use crate::catalog_service::CatalogSearchOutcome;

    fn outcome(matches: usize) -> CatalogSearchOutcome {
        CatalogSearchOutcome {
            matches: Vec::new(),
            raw_hits: u32::try_from(matches).unwrap_or(0),
            fetched: u32::try_from(matches).unwrap_or(0),
            cached: 0,
        }
    }

    #[test]
    fn a_successful_record_has_nothing_to_report_as_an_error() {
        let record = FanoutRecord::done(outcome(3));
        assert!(record.answered());
        assert_eq!(record.status, FanoutStatus::Done);
        assert_eq!(record.error, None);
        assert_eq!(record.detail, "3 fetched, 0 cached, 3 reported");
    }

    #[test]
    fn a_provider_that_was_never_asked_is_not_the_same_as_one_that_found_nothing() {
        let unconfigured = FanoutRecord::not_configured();
        let empty = FanoutRecord::done(outcome(0));

        assert_eq!(unconfigured.reason, FanoutReason::NotConfigured);
        assert_eq!(empty.reason, FanoutReason::Answered);

        // Both have zero results, so the state is what tells them apart.
        assert!(unconfigured.matches.is_empty());
        assert!(empty.matches.is_empty());
        assert_ne!(unconfigured.status, empty.status);
    }

    #[test]
    fn a_refused_budget_keeps_how_long_to_wait() {
        let record = FanoutRecord::rate_limited(Duration::from_secs(12));
        assert_eq!(record.reason, FanoutReason::BudgetDenied);
        assert_eq!(record.status, FanoutStatus::BudgetDenied);
        assert_eq!(record.retry_after, Some(Duration::from_secs(12)));
        assert!(!record.answered());
    }

    #[test]
    fn a_failure_records_its_reason() {
        let record = FanoutRecord::failed("connection reset".to_owned());
        assert_eq!(record.reason, FanoutReason::Failed);
        assert_eq!(record.status, FanoutStatus::Error);
        assert_eq!(record.error.as_deref(), Some("connection reset"));
        assert!(!record.answered());
    }

    #[tokio::test]
    async fn a_fanout_is_recorded_even_when_no_provider_is_configured() {
        let store = Store::open_temporary().await.expect("store opens");
        mud_store::repo::ensure_local_user(store.pool(), "/tmp", 0)
            .await
            .expect("user");
        let sessions = SessionStore::new(store.pool().clone());

        let query = SearchQuery::parse(
            &mud_core::search::RawQuery::new("radiohead - ok computer"),
            mud_core::search::SearchFilters::default(),
        )
        .expect("query");
        let session_id = sessions
            .create(LOCAL_USER, &query, 0)
            .await
            .expect("session");

        let record = discogs(None, &sessions, session_id, &query, 5, 0)
            .await
            .expect("recorded");

        assert_eq!(record.reason, FanoutReason::NotConfigured);
        let rows = sessions.list_fanouts(session_id).await.expect("fanouts");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].provider, ProviderId::Discogs);
        assert_eq!(rows[0].status, FanoutStatus::Error);
        assert!(
            rows[0]
                .error
                .as_deref()
                .is_some_and(|error| error.contains("MUD_DISCOGS_TOKEN")),
            "the recorded error does not say what to do: {:?}",
            rows[0].error
        );
    }
}
