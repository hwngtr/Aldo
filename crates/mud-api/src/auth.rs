//! Loopback bearer-token authentication.
//!
//! MuD drives downloads and holds a Discogs token, so the API must not be
//! reachable by any other process on the machine. The server binds to
//! `127.0.0.1` only, and every route additionally requires a per-install
//! secret compared in constant time.

use std::sync::Arc;

use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use rand::RngCore;

/// The shared secret guarding every route. Wrapped so a stray `Debug` cannot
/// print the value.
#[derive(Clone)]
pub struct ApiToken(Arc<[u8; 32]>);

impl ApiToken {
    /// 32 bytes from the OS RNG.
    pub fn generate() -> Self {
        let mut bytes = [0_u8; 32];
        rand::rng().fill_bytes(&mut bytes);
        Self(Arc::new(bytes))
    }

    #[cfg(test)]
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(Arc::new(bytes))
    }

    #[must_use]
    pub fn expose(&self) -> &[u8; 32] {
        &self.0
    }

    /// The header representation: lowercase hex, because a bearer token travels
    /// in an HTTP header where raw bytes are not safe.
    #[must_use]
    pub fn to_hex(&self) -> String {
        mud_core::encode_hex(self.expose())
    }

    /// Constant time for equal lengths, so a local process cannot time its way to
    /// the secret. A length mismatch returns early, which leaks only the length.
    pub fn matches(&self, presented: &[u8]) -> bool {
        let expected = &self.0;
        if presented.len() != expected.len() {
            return false;
        }
        expected
            .iter()
            .zip(presented)
            .fold(0_u8, |acc, (a, b)| acc | (a ^ b))
            == 0
    }

    fn accepts_header(&self, presented: &str) -> bool {
        mud_core::decode_hex(presented).is_ok_and(|bytes| self.matches(&bytes))
    }
}

impl std::fmt::Debug for ApiToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ApiToken(redacted)")
    }
}

/// Extractor that admits only requests carrying the correct bearer token.
#[derive(Debug, Clone, Copy)]
pub struct Authenticated;

