//! Discogs lookup as a service.
//!
//! Owns the three things a catalog search must do together: spend the rate
//! budget, fetch only what is not already cached, and map the result into the
//! local store. Nothing here sleeps or retries on its own; a refusal is
//! reported so the caller can decide.

use std::time::Duration;

use crate::{DiscogsClient, identity_from_payload};
use aldo_core::error::ProviderError;
use aldo_core::search::SearchQuery;
use aldo_store::StoreError;
use aldo_store::repo::{BudgetStore, CatalogRow, CatalogStore, RateBudget};

/// The label used for the persisted budget and the fanout row.
pub const PROVIDER: &str = "discogs";

/// How many release bodies one search fetches.
///
/// Every release is a separate API call against a sixty-per-minute budget, so
/// this decides how much of that budget a single search may spend.
pub const DEFAULT_RELEASE_LIMIT: u32 = 5;

/// The ceiling a request may ask for. Ten releases still leaves room for other
/// searches in the same minute.
pub const MAX_RELEASE_LIMIT: u32 = 10;

/// What a catalog search produced.
#[derive(Debug, Clone)]
pub struct CatalogSearchOutcome {
    pub matches: Vec<CatalogRow>,
    /// Hits Discogs reported, before any release body was fetched. Zero here
    /// with zero matches means the album is not on Discogs.
    pub raw_hits: u32,
    /// Release bodies fetched from Discogs on this call.
    pub fetched: u32,
    /// Release bodies served from the permanent local cache.
    pub cached: u32,
}

/// Why a catalog search did not produce matches.
#[derive(Debug, thiserror::Error)]
pub enum CatalogSearchError {
    #[error("Discogs is not configured: set a personal access token")]
    NotConfigured,

    #[error("Discogs rate limit exhausted, retry in {retry_after:?}")]
    RateLimited { retry_after: Duration },

