//! Persistent store. Owns the SQLite connection and every SQL statement in the
//! codebase: no other crate writes SQL, so parameter binding cannot be
//! forgotten.

pub mod error;
pub mod migrate;

pub mod repo;

use std::str::FromStr as _;
use std::sync::atomic::{AtomicU64, Ordering};

use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

pub use error::{LibraryRootError, StoreError, now_ms, validate_library_root};
pub use repo::{
    AcquisitionRow, AcquisitionStore, BudgetSnapshot, BudgetStore, CachedMetainfo, CandidateRow,
    CandidateStore, CatalogRow, CatalogStore, FanoutRow, RateBudget, SessionRow, SessionStore,
};

/// A handle to the database.
#[derive(Debug, Clone)]
pub struct Store {
    pool: SqlitePool,
}

impl Store {
    /// The connection URL for a database file.
    ///
    /// A SQLite URL treats everything before the last `//` as a host, so a
    /// relative or single-slash path silently becomes a host name and the open
    /// fails with "unable to open database file".
    pub fn url_for(path: &std::path::Path) -> Result<String, StoreError> {
        let mut parts: Vec<&std::ffi::OsStr> = Vec::new();
        for component in path.components() {
            match component {
                std::path::Component::Normal(part) => parts.push(part),
                std::path::Component::RootDir | std::path::Component::CurDir => {}
                std::path::Component::ParentDir | std::path::Component::Prefix(_) => {
                    return Err(StoreError::Connect(format!(
                        "{} contains a parent-directory component",
                        path.display()
                    )));
                }
            }
        }

        // Walk down from the root while the path exists, then re-attach the rest:
        // the file and its directory are both created on first open.
        let mut resolved = std::path::PathBuf::from("/");
        let mut existing = 0;
        for part in &parts {
            let candidate = resolved.join(part);
            if candidate.exists() {
                resolved = candidate;
                existing += 1;
            } else {
                break;
            }
        }

        let full = parts[existing..]
            .iter()
            .fold(resolved, |base, part| base.join(part));

        if !full.is_absolute() {
            return Err(StoreError::Connect(format!(
                "{} did not resolve to an absolute path",
                path.display()
            )));
        }

        Ok(format!("sqlite://{}", full.display()))
    }

    /// Opens the database at `url`, creating the file if needed.
    ///
    /// Foreign keys are enabled explicitly: SQLite disables them per connection
    /// and silently ignores `ON DELETE CASCADE` without this.
    pub async fn open(url: &str) -> Result<Self, StoreError> {
        let options = SqliteConnectOptions::from_str(url)
            .map_err(|source| StoreError::Connect(source.to_string()))?
            // A first run has no database file yet.
            .create_if_missing(true)
            .foreign_keys(true)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
            .synchronous(sqlx::sqlite::SqliteSynchronous::Normal)
            .busy_timeout(std::time::Duration::from_secs(5));

        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .acquire_timeout(std::time::Duration::from_secs(5))
            .connect_with(options)
            .await
            .map_err(|source| StoreError::Connect(source.to_string()))?;

        let store = Self { pool };
        migrate::run(&store.pool).await?;
        Ok(store)
    }

    /// Opens a private in-memory database. Tests only.
    ///
    /// The name is unique per call and the cache shared, because an unshared
    /// `sqlite::memory:` gives every pooled connection its own empty database,
    /// so migrations applied on one connection would be invisible to the next.
    /// A fixed name would make concurrent tests share one database.
    pub async fn open_temporary() -> Result<Self, StoreError> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let name = NEXT.fetch_add(1, Ordering::Relaxed);
        Self::open(&format!(
            "sqlite:file:aldo-test-{name}?mode=memory&cache=shared"
        ))
        .await
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }
}

/// The seed rows every repository test stands on.
///
/// A search hangs off a user, a session and a candidate, so a repository that
/// touches any of them needs the others inserted first, and four test modules
/// each spelled out the same three statements. Their shape is dictated by the
/// migration, not by any one test: a column added to `search_session` would
/// otherwise mean editing four copies of one insert to keep the suite green.
#[cfg(test)]
pub(crate) mod fixtures {
    use aldo_core::DedupeKey;
    use aldo_core::ids::{CandidateId, SessionId, Sha256, UserId};
    use sqlx::SqlitePool;

    use crate::Store;
    use crate::error::now_ms;

    /// A migrated in-memory store holding the one `app_user` row Aldo, a
    /// single-user install, always has.
    pub async fn store_with_a_user() -> (Store, UserId) {
        let store = Store::open_temporary().await.expect("store opens");
        let user = seed_user(store.pool()).await;
        (store, user)
    }

    pub async fn seed_user(pool: &SqlitePool) -> UserId {
        sqlx::query(
            "INSERT INTO app_user (user_id, library_root, created_at) VALUES (1, '/tmp/lib', ?)",
        )
        .bind(now_ms())
        .execute(pool)
        .await
        .expect("user inserted");
        UserId(1)
    }

    pub async fn seed_session(pool: &SqlitePool, user_id: UserId) -> SessionId {
        let raw: i64 = sqlx::query_scalar(
            "INSERT INTO search_session (user_id, raw_query, normalized_query, filters_json, created_at)
             VALUES (?, 'q', 'q', '{}', 0) RETURNING session_id",
        )
        .bind(user_id.get())
        .fetch_one(pool)
        .await
        .expect("session inserted");
        SessionId(raw)
    }

