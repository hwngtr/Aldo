//! The HTTP surface.
//!
//! Deliberately small: a health probe plus the routes the Tauri shell calls.
//!
//! Only the Discogs fan-out is connected. SoulSeek and the trackers are not,
//! so a search reports what it could not do instead of inventing results.

use axum::Router;
use axum::extract::State;
use axum::routing::{get, post};

use serde::Serialize;

use crate::auth::{reject_foreign_origin, require_token};
use crate::error::ApiResult;
use crate::state::AppState;

/// Builds the router. Every route sits behind the bearer token.
pub fn router(state: AppState) -> Router {
    let token = state.token.clone();
    let origin_policy = state.origins.clone();
    // An unauthenticated search route would let any local process drive the
    // network engines, so only the health probe is public.
    Router::new()
        .route("/health", get(health))
        .nest(
            "/api/v1",
            Router::new()
                .route("/search", post(search))
                .route("/budgets", get(budgets))
                .route("/sessions", get(sessions))
                .layer(axum::middleware::from_fn_with_state(token, require_token))
                .layer(axum::middleware::from_fn_with_state(
                    origin_policy,
                    reject_foreign_origin,
                )),
        )
        .with_state(state)
}

#[derive(Debug, Serialize)]
struct HealthBody {
    status: &'static str,
    version: &'static str,
}

async fn health() -> impl axum::response::IntoResponse {
    axum::Json(HealthBody {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
    })
}

/// Runs the shared Discogs fan-out and turns its record into a status view.
async fn discogs_fanout(
    state: &AppState,
    session_id: aldo_core::ids::SessionId,
    query: &aldo_core::search::SearchQuery,
    limit: u32,
    now: i64,
) -> ApiResult<(
    Vec<aldo_store::repo::CatalogRow>,
    crate::dto::ProviderStatusView,
)> {
    let started = std::time::Instant::now();
    let record = aldo_catalog::record_discogs(
        state.discogs.as_ref(),
        &state.sessions,
        session_id,
        query,
        limit,
        now,
    )
    .await?;
    let latency_ms = u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX);

    let state_view = match record.reason {
        aldo_catalog::FanoutReason::Answered => crate::dto::ProviderState::Done,
        aldo_catalog::FanoutReason::NotConfigured => crate::dto::ProviderState::NotConfigured,
        aldo_catalog::FanoutReason::BudgetDenied => crate::dto::ProviderState::BudgetDenied,
        aldo_catalog::FanoutReason::Failed => crate::dto::ProviderState::Failed,
    };

    let view = crate::dto::ProviderStatusView {
        provider: "discogs".to_owned(),
        state: state_view,
        result_count: record.result_count(),
        latency_ms,
        detail: Some(record.detail),
        retry_after_ms: record
            .retry_after
            .map(|wait| u64::try_from(wait.as_millis()).unwrap_or(u64::MAX)),
    };

    Ok((record.matches, view))
}

async fn search(
    State(state): State<AppState>,
    axum::extract::Json(request): axum::extract::Json<crate::dto::SearchRequest>,
) -> ApiResult<axum::Json<crate::dto::SearchResponse>> {
    let parsed = request.parsed().ok_or(crate::error::ApiError::BlankQuery)?;
    let now = aldo_store::now_ms();

    // Aldo is a single-user install; the row is seeded at startup.
    let session_id = state
        .sessions
        .create(aldo_store::repo::LOCAL_USER, &parsed, now)
        .await?;

    let (matches, provider) =
        discogs_fanout(&state, session_id, &parsed, request.release_limit(), now).await?;

    let result_count = u32::try_from(matches.len()).unwrap_or(u32::MAX);
    state
        .sessions
        .set_result_count(session_id, result_count)
        .await?;

    tracing::info!(session = %session_id, matches = result_count, "search completed");

    Ok(axum::Json(crate::dto::SearchResponse {
        session_id,
        artist: parsed.artist.clone(),
        album: parsed.album.clone(),
        year: parsed.year,
        matches: matches.into_iter().map(Into::into).collect(),
        providers: vec![provider],
    }))
}

