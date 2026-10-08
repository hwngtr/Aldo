//! One HTTP client for every remote metadata source.
//!
//! All four upstreams require a distinctive `User-Agent`: Discogs blocks
//! default library agents outright and MusicBrainz asks for a contact address.
//! Building the clients in one place makes that impossible to forget and keeps
//! the proxy configuration in a single place too.

use std::time::Duration;

use mud_core::audio::DurationMs;
use mud_core::error::CatalogError;
use reqwest::StatusCode;
use url::Url;

/// Egress configuration. Only the tracker and catalog HTTP clients use it: the
/// SoulSeek client owns its own socket, and qbittorrent has its own proxy
/// setting, so neither can be routed through here.
///
/// A direct connection and a proxied one are the only two states, so the target
/// is one optional value rather than three independent `Option<String>` fields:
/// those allowed a half-configured proxy with a username but no password and a
/// proxy whose "URL" was `None`.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct ProxyConfig {
    /// `None` means a direct connection.
    target: Option<ProxyTarget>,
}

/// Hand-written so the password cannot reach a log through `{:?}`. A derived
/// `Debug` prints every field, which for this type means the proxy password.
impl std::fmt::Debug for ProxyConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.target {
            Some(target) => f
                .debug_struct("ProxyConfig")
                .field("url", &target.url)
                .field("authenticated", &target.credentials.is_some())
                .finish(),
            None => f.write_str("ProxyConfig(direct)"),
        }
    }
}

/// A proxy endpoint. The URL never carries credentials, so it is safe to log,
/// and the credentials exist only as a separate value.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ProxyTarget {
    url: String,
    credentials: Option<ProxyCredentials>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProxyCredentials {
    username: String,
    password: String,
}

impl ProxyConfig {
    #[must_use]
    pub fn direct() -> Self {
        Self::default()
    }

    /// Parses `socks5://host:port` or `http://host:port`, optionally with
    /// credentials embedded as `scheme://user:pass@host:port`.
    ///
    /// # Errors
    /// Returns [`ProxyError`] for an unparseable URL, an unsupported scheme, a
    /// URL with no host, or credentials without a username. Every error carries
    /// the credential-stripped form of the URL, so a rejected configuration
    /// cannot leak a password into a log.
    pub fn parse(url: &str) -> Result<Self, ProxyError> {
        let safe = mask_credentials(url);
        let parsed = Url::parse(url).map_err(|error| ProxyError::Malformed {
            url: safe.clone(),
            detail: error.to_string(),
        })?;

        let scheme = parsed.scheme().to_owned();
        if !matches!(scheme.as_str(), "socks5" | "socks5h" | "http" | "https") {
            return Err(ProxyError::UnsupportedScheme { scheme });
        }

        if parsed.host_str().is_none() {
            return Err(ProxyError::Malformed {
                url: safe.clone(),
                detail: "no host".to_owned(),
            });
        }

        let username = parsed.username();
        let password = parsed.password();
        let credentials = if username.is_empty() {
            if password.is_some() {
                return Err(ProxyError::Malformed {
                    url: safe.clone(),
                    detail: "a password without a username".to_owned(),
                });
            }
            None
        } else {
            Some(ProxyCredentials {
                username: username.to_owned(),
                password: password.unwrap_or_default().to_owned(),
            })
        };

        let mut endpoint = parsed.clone();
        let rejects_credentials = |url: String, detail: &str| ProxyError::Malformed {
            url,
            detail: detail.to_owned(),
        };
        endpoint.set_username("").map_err(|()| {
            rejects_credentials(safe.clone(), "the URL does not accept a username")
        })?;
        endpoint.set_password(None).map_err(|()| {
            rejects_credentials(safe.clone(), "the URL does not accept a password")
        })?;

        Ok(Self {
            target: Some(ProxyTarget {
                url: endpoint.to_string(),
                credentials,
            }),
        })
    }

    #[must_use]
    pub fn is_configured(&self) -> bool {
        self.target.is_some()
    }

    /// The proxy URL, credential-free by construction, for logging.
    #[must_use]
    pub fn redacted(&self) -> Option<&str> {
        self.target.as_ref().map(|target| target.url.as_str())
    }
}

/// Replaces the `user:pass@` part of a URL with nothing, for an error message
/// about a URL that could not be parsed into a form that strips it itself.
fn mask_credentials(url: &str) -> String {
    let Some(scheme_end) = url.find("://") else {
        return url.to_owned();
    };
    let authority = &url[scheme_end + 3..];
    match authority.rfind('@') {
        Some(at) => format!("{}{}", &url[..scheme_end + 3], &authority[at + 1..]),
        None => url.to_owned(),
    }
}

