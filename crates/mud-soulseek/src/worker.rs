//! The client, on a thread of its own.
//!
//! `soulseek-rs-lib` is blocking and owns its own sockets, so it cannot run on
//! a tokio worker. It lives on a dedicated thread and is reached over channels:
//! one `Search` in, one result out. Nothing here sleeps or retries; the library
//! bounds the search window itself.

use std::time::Duration;

use mud_core::ProviderId;
use mud_core::error::ProviderError;
use soulseek_rs::{Client, ClientSettings, DownloadStatus, SearchResult};
use tokio::sync::{mpsc, oneshot};

/// Live progress of an ongoing download.
#[derive(Debug, Clone)]
pub enum DownloadProgress {
    Queued,
    InProgress {
        bytes_downloaded: u64,
        total_bytes: u64,
        speed_bps: u32,
    },
    Completed,
    Failed(String),
}

/// A request the worker performs.
pub(crate) enum Command {
    Search {
        query: String,
        timeout: Duration,
        reply: oneshot::Sender<Result<Vec<SearchResult>, ProviderError>>,
    },
    Download {
        peer: String,
        filename: String,
        size: u64,
        download_directory: String,
        progress: mpsc::Sender<DownloadProgress>,
        reply: oneshot::Sender<Result<(), ProviderError>>,
    },
}

/// Connects, logs in, then serves commands until every sender is dropped.
///
/// The connect and login outcome is sent back exactly once, through `ready`, so
/// the caller learns about a bad password or an unreachable server rather than
/// waiting for a search that will never work.
pub(crate) fn run(
    settings: ClientSettings,
    mut commands: mpsc::Receiver<Command>,
    ready: oneshot::Sender<Result<(), ProviderError>>,
) {
    let mut client = Client::with_settings(settings);

    if let Err(error) = client.connect() {
        let _ = ready.send(Err(connect_error(&error.to_string())));
        return;
    }

    match client.login() {
        Ok(true) => {
            if ready.send(Ok(())).is_err() {
                // The caller went away during login; nothing to serve.
                return;
            }
        }
        Ok(false) => {
            let _ = ready.send(Err(ProviderError::Unauthorised {
                provider: ProviderId::SoulSeek,
            }));
            return;
        }
        Err(error) => {
            let _ = ready.send(Err(connect_error(&error.to_string())));
            return;
        }
    }

    // `blocking_recv` ends when every sender is dropped, which is how the
    // thread shuts down when the provider does.
    while let Some(command) = commands.blocking_recv() {
        match command {
            Command::Search {
                query,
                timeout,
                reply,
            } => {
                let result =
                    client
                        .search(&query, timeout)
                        .map_err(|error| ProviderError::Unreachable {
                            provider: ProviderId::SoulSeek,
                            message: error.to_string(),
                        });
                let _ = reply.send(result);
            }
            Command::Download {
                peer,
                filename,
                size,
                download_directory,
                progress,
                reply,
            } => {
                handle_download(
                    &client,
                    &peer,
                    filename,
                    size,
                    download_directory,
                    &progress,
                    reply,
                );
            }
        }
    }
}

fn handle_download(
    client: &Client,
    peer: &str,
    filename: String,
    size: u64,
    download_directory: String,
    progress: &mpsc::Sender<DownloadProgress>,
    reply: oneshot::Sender<Result<(), ProviderError>>,
) {
    match client.download(filename, peer.to_owned(), size, download_directory) {
        Ok((_download, rx)) => {
            let mut completed = false;
            while let Ok(status) = rx.recv() {
                match status {
                    DownloadStatus::Queued => {
                        let _ = progress.blocking_send(DownloadProgress::Queued);
                    }
                    DownloadStatus::InProgress {
                        bytes_downloaded,
                        total_bytes,
                        speed_bytes_per_sec,
                    } => {
                        let speed_bps =
                            if speed_bytes_per_sec.is_finite() && speed_bytes_per_sec >= 0.0 {
                                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                                {
                                    speed_bytes_per_sec as u32
                                }
                            } else {
                                0
                            };
                        let _ = progress.blocking_send(DownloadProgress::InProgress {
                            bytes_downloaded,
                            total_bytes,
                            speed_bps,
                        });
                    }
                    DownloadStatus::Completed => {
                        let _ = progress.blocking_send(DownloadProgress::Completed);
                        completed = true;
                        break;
                    }
                    DownloadStatus::Failed(reason) => {
                        let msg = reason.unwrap_or_else(|| "download failed".to_string());
                        let _ = progress.blocking_send(DownloadProgress::Failed(msg.clone()));
                        let _ = reply.send(Err(ProviderError::Unreachable {
                            provider: ProviderId::SoulSeek,
                            message: msg,
                        }));
                        return;
                    }
                    DownloadStatus::Cancelled => {
                        let msg = "download cancelled".to_string();
                        let _ = progress.blocking_send(DownloadProgress::Failed(msg.clone()));
                        let _ = reply.send(Err(ProviderError::Unreachable {
                            provider: ProviderId::SoulSeek,
                            message: msg,
                        }));
                        return;
                    }
                    DownloadStatus::TimedOut => {
                        let msg = "download timed out".to_string();
                        let _ = progress.blocking_send(DownloadProgress::Failed(msg.clone()));
                        let _ = reply.send(Err(ProviderError::Unreachable {
                            provider: ProviderId::SoulSeek,
                            message: msg,
                        }));
                        return;
                    }
                    DownloadStatus::Paused { .. } => {}
                }
            }
            if completed {
                let _ = reply.send(Ok(()));
            } else if !reply.is_closed() {
                let _ = reply.send(Err(ProviderError::Unreachable {
                    provider: ProviderId::SoulSeek,
                    message: "transfer closed unexpectedly".to_string(),
                }));
            }
        }
        Err(error) => {
            let msg = error.to_string();
            let _ = progress.blocking_send(DownloadProgress::Failed(msg.clone()));
            let _ = reply.send(Err(ProviderError::Unreachable {
                provider: ProviderId::SoulSeek,
                message: msg,
            }));
        }
    }
}

fn connect_error(message: &str) -> ProviderError {
    ProviderError::Unreachable {
        provider: ProviderId::SoulSeek,
        message: message.to_owned(),
    }
}
