//! The single local user.
//!
//! Aldo is a single-user install and the schema enforces exactly one row. The
//! row exists so that search sessions and acquisitions have something to belong
//! to; a fresh install has no user until one is seeded.

use aldo_core::ids::UserId;
use sqlx::SqlitePool;

use crate::error::StoreError;

/// The one user id. The migration constrains `app_user` to this value.
pub const LOCAL_USER: UserId = UserId(1);

/// Creates the local user if it is absent, keeping its library root in step
/// with configuration.
///
/// # Errors
/// Returns a store error if the row cannot be written.
pub async fn ensure_local_user(
    pool: &SqlitePool,
    library_root: &str,
    now_ms: i64,
) -> Result<UserId, StoreError> {
    sqlx::query(
        "INSERT INTO app_user (user_id, library_root, created_at)
         VALUES (?, ?, ?)
         ON CONFLICT (user_id) DO UPDATE SET library_root = excluded.library_root",
    )
    .bind(i64::from(LOCAL_USER))
    .bind(library_root)
    .bind(now_ms)
    .execute(pool)
    .await?;

    Ok(LOCAL_USER)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn pool() -> SqlitePool {
        let store = crate::Store::open_temporary().await.expect("store opens");
        store.pool().clone()
    }

    #[tokio::test]
    async fn seeds_the_local_user() {
        let pool = pool().await;
        let user = ensure_local_user(&pool, "/music", 0).await.expect("seeded");
        assert_eq!(user, LOCAL_USER);

        let root: String =
            sqlx::query_scalar("SELECT library_root FROM app_user WHERE user_id = 1")
                .fetch_one(&pool)
                .await
                .expect("row");
        assert_eq!(root, "/music");
    }

    #[tokio::test]
    async fn is_idempotent_and_keeps_the_library_root_current() {
        let pool = pool().await;
        ensure_local_user(&pool, "/music", 0).await.expect("first");
        ensure_local_user(&pool, "/other", 1).await.expect("second");

        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM app_user")
            .fetch_one(&pool)
            .await
            .expect("count");
        assert_eq!(count, 1, "seeding twice created two users");

        let root: String =
            sqlx::query_scalar("SELECT library_root FROM app_user WHERE user_id = 1")
                .fetch_one(&pool)
                .await
                .expect("row");
        assert_eq!(root, "/other", "a moved library root was not recorded");
    }

    #[tokio::test]
    async fn a_session_can_attach_to_the_seeded_user() {
        // The foreign key is the point: without the user row a search cannot
        // even be recorded.
        let pool = pool().await;
        let user = ensure_local_user(&pool, "/music", 0).await.expect("seeded");

        let session: i64 = sqlx::query_scalar(
            "INSERT INTO search_session
                (user_id, raw_query, normalized_query, filters_json, created_at)
             VALUES (?, 'q', 'q', '{}', 0) RETURNING session_id",
        )
        .bind(user.get())
        .fetch_one(&pool)
        .await
        .expect("session inserted");
        assert!(session > 0);
    }

    #[tokio::test]
    async fn a_session_without_the_user_row_is_refused() {
        let pool = pool().await;
        let result = sqlx::query(
            "INSERT INTO search_session
                (user_id, raw_query, normalized_query, filters_json, created_at)
             VALUES (1, 'q', 'q', '{}', 0)",
        )
        .execute(&pool)
        .await;

        assert!(result.is_err(), "an orphaned session was accepted");
    }
}
