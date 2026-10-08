//! Discogs client.
//!
//! `/database/search` requires authentication and is limited to 60 requests per
//! minute, so every release body is written to the local cache the first time it
//! is fetched and read from there forever after. The caller supplies the rate
//! budget; this type never sleeps or retries on its own.

use mud_core::ProviderId;
use mud_core::audio::DurationMs;
use mud_core::error::{CatalogError, ProviderError};
use mud_core::ids::DiscogsReleaseId;
use mud_core::release::ReleaseIdentity;
use mud_core::search::SearchQuery;
use mud_net::{HttpClient, NetworkError, RateLimitHeaders, UserAgent};
use serde::de::DeserializeOwned;

use crate::discogs_mapper::to_identity;
use crate::discogs_wire::{DiscogsToken, ReleaseResponse, SearchResponse};

/// The public API root. Tests point a client at a local server instead.
pub const BASE: &str = "https://api.discogs.com";

/// A release as Discogs returned it, plus the rate-limit state from the headers.
#[derive(Debug, Clone)]
pub struct FetchedRelease {
    pub identity: ReleaseIdentity,
    pub payload: String,
    pub rate_limit: RateLimitHeaders,
}

/// One page of search hits. `total_items` counts every match across pages, not
/// the length of `results`, so a caller can page without guessing. It is `None`
/// when Discogs sent no count at all, which is not the same as a page of zero.
#[derive(Debug, Clone)]
pub struct DiscogsSearchPage {
    pub results: Vec<SearchHit>,
    pub total_items: Option<u32>,
    pub rate_limit: RateLimitHeaders,
}

#[derive(Debug, Clone)]
pub struct SearchHit {
    pub release_id: DiscogsReleaseId,
    pub title: String,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub year: Option<u16>,
    pub release_types: Vec<mud_core::ReleaseType>,
    pub cover_art_url: Option<String>,
    pub media_labels: Vec<String>,
    pub country: Option<String>,
    pub catalog_number: Option<String>,
    pub genres: Vec<String>,
    pub styles: Vec<String>,
    pub is_physical_medium: bool,
}

#[derive(Debug, Clone)]
pub struct DiscogsClient {
    http: HttpClient,
    token: DiscogsToken,
    base: String,
}

impl DiscogsClient {
    /// # Errors
    /// Fails when no token is configured: an anonymous `/database/search` returns
    /// 401, so searching without one is a mistake worth reporting immediately.
    pub fn new(http: HttpClient, token: Option<DiscogsToken>) -> Result<Self, CatalogError> {
        Self::with_base(http, token, BASE)
    }

    /// Builds a client against a different API root. Used by tests to point at a
    /// local server; nothing else should need it.
    ///
    /// # Errors
    /// Fails when no token is configured, as [`DiscogsClient::new`].
    pub fn with_base(
        http: HttpClient,
        token: Option<DiscogsToken>,
        base: impl Into<String>,
    ) -> Result<Self, CatalogError> {
        let token = token.ok_or(CatalogError::MissingDiscogsToken)?;
        Ok(Self {
            http,
            token,
            base: base.into().trim_end_matches('/').to_owned(),
        })
    }

    fn auth_header(&self) -> (&'static str, String) {
        (
            "Authorization",
            format!("Discogs token={}", self.token.expose()),
        )
    }

    /// Builds the search URL. Discogs has no field syntax in a free-form query,
    /// but `artist` and `release_title` disambiguate far better than `q`.
    ///
    /// `lossless_only` is deliberately absent: Discogs has no such parameter, so
    /// the caller filters the returned hits itself.
    #[must_use]
    pub fn search_url(&self, query: &SearchQuery, page: u32) -> String {
        let mut params: Vec<(&str, String)> = vec![
            ("type", "release".to_owned()),
            ("per_page", "50".to_owned()),
            ("page", page.to_string()),
        ];

        if let Some(artist) = &query.artist {
            params.push(("artist", artist.clone()));
        }
        if let Some(album) = query.discogs_release_title() {
            params.push(("release_title", album.to_owned()));
        }
        if let Some(year) = query.year {
            params.push(("year", year.to_string()));
        }

        let encoded: Vec<String> = params
            .iter()
            .map(|(key, value)| format!("{key}={}", percent_encode(value)))
            .collect();

        format!("{}/database/search?{}", self.base, encoded.join("&"))
    }