/// Every variant carries a credential-free URL.
#[derive(Debug, thiserror::Error)]
pub enum ProxyError {
    #[error("proxy URL {url} could not be parsed: {detail}")]
    Malformed { url: String, detail: String },
    #[error("unsupported proxy scheme {scheme}; use socks5 or http")]
    UnsupportedScheme { scheme: String },
}

/// Contact details sent as `User-Agent`. Discogs blocks default library agents
/// and MusicBrainz requires a reachable address.
///
/// The three parts are private and cannot be blank: `MuD/0.1.0 ()` is a header
/// value that looks valid and is rejected by both upstreams at request time,
/// which is a long way from the code that dropped the contact address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserAgent {
    project: String,
    version: String,
    contact: String,
}

impl UserAgent {
    /// # Errors
    /// Returns [`UserAgentError::EmptyPart`] naming whichever of the project,
    /// version, or contact is blank.
    pub fn new(
        project: impl Into<String>,
        version: impl Into<String>,
        contact: impl Into<String>,
    ) -> Result<Self, UserAgentError> {
        let project = project.into();
        let version = version.into();
        let contact = contact.into();

        for (part, value) in [
            (UserAgentPart::Project, &project),
            (UserAgentPart::Version, &version),
            (UserAgentPart::Contact, &contact),
        ] {
            if value.trim().is_empty() {
                return Err(UserAgentError::EmptyPart { part });
            }
        }

        Ok(Self {
            project,
            version,
            contact,
        })
    }

    /// Renders the header value. The contact is a mailto URL so the format
    /// matches what MusicBrainz asks for.
    #[must_use]
    pub fn header(&self) -> String {
        format!("{}/{} ({})", self.project, self.version, self.contact)
    }
}

/// Which part of a `User-Agent` was left blank.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserAgentPart {
    Project,
    Version,
    Contact,
}

impl std::fmt::Display for UserAgentPart {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Project => "project name",
            Self::Version => "version",
            Self::Contact => "contact address",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum UserAgentError {
    #[error("the {part} of the User-Agent cannot be blank")]
    EmptyPart { part: UserAgentPart },
}

#[derive(Debug, Clone)]
pub struct HttpClient {
    inner: reqwest::Client,
}

impl HttpClient {
    /// Builds a client with a 20 second timeout and the given proxy.
    pub fn new(user_agent: &UserAgent, proxy: &ProxyConfig) -> Result<Self, NetworkError> {
        let mut builder = reqwest::Client::builder()
            .user_agent(user_agent.header())
            .timeout(Duration::from_secs(20))
            .connect_timeout(Duration::from_secs(10));

        if let Some(target) = &proxy.target {
            let mut proxy_config =
                reqwest::Proxy::all(&target.url).map_err(|error| NetworkError::Proxy {
                    url: target.url.clone(),
                    detail: error.to_string(),
                })?;
            if let Some(credentials) = &target.credentials {
                proxy_config =
                    proxy_config.basic_auth(&credentials.username, &credentials.password);
            }
            builder = builder.proxy(proxy_config);
        }

        Ok(Self {
            inner: builder
                .build()
                .map_err(|source| NetworkError::Build(source.to_string()))?,
        })
    }

    pub fn get(&self, url: &str) -> reqwest::RequestBuilder {
        self.inner.get(url)
    }

    pub fn post(&self, url: &str) -> reqwest::RequestBuilder {
        self.inner.post(url)
    }

    pub fn inner(&self) -> &reqwest::Client {
        &self.inner
    }

    /// Issues a request and parses a JSON body.
    ///
    /// # Errors
    /// Returns a typed [`CatalogError`] rather than a raw error: callers act on
    /// "rate limited" and "not found" differently from "the network is down",
    /// and must not have to inspect a status code to tell them apart.
    pub async fn get_json<T: serde::de::DeserializeOwned>(
        &self,
        provider: &'static str,
        url: &str,
        headers: &[(&str, &str)],
    ) -> Result<(T, RateLimitHeaders), NetworkError> {
        let mut request = self.get(url);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }

        let response = request
            .send()
            .await
            .map_err(|source| NetworkError::Unreachable {
                provider,
                message: source.to_string(),
            })?;

        let status = response.status();
        let headers = RateLimitHeaders::from_response(response.headers());

        if status == StatusCode::TOO_MANY_REQUESTS || status == StatusCode::SERVICE_UNAVAILABLE {
            return Err(NetworkError::RateLimited {
                provider,
                retry_after_ms: headers.retry_after_ms.unwrap_or(60_000),
            });
        }
        if !status.is_success() {
            return Err(NetworkError::HttpStatus {
                provider,
                status: status.as_u16(),
            });
        }

        let body = response
            .text()
            .await
            .map_err(|source| NetworkError::Unreachable {
                provider,
                message: source.to_string(),
            })?;

