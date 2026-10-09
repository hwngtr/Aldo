//! The provider abstraction. Each search source implements `DiscoveryProvider`
//! and returns candidates; the orchestrator handles fan-out, rate limiting and
//! merging, so no provider knows about any other.

use async_trait::async_trait;

use crate::candidate::{Candidate, RejectionCounts};
use crate::error::ProviderError;
pub use crate::ids::ProviderId;
use crate::search::SearchQuery;

/// What a provider found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderOutcome {
    pub candidates: Vec<Candidate>,
    /// Hits seen before filtering. Used to explain an empty result.
    pub raw_hits: u32,
    pub rejected: RejectionCounts,
}

impl ProviderOutcome {
    pub fn empty() -> Self {
        Self {
            candidates: Vec::new(),
            raw_hits: 0,
            rejected: RejectionCounts::default(),
        }
    }

    /// True when the provider saw no hits at all, which distinguishes "no such
    /// music exists" from "everything was filtered". A provider that found
    /// nothing is definitive; one that found only rejected hits is not.
    pub fn is_definitive_empty(&self) -> bool {
        self.raw_hits == 0
    }
}

/// A searchable source of candidates.
///
/// Implementations must not perform their own rate limiting: the orchestrator
/// owns the budget registry so a single global budget covers every call site.
#[async_trait]
pub trait DiscoveryProvider: Send + Sync + std::fmt::Debug {
    fn id(&self) -> ProviderId;

    /// Returns candidates matching `query`, already filtered by
    /// `query.filters`.
    async fn search(&self, query: &SearchQuery) -> Result<ProviderOutcome, ProviderError>;
}