    async fn get<T: DeserializeOwned>(
        &self,
        url: &str,
    ) -> Result<(T, RateLimitHeaders), ProviderError> {
        let (name, value) = self.auth_header();
        self.http
            .get_json("discogs", url, &[(name, value.as_str())])
            .await
            .map_err(discogs_error)
    }

    /// Searches for releases matching `query`.
    ///
    /// # Errors
    /// Returns a typed error: a rate-limited Discogs is a back-off condition,
    /// not a failure.
    pub async fn search(
        &self,
        query: &SearchQuery,
        page: u32,
    ) -> Result<DiscogsSearchPage, ProviderError> {
        let (response, rate_limit): (SearchResponse, RateLimitHeaders) =
            self.get(&self.search_url(query, page)).await?;
        let total_items = response.pagination.items;

        let results = response
            .results
            .into_iter()
            .map(|hit| {
                let artist = hit.artist().map(str::to_owned);
                let album = hit.album().map(str::to_owned);
                let release_types = hit.release_types();
                let is_physical_medium = hit.is_physical_medium();

                SearchHit {
                    release_id: DiscogsReleaseId(hit.id),
                    artist,
                    album,
                    title: hit.title,
                    year: hit.year,
                    release_types,
                    cover_art_url: hit.cover_image,
                    media_labels: hit.format,
                    country: hit.country,
                    catalog_number: hit.catno,
                    genres: hit.genre,
                    styles: hit.style,
                    is_physical_medium,
                }
            })
            .collect();

        Ok(DiscogsSearchPage {
            results,
            total_items,
            rate_limit,
        })
    }

    /// Fetches one release.
    ///
    /// Callers are expected to consult the local cache first: at 60 requests
    /// per minute, 200,000 releases is 55 hours of fetching.
    pub async fn release(&self, id: DiscogsReleaseId) -> Result<FetchedRelease, ProviderError> {
        let (response, rate_limit): (ReleaseResponse, RateLimitHeaders) = self
            .get(&format!("{}/releases/{}", self.base, id.get()))
            .await?;

        let payload =
            serde_json::to_string(&response).map_err(|error| ProviderError::Rejected {
                provider: ProviderId::Discogs,
                message: format!("could not re-encode release {id}: {error}"),
            })?;

        Ok(FetchedRelease {
            identity: to_identity(&response),
            payload,
            rate_limit,
        })
    }
}

/// Rebuilds a release identity from a cached payload.
///
/// A cached body is the only copy of a release once Discogs has been asked for
/// it, so reading it back must not require another request.
///
/// # Errors
/// Returns [`CatalogError::Malformed`] when the stored body is not a release.
pub fn identity_from_payload(payload: &str) -> Result<ReleaseIdentity, CatalogError> {
    let response: ReleaseResponse =
        serde_json::from_str(payload).map_err(|error| CatalogError::Malformed {
            message: error.to_string(),
        })?;
    Ok(to_identity(&response))
}

/// Maps a transport failure onto the provider error vocabulary, preserving the
/// distinction between "slow down" and "unreachable".
fn discogs_error(error: NetworkError) -> ProviderError {
    match error {
        NetworkError::RateLimited { retry_after_ms, .. } => ProviderError::RateLimited {
            provider: ProviderId::Discogs,
            retry_after: DurationMs::from_millis(retry_after_ms),
        },
        NetworkError::HttpStatus { status: 401, .. }
        | NetworkError::HttpStatus { status: 403, .. } => ProviderError::Unauthorised {
            provider: ProviderId::Discogs,
        },
        NetworkError::HttpStatus { status, .. } => ProviderError::HttpStatus {
            provider: ProviderId::Discogs,
            status,
        },
        NetworkError::Unreachable { message, .. } => ProviderError::Unreachable {
            provider: ProviderId::Discogs,
            message,
        },
        NetworkError::Malformed { message, .. } => ProviderError::Rejected {
            provider: ProviderId::Discogs,
            message,
        },
        other => ProviderError::Rejected {
            provider: ProviderId::Discogs,
            message: other.to_string(),
        },
    }
}

