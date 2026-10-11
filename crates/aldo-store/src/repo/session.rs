//! Search sessions and provider fan-outs.
//!
//! A session is one user query. A fanout is one provider's attempt to answer
//! it. Fanouts are rows rather than log lines because a search that returns
//! nothing must be explainable after the fact: how many providers were asked,
//! which were refused for budget, and how long each took.

use aldo_core::ProviderId;
use aldo_core::ids::{DiscogsReleaseId, FanoutId, SessionId, UserId};
use aldo_core::search::SearchQuery;
use sqlx::Row;
use sqlx::SqlitePool;

use crate::error::{StoreError, narrow_id, narrow_u16, narrow_u32};

stored_enum!(
    /// Where a fanout ended up.
    FanoutStatus,
    "provider_fanout.status" => {
        Pending => "pending",
        Running => "running",
        Done => "done",
        /// The rate budget refused the request. Not an error: the search was
        /// never sent.
        BudgetDenied => "budget_denied",
        Error => "error",
    }
);

#[derive(Debug, Clone)]
pub struct SessionRow {
    pub session_id: SessionId,
    pub user_id: UserId,
    pub query: SearchQuery,
    /// How many results the search left the user with, across every provider.
    pub result_count: u32,
    pub created_at: i64,
}

#[derive(Debug, Clone)]
pub struct FanoutRow {
    pub fanout_id: FanoutId,
    pub session_id: SessionId,
    pub parent: Option<FanoutId>,
    pub provider: ProviderId,
    pub query_sent: String,
    pub status: FanoutStatus,
    pub result_count: u32,
    pub latency_ms: Option<u32>,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SessionStore {
    pool: SqlitePool,
}

impl SessionStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn create(
        &self,
        user_id: UserId,
        query: &SearchQuery,
        now_ms: i64,
    ) -> Result<SessionId, StoreError> {
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO search_session
                (user_id, raw_query, normalized_query, parsed_artist, parsed_album,
                 parsed_year, filters_json, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?) RETURNING session_id",
        )
        .bind(user_id.get())
        .bind(&query.phrase)
        .bind(query.soulseek_query())
        .bind(query.artist.as_deref())
        .bind(query.album.as_deref())
        .bind(query.year.map(i64::from))
        .bind(
            serde_json::to_string(&query.filters)
                .map_err(|e| StoreError::corrupt("filters_json", e.to_string()))?,
        )
        .bind(now_ms)
        .fetch_one(&self.pool)
        .await?;

