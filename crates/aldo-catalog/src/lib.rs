//! Metadata sources: Discogs search hits and releases.
//!
//! Mapping from wire format to domain lives here, separate from the transport,
//! so every Discogs quirk is handled once and is testable without a network.

pub mod catalog_service;
pub mod discogs;
pub mod discogs_mapper;
pub mod discogs_wire;
pub mod fanout;

pub use catalog_service::{
    CatalogSearchError, CatalogSearchOutcome, CatalogService, DEFAULT_RELEASE_LIMIT,
    MAX_RELEASE_LIMIT,
};
pub use discogs::{
    BASE, DiscogsClient, DiscogsSearchPage, FetchedRelease, SearchHit, identity_from_payload,
    user_agent,
};
pub use discogs_mapper::{parse_discogs_duration, released_year, to_identity};
pub use discogs_wire::DiscogsToken;
pub use fanout::{FanoutReason, FanoutRecord, discogs as record_discogs};
