//! Which origins may call the API.
//!
//! A page served from another site could otherwise drive the user's downloads
//! from their browser. The check compares against the address the daemon
//! actually bound to, taken from configuration, because a request line carries
//! no authority of its own.

use std::net::SocketAddr;

/// The origins that may call the API.
///
/// The daemon answers on one loopback authority. The desktop shell may present
/// its own origin, which is platform-dependent: `tauri://localhost` on macOS and
/// Windows, `http://localhost:<port>` where the webview is served over HTTP.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OriginPolicy {
    bound: SocketAddr,
    shell_origins: Vec<String>,
}

impl OriginPolicy {
    #[must_use]
    pub fn new(bound: SocketAddr) -> Self {
        Self {
            bound,
            shell_origins: vec![
                "tauri://localhost".to_owned(),
                "http://tauri.localhost".to_owned(),
            ],
        }
    }

    /// Adds an origin the shell is known to present.
    #[must_use]
    pub fn allow(mut self, origin: impl Into<String>) -> Self {
        self.shell_origins.push(origin.into());
        self
    }

    fn bound_authority(&self) -> String {
        self.bound.to_string()
    }

    /// Whether an `Origin` header value is permitted.
    ///
    /// No `Origin` at all means a non-browser client, which the bearer token
    /// already governs.
    #[must_use]
    pub fn allows(&self, origin: Option<&str>) -> bool {
        let Some(origin) = origin.map(str::trim).filter(|o| !o.is_empty()) else {
            return true;
        };

        if self.shell_origins.iter().any(|allowed| allowed == origin) {
            return true;
        }

        // An exact authority match, not a suffix match: `127.0.0.1:8137.evil`
        // must not pass because it ends with our own authority.
        origin == self.bound_authority()
            || origin.ends_with(&format!("://{}", self.bound_authority()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> OriginPolicy {
        OriginPolicy::new(SocketAddr::from(([127, 0, 0, 1], 8137)))
    }

    #[test]
    fn a_request_with_no_origin_is_allowed() {
        assert!(policy().allows(None));
        assert!(policy().allows(Some("")));
        assert!(policy().allows(Some("   ")));
    }

    #[test]
    fn the_bound_authority_is_allowed_in_both_spellings() {
        let policy = policy();
        assert!(policy.allows(Some("http://127.0.0.1:8137")));
        assert!(policy.allows(Some("127.0.0.1:8137")));
    }

    #[test]
    fn a_lookalike_host_is_refused() {
        let policy = policy();
        assert!(!policy.allows(Some("http://127.0.0.1:8137.evil.invalid")));
        assert!(!policy.allows(Some("http://evil.invalid/127.0.0.1:8137")));
        assert!(!policy.allows(Some("http://127.0.0.1:9999")));
        assert!(!policy.allows(Some("http://evil.invalid")));
    }

    #[test]
    fn the_shell_origins_are_allowed() {
        let policy = policy();
        assert!(policy.allows(Some("tauri://localhost")));
        assert!(policy.allows(Some("http://tauri.localhost")));
    }

    #[test]
    fn an_extra_shell_origin_can_be_declared() {
        let policy = policy().allow("http://localhost:8137");
        assert!(policy.allows(Some("http://localhost:8137")));
        assert!(
            !OriginPolicy::new(SocketAddr::from(([127, 0, 0, 1], 8137)))
                .allows(Some("http://localhost:8137"))
        );
    }

    #[test]
    fn a_different_loopback_port_is_refused() {
        let other = OriginPolicy::new(SocketAddr::from(([127, 0, 0, 1], 9999)));
        assert!(!other.allows(Some("http://127.0.0.1:8137")));
        assert!(other.allows(Some("http://127.0.0.1:9999")));
    }
}
