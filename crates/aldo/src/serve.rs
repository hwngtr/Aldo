//! `aldo serve`: the loopback HTTP daemon.
//!
//! Behind the `api` feature and off by default. Aldo is a single-user CLI on one
//! machine; this exists so a GUI shell could drive the same libraries, which is
//! also why it binds loopback only and demands a token.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;

use aldo_api::{ApiToken, AppState, router};

use crate::cli::ServeArgs;
use crate::settings::{Settings, SettingsError};

pub async fn run(settings: &Settings, args: &ServeArgs) -> ExitCode {
    match serve(settings, args).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("aldo: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn serve(settings: &Settings, args: &ServeArgs) -> Result<(), ServeError> {
    let store = settings.open_store().await?;
    // Built before the store is moved into the shared state.
    let discogs = settings.discogs(&store)?;

    let token = ApiToken::generate();
    let token_path = settings.data_dir.join("api-token");
    write_token(&token_path, &token).map_err(|source| ServeError::Token {
        path: token_path.clone(),
        source,
    })?;

    // Loopback is not configurable: the daemon holds a Discogs token.
    let address = SocketAddr::from(([127, 0, 0, 1], args.port));
    let mut state = AppState::new(store, token, settings.library_root.clone(), address);

    if let Some(service) = discogs {
        state = state.with_discogs(service);
        tracing::info!("Discogs search enabled");
    } else {
        tracing::warn!(
            "no Discogs token configured; searches will report the provider as unconfigured"
        );
    }

    let listener = tokio::net::TcpListener::bind(address)
        .await
        .map_err(|error| ServeError::Bind { address, error })?;

    tracing::info!(%address, token = %token_path.display(), "Aldo listening on loopback");
    axum::serve(listener, router(state))
        .await
        .map_err(|error| ServeError::Serving(error.to_string()))?;

    Ok(())
}

/// Writes the token with owner-only permissions: any local process that can read
/// it can drive downloads.
fn write_token(path: &std::path::Path, token: &ApiToken) -> std::io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(token.to_hex().as_bytes())
}

#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    #[error(transparent)]
    Settings(#[from] SettingsError),

    #[error("could not write {path}: {source}")]
    Token {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("could not bind {address}: {error}")]
    Bind {
        address: SocketAddr,
        error: std::io::Error,
    },

    #[error("the server stopped: {0}")]
    Serving(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::os::unix::fs::PermissionsExt as _;

    #[test]
    fn the_token_file_is_owner_only() {
        let dir = std::env::temp_dir().join(format!("aldo-token-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("api-token");

        let token = ApiToken::generate();
        write_token(&path, &token).expect("token written");

        let mode = std::fs::metadata(&path)
            .expect("metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "token is readable by other users");

        let contents = std::fs::read_to_string(&path).expect("read back");
        assert_eq!(contents, token.to_hex());

        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    #[test]
    fn the_token_is_twice_written_to_the_same_file_without_appending() {
        let dir = std::env::temp_dir().join(format!("aldo-token2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("api-token");

        let first = ApiToken::generate();
        write_token(&path, &first).expect("first");
        let second = ApiToken::generate();
        write_token(&path, &second).expect("second");

        let contents = std::fs::read_to_string(&path).expect("read back");
        assert_eq!(contents, second.to_hex(), "the old token was appended to");

        std::fs::remove_dir_all(&dir).expect("cleanup");
    }
}