/// Percent-encodes every byte outside the URL unreserved set, which is all the
/// `/database/search` query needs. Hand-rolled to avoid a dependency for one
/// parameter list.
fn percent_encode(value: &str) -> String {
    use std::fmt::Write as _;

    value.as_bytes().iter().fold(
        String::with_capacity(value.len()),
        |mut out, byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char);
                out
            }
            _ => {
                let _ = write!(out, "%{byte:02X}");
                out
            }
        },
    )
}

/// The user agent Discogs and MusicBrainz both require.
#[must_use]
pub fn user_agent() -> UserAgent {
    // All three parts are literals, so the blank-part error is unreachable.
    UserAgent::new(
        "MuD",
        env!("CARGO_PKG_VERSION"),
        "https://github.com/mud-app",
    )
    .expect("user-agent parts are non-blank literals")
}

#[cfg(test)]
mod tests {
    use super::*;
    use mud_core::search::{RawQuery, SearchFilters};

    fn query(text: &str) -> SearchQuery {
        SearchQuery::parse(&RawQuery::new(text), SearchFilters::default()).expect("parses")
    }

    #[test]
    fn refuses_to_build_without_a_token() {
        let http = HttpClient::new(&user_agent(), &mud_net::ProxyConfig::direct()).expect("client");
        assert!(matches!(
            DiscogsClient::new(http, None),
            Err(CatalogError::MissingDiscogsToken)
        ));
    }

    #[test]
    fn search_url_prefers_structured_parameters_over_free_text() {
        let http = HttpClient::new(&user_agent(), &mud_net::ProxyConfig::direct()).expect("client");
        let client = DiscogsClient::new(http, Some(DiscogsToken::new("t"))).expect("client");

        let url = client.search_url(&query("Radiohead - OK Computer - 1997"), 1);
        assert!(url.starts_with("https://api.discogs.com/database/search?"));
        assert!(url.contains("artist=Radiohead"), "{url}");
        assert!(url.contains("release_title=OK%20Computer"), "{url}");
        assert!(url.contains("year=1997"), "{url}");
        assert!(url.contains("type=release"), "{url}");
        assert!(url.contains("page=1"), "{url}");
    }

    #[test]
    fn search_url_omits_fields_the_query_does_not_carry() {
        let http = HttpClient::new(&user_agent(), &mud_net::ProxyConfig::direct()).expect("client");
        let client = DiscogsClient::new(http, Some(DiscogsToken::new("t"))).expect("client");

        let url = client.search_url(&query("OK Computer"), 2);
        assert!(!url.contains("artist="), "{url}");
        assert!(url.contains("release_title=OK%20Computer"), "{url}");
        assert!(url.contains("page=2"), "{url}");
    }

    #[test]
    fn percent_encoding_covers_spaces_and_quotes() {
        assert_eq!(percent_encode("OK Computer"), "OK%20Computer");
        assert_eq!(percent_encode("Jay-Z"), "Jay-Z");
        assert_eq!(percent_encode(r#"a"b'c&d"#), "a%22b%27c%26d");
    }

    #[test]
    fn unauthorised_is_distinct_from_an_ordinary_http_failure() {
        for status in [401, 403] {
            let error = discogs_error(NetworkError::HttpStatus {
                provider: "discogs",
                status,
            });
            assert!(
                matches!(
                    error,
                    ProviderError::Unauthorised {
                        provider: ProviderId::Discogs
                    }
                ),
                "HTTP {status} is not an authorisation failure"
            );
        }

        let error = discogs_error(NetworkError::HttpStatus {
            provider: "discogs",
            status: 500,
        });
        assert!(matches!(
            error,
            ProviderError::HttpStatus { status: 500, .. }
        ));
    }

    #[test]
    fn a_rate_limit_keeps_its_retry_after_value() {
        let error = discogs_error(NetworkError::RateLimited {
            provider: "discogs",
            retry_after_ms: 12_000,
        });
        assert!(
            matches!(
                error,
                ProviderError::RateLimited {
                    retry_after,
                    ..
                } if retry_after == DurationMs::from_millis(12_000)
            ),
            "the back-off delay was lost"
        );
    }

    #[test]
    fn the_user_agent_carries_a_version_and_a_contact_url() {
        // Discogs rejects unauthenticated calls whose user agent names neither.
        let header = user_agent().header();
        assert!(
            header.starts_with(&format!("MuD/{} (", env!("CARGO_PKG_VERSION"))),
            "{header}"
        );
        assert!(header.contains("https://"), "no contact URL: {header}");
    }
}