        narrow_id::<SessionId>(id, "session_id")
    }

    pub async fn list(&self, user_id: UserId, limit: u32) -> Result<Vec<SessionRow>, StoreError> {
        let rows = sqlx::query(
            "SELECT session_id, user_id, raw_query, parsed_artist, parsed_album,
                    parsed_year, filters_json, result_count, created_at
             FROM search_session WHERE user_id = ? ORDER BY created_at DESC LIMIT ?",
        )
        .bind(user_id.get())
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await?;

        rows.iter()
            .map(|row| {
                let filters_json: String = row.try_get("filters_json")?;
                let filters = serde_json::from_str(&filters_json)
                    .map_err(|e| StoreError::corrupt("filters_json", e.to_string()))?;
                let phrase: String = row.try_get("raw_query")?;
                let year: Option<i64> = row.try_get("parsed_year")?;
                let raw_session: i64 = row.try_get("session_id")?;

                Ok(SessionRow {
                    session_id: narrow_id::<SessionId>(raw_session, "session_id")?,
                    user_id,
                    query: SearchQuery {
                        phrase,
                        artist: row.try_get("parsed_artist")?,
                        album: row.try_get("parsed_album")?,
                        year: year
                            .map(|y| narrow_u16(y, "search_session.parsed_year"))
                            .transpose()?,
                        filters,
                    },
                    result_count: narrow_u32(
                        row.try_get("result_count")?,
                        "search_session.result_count",
                    )?,
                    created_at: row.try_get("created_at")?,
                })
            })
            .collect::<Result<_, StoreError>>()
    }

    pub async fn set_result_count(
        &self,
        session_id: SessionId,
        count: u32,
    ) -> Result<(), StoreError> {
        sqlx::query("UPDATE search_session SET result_count = ? WHERE session_id = ?")
            .bind(i64::from(count))
            .bind(session_id.get())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn set_selected_discogs_release(
        &self,
        session_id: SessionId,
        release_id: DiscogsReleaseId,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "UPDATE search_session
             SET selected_discogs_release_id = ?
             WHERE session_id = ?",
        )
        .bind(release_id.get())
        .bind(session_id.get())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn begin_fanout(
        &self,
        session_id: SessionId,
        provider: ProviderId,
        query_sent: &str,
        now_ms: i64,
    ) -> Result<FanoutId, StoreError> {
        insert_fanout(
            &self.pool,
            session_id,
            None,
            provider,
            query_sent,
            FanoutStatus::Running,
            now_ms,
        )
        .await
    }

    pub async fn finish_fanout(
        &self,
        fanout_id: FanoutId,
        status: FanoutStatus,
        result_count: u32,
        latency_ms: u32,
        error: Option<&str>,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "UPDATE provider_fanout
             SET status = ?, result_count = ?, latency_ms = ?, error = ?
             WHERE fanout_id = ?",
        )
        .bind(status.as_str())
        .bind(i64::from(result_count))
        .bind(i64::from(latency_ms))
        .bind(error)
        .bind(fanout_id.get())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn list_fanouts(&self, session_id: SessionId) -> Result<Vec<FanoutRow>, StoreError> {
        let rows = sqlx::query(
            "SELECT fanout_id, session_id, parent_fanout_id, provider, query_sent,
                    status, result_count, latency_ms, error
             FROM provider_fanout WHERE session_id = ? ORDER BY fanout_id",
        )
        .bind(session_id.get())
        .fetch_all(&self.pool)
        .await?;

        rows.iter()
            .map(|row| {
                let status_text: String = row.try_get("status")?;
                let raw_provider: String = row.try_get("provider")?;
                let raw_id: i64 = row.try_get("fanout_id")?;
                let raw_parent: Option<i64> = row.try_get("parent_fanout_id")?;
                let raw_session: i64 = row.try_get("session_id")?;
                let latency: Option<i64> = row.try_get("latency_ms")?;

                Ok(FanoutRow {
                    fanout_id: narrow_id::<FanoutId>(raw_id, "fanout_id")?,
                    session_id: narrow_id::<SessionId>(raw_session, "session_id")?,
                    parent: raw_parent
                        .map(|p| narrow_id::<FanoutId>(p, "parent_fanout_id"))
                        .transpose()?,
                    provider: parse_provider(&raw_provider)?,
                    query_sent: row.try_get("query_sent")?,
                    status: FanoutStatus::parse(&status_text)?,
                    result_count: narrow_u32(
                        row.try_get("result_count")?,
                        "provider_fanout.result_count",
                    )?,
                    latency_ms: latency
                        .map(|l| narrow_u32(l, "provider_fanout.latency_ms"))
                        .transpose()?,
                    error: row.try_get("error")?,
                })
            })
            .collect::<Result<_, StoreError>>()
    }
}

pub(crate) async fn insert_fanout(
    pool: &SqlitePool,
    session_id: SessionId,
    parent: Option<FanoutId>,
    provider: ProviderId,
    query_sent: &str,
    status: FanoutStatus,
    now_ms: i64,
) -> Result<FanoutId, StoreError> {
    // RETURNING, not `last_insert_rowid()`: the pool may hand a follow-up
    // query a different connection.
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO provider_fanout
            (session_id, parent_fanout_id, provider, query_sent, status, created_at)
         VALUES (?, ?, ?, ?, ?, ?) RETURNING fanout_id",
    )
    .bind(session_id.get())
    .bind(parent.map(i64::from))
    .bind(provider.as_str())
    .bind(query_sent)
    .bind(status.as_str())
    .bind(now_ms)
    .fetch_one(pool)
    .await?;

    narrow_id::<FanoutId>(id, "fanout_id")
}