    /// `dedupe_key` is a hex SHA-256 digest, so the fixture stores one.
    pub async fn seed_candidate(pool: &SqlitePool, session_id: SessionId) -> CandidateId {
        let dedupe_key = DedupeKey::from_sha256(Sha256([7; 32])).as_sha256().to_hex();
        let raw: i64 = sqlx::query_scalar(
            "INSERT INTO candidate
                (session_id, dedupe_key, display_artist, display_album, created_at)
             VALUES (?, ?, 'A', 'B', 0) RETURNING candidate_id",
        )
        .bind(session_id.get())
        .bind(dedupe_key)
        .fetch_one(pool)
        .await
        .expect("candidate inserted");
        CandidateId(raw)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures;

    #[tokio::test]
    async fn opens_and_migrates_an_in_memory_database() {
        let store = Store::open_temporary().await.expect("store opens");
        let tables: Vec<String> =
            sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
                .fetch_all(store.pool())
                .await
                .expect("query runs");

        for expected in [
            "acquisition",
            "asset",
            "candidate",
            "discogs_lookup",
            "release",
            "release_track",
            "rate_budget",
            "search_session",
        ] {
            assert!(tables.iter().any(|t| t == expected), "missing {expected}");
        }
    }

    #[tokio::test]
    async fn foreign_keys_are_enforced() {
        let store = Store::open_temporary().await.expect("store opens");

        let inserted = sqlx::query(
            "INSERT INTO artist (canonical_name) VALUES ('Nirvana') RETURNING artist_id",
        )
        .fetch_one(store.pool())
        .await
        .expect("artist inserted");
        let artist_id: i64 = sqlx::Row::get(&inserted, "artist_id");

        // release_artist has a foreign key to artist, and a bogus artist_id must
        // be refused rather than creating an orphan row.
        let result = sqlx::query(
            "INSERT INTO release_artist (release_id, artist_id, position) VALUES (999, ?, 0)",
        )
        .bind(artist_id)
        .execute(store.pool())
        .await;

        assert!(result.is_err(), "orphaned foreign key was accepted");
    }

    #[tokio::test]
    async fn a_first_run_creates_the_database_file() {
        let dir = std::env::temp_dir().join(format!("aldo-create-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("fresh.sqlite3");
        assert!(!path.exists());

        let url = Store::url_for(&path).expect("url");
        let store = Store::open(&url).await.expect("opens");
        store.pool().close().await;

        assert!(path.exists(), "the database file was not created");

        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    #[test]
    fn the_url_for_a_file_keeps_the_path_absolute() {
        let url = Store::url_for(std::path::Path::new("/var/lib/aldo/aldo.sqlite3")).expect("url");
        assert_eq!(url, "sqlite:///var/lib/aldo/aldo.sqlite3");
        assert!(
            url.starts_with("sqlite:///"),
            "a single slash turns the path into a host name"
        );
    }

    #[test]
    fn the_url_for_a_relative_path_is_still_absolute() {
        let url = Store::url_for(std::path::Path::new("aldo.sqlite3")).expect("url");
        assert!(url.starts_with("sqlite:///"), "{url}");
        assert!(url.ends_with("/aldo.sqlite3"));
    }

    #[test]
    fn the_url_refuses_a_parent_directory_component() {
        assert!(Store::url_for(std::path::Path::new("/var/lib/../etc/passwd")).is_err());
    }

    #[tokio::test]
    async fn rejects_out_of_range_year_at_the_boundary() {
        let store = Store::open_temporary().await.expect("store opens");

        let result = sqlx::query(
            "INSERT INTO release (discogs_release_id, title, status, year, fetched_at)
             VALUES (1, 'x', 'official', 1200, 0)",
        )
        .execute(store.pool())
        .await;

        assert!(result.is_err(), "out-of-range year was accepted");
    }

    /// The shared fixtures are only worth having if they insert rows the schema
    /// actually accepts, in an order the foreign keys allow.
    #[tokio::test]
    async fn the_shared_fixtures_satisfy_every_foreign_key() {
        let (store, user) = fixtures::store_with_a_user().await;
        let session = fixtures::seed_session(store.pool(), user).await;
        let candidate = fixtures::seed_candidate(store.pool(), session).await;

        assert_eq!(candidate.get(), 1);
        assert_eq!(session.get(), 1);
        assert_eq!(user.get(), 1);

        let joined: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM candidate c
             JOIN search_session s ON s.session_id = c.session_id
             JOIN app_user u ON u.user_id = s.user_id",
        )
        .fetch_one(store.pool())
        .await
        .expect("query runs");
        assert_eq!(joined, 1, "the seeded rows do not join up");
    }

    #[tokio::test]
    async fn rejects_a_bit_depth_flac_cannot_have() {
        let store = Store::open_temporary().await.expect("store opens");

        let result = sqlx::query(
            "INSERT INTO asset_probe (asset_id, duration_ms, sample_rate_hz, bit_depth, channels, codec)
             VALUES (1, 1000, 44100, 20, 2, 'flac')",
        )
        .execute(store.pool())
        .await;

        assert!(result.is_err(), "20-bit depth was accepted");
    }
}