impl Authenticated {
    /// # Errors
    /// Returns `Unauthorized` when the header is absent, malformed, or holds the
    /// wrong secret.
    pub fn from_headers(headers: &HeaderMap, token: &ApiToken) -> Result<Self, AuthError> {
        let header_value = headers
            .get(header::AUTHORIZATION)
            .ok_or(AuthError::Missing)?;

        let presented = header_value
            .to_str()
            .map_err(|_| AuthError::Malformed)?
            .strip_prefix("Bearer ")
            .ok_or(AuthError::Malformed)?
            .trim();

        if token.accepts_header(presented) {
            Ok(Self)
        } else {
            Err(AuthError::Wrong)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AuthError {
    #[error("no Authorization header")]
    Missing,
    #[error("Authorization header was not `Bearer <token>`")]
    Malformed,
    #[error("the bearer token was not accepted")]
    Wrong,
}

impl IntoResponse for AuthError {
    fn into_response(self) -> Response {
        // The body deliberately says nothing about which part failed.
        (StatusCode::UNAUTHORIZED, "unauthorized").into_response()
    }
}

/// Refuses a request whose `Origin` is not permitted.
///
/// The check lives in [`crate::origin::OriginPolicy`]; this is the middleware
/// shell around it.
pub async fn reject_foreign_origin(
    axum::extract::State(policy): axum::extract::State<crate::origin::OriginPolicy>,
    headers: HeaderMap,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    let presented = headers.get(header::ORIGIN);
    // An `Origin` that is not readable text cannot be checked, so it is not
    // trusted to be absent: that would fail open for any client sending bytes a
    // browser would never send.
    let origin = match presented {
        None => None,
        Some(value) => match value.to_str() {
            Ok(text) => Some(text),
            Err(_) => return (StatusCode::FORBIDDEN, "unreadable origin").into_response(),
        },
    };

    if policy.allows(origin) {
        next.run(request).await
    } else {
        (StatusCode::FORBIDDEN, "foreign origin").into_response()
    }
}

/// Middleware form of [`Authenticated::from_headers`].
pub async fn require_token(
    axum::extract::State(token): axum::extract::State<ApiToken>,
    headers: HeaderMap,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    match Authenticated::from_headers(&headers, &token) {
        Ok(Authenticated) => next.run(request).await,
        Err(error) => error.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers_with(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(value).expect("valid"),
        );
        headers
    }

    #[test]
    fn accepts_the_correct_token() {
        let token = ApiToken::from_bytes([7; 32]);
        assert!(
            Authenticated::from_headers(
                &headers_with(&format!("Bearer {}", token.to_hex())),
                &token
            )
            .is_ok()
        );
    }

    #[test]
    fn accepts_uppercase_hex() {
        let token = ApiToken::from_bytes([0xab; 32]);
        let uppercase = token.to_hex().to_uppercase();
        assert!(
            Authenticated::from_headers(&headers_with(&format!("Bearer {uppercase}")), &token)
                .is_ok()
        );
    }

    #[test]
    fn the_header_form_round_trips() {
        let token = ApiToken::generate();
        assert_eq!(token.to_hex().len(), 64);
        assert_eq!(
            mud_core::decode_hex(&token.to_hex()).expect("decodes"),
            token.expose().to_vec()
        );
    }

    #[test]
    fn rejects_a_header_of_the_wrong_shape_rather_than_truncating_it() {
        let token = ApiToken::from_bytes([7; 32]);
        for value in [
            "7".repeat(63),  // odd length
            "7".repeat(66),  // too long
            "zz".repeat(32), // not hex
            format!("{}{}", token.to_hex(), "7"),
        ] {
            assert_eq!(
                Authenticated::from_headers(&headers_with(&format!("Bearer {value}")), &token)
                    .expect_err("rejected"),
                AuthError::Wrong,
                "accepted {value}"
            );
        }
    }

    #[test]
    fn rejects_a_wrong_token_without_saying_which_part_was_wrong() {
        let token = ApiToken::from_bytes([7; 32]);
        let error = Authenticated::from_headers(&headers_with("Bearer 0000"), &token)
            .expect_err("wrong token");

        assert_eq!(error, AuthError::Wrong);
        assert_eq!(error.into_response().status(), StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn rejects_a_missing_header() {
        let token = ApiToken::from_bytes([7; 32]);
        assert_eq!(
            Authenticated::from_headers(&HeaderMap::new(), &token).expect_err("missing"),
            AuthError::Missing
        );
    }

    #[test]
    fn rejects_a_header_without_the_bearer_scheme() {
        let token = ApiToken::from_bytes([7; 32]);
        for value in [token.to_hex(), "Basic abc".to_owned(), "Bearer".to_owned()] {
            assert_eq!(
                Authenticated::from_headers(&headers_with(&value), &token).expect_err("rejected"),
                AuthError::Malformed,
                "accepted {value:?}"
            );
        }
    }

    #[test]
    fn compares_in_constant_time_by_rejecting_any_length_mismatch() {
        let token = ApiToken::from_bytes([7; 32]);
        assert!(!token.matches(b"short"));
        assert!(!token.matches(b""));
        assert!(token.matches(&[7; 32]));
        assert!(!token.matches(&[8; 32]));
    }

    #[test]
    fn generated_tokens_differ_between_installs() {
        let first = ApiToken::generate();
        let second = ApiToken::generate();
        assert_ne!(first.expose(), second.expose());
        assert_eq!(first.expose().len(), 32);
    }

    #[test]
    fn the_token_never_prints_its_value() {
        assert_eq!(
            format!("{:?}", ApiToken::from_bytes([9; 32])),
            "ApiToken(redacted)"
        );
    }
}
