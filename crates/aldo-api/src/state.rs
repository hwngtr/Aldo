//! Shared application state.

use std::path::PathBuf;

use aldo_store::Store;
use aldo_store::repo::{AcquisitionStore, BudgetStore, CandidateStore, CatalogStore, SessionStore};

use crate::auth::ApiToken;
use crate::origin::OriginPolicy;
use aldo_catalog::CatalogService;

#[derive(Debug, Clone)]
pub struct AppState {
    pub store: Store,
    pub sessions: SessionStore,
    pub candidates: CandidateStore,
    pub catalog: CatalogStore,
    pub acquisitions: AcquisitionStore,
    pub budgets: BudgetStore,
    /// Absent until a Discogs token is configured. A search then reports
    /// that it was never asked rather than that it found nothing.
    pub discogs: Option<CatalogService>,
    pub token: ApiToken,
    pub origins: OriginPolicy,
    pub library_root: PathBuf,
}

impl AppState {
    pub fn new(
        store: Store,
        token: ApiToken,
        library_root: PathBuf,
        bind_address: std::net::SocketAddr,
    ) -> Self {
        let pool = store.pool().clone();
        Self {
            sessions: SessionStore::new(pool.clone()),
            candidates: CandidateStore::new(pool.clone()),
            catalog: CatalogStore::new(pool.clone()),
            acquisitions: AcquisitionStore::new(pool.clone()),
            budgets: BudgetStore::new(pool),
            discogs: None,
            origins: OriginPolicy::new(bind_address),
            token,
            library_root,
            store,
        }
    }

    /// Enables the Discogs fan-out. Without this a search reports that the
    /// provider is not configured.
    #[must_use]
    pub fn with_discogs(mut self, service: CatalogService) -> Self {
        self.discogs = Some(service);
        self
    }
}
