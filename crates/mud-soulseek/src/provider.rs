//! The SoulSeek provider.

use std::time::Duration;

use async_trait::async_trait;
use mud_core::error::ProviderError;
use mud_core::provider::{DiscoveryProvider, ProviderId, ProviderOutcome};
use mud_core::search::SearchQuery;
use soulseek_rs::{ClientSettings, PeerAddress, SearchResult};
use tokio::sync::{mpsc, oneshot};

use crate::config::SoulSeekConfig;
use crate::mapping;
use crate::worker::{self, Command};

/// A live connection to the SoulSeek network.
///
/// Dropping it stops the worker thread and closes the connection.
#[derive(Debug)]
pub struct SoulSeekProvider {
    commands: mpsc::Sender<Command>,
    search_timeout: Duration,
    shares_anything: bool,
}

impl SoulSeekProvider {
    /// Connects and logs in.
    ///
    /// # Errors
    /// Fails with [`ProviderError::Unauthorised`] for a rejected login, and
    /// [`ProviderError::Unreachable`] when the server cannot be reached or the
    /// login does not complete.
    pub async fn connect(config: SoulSeekConfig) -> Result<Self, ProviderError> {
        let (commands_tx, commands_rx) = mpsc::channel(4);
        let (ready_tx, ready_rx) = oneshot::channel();

        let settings = client_settings(&config);
        std::thread::Builder::new()
            .name("mud-soulseek".to_owned())
            .spawn(move || worker::run(settings, commands_rx, ready_tx))
            .map_err(|error| ProviderError::Unreachable {
                provider: ProviderId::SoulSeek,
                message: format!("could not start the client thread: {error}"),
            })?;

        match ready_rx.await {
            Ok(Ok(())) => Ok(Self {
                commands: commands_tx,
                search_timeout: config.search_timeout,
                shares_anything: config.shares_anything(),
            }),
            Ok(Err(error)) => Err(error),
            Err(_) => Err(ProviderError::Unreachable {
                provider: ProviderId::SoulSeek,
                message: "the client thread stopped before logging in".to_owned(),
            }),
        }
    }

    /// Whether this client offers anything to the network.
    ///
    /// False means peers will queue downloads last. Search still works.
    #[must_use]
    pub fn shares_anything(&self) -> bool {
        self.shares_anything
    }

    /// Sends one search and waits for the results.
    async fn search_raw(&self, query: &str) -> Result<Vec<SearchResult>, ProviderError> {
        let (reply_tx, reply_rx) = oneshot::channel();

        self.commands
            .send(Command::Search {
                query: query.to_owned(),
                timeout: self.search_timeout,
                reply: reply_tx,
            })
            .await
            .map_err(|_| ProviderError::Unreachable {
                provider: ProviderId::SoulSeek,
                message: "the SoulSeek client is no longer running".to_owned(),
            })?;

        reply_rx.await.map_err(|_| ProviderError::Unreachable {
            provider: ProviderId::SoulSeek,
            message: "the SoulSeek client stopped before answering".to_owned(),
        })?
    }

    /// Downloads one file from a peer into `download_directory`.
    pub async fn download(
        &self,
        peer: &str,
        remote_path: &str,
        size: u64,
        download_directory: &std::path::Path,
        progress: mpsc::Sender<worker::DownloadProgress>,
    ) -> Result<(), ProviderError> {
        let (reply_tx, reply_rx) = oneshot::channel();

        self.commands
            .send(Command::Download {
                peer: peer.to_owned(),
                filename: remote_path.to_owned(),
                size,
                download_directory: download_directory.to_string_lossy().to_string(),
                progress,
                reply: reply_tx,
            })
            .await
            .map_err(|_| ProviderError::Unreachable {
                provider: ProviderId::SoulSeek,
                message: "the SoulSeek client is no longer running".to_owned(),
            })?;

        reply_rx.await.map_err(|_| ProviderError::Unreachable {
            provider: ProviderId::SoulSeek,
            message: "the SoulSeek client stopped during download".to_owned(),
        })?
    }
}

#[async_trait]
impl DiscoveryProvider for SoulSeekProvider {
    fn id(&self) -> ProviderId {
        ProviderId::SoulSeek
    }

    async fn search(&self, query: &SearchQuery) -> Result<ProviderOutcome, ProviderError> {
        let results = self.search_raw(&query.soulseek_query()).await?;

        let raw_hits = u32::try_from(
            results
                .iter()
                .map(|result| result.files.len())
                .sum::<usize>(),
        )
        .unwrap_or(u32::MAX);

        let mut rejected = mud_core::candidate::RejectionCounts::default();
        let candidates = mapping::candidates_from(results, &mut rejected);

        Ok(ProviderOutcome {
            candidates,
            raw_hits,
            rejected,
        })
    }
}

fn client_settings(config: &SoulSeekConfig) -> ClientSettings {
    ClientSettings {
        username: config.username.clone(),
        password: config.password.clone(),
        server_address: PeerAddress::new(config.server.clone(), config.server_port),
        listen_port: config.listen_port,
        shared_directories: config.shared_directories.clone(),
        // Serving children costs a socket and the network's whole search stream
        // per child, and only helps the tree, not this client's own results.
        accept_children: false,
        ..ClientSettings::new(config.username.clone(), config.password.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_carry_the_server_the_port_and_the_shares() {
        let config = SoulSeekConfig::new("alice", "secret")
            .with_server("127.0.0.1", 2242)
            .with_shared_directories(vec!["/music".to_owned()]);

        let settings = client_settings(&config);

        assert_eq!(settings.username, "alice");
        assert_eq!(settings.password, "secret");
        assert_eq!(settings.listen_port, crate::config::DEFAULT_LISTEN_PORT);
        assert_eq!(settings.shared_directories, vec!["/music".to_owned()]);
        assert!(!settings.accept_children, "children are off deliberately");
    }
}