async fn budgets(
    State(state): State<AppState>,
) -> ApiResult<axum::Json<Vec<crate::dto::BudgetView>>> {
    let now = aldo_store::now_ms();
    let mut views = Vec::new();

    for (provider, budget) in [
        ("soulseek", aldo_store::RateBudget::SOULSEEK_SEARCH),
        ("discogs", aldo_store::RateBudget::DISCOGS_AUTHENTICATED),
        ("musicbrainz", aldo_store::RateBudget::MUSICBRAINZ),
        ("acoustid", aldo_store::RateBudget::ACOUSTID),
    ] {
        state.budgets.ensure(provider, budget, now).await?;
        let snapshot = state.budgets.snapshot(provider, now).await?;
        views.push(crate::dto::BudgetView::new(provider, snapshot));
    }

    Ok(axum::Json(views))
}

async fn sessions(
    State(state): State<AppState>,
) -> ApiResult<axum::Json<Vec<crate::dto::SessionView>>> {
    // Aldo is a single-user install: the app_user row is seeded as id 1.
    let sessions = state.sessions.list(aldo_core::UserId(1), 50).await?;
    Ok(axum::Json(sessions.into_iter().map(Into::into).collect()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::ApiToken;
    use aldo_store::repo::FanoutStatus;
    use aldo_testkit::{DiscogsStub, RELEASE_123, SEARCH_NO_HITS, SEARCH_ONE_HIT, discogs_stub};
    use axum::body::Body;
    use axum::http::{HeaderValue, Request, StatusCode, header};
    use tower::ServiceExt as _;

    /// A router with no catalog provider configured.
    async fn app() -> (Router, ApiToken) {
        app_with(None).await
    }

    async fn app_with(stub: Option<&DiscogsStub>) -> (Router, ApiToken) {
        let (state, token) = crate::test_support::state_with_discogs(stub).await;
        (router(state), token)
    }

    fn bearer(token: &ApiToken) -> String {
        format!("Bearer {}", token.to_hex())
    }

    #[tokio::test]
    async fn health_needs_no_token() {
        let (app, _) = app().await;
        let response = app
            .oneshot(
                Request::get("/health")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn the_shells_own_origins_are_accepted() {
        // The request line is origin-form and carries no authority, so the
        // check cannot read the host from it. It compares against the bind
        // address instead; deriving it from the request silently refused every
        // legitimate browser call.
        for origin in ["http://127.0.0.1:8137", "tauri://localhost"] {
            let (app, token) = app().await;
            let response = app
                .oneshot(
                    Request::get("/api/v1/budgets")
                        .header(header::AUTHORIZATION, bearer(&token))
                        .header(header::ORIGIN, origin)
                        .body(Body::empty())
                        .expect("request"),
                )
                .await
                .expect("response");

            assert_eq!(response.status(), StatusCode::OK, "refused {origin}");
        }
    }

    #[tokio::test]
    async fn a_request_with_no_origin_is_allowed() {
        let (app, token) = app().await;
        let response = app
            .oneshot(
                Request::get("/api/v1/budgets")
                    .header(header::AUTHORIZATION, bearer(&token))
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn budgets_refuses_a_foreign_origin() {
        let (app, _) = app().await;
        let response = app
            .oneshot(
                Request::get("/api/v1/budgets")
                    .header(header::ORIGIN, "https://evil.invalid")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn budgets_refuses_an_origin_that_merely_ends_with_our_address() {
        let (app, token) = app().await;
        let response = app
            .oneshot(
                Request::get("/api/v1/budgets")
                    .header(header::AUTHORIZATION, bearer(&token))
                    .header(header::ORIGIN, "http://127.0.0.1:8137.evil.invalid")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn an_origin_that_is_not_readable_text_is_refused_not_read_as_absent() {
        let (app, token) = app().await;
        // obs-text is a legal header byte, so this reaches the middleware, and
        // `origin_is_allowed` reads an empty origin as "no origin" and allows.
        // Defaulting an unreadable origin to `""` would therefore have let a
        // valid token through instead of refusing it.
        let unreadable = HeaderValue::from_bytes(&[0x80]).expect("obs-text is a legal byte");
        assert!(
            unreadable.to_str().is_err(),
            "the fixture must be unreadable"
        );

        let response = app
            .oneshot(
                Request::get("/api/v1/budgets")
                    .header(header::AUTHORIZATION, bearer(&token))
                    .header(header::ORIGIN, unreadable)
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn budgets_refuses_a_request_with_no_token() {
        let (app, _) = app().await;
        let response = app
            .oneshot(
                Request::get("/api/v1/budgets")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn budgets_refuses_a_wrong_token() {
        let (app, _) = app().await;
        let response = app
            .oneshot(
                Request::get("/api/v1/budgets")
                    .header(header::AUTHORIZATION, "Bearer 00")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn budgets_succeeds_with_the_right_token() {
        let (app, token) = app().await;
        let response = app
            .oneshot(
                Request::get("/api/v1/budgets")
                    .header(header::AUTHORIZATION, bearer(&token))
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn budgets_report_the_real_soulseek_allowance() {
        let (app, token) = app().await;
        let response = app
            .oneshot(
                Request::get("/api/v1/budgets")
                    .header(header::AUTHORIZATION, bearer(&token))
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("body read");
        let parsed: Vec<crate::dto::BudgetView> =
            serde_json::from_slice(body.as_ref()).expect("json");

        let soulseek = parsed
            .iter()
            .find(|b| b.provider == "soulseek")
            .expect("entry");
        assert_eq!(
            soulseek.capacity, 34,
            "SoulSeek allows 34 searches per 220s"
        );
        assert_eq!(soulseek.available, 34);
    }

    #[tokio::test]
    async fn search_rejects_a_blank_query() {
        let (app, token) = app().await;
        let response = app
            .oneshot(
                Request::post("/api/v1/search")
                    .header(header::AUTHORIZATION, bearer(&token))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"query":"   "}"#))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn search_is_protected_like_everything_else() {
        let (app, _) = app().await;
        let response = app
            .oneshot(
                Request::post("/api/v1/search")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"query":"Artist - Album"}"#))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn search_admits_a_parsed_query() {
        let (app, token) = app().await;
        let response = app
            .oneshot(
                Request::post("/api/v1/search")
                    .header(header::AUTHORIZATION, bearer(&token))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"query":"Radiohead - OK Computer"}"#))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
    }

    async fn post_search(app: Router, token: &ApiToken, body: &str) -> crate::dto::SearchResponse {
        let response = app
            .oneshot(
                Request::post("/api/v1/search")
                    .header(header::AUTHORIZATION, bearer(token))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body.to_owned()))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 256 * 1024)
            .await
            .expect("body read");
        serde_json::from_slice(bytes.as_ref()).expect("search response")
    }

    #[tokio::test]
    async fn a_search_without_a_token_is_recorded_as_not_configured() {
        let (app, token) = app().await;
        let body = post_search(app, &token, r#"{"query":"Radiohead - OK Computer"}"#).await;

        assert!(body.matches.is_empty());
        assert_eq!(body.providers.len(), 1);
        assert_eq!(
            body.providers[0].state,
            crate::dto::ProviderState::NotConfigured
        );
        assert_eq!(body.providers[0].provider, "discogs");
        // The detail must say what to do, not merely that something failed.
        assert!(
            body.providers[0]
                .detail
                .as_deref()
                .is_some_and(|detail| detail.contains("ALDO_DISCOGS_TOKEN")),
            "{:?}",
            body.providers[0].detail
        );
    }

    #[tokio::test]
    async fn a_search_returns_the_releases_discogs_matched() {
        let stub = discogs_stub(SEARCH_ONE_HIT, RELEASE_123).await;
        let (app, token) = app_with(Some(&stub)).await;

        let body = post_search(app, &token, r#"{"query":"Radiohead - OK Computer - 1997"}"#).await;

        assert_eq!(body.artist.as_deref(), Some("Radiohead"));
        assert_eq!(body.album.as_deref(), Some("OK Computer"));
        assert_eq!(body.year, Some(1997));

        assert_eq!(body.matches.len(), 1);
        let matched = &body.matches[0];
        assert_eq!(matched.title, "OK Computer");
        assert_eq!(matched.artist.as_deref(), Some("Radiohead"));
        assert_eq!(matched.track_count, 2);
        assert_eq!(matched.release_types, vec!["album"]);

        assert_eq!(body.providers[0].state, crate::dto::ProviderState::Done);
        assert_eq!(body.providers[0].result_count, 1);
    }

    #[tokio::test]
    async fn a_search_with_no_matches_is_a_successful_empty_answer() {
        let stub = discogs_stub(SEARCH_NO_HITS, RELEASE_123).await;
        let (app, token) = app_with(Some(&stub)).await;

        let body = post_search(app, &token, r#"{"query":"Nothing - Nothing"}"#).await;

        assert!(body.matches.is_empty());
        // `done` with zero results is an answer; it must not look like a fault.
        assert_eq!(body.providers[0].state, crate::dto::ProviderState::Done);
        assert_eq!(body.providers[0].result_count, 0);
    }

    #[tokio::test]
    async fn a_second_identical_search_does_not_refetch_the_release() {
        let stub = discogs_stub(SEARCH_ONE_HIT, RELEASE_123).await;
        let (app, token) = app_with(Some(&stub)).await;

        post_search(
            app.clone(),
            &token,
            r#"{"query":"Radiohead - OK Computer"}"#,
        )
        .await;
        let second = post_search(app, &token, r#"{"query":"Radiohead - OK Computer"}"#).await;

        assert_eq!(second.matches.len(), 1);
        assert_eq!(
            stub.release_calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the release body was fetched more than once"
        );
    }

    #[tokio::test]
    async fn every_search_records_a_fanout_even_when_unconfigured() {
        // "could not search" and "searched and found nothing" must be
        // distinguishable afterwards, so both leave a row.
        let (state, token) = crate::test_support::state_with_discogs(None).await;
        let sessions = state.sessions.clone();
        let body = post_search(router(state), &token, r#"{"query":"Radiohead"}"#).await;

        let fanouts = sessions
            .list_fanouts(body.session_id)
            .await
            .expect("fanouts");
        assert_eq!(fanouts.len(), 1);
        assert_eq!(fanouts[0].provider, aldo_core::ProviderId::Discogs);
        assert_eq!(fanouts[0].status, FanoutStatus::Error);
        assert!(
            fanouts[0]
                .error
                .as_deref()
                .is_some_and(|error| error.contains("ALDO_DISCOGS_TOKEN")),
            "an unconfigured provider must record why: {:?}",
            fanouts[0].error
        );
    }

    #[tokio::test]
    async fn a_successful_search_records_a_done_fanout_with_its_count() {
        let stub = discogs_stub(SEARCH_ONE_HIT, RELEASE_123).await;
        let (state, token) = crate::test_support::state_with_discogs(Some(&stub)).await;
        let sessions = state.sessions.clone();
        let body = post_search(router(state), &token, r#"{"query":"Radiohead"}"#).await;

        let fanouts = sessions
            .list_fanouts(body.session_id)
            .await
            .expect("fanouts");
        assert_eq!(fanouts[0].status, FanoutStatus::Done);
        assert_eq!(fanouts[0].result_count, 1);
        assert_eq!(fanouts[0].error, None);
    }
}
