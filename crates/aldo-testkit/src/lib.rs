//! A loopback Discogs server, for tests that need the wire format.
//!
//! Shared by `aldo-catalog` and `aldo-api`, which is why it is a crate of its own
//! rather than a `#[cfg(test)]` module in either.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;

/// A loopback Discogs server.
#[derive(Debug)]
pub struct DiscogsStub {
    pub base_url: String,
    /// How many `/database/search` requests arrived.
    pub search_calls: Arc<AtomicU32>,
    /// How many `/releases/{id}` requests arrived.
    pub release_calls: Arc<AtomicU32>,
    handle: tokio::task::JoinHandle<()>,
}

impl Drop for DiscogsStub {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

#[derive(Clone)]
struct StubState {
    search_json: &'static str,
    release_json: &'static str,
    search_calls: Arc<AtomicU32>,
    release_calls: Arc<AtomicU32>,
}

/// Starts a stub server. The returned base URL is what a `DiscogsClient` should
/// be pointed at with `with_base`.
pub async fn discogs_stub(search_json: &'static str, release_json: &'static str) -> DiscogsStub {
    let search_calls = Arc::new(AtomicU32::new(0));
    let release_calls = Arc::new(AtomicU32::new(0));

    let state = StubState {
        search_json,
        release_json,
        search_calls: search_calls.clone(),
        release_calls: release_calls.clone(),
    };

    let app = Router::new()
        .route("/database/search", get(search))
        .route("/releases/{id}", get(release))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind an ephemeral port");
    let address = listener.local_addr().expect("local address");

    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("stub server runs");
    });

    DiscogsStub {
        base_url: format!("http://{address}"),
        search_calls,
        release_calls,
        handle,
    }
}

async fn search(State(state): State<StubState>) -> Response {
    state.search_calls.fetch_add(1, Ordering::SeqCst);
    raw_json(state.search_json)
}

async fn release(
    State(state): State<StubState>,
    axum::extract::Path(_id): axum::extract::Path<String>,
) -> Response {
    state.release_calls.fetch_add(1, Ordering::SeqCst);
    raw_json(state.release_json)
}

/// Sends a body verbatim. Wrapping the fixture in `Json` would serialise the
/// string itself, quoting it, which is not what Discogs sends.
fn raw_json(body: &'static str) -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        Body::from(body),
    )
        .into_response()
}

/// A search response with one hit for release 123.
pub const SEARCH_ONE_HIT: &str = r#"{
    "pagination": {"page": 1, "pages": 1, "per_page": 50, "items": 1},
    "results": [
        {
            "id": 123,
            "title": "Radiohead - OK Computer",
            "year": 1997,
            "format": ["CD, Album"],
            "label": ["Parlophone"],
            "catno": "7243 8 55229 2 9",
            "country": "UK",
            "genre": ["Rock"],
            "style": ["Alternative Rock"],
            "cover_image": "https://img.example/cover.jpg",
            "master_id": 456,
            "type": "release"
        }
    ]
}"#;

/// A search response with no hits at all.
pub const SEARCH_NO_HITS: &str = r#"{
    "pagination": {"page": 1, "pages": 1, "per_page": 50, "items": 0},
    "results": []
}"#;

/// The release body for id 123, matching [`SEARCH_ONE_HIT`].
pub const RELEASE_123: &str = r#"{
    "id": 123,
    "master_id": 456,
    "title": "OK Computer",
    "artists": [{"id": 3840, "name": "Radiohead", "anv": null, "join": ""}],
    "tracklist": [
        {"position": "1", "title": "Airbag", "duration": "4:38", "type_": "track"},
        {"position": "2", "title": "Paranoid Android", "duration": "6:23", "type_": "track"}
    ],
    "labels": [{"id": 1, "name": "Parlophone", "catno": "7243 8 55229 2 9"}],
    "identifiers": [{"type": "Barcode", "value": "724385522929"}],
    "genres": ["Rock"],
    "styles": ["Alternative Rock"],
    "formats": [{"name": "CD", "qty": "1", "descriptions": ["Album"]}],
    "status": "Official",
    "released": "1997-05-21",
    "country": "UK",
    "images": [{"type": "primary", "uri": "https://img.example/cover.jpg"}],
    "community": {"have": 4200, "want": 900}
}"#;

/// A release body whose `released` and `formats` are missing, to prove the
/// mapper does not require them.
pub const RELEASE_MINIMAL: &str = r#"{
    "id": 123,
    "title": "OK Computer",
    "artists": [{"id": 3840, "name": "Radiohead"}],
    "tracklist": []
}"#;