/// The spelling is [`ProviderId::parse`], which owns it; this only names the
/// column the value came from when a stored row holds a name Aldo does not know.
pub(crate) fn parse_provider(text: &str) -> Result<ProviderId, StoreError> {
    ProviderId::parse(text).ok_or_else(|| StoreError::corrupt("provider_fanout.provider", text))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Store;
    use crate::fixtures;
    use aldo_core::search::{RawQuery, SearchFilters};

    async fn store_and_user() -> (SessionStore, Store, UserId) {
        let (store, user) = fixtures::store_with_a_user().await;
        (SessionStore::new(store.pool().clone()), store, user)
    }

    fn query(text: &str) -> SearchQuery {
        SearchQuery::parse(&RawQuery::new(text), SearchFilters::default()).expect("query parses")
    }

    #[tokio::test]
    async fn creates_a_session_and_reads_it_back() {
        let (sessions, _store, user) = store_and_user().await;

        let session_id = sessions
            .create(user, &query("Radiohead - OK Computer - 1997"), 1000)
            .await
            .expect("session created");

        let listed = sessions.list(user, 10).await.expect("listed");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].session_id, session_id);
        assert_eq!(listed[0].query.artist.as_deref(), Some("Radiohead"));
        assert_eq!(listed[0].query.album.as_deref(), Some("OK Computer"));
        assert_eq!(listed[0].query.year, Some(1997));
        assert!(listed[0].query.filters.lossless_only);
    }

    #[tokio::test]
    async fn a_session_reports_how_many_results_it_left() {
        // Written by `set_result_count` and read back by `aldo sessions`; an
        // unread column is a column that drifts.
        let (sessions, _store, user) = store_and_user().await;
        let session_id = sessions
            .create(user, &query("Artist - Album"), 0)
            .await
            .expect("session");

        assert_eq!(
            sessions.list(user, 10).await.expect("listed")[0].result_count,
            0
        );
        sessions
            .set_result_count(session_id, 95)
            .await
            .expect("counted");

        let listed = sessions.list(user, 10).await.expect("listed");
        assert_eq!(listed[0].result_count, 95);
    }

    #[tokio::test]
    async fn sessions_are_listed_newest_first() {
        let (sessions, _store, user) = store_and_user().await;

        sessions
            .create(user, &query("First"), 1000)
            .await
            .expect("first");
        sessions
            .create(user, &query("Second"), 2000)
            .await
            .expect("second");

        let listed = sessions.list(user, 10).await.expect("listed");
        assert_eq!(listed[0].query.phrase, "Second");
        assert_eq!(listed[1].query.phrase, "First");
    }

    #[tokio::test]
    async fn limit_bounds_the_result_set() {
        let (sessions, _store, user) = store_and_user().await;

        for index in 0_i64..5 {
            sessions
                .create(user, &query(&format!("Query {index}")), index)
                .await
                .expect("created");
        }

        assert_eq!(sessions.list(user, 2).await.expect("listed").len(), 2);
    }

    #[tokio::test]
    async fn records_a_fanout_that_succeeded() {
        let (sessions, _store, user) = store_and_user().await;
        let session_id = sessions
            .create(user, &query("Artist - Album"), 0)
            .await
            .expect("session");

        let fanout_id = sessions
            .begin_fanout(session_id, ProviderId::SoulSeek, "Artist Album", 10)
            .await
            .expect("fanout started");

        sessions
            .finish_fanout(fanout_id, FanoutStatus::Done, 7, 850, None)
            .await
            .expect("fanout finished");

        let fanouts = sessions.list_fanouts(session_id).await.expect("listed");
        assert_eq!(fanouts.len(), 1);
        assert_eq!(fanouts[0].provider, ProviderId::SoulSeek);
        assert_eq!(fanouts[0].status, FanoutStatus::Done);
        assert_eq!(fanouts[0].result_count, 7);
        assert_eq!(fanouts[0].latency_ms, Some(850));
        assert_eq!(fanouts[0].error, None);
    }

    #[tokio::test]
    async fn a_refused_fanout_is_distinguishable_from_a_failure() {
        let (sessions, _store, user) = store_and_user().await;
        let session_id = sessions
            .create(user, &query("Artist"), 0)
            .await
            .expect("session");

        let fanout_id = sessions
            .begin_fanout(session_id, ProviderId::SoulSeek, "Artist", 10)
            .await
            .expect("started");
        sessions
            .finish_fanout(
                fanout_id,
                FanoutStatus::BudgetDenied,
                0,
                0,
                Some("34 searches per 220s exhausted"),
            )
            .await
            .expect("finished");

        let fanouts = sessions.list_fanouts(session_id).await.expect("listed");
        assert_eq!(fanouts[0].status, FanoutStatus::BudgetDenied);
        assert!(fanouts[0].error.is_some());
    }

    #[tokio::test]
    async fn deleting_a_session_cascades_to_its_fanouts() {
        let (sessions, store, user) = store_and_user().await;
        let session_id = sessions
            .create(user, &query("Artist"), 0)
            .await
            .expect("session");
        sessions
            .begin_fanout(session_id, ProviderId::SoulSeek, "Artist", 10)
            .await
            .expect("fanout");

        sqlx::query("DELETE FROM search_session WHERE session_id = ?")
            .bind(session_id.get())
            .execute(store.pool())
            .await
            .expect("deleted");

        assert!(
            sessions
                .list_fanouts(session_id)
                .await
                .expect("listed")
                .is_empty()
        );
    }

    #[test]
    fn fanout_status_round_trips_through_its_stored_form() {
        for status in [
            FanoutStatus::Pending,
            FanoutStatus::Running,
            FanoutStatus::Done,
            FanoutStatus::BudgetDenied,
            FanoutStatus::Error,
        ] {
            assert_eq!(FanoutStatus::parse(status.as_str()).ok(), Some(status));
        }
    }

    #[test]
    fn an_unknown_stored_status_is_reported_not_guessed() {
        assert!(matches!(
            FanoutStatus::parse("banana"),
            Err(StoreError::Corrupt {
                column: "provider_fanout.status",
                ..
            })
        ));
    }

    #[test]
    fn an_unknown_stored_provider_is_reported_not_guessed() {
        assert!(matches!(
            parse_provider("napster"),
            Err(StoreError::Corrupt {
                column: "provider_fanout.provider",
                ..
            })
        ));
    }
}
