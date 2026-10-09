//! Connection settings for the SoulSeek client.

use std::time::Duration;

/// The public server. Both host and port are configurable, because the network
/// has historically answered on more than one.
pub const DEFAULT_SERVER: &str = "server.slsknet.org";
/// The port that server listens on.
pub const DEFAULT_SERVER_PORT: u16 = 2416;
/// The port peers are told to reach this client on.
pub const DEFAULT_LISTEN_PORT: u16 = 2234;
/// How long a search collects responses.
///
/// Peers answer a popular query for minutes after it is sent, so this bounds
/// the wait rather than waiting for silence. Too short and the shortlist is
/// thin; too long and the command feels hung.
pub const DEFAULT_SEARCH_TIMEOUT: Duration = Duration::from_secs(8);

/// Everything needed to connect and search.
#[derive(Debug, Clone)]
pub struct SoulSeekConfig {
    pub username: String,
    pub password: String,
    pub server: String,
    pub server_port: u16,
    pub listen_port: u16,
    /// Directories offered to other peers. Empty means nothing is shared, which
    /// is allowed but makes this client a leecher: peers queue it last, and
    /// search results get thin.
    pub shared_directories: Vec<String>,
    pub search_timeout: Duration,
}

impl SoulSeekConfig {
    /// Settings for one account, with the network defaults.
    #[must_use]
    pub fn new(username: impl Into<String>, password: impl Into<String>) -> Self {
        Self {
            username: username.into(),
            password: password.into(),
            server: DEFAULT_SERVER.to_owned(),
            server_port: DEFAULT_SERVER_PORT,
            listen_port: DEFAULT_LISTEN_PORT,
            shared_directories: Vec::new(),
            search_timeout: DEFAULT_SEARCH_TIMEOUT,
        }
    }

    #[must_use]
    pub fn with_server(mut self, host: impl Into<String>, port: u16) -> Self {
        self.server = host.into();
        self.server_port = port;
        self
    }

    #[must_use]
    pub fn with_listen_port(mut self, port: u16) -> Self {
        self.listen_port = port;
        self
    }

    #[must_use]
    pub fn with_shared_directories(mut self, directories: Vec<String>) -> Self {
        self.shared_directories = directories;
        self
    }

    #[must_use]
    pub fn with_search_timeout(mut self, timeout: Duration) -> Self {
        self.search_timeout = timeout;
        self
    }

    /// Whether this client offers anything to the network.
    #[must_use]
    pub fn shares_anything(&self) -> bool {
        !self.shared_directories.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_are_the_public_network() {
        let config = SoulSeekConfig::new("user", "pass");
        assert_eq!(config.server, DEFAULT_SERVER);
        assert_eq!(config.server_port, DEFAULT_SERVER_PORT);
        assert_eq!(config.listen_port, DEFAULT_LISTEN_PORT);
        assert_eq!(config.search_timeout, DEFAULT_SEARCH_TIMEOUT);
        assert!(!config.shares_anything());
    }

    #[test]
    fn sharing_is_reported_from_the_directory_list() {
        let config =
            SoulSeekConfig::new("user", "pass").with_shared_directories(vec!["/music".to_owned()]);
        assert!(config.shares_anything());
    }

    #[test]
    fn the_listen_port_is_configurable() {
        // Port forwarding only helps if the announced port is the forwarded one.
        let config = SoulSeekConfig::new("user", "pass").with_listen_port(3000);
        assert_eq!(config.listen_port, 3000);
    }

    #[test]
    fn a_custom_server_replaces_both_host_and_port() {
        // Changing only the host would leave the port pointing at the public
        // server, which reads as a working connection to the wrong place.
        let config = SoulSeekConfig::new("user", "pass").with_server("127.0.0.1", 2242);
        assert_eq!(config.server, "127.0.0.1");
        assert_eq!(config.server_port, 2242);
    }
}
