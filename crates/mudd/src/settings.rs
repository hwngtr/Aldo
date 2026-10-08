//! Configuration resolved from flags and environment, and the pieces built
//! from it: the database and the metadata clients.

use std::path::PathBuf;

use std::time::Duration;

use mud_catalog::{BASE, CatalogService, DiscogsClient, DiscogsToken, user_agent};
use mud_net::{HttpClient, ProxyConfig};
use mud_soulseek::SoulSeekConfig;
use mud_store::{Store, StoreError};

use crate::cli::GlobalArgs;

/// Everything a command needs to reach the local database and the metadata
/// sources.
#[derive(Debug, Clone)]
pub struct Settings {
    pub data_dir: PathBuf,
    pub library_root: PathBuf,
    pub discogs_token: Option<String>,
    pub proxy: Option<String>,
    pub slsk_username: Option<String>,
    pub slsk_password: Option<String>,
    pub slsk_server: String,
    pub slsk_port: u16,
    pub slsk_shared: Vec<PathBuf>,
    pub slsk_timeout: Duration,
}

impl Settings {
    /// Opens the database, creating it on first run, and seeds the local user.
    ///
    /// # Errors
    /// Fails when the data directory cannot be created, the library root is not
    /// usable, or the database cannot be opened or migrated.
    pub async fn open_store(&self) -> Result<Store, SettingsError> {
        std::fs::create_dir_all(&self.data_dir).map_err(|source| SettingsError::DataDir {
            path: self.data_dir.clone(),
            source,
        })?;

        // A mistyped root scatters files across the filesystem later, so it is
        // refused before anything is written.
        mud_store::validate_library_root(&self.library_root)?;

        let url = Store::url_for(&self.data_dir.join("mud.sqlite3"))
            .map_err(SettingsError::DatabasePath)?;
        let store = Store::open(&url).await.map_err(SettingsError::Database)?;

        mud_store::repo::ensure_local_user(
            store.pool(),
            &self.library_root.to_string_lossy(),
            mud_store::now_ms(),
        )
        .await
        .map_err(SettingsError::SeedUser)?;

        Ok(store)
    }

    /// Builds the Discogs service, or `None` when no token is configured.
    ///
    /// A missing token is not an error: Discogs is one of several providers, and
    /// a search reports it as not configured rather than failing.
    ///
    /// # Errors
    /// Fails only for configuration that is present but unusable.
    pub fn discogs(&self, store: &Store) -> Result<Option<CatalogService>, SettingsError> {
        // The proxy is checked before the token, because it applies to every
        // metadata source and a typo in it should surface either way.
        let proxy = match non_blank(self.proxy.as_deref()) {
            Some(url) => ProxyConfig::parse(url)?,
            None => ProxyConfig::direct(),
        };

        let Some(token) = non_blank(self.discogs_token.as_deref()) else {
            return Ok(None);
        };

        let http = HttpClient::new(&user_agent(), &proxy)
            .map_err(|error| SettingsError::Http(error.to_string()))?;
        let client = DiscogsClient::with_base(http, Some(DiscogsToken::new(token)), BASE)?;

        Ok(Some(CatalogService::new(
            client,
            mud_store::repo::CatalogStore::new(store.pool().clone()),
            mud_store::repo::BudgetStore::new(store.pool().clone()),
        )))
    }

    /// Builds the SoulSeek configuration, or `None` when no account is set.
    ///
    /// Both halves of the credential are required: a username without a
    /// password cannot log in, and reporting that as "not configured" is
    /// clearer than a login that fails.
    ///
    /// # Errors
    /// Fails when the server is not `host:port`.
    pub fn soulseek(&self) -> Result<Option<SoulSeekConfig>, SettingsError> {
        let (Some(username), Some(password)) = (
            non_blank(self.slsk_username.as_deref()),
            non_blank(self.slsk_password.as_deref()),
        ) else {
            return Ok(None);
        };

        let (host, port) = parse_endpoint(&self.slsk_server)?;

        Ok(Some(
            SoulSeekConfig::new(username, password)
                .with_server(host, port)
                .with_listen_port(self.slsk_port)
                .with_shared_directories(
                    self.slsk_shared
                        .iter()
                        .map(|path| path.to_string_lossy().into_owned())
                        .collect(),
                )
                .with_search_timeout(self.slsk_timeout),
        ))
    }
}

/// Splits a `host:port` endpoint.
fn parse_endpoint(text: &str) -> Result<(String, u16), SettingsError> {
    let Some((host, port)) = text.rsplit_once(':') else {
        return Err(SettingsError::MalformedEndpoint {
            text: text.to_owned(),
        });
    };
    let port = port
        .trim()
        .parse::<u16>()
        .map_err(|_| SettingsError::MalformedEndpoint {
            text: text.to_owned(),
        })?;
    if host.trim().is_empty() {
        return Err(SettingsError::MalformedEndpoint {
            text: text.to_owned(),
        });
    }
    Ok((host.trim().to_owned(), port))
}

impl From<&GlobalArgs> for Settings {
    fn from(args: &GlobalArgs) -> Self {
        Self {
            data_dir: args.data_dir.clone(),
            library_root: args.library_root.clone(),
            discogs_token: args.discogs_token.clone(),
            proxy: args.proxy.clone(),
            slsk_username: args.slsk_username.clone(),
            slsk_password: args.slsk_password.clone(),
            slsk_server: args.slsk_server.clone(),
            slsk_port: args.slsk_port,
            slsk_shared: args.slsk_shared.clone(),
            slsk_timeout: Duration::from_secs(args.slsk_timeout),
        }
    }
}

