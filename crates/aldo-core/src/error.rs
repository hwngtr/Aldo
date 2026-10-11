//! Error types. Every fallible operation returns a typed error; nothing is
//! swallowed into a default value.

use thiserror::Error;

use crate::audio::DurationMs;
use crate::ids::{DiscogsReleaseId, InfoHash, ProviderId};

/// A provider search failed. The `provider` is always a real provider: every
/// variant of this error is raised by a call to one.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ProviderError {
    #[error("provider {provider} is not configured")]
    NotConfigured { provider: ProviderId },

    #[error("provider {provider} rate limit exhausted, retry in {retry_after}")]
    RateLimited {
        provider: ProviderId,
        retry_after: DurationMs,
    },

    #[error("provider {provider} returned HTTP {status}")]
    HttpStatus { provider: ProviderId, status: u16 },

    #[error("provider {provider} rejected the request: {message}")]
    Rejected {
        provider: ProviderId,
        message: String,
    },

    #[error("credentials for {provider} are missing or invalid")]
    Unauthorised { provider: ProviderId },

    #[error("could not reach {provider}: {message}")]
    Unreachable {
        provider: ProviderId,
        message: String,
    },

    #[error("no peer supplied metadata for {infohash} within the timeout")]
    NoMetadataPeers { infohash: InfoHash },
}

/// A catalog lookup failed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CatalogError {
    #[error("no Discogs token is configured")]
    MissingDiscogsToken,

    #[error("Discogs returned HTTP {status} for release {release}")]
    DiscogsStatus {
        status: u16,
        release: DiscogsReleaseId,
    },

    #[error("Discogs rate limit exhausted")]
    DiscogsRateLimited,

    #[error("no Discogs release matched the query")]
    NoReleaseMatch,

    #[error("MusicBrainz rate limit exceeded; retry in {retry_after}")]
    MusicBrainzRateLimited { retry_after: DurationMs },

    /// `provider` stays a free label rather than a `ProviderId`: a transport
    /// failure can be attributed to something that is not a configured
    /// provider, such as the HTTP client itself.
    #[error("could not reach {provider}: {message}")]
    Unreachable {
        provider: &'static str,
        message: String,
    },

    #[error("response body was not the expected shape: {message}")]
    Malformed { message: String },
}

/// A value read from disk or from an untrusted source failed validation.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ValidationError {
    #[error("path escapes its root: {path}")]
    PathEscape { path: String },

    #[error("path contains an absolute component: {path}")]
    AbsolutePath { path: String },

    #[error("path contains a disallowed component: {path}")]
    DisallowedPath { path: String },

    #[error("not a FLAC file: {extension}")]
    NotFlac { extension: String },

    #[error("no FLAC files in {name}")]
    NoFlacFiles { name: String },

    #[error("value out of range: {field} = {value}")]
    OutOfRange { field: &'static str, value: i64 },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_retry_delay_reads_as_a_duration_not_a_bare_number() {
        let error = ProviderError::RateLimited {
            provider: ProviderId::Discogs,
            retry_after: DurationMs::from_millis(12_000),
        };
        assert_eq!(
            error.to_string(),
            "provider discogs rate limit exhausted, retry in 12000 ms"
        );
    }

    #[test]
    fn a_provider_error_names_a_provider_and_not_a_free_label() {
        assert_eq!(
            ProviderError::NotConfigured {
                provider: ProviderId::SoulSeek
            }
            .to_string(),
            "provider soulseek is not configured"
        );
    }

    #[test]
    fn a_catalog_release_is_identified_by_its_discogs_id() {
        let error = CatalogError::DiscogsStatus {
            status: 503,
            release: DiscogsReleaseId(4242),
        };
        assert_eq!(
            error.to_string(),
            "Discogs returned HTTP 503 for release 4242"
        );
    }

    #[test]
    fn a_transport_label_may_name_something_other_than_a_provider() {
        // `CatalogError::Unreachable` deliberately keeps a free label, because
        // a transport failure can belong to the HTTP client itself.
        assert_eq!(
            CatalogError::Unreachable {
                provider: "http",
                message: "connection reset".into(),
            }
            .to_string(),
            "could not reach http: connection reset"
        );
    }
}
