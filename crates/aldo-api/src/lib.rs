//! The daemon's HTTP surface and configuration.
//!
//! Binds to loopback only and requires a per-install bearer token: the API
//! drives downloads and holds a Discogs token, so no other process on the
//! machine may reach it.

pub mod auth;
pub mod dto;
pub mod error;
pub mod origin;
pub mod routes;
pub mod state;
#[cfg(test)]
pub(crate) mod test_support;

pub use auth::{ApiToken, AuthError, Authenticated};
pub use error::{ApiError, ApiResult};
pub use origin::OriginPolicy;
pub use routes::router;
pub use state::AppState;