        let parsed = serde_json::from_str(&body).map_err(|source| NetworkError::Malformed {
            provider,
            message: source.to_string(),
        })?;

        Ok((parsed, headers))
    }
}

/// Rate-limit state as reported by the upstream in response headers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RateLimitHeaders {
    pub limit: Option<u32>,
    pub remaining: Option<u32>,
    pub retry_after_ms: Option<u64>,
}

impl RateLimitHeaders {
    fn from_response(headers: &reqwest::header::HeaderMap) -> Self {
        let parse = |name: &str| -> Option<u32> {
            headers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse().ok())
        };
        // Remote input: saturating, because a server answering
        // `retry-after: 18446744073709551` must not overflow this multiply.
        let retry_after_ms = headers
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok())
            .map(|seconds| seconds.saturating_mul(1_000));

        Self {
            limit: parse("x-discogs-ratelimit"),
            remaining: parse("x-discogs-ratelimit-remaining"),
            retry_after_ms,
        }
    }

    /// Seconds until the budget resets, when the upstream says so.
    #[must_use]
    pub fn reset_seconds(&self) -> Option<u64> {
        self.retry_after_ms.map(|ms| ms / 1_000)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum NetworkError {
    #[error("could not build an HTTP client: {0}")]
    Build(String),
    #[error("proxy {url} was rejected: {detail}")]
    Proxy { url: String, detail: String },
    #[error("could not reach {provider}: {message}")]
    Unreachable {
        provider: &'static str,
        message: String,
    },
    #[error("{provider} returned HTTP {status}")]
    HttpStatus { provider: &'static str, status: u16 },
    #[error("{provider} rate limit exhausted, retry in {retry_after_ms} ms")]
    RateLimited {
        provider: &'static str,
        retry_after_ms: u64,
    },
    #[error("response from {provider} was not the expected shape: {message}")]
    Malformed {
        provider: &'static str,
        message: String,
    },
}

impl From<NetworkError> for CatalogError {
    fn from(error: NetworkError) -> Self {
        match error {
            // The provider decides which catalogue is being rate limited, not
            // the length of the wait: Discogs answers 429 with no `Retry-After`,
            // so a long wait from Discogs used to be reported as MusicBrainz.
            NetworkError::RateLimited {
                provider,
                retry_after_ms,
            } => {
                if provider == "musicbrainz" {
                    CatalogError::MusicBrainzRateLimited {
                        retry_after: DurationMs::from_millis(retry_after_ms),
                    }
                } else {
                    CatalogError::DiscogsRateLimited
                }
            }
            NetworkError::Unreachable { provider, message } => {
                CatalogError::Unreachable { provider, message }
            }
            NetworkError::Malformed { message, .. } => CatalogError::Malformed { message },
            other => CatalogError::Unreachable {
                provider: "http",
                message: other.to_string(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_socks5_proxy() {
        let proxy = ProxyConfig::parse("socks5://127.0.0.1:9050").expect("valid");
        assert!(proxy.is_configured());
        assert_eq!(proxy.redacted(), Some("socks5://127.0.0.1:9050"));
        assert!(
            proxy.target.as_ref().expect("target").credentials.is_none(),
            "a URL with no credentials invented some"
        );
    }

    #[test]
    fn extracts_credentials_from_the_url() {
        let proxy = ProxyConfig::parse("socks5://bob:hunter2@proxy:1080").expect("valid");
        let credentials = proxy
            .target
            .as_ref()
            .and_then(|target| target.credentials.as_ref())
            .expect("credentials");
        assert_eq!(credentials.username, "bob");
        assert_eq!(credentials.password, "hunter2");
    }

    #[test]
    fn the_loggable_url_never_carries_the_password() {
        let proxy = ProxyConfig::parse("socks5://bob:hunter2@proxy:1080").expect("valid");
        let redacted = proxy.redacted().expect("configured");
        assert!(!redacted.contains("hunter2"), "{redacted}");
        assert!(!redacted.contains('@'), "{redacted}");
        assert!(!format!("{proxy:?}").contains("hunter2"), "{proxy:?}");
    }

    #[test]
    fn a_rejected_url_never_carries_the_password_either() {
        for url in [
            "ftp://bob:hunter2@proxy:21",
            "socks5://bob:hunter2@",
            "socks5://:hunter2@proxy:1080",
        ] {
            let error = ProxyConfig::parse(url).expect_err("rejected");
            assert!(!error.to_string().contains("hunter2"), "{error}");
        }
    }

    #[test]
    fn a_password_without_a_username_is_refused() {
        // Half credentials would otherwise configure a proxy that
        // authenticates as nobody, or silently sends no header at all.
        let error = ProxyConfig::parse("socks5://:hunter2@proxy:1080").expect_err("rejected");
        assert!(matches!(error, ProxyError::Malformed { .. }));
    }

    #[test]
    fn rejects_a_scheme_that_cannot_carry_http_traffic() {
        let error = ProxyConfig::parse("ftp://proxy:21").expect_err("rejected");
        assert!(matches!(error, ProxyError::UnsupportedScheme { .. }));
    }

    #[test]
    fn rejects_a_url_with_no_host() {
        assert!(ProxyConfig::parse("socks5://").is_err());
    }

    #[test]
    fn a_direct_client_has_no_proxy() {
        assert!(!ProxyConfig::direct().is_configured());
        assert!(!ProxyConfig::default().is_configured());
    }

    #[test]
    fn user_agent_names_the_project_version_and_contact() {
        let agent = UserAgent::new("MuD", "0.1.0", "https://example.invalid/mud").expect("valid");
        assert_eq!(agent.header(), "MuD/0.1.0 (https://example.invalid/mud)");
    }

    #[test]
    fn a_blank_part_of_the_user_agent_cannot_be_built() {
        for (project, version, contact, part) in [
            ("", "0.1.0", "a@b", UserAgentPart::Project),
            ("MuD", "  ", "a@b", UserAgentPart::Version),
            ("MuD", "0.1.0", "", UserAgentPart::Contact),
        ] {
            assert_eq!(
                UserAgent::new(project, version, contact).expect_err("rejected"),
                UserAgentError::EmptyPart { part }
            );
        }
    }

    #[test]
    fn builds_a_client_for_a_direct_and_a_proxied_configuration() {
        let agent = UserAgent::new("MuD", "0.1.0", "https://example.invalid/mud").expect("valid");
        assert!(HttpClient::new(&agent, &ProxyConfig::direct()).is_ok());

        let proxy = ProxyConfig::parse("socks5://127.0.0.1:9050").expect("proxy");
        assert!(HttpClient::new(&agent, &proxy).is_ok());
    }

    #[test]
    fn rate_limit_headers_parse_from_a_realistic_response() {
        use reqwest::header::{HeaderMap, HeaderValue};

        let mut headers = HeaderMap::new();
        headers.insert("x-discogs-ratelimit", HeaderValue::from_static("60"));
        headers.insert(
            "x-discogs-ratelimit-remaining",
            HeaderValue::from_static("57"),
        );
        headers.insert("retry-after", HeaderValue::from_static("12"));

        let parsed = RateLimitHeaders::from_response(&headers);
        assert_eq!(parsed.limit, Some(60));
        assert_eq!(parsed.remaining, Some(57));
        assert_eq!(parsed.reset_seconds(), Some(12));
    }

    #[test]
    fn absent_headers_read_as_absent_rather_than_zero() {
        let parsed = RateLimitHeaders::from_response(&reqwest::header::HeaderMap::new());
        assert_eq!(parsed.limit, None);
        assert_eq!(parsed.remaining, None);
        assert_eq!(parsed.retry_after_ms, None);
    }

    #[test]
    fn a_malformed_header_is_ignored_not_guessed_at() {
        use reqwest::header::{HeaderMap, HeaderValue};

        let mut headers = HeaderMap::new();
        headers.insert("x-discogs-ratelimit", HeaderValue::from_static("lots"));

        assert_eq!(RateLimitHeaders::from_response(&headers).limit, None);
    }

    #[test]
    fn an_absurd_retry_after_saturates_instead_of_overflowing() {
        use reqwest::header::{HeaderMap, HeaderValue};

        let mut headers = HeaderMap::new();
        headers.insert("retry-after", HeaderValue::from_static("18446744073709552"));

        let parsed = RateLimitHeaders::from_response(&headers);
        assert_eq!(parsed.retry_after_ms, Some(u64::MAX));
        assert_eq!(parsed.reset_seconds(), Some(u64::MAX / 1_000));
    }

    #[test]
    fn the_provider_decides_which_catalogue_was_rate_limited() {
        let error = CatalogError::from(NetworkError::RateLimited {
            provider: "musicbrainz",
            retry_after_ms: 30_000,
        });
        assert!(matches!(error, CatalogError::MusicBrainzRateLimited { .. }));
    }

    #[test]
    fn a_long_retry_from_discogs_is_still_a_discogs_rate_limit() {
        let error = CatalogError::from(NetworkError::RateLimited {
            provider: "discogs",
            retry_after_ms: 200,
        });
        assert!(matches!(error, CatalogError::DiscogsRateLimited));

        // The wait length is not evidence of which upstream refused.
        let long = CatalogError::from(NetworkError::RateLimited {
            provider: "discogs",
            retry_after_ms: 30_000,
        });
        assert!(matches!(long, CatalogError::DiscogsRateLimited));
    }
}