/// Treats a whitespace-only value as absent, so an empty environment variable
/// does not read as configured.
fn non_blank(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|text| !text.is_empty())
}

#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error("could not create {path}: {source}")]
    DataDir {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error(transparent)]
    LibraryRoot(#[from] mud_store::LibraryRootError),

    #[error("could not resolve the database path: {0}")]
    DatabasePath(StoreError),

    #[error("could not open the database: {0}")]
    Database(StoreError),

    #[error("could not seed the local user: {0}")]
    SeedUser(StoreError),

    #[error(transparent)]
    Proxy(#[from] mud_net::ProxyError),

    #[error("could not build the HTTP client: {0}")]
    Http(String),

    #[error(transparent)]
    Catalog(#[from] mud_core::error::CatalogError),

    #[error("`{text}` is not a `host:port` endpoint")]
    MalformedEndpoint { text: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_blank_token_is_absent_not_configured() {
        assert_eq!(non_blank(None), None);
        assert_eq!(non_blank(Some("")), None);
        assert_eq!(non_blank(Some("   ")), None);
        assert_eq!(non_blank(Some(" token ")), Some("token"));
    }

    #[test]
    fn settings_are_taken_from_the_parsed_arguments() {
        let args = GlobalArgs {
            data_dir: PathBuf::from("/var/lib/mud"),
            library_root: PathBuf::from("/music"),
            discogs_token: Some("t".to_owned()),
            proxy: Some("socks5://127.0.0.1:9050".to_owned()),
            slsk_username: Some("alice".to_owned()),
            slsk_password: Some("secret".to_owned()),
            slsk_server: "server.slsknet.org:2416".to_owned(),
            slsk_port: 2234,
            slsk_shared: vec![PathBuf::from("/music")],
            slsk_timeout: 8,
        };

        let settings = Settings::from(&args);
        assert_eq!(settings.data_dir, PathBuf::from("/var/lib/mud"));
        assert_eq!(settings.library_root, PathBuf::from("/music"));
        assert_eq!(settings.discogs_token.as_deref(), Some("t"));
        assert!(settings.proxy.is_some());
    }

    #[test]
    fn a_relative_library_root_is_refused_before_the_store_is_opened() {
        let settings = Settings {
            data_dir: std::env::temp_dir(),
            library_root: PathBuf::from("relative/music"),
            discogs_token: None,
            proxy: None,
            slsk_username: None,
            slsk_password: None,
            slsk_server: "server.slsknet.org:2416".to_owned(),
            slsk_port: 2234,
            slsk_shared: Vec::new(),
            slsk_timeout: Duration::from_secs(8),
        };

        let error = tokio::runtime::Runtime::new()
            .expect("runtime")
            .block_on(settings.open_store())
            .expect_err("refused");

        assert!(matches!(
            error,
            SettingsError::LibraryRoot(mud_store::LibraryRootError::NotAbsolute { .. })
        ));
    }

    #[test]
    fn a_half_configured_soulseek_account_is_not_configured() {
        // A username without a password cannot log in; reporting that as "not
        // configured" is clearer than a login failure.
        let mut args = GlobalArgs {
            data_dir: PathBuf::from("/tmp"),
            library_root: PathBuf::from("/tmp"),
            discogs_token: None,
            proxy: None,
            slsk_username: Some("alice".to_owned()),
            slsk_password: None,
            slsk_server: "server.slsknet.org:2416".to_owned(),
            slsk_port: 2234,
            slsk_shared: Vec::new(),
            slsk_timeout: 8,
        };
        assert!(Settings::from(&args).soulseek().expect("parsed").is_none());

        args.slsk_password = Some("secret".to_owned());
        assert!(Settings::from(&args).soulseek().expect("parsed").is_some());
    }

    #[test]
    fn the_soulseek_server_must_be_host_and_port() {
        assert!(parse_endpoint("server.slsknet.org:2416").is_ok());
        assert!(parse_endpoint("server.slsknet.org").is_err());
        assert!(parse_endpoint("server.slsknet.org:not-a-port").is_err());
        assert!(parse_endpoint(":2416").is_err());
    }

    #[test]
    fn the_soulseek_config_carries_the_whole_credential_and_the_shares() {
        let settings = Settings {
            data_dir: PathBuf::from("/tmp"),
            library_root: PathBuf::from("/tmp"),
            discogs_token: None,
            proxy: None,
            slsk_username: Some("alice".to_owned()),
            slsk_password: Some("secret".to_owned()),
            slsk_server: "127.0.0.1:2242".to_owned(),
            slsk_port: 3000,
            slsk_shared: vec![PathBuf::from("/music")],
            slsk_timeout: Duration::from_secs(5),
        };

        let config = settings.soulseek().expect("parsed").expect("configured");
        assert_eq!(config.username, "alice");
        assert_eq!(config.server, "127.0.0.1");
        assert_eq!(config.server_port, 2242);
        assert_eq!(config.listen_port, 3000);
        assert_eq!(config.search_timeout, Duration::from_secs(5));
        assert!(config.shares_anything());
    }

    #[test]
    fn a_proxy_url_that_cannot_be_parsed_is_reported() {
        let error = ProxyConfig::parse("ftp://proxy:21").expect_err("unsupported scheme");
        assert!(matches!(
            SettingsError::from(error),
            SettingsError::Proxy(_)
        ));
    }
}
