//! Test-only helpers for the HTTP layer.
//!
//! The Discogs stub lives in `mud-testkit`, because the catalog service it
//! serves is tested from `mud-catalog` as well.

use std::net::SocketAddr;
use std::path::PathBuf;

use mud_catalog::{CatalogService, DiscogsClient, DiscogsToken};
use mud_net::{HttpClient, ProxyConfig};
use mud_store::Store;
use mud_testkit::DiscogsStub;

use crate::auth::ApiToken;
use crate::state::AppState;

/// The address the test routers bind. The origin policy compares against it, so
/// it must match what the origin tests send.
pub const TEST_ADDR: SocketAddr =
    SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), 8137);

/// Application state with the local user seeded, as the daemon does at startup,
/// and optionally a Discogs service pointed at `stub`.
pub async fn state_with_discogs(stub: Option<&DiscogsStub>) -> (AppState, ApiToken) {
    let store = Store::open_temporary().await.expect("store opens");
    mud_store::repo::ensure_local_user(store.pool(), "/tmp", 0)
        .await
        .expect("local user seeded");

    let token = ApiToken::generate();
    let mut state = AppState::new(store, token.clone(), PathBuf::from("/tmp"), TEST_ADDR);

    if let Some(stub) = stub {
        let http = HttpClient::new(&mud_catalog::user_agent(), &ProxyConfig::direct())
            .expect("http client");
        let client =
            DiscogsClient::with_base(http, Some(DiscogsToken::new("token")), &stub.base_url)
                .expect("discogs client");

        let catalog = state.catalog.clone();
        let budgets = state.budgets.clone();
        state = state.with_discogs(CatalogService::new(client, catalog, budgets));
    }

    (state, token)
}