    #[error(transparent)]
    Provider(#[from] ProviderError),

    #[error(transparent)]
    Store(#[from] StoreError),

    #[error("a cached Discogs release could not be read: {0}")]
    Malformed(String),
}

/// A Discogs client bound to the store's cache and budget.
#[derive(Debug, Clone)]
pub struct CatalogService {
    client: DiscogsClient,
    catalog: CatalogStore,
    budgets: BudgetStore,
}

impl CatalogService {
    #[must_use]
    pub fn new(client: DiscogsClient, catalog: CatalogStore, budgets: BudgetStore) -> Self {
        Self {
            client,
            catalog,
            budgets,
        }
    }

    /// Searches Discogs and returns the stored releases.
    ///
    /// `limit` is clamped to [`MAX_RELEASE_LIMIT`]; a caller cannot spend more of
    /// the budget than the ceiling allows.
    ///
    /// # Errors
    /// Returns [`CatalogSearchError::RateLimited`] when the budget refuses: the
    /// request was never sent, which is a different fact from a failed one.
    pub async fn search(
        &self,
        query: &SearchQuery,
        limit: u32,
        now_ms: i64,
    ) -> Result<CatalogSearchOutcome, CatalogSearchError> {
        let limit = limit.clamp(1, MAX_RELEASE_LIMIT);
        self.budgets
            .ensure(PROVIDER, RateBudget::DISCOGS_AUTHENTICATED, now_ms)
            .await?;

        self.spend(now_ms).await?;
        let page = self.client.search(query, 1).await?;
        let raw_hits = u32::try_from(page.results.len()).unwrap_or(u32::MAX);

        let mut matches = Vec::new();
        let mut fetched = 0_u32;
        let mut cached = 0_u32;

        for hit in page.results.into_iter().take(limit as usize) {
            let cached_payload = self.catalog.cached_discogs_payload(hit.release_id).await?;

            let identity = if let Some(payload) = cached_payload {
                cached += 1;
                identity_from_payload(&payload).map_err(
                    |error: aldo_core::error::CatalogError| {
                        CatalogSearchError::Malformed(error.to_string())
                    },
                )?
            } else {
                // A cache miss is a request, so it costs a token.
                self.spend(now_ms).await?;
                let release = self.client.release(hit.release_id).await?;
                self.catalog
                    .cache_discogs_payload(hit.release_id, &release.payload, now_ms)
                    .await?;
                fetched += 1;
                release.identity
            };

            let release_id = self.catalog.upsert_release(&identity, now_ms).await?;
            matches.push(self.catalog.get(release_id).await?);
        }

        Ok(CatalogSearchOutcome {
            matches,
            raw_hits,
            fetched,
            cached,
        })
    }

    /// Searches both interpretations of a structured `artist - album` query.
    ///
    /// Discogs field searches are directional, while users commonly type
    /// either order. Duplicate release IDs are merged before the limit is
    /// applied to the displayed result.
    pub async fn search_symmetric(
        &self,
        query: &SearchQuery,
        limit: u32,
        now_ms: i64,
    ) -> Result<CatalogSearchOutcome, CatalogSearchError> {
        if query.artist.is_none() || query.album.is_none() {
            return self.search(query, limit, now_ms).await;
        }

        let first = self.search(query, limit, now_ms).await?;
        let second = self
            .search(&query.swapped_artist_album(), limit, now_ms)
            .await?;

        let mut matches = first.matches;
        for row in second.matches {
            if !matches
                .iter()
                .any(|existing| existing.discogs_release_id == row.discogs_release_id)
            {
                matches.push(row);
            }
        }
        matches.truncate(limit.clamp(1, MAX_RELEASE_LIMIT) as usize);

        Ok(CatalogSearchOutcome {
            matches,
            raw_hits: first.raw_hits.saturating_add(second.raw_hits),
            fetched: first.fetched.saturating_add(second.fetched),
            cached: first.cached.saturating_add(second.cached),
        })
    }

    /// Consumes one unit of the Discogs budget.
    async fn spend(&self, now_ms: i64) -> Result<(), CatalogSearchError> {
        match self.budgets.acquire(PROVIDER, now_ms).await? {
            Ok(_) => Ok(()),
            Err(retry_after) => Err(CatalogSearchError::RateLimited { retry_after }),
        }
    }

    /// How much of the Discogs allowance is left, for diagnostics.
    ///
    /// # Errors
    /// Returns a store error if the budget row cannot be read.
    pub async fn remaining(
        &self,
        now_ms: i64,
    ) -> Result<aldo_store::repo::BudgetSnapshot, CatalogSearchError> {
        self.budgets
            .ensure(PROVIDER, RateBudget::DISCOGS_AUTHENTICATED, now_ms)
            .await?;
        Ok(self.budgets.snapshot(PROVIDER, now_ms).await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DiscogsClient, DiscogsToken};
    use aldo_net::{HttpClient, ProxyConfig};
    use aldo_store::Store;
    use aldo_store::repo::{BudgetStore, CatalogStore};
    use aldo_testkit::{DiscogsStub, RELEASE_123, SEARCH_NO_HITS, SEARCH_ONE_HIT, discogs_stub};

    /// A fixed instant, so the budget never refills mid-test.
    const NOW: i64 = 1_700_000_000_000;

    async fn service_with(stub: &DiscogsStub) -> (CatalogService, Store) {
        let store = Store::open_temporary().await.expect("store opens");
        let http =
            HttpClient::new(&crate::user_agent(), &ProxyConfig::direct()).expect("http client");
        let client =
            DiscogsClient::with_base(http, Some(DiscogsToken::new("token")), &stub.base_url)
                .expect("discogs client");

        let service = CatalogService::new(
            client,
            CatalogStore::new(store.pool().clone()),
            BudgetStore::new(store.pool().clone()),
        );
        (service, store)
    }

    fn query() -> SearchQuery {
        SearchQuery::parse(
            &aldo_core::search::RawQuery::new("Radiohead - OK Computer"),
            aldo_core::search::SearchFilters::default(),
        )
        .expect("query parses")
    }

    #[tokio::test]
    async fn a_search_stores_the_matched_release_with_its_artist() {
        let stub = discogs_stub(SEARCH_ONE_HIT, RELEASE_123).await;
        let (service, _store) = service_with(&stub).await;

        let outcome = service.search(&query(), 5, NOW).await.expect("search");

        assert_eq!(outcome.raw_hits, 1);
        assert_eq!(outcome.fetched, 1);
        assert_eq!(outcome.cached, 0);
        assert_eq!(outcome.matches.len(), 1);

        let matched = &outcome.matches[0];
        assert_eq!(matched.title, "OK Computer");
        assert_eq!(
            matched.artist.as_deref(),
            Some("Radiohead"),
            "the album artist must survive the mapping and persistence"
        );
        assert_eq!(matched.year, Some(1997));
        assert_eq!(matched.track_count, 2);
        assert_eq!(
            matched.cover_art_url.as_deref(),
            Some("https://img.example/cover.jpg")
        );
    }

    #[tokio::test]
    async fn a_repeated_search_serves_the_release_from_the_cache() {
        let stub = discogs_stub(SEARCH_ONE_HIT, RELEASE_123).await;
        let (service, _store) = service_with(&stub).await;

        let first = service.search(&query(), 5, NOW).await.expect("first");
        let second = service.search(&query(), 5, NOW).await.expect("second");

        assert_eq!(first.fetched, 1);
        assert_eq!(second.fetched, 0, "the release body was fetched twice");
        assert_eq!(second.cached, 1);
        assert_eq!(
            stub.release_calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the cached body should not have cost a request"
        );
        // The search itself is never cached: it is the discovery step.
        assert_eq!(
            stub.search_calls.load(std::sync::atomic::Ordering::SeqCst),
            2
        );
    }

    #[tokio::test]
    async fn no_hits_is_an_empty_result_and_not_a_failure() {
        let stub = discogs_stub(SEARCH_NO_HITS, RELEASE_123).await;
        let (service, _store) = service_with(&stub).await;

        let outcome = service.search(&query(), 5, NOW).await.expect("search");

        assert!(outcome.matches.is_empty());
        assert_eq!(outcome.raw_hits, 0);
        assert_eq!(outcome.fetched, 0);
        assert_eq!(
            stub.release_calls.load(std::sync::atomic::Ordering::SeqCst),
            0
        );
    }

    #[tokio::test]
    async fn a_drained_budget_refuses_before_the_request_is_sent() {
        let stub = discogs_stub(SEARCH_ONE_HIT, RELEASE_123).await;
        let (service, _store) = service_with(&stub).await;

        service
            .budgets
            .ensure(PROVIDER, RateBudget::DISCOGS_AUTHENTICATED, NOW)
            .await
            .expect("budget registered");
        for _ in 0..RateBudget::DISCOGS_AUTHENTICATED.capacity {
            service
                .budgets
                .acquire(PROVIDER, NOW)
                .await
                .expect("acquire")
                .expect("granted");
        }

        let error = service.search(&query(), 5, NOW).await.expect_err("refused");

        assert!(
            matches!(error, CatalogSearchError::RateLimited { .. }),
            "{error:?}"
        );
        assert_eq!(
            stub.search_calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "a refused search must not reach the network"
        );
    }

    #[tokio::test]
    async fn a_search_spends_one_token_for_the_query_and_one_per_body() {
        let stub = discogs_stub(SEARCH_ONE_HIT, RELEASE_123).await;
        let (service, _store) = service_with(&stub).await;

        let before = service.remaining(NOW).await.expect("budget");
        service.search(&query(), 5, NOW).await.expect("search");
        let after = service.remaining(NOW).await.expect("budget");

        assert_eq!(
            before.available - after.available,
            2,
            "one search, one fetch"
        );
    }

    #[tokio::test]
    async fn a_release_missing_optional_sections_is_still_stored() {
        let stub = discogs_stub(SEARCH_ONE_HIT, aldo_testkit::RELEASE_MINIMAL).await;
        let (service, _store) = service_with(&stub).await;

        let outcome = service.search(&query(), 5, NOW).await.expect("search");

        assert_eq!(outcome.matches.len(), 1);
        assert_eq!(outcome.matches[0].artist.as_deref(), Some("Radiohead"));
        assert_eq!(
            outcome.matches[0].track_count, 0,
            "no tracklist is not a failure"
        );
        assert_eq!(outcome.matches[0].year, None);
    }
}
