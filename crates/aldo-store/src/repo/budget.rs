//! Persistent rate-limit state.
//!
//! A SoulSeek protocol violation costs a 30-minute ban, so budgets outlive the
//! process. Restarts must not hand the client a fresh allowance.

use std::time::Duration;

use aldo_core::audio::DurationMs;
use sqlx::Row;
use sqlx::SqlitePool;

use crate::error::{StoreError, millis_from_stored, narrow_u32, widen_millis};

/// The persisted state of one budget, narrowed on the way out of the database.
///
/// A struct rather than a run of positional arguments: `refill` took seven
/// integers, four of them milliseconds and three of them counts, and swapping
/// two adjacent ones still compiled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BudgetState {
    capacity: u32,
    available: u32,
    /// When the last refill was accounted for.
    refilled_at: i64,
    window: Duration,
    min_interval: Duration,
    /// When a request was last allowed through, if ever.
    last_used_at: Option<i64>,
}

/// A fixed-capacity budget that refills over a rolling window, plus an optional
/// minimum gap between uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateBudget {
    pub capacity: u32,
    pub window: Duration,
    pub min_interval: Duration,
}

impl RateBudget {
    /// SoulSeek's measured search budget: 34 searches per 220 seconds, spaced at
    /// least 400 ms apart. Exceeding it triggers a 30-minute ban.
    pub const SOULSEEK_SEARCH: Self = Self {
        capacity: 34,
        window: Duration::from_secs(220),
        min_interval: Duration::from_millis(400),
    };

    /// Discogs: 60 requests per rolling minute with a personal access token, 25
    /// without. There is no documented per-request gap, so the bucket alone is
    /// the limit and a burst of release fetches is permitted.
    pub const DISCOGS_AUTHENTICATED: Self = Self {
        capacity: 60,
        window: Duration::from_secs(60),
        min_interval: Duration::ZERO,
    };

    /// MusicBrainz allows a hard maximum of one request per second.
    pub const MUSICBRAINZ: Self = Self {
        capacity: 1,
        window: Duration::from_secs(1),
        min_interval: Duration::from_millis(1100),
    };

    /// AcoustID allows three requests per second per API key.
    pub const ACOUSTID: Self = Self {
        capacity: 3,
        window: Duration::from_secs(1),
        min_interval: Duration::from_millis(340),
    };
}

/// What remains of a budget, for display in the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BudgetSnapshot {
    pub capacity: u32,
    pub available: u32,
    /// When the next request becomes permissible.
    pub retry_after: Duration,
}

/// A budget whose state is tracked in memory and mirrored to SQLite, so the
/// state survives a restart without a database round trip on the hot path.
#[derive(Debug, Clone)]
pub struct BudgetStore {
    pool: SqlitePool,
}

impl BudgetStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Registers a budget if absent, returning the persisted definition.
    pub async fn ensure(
        &self,
        provider: &str,
        budget: RateBudget,
        now_ms: i64,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO rate_budget (provider, capacity, window_ms, available, refilled_at, min_interval_ms)
             VALUES (?, ?, ?, ?, ?, ?)
             ON CONFLICT (provider) DO NOTHING",
        )
        .bind(provider)
        .bind(i64::from(budget.capacity))
        .bind(widen_millis(
            DurationMs::from_std(budget.window).as_millis(),
            "rate_budget.window_ms",
        )?)
        .bind(i64::from(budget.capacity))
        .bind(now_ms)
        .bind(widen_millis(
            DurationMs::from_std(budget.min_interval).as_millis(),
            "rate_budget.min_interval_ms",
        )?)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Reads the current state, refilling according to elapsed time.
    pub async fn snapshot(
        &self,
        provider: &str,
        now_ms: i64,
    ) -> Result<BudgetSnapshot, StoreError> {
        let row = sqlx::query(
            "SELECT capacity, available, refilled_at, window_ms, min_interval_ms, last_used_at
             FROM rate_budget WHERE provider = ?",
        )
        .bind(provider)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| StoreError::not_found("rate_budget"))?;

        // Every field is narrowed or converted through a checked helper, so a
        // negative allowance is reported rather than clamped to zero.
        Ok(refill(
            BudgetState {
                capacity: narrow_u32(row.try_get("capacity")?, "rate_budget.capacity")?,
                available: narrow_u32(row.try_get("available")?, "rate_budget.available")?,
                refilled_at: row.try_get("refilled_at")?,
                window: Duration::from_millis(millis_from_stored(
                    row.try_get("window_ms")?,
                    "rate_budget.window_ms",
                )?),
                min_interval: Duration::from_millis(millis_from_stored(
                    row.try_get("min_interval_ms")?,
                    "rate_budget.min_interval_ms",
                )?),
                last_used_at: row.try_get("last_used_at")?,
            },
            now_ms,
        ))
    }

    /// Consumes one unit, or returns the wait time if the budget is empty.
    pub async fn acquire(
        &self,
        provider: &str,
        now_ms: i64,
    ) -> Result<Result<Duration, Duration>, StoreError> {
        let mut transaction = self.pool.begin().await?;
        let row = sqlx::query(
            "SELECT capacity, available, refilled_at, window_ms, min_interval_ms, last_used_at
             FROM rate_budget WHERE provider = ?",
        )
        .bind(provider)
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or_else(|| StoreError::not_found("rate_budget"))?;

        let state = BudgetState {
            capacity: narrow_u32(row.try_get("capacity")?, "rate_budget.capacity")?,
            available: narrow_u32(row.try_get("available")?, "rate_budget.available")?,
            refilled_at: row.try_get("refilled_at")?,
            window: Duration::from_millis(millis_from_stored(
                row.try_get("window_ms")?,
                "rate_budget.window_ms",
            )?),
            min_interval: Duration::from_millis(millis_from_stored(
                row.try_get("min_interval_ms")?,
                "rate_budget.min_interval_ms",
            )?),
            last_used_at: row.try_get("last_used_at")?,
        };
        let snapshot = refill(state, now_ms);
        let retry_after = snapshot.retry_after;

        if retry_after.is_zero() && snapshot.available > 0 {
            sqlx::query(
                "UPDATE rate_budget
                 SET available = ?, refilled_at = ?, last_used_at = ?
                 WHERE provider = ?",
            )
            .bind(i64::from(snapshot.available - 1))
            .bind(now_ms)
            .bind(now_ms)
            .bind(provider)
            .execute(&mut *transaction)
            .await?;
            transaction.commit().await?;
            return Ok(Ok(Duration::ZERO));
        }

        transaction.commit().await?;
        Ok(Err(retry_after.max(Duration::from_millis(1))))
    }
}

/// Milliseconds from `earlier` to `now`, or zero if the clock went backwards.
///
/// `.max(0)` makes the difference non-negative before it is read as unsigned, so
/// there is no conversion here that can fail and no fallback to invent.
fn elapsed_millis(now_ms: i64, earlier: i64) -> u64 {
    now_ms.saturating_sub(earlier).max(0).unsigned_abs()
}

/// Pure refill calculation, separated from I/O so it can be tested against a
/// real clock.
fn refill(state: BudgetState, now_ms: i64) -> BudgetSnapshot {
    let capacity = state.capacity;
    let capacity_u64 = u64::from(capacity);
    // Both divisors below come straight from `rate_budget`, whose `capacity` and
    // `window_ms` columns are `CHECK (... > 0)`, and both are narrowed on the way
    // out by a helper that refuses a negative. Neither can be zero.
    let window_ms = DurationMs::from_std(state.window).as_millis();

    // Tokens accrue continuously: `capacity` per `window_ms`, pro-rated. The
    // elapsed span is clamped at zero and the product saturates, so a clock far
    // ahead of `refilled_at` cannot wrap into a negative accrual.
    let elapsed = elapsed_millis(now_ms, state.refilled_at);
    let accrued = elapsed.saturating_mul(capacity_u64) / window_ms;
    let refilled = u64::from(state.available)
        .saturating_add(accrued)
        .min(u64::from(capacity));
    // `refilled` is bounded by `capacity`, so this conversion cannot fail.
    let available = u32::try_from(refilled).unwrap_or(capacity);

    // The minimum interval can outlast the window: a budget with both set must
    // wait for whichever is longer.
    let min_interval_ms = DurationMs::from_std(state.min_interval).as_millis();
    // Never used means the gap has already elapsed, so nothing is owed. The
    // original code expressed that as `i64::MAX` and relied on the subtraction
    // wrapping to zero; saturating arithmetic states it directly.
    let since_last = state
        .last_used_at
        .map_or(u64::MAX, |last| elapsed_millis(now_ms, last));
    let interval_wait = min_interval_ms.saturating_sub(since_last);

    // A partly drained budget is usable at once. The window wait only applies
    // when nothing is left, and then only long enough for one unit to return.
    let window_wait = if available > 0 {
        0
    } else {
        window_ms / capacity_u64
    };

    BudgetSnapshot {
        capacity,
        available,
        retry_after: Duration::from_millis(interval_wait.max(window_wait)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOULSEEK: &str = "soulseek";

    /// A full SoulSeek-style budget that has never been used.
    fn soulseek_state(available: u32) -> BudgetState {
        BudgetState {
            capacity: 34,
            available,
            refilled_at: 0,
            window: Duration::from_millis(220_000),
            min_interval: Duration::from_millis(400),
            last_used_at: None,
        }
    }

    #[test]
    fn fresh_budget_is_full_and_unblocked() {
        let snapshot = refill(soulseek_state(34), 0);
        assert_eq!(snapshot.available, 34);
        assert_eq!(snapshot.retry_after, Duration::ZERO);
    }

    #[test]
    fn empty_budget_reports_a_wait() {
        let snapshot = refill(soulseek_state(0), 0);
        assert_eq!(snapshot.available, 0);
        assert!(snapshot.retry_after > Duration::ZERO);
    }

    #[test]
    fn available_never_exceeds_capacity_when_time_passes() {
        let snapshot = refill(soulseek_state(34), 10 * 60_000);
        assert_eq!(snapshot.available, 34);
    }

    #[test]
    fn usage_refills_over_time() {
        let drained = refill(
            BudgetState {
                capacity: 60,
                available: 0,
                refilled_at: 0,
                window: Duration::from_millis(60_000),
                min_interval: Duration::ZERO,
                last_used_at: None,
            },
            30_000,
        );
        assert_eq!(drained.available, 30);
    }

    #[test]
    fn minimum_interval_outlasts_the_window() {
        // MusicBrainz: one request per second, so even a full budget must wait.
        let snapshot = refill(
            BudgetState {
                capacity: 1,
                available: 1,
                refilled_at: 0,
                window: Duration::from_millis(1_000),
                min_interval: Duration::from_millis(1_100),
                last_used_at: Some(0),
            },
            200,
        );
        assert_eq!(snapshot.retry_after, Duration::from_millis(900));
    }

    #[test]
    fn a_clock_far_ahead_of_the_refill_refills_instead_of_wrapping() {
        // A saturated product would otherwise wrap negative and drain the
        // budget instead of filling it.
        let snapshot = refill(
            BudgetState {
                capacity: 34,
                available: 0,
                refilled_at: i64::MIN,
                window: Duration::from_millis(220_000),
                min_interval: Duration::ZERO,
                last_used_at: None,
            },
            i64::MAX,
        );
        assert_eq!(snapshot.available, 34);
    }

    #[tokio::test]
    async fn a_negative_allowance_cannot_be_stored_at_all() {
        let pool = test_pool().await;
        let store = BudgetStore::new(pool.clone());
        store
            .ensure(SOULSEEK, RateBudget::SOULSEEK_SEARCH, 0)
            .await
            .expect("registered");

        let refused = sqlx::query("UPDATE rate_budget SET available = -1 WHERE provider = ?")
            .bind(SOULSEEK)
            .execute(&pool)
            .await;

        assert!(
            refused.is_err(),
            "a negative allowance was stored and would read back as zero"
        );
        // The row still reads as the full budget it was created with.
        assert_eq!(
            store
                .snapshot(SOULSEEK, 0)
                .await
                .expect("snapshot")
                .available,
            34
        );
    }

    #[tokio::test]
    async fn acquire_consumes_a_unit() {
        let store = BudgetStore::new(test_pool().await);
        store
            .ensure(SOULSEEK, RateBudget::SOULSEEK_SEARCH, 0)
            .await
            .expect("budget registered");

        // The 400 ms minimum gap is part of the SoulSeek budget, so each
        // acquisition happens at a distinct instant.
        for (step, expected_remaining) in [0_i64, 400].into_iter().zip([33, 32]) {
            let now = step;
            let result = store.acquire(SOULSEEK, now).await.expect("acquire");
            assert_eq!(result, Ok(Duration::ZERO), "acquisition was refused");
            let snapshot = store.snapshot(SOULSEEK, now).await.expect("snapshot");
            assert_eq!(snapshot.available, expected_remaining);
        }
    }

    #[tokio::test]
    async fn the_minimum_interval_refuses_a_second_call_at_the_same_instant() {
        let store = BudgetStore::new(test_pool().await);
        store
            .ensure(SOULSEEK, RateBudget::SOULSEEK_SEARCH, 0)
            .await
            .expect("budget registered");

        assert_eq!(
            store.acquire(SOULSEEK, 0).await.expect("first"),
            Ok(Duration::ZERO)
        );

        // Budget was not exhausted; only the 400 ms gap refuses this.
        let second = store.acquire(SOULSEEK, 100).await.expect("second");
        assert_eq!(second, Err(Duration::from_millis(300)));
    }

    #[tokio::test]
    async fn acquire_refuses_and_reports_a_wait_once_drained() {
        let store = BudgetStore::new(test_pool().await);
        let budget = RateBudget {
            capacity: 2,
            window: Duration::from_secs(60),
            min_interval: Duration::ZERO,
        };
        store.ensure(SOULSEEK, budget, 0).await.expect("registered");

        assert_eq!(
            store.acquire(SOULSEEK, 0).await.expect("acquire"),
            Ok(Duration::ZERO)
        );
        assert_eq!(
            store.acquire(SOULSEEK, 0).await.expect("acquire"),
            Ok(Duration::ZERO)
        );

        let refused = store.acquire(SOULSEEK, 0).await.expect("acquire");
        assert!(refused.is_err(), "drained budget allowed a third request");
        assert!(refused.expect_err("expected refusal") > Duration::ZERO);
    }

    #[tokio::test]
    async fn budget_state_survives_a_new_store_instance() {
        let pool = test_pool().await;
        let first = BudgetStore::new(pool.clone());
        first
            .ensure(SOULSEEK, RateBudget::SOULSEEK_SEARCH, 0)
            .await
            .expect("registered");
        first
            .acquire(SOULSEEK, 0)
            .await
            .expect("acquire")
            .expect("granted");

        let after_restart = BudgetStore::new(pool);
        let snapshot = after_restart.snapshot(SOULSEEK, 0).await.expect("snapshot");
        assert_eq!(snapshot.available, 33, "restart reset the budget");
    }

    #[tokio::test]
    async fn ensure_is_idempotent() {
        let store = BudgetStore::new(test_pool().await);
        store
            .ensure(SOULSEEK, RateBudget::SOULSEEK_SEARCH, 0)
            .await
            .expect("registered");
        store
            .acquire(SOULSEEK, 0)
            .await
            .expect("acquire")
            .expect("granted");

        // Re-registering must not hand out a fresh allowance.
        store
            .ensure(SOULSEEK, RateBudget::SOULSEEK_SEARCH, 0)
            .await
            .expect("re-registered");

        let snapshot = store.snapshot(SOULSEEK, 0).await.expect("snapshot");
        assert_eq!(snapshot.available, 33);
    }

    #[tokio::test]
    async fn unknown_provider_is_reported_not_defaulted() {
        let store = BudgetStore::new(test_pool().await);
        assert!(matches!(
            store.snapshot("nope", 0).await,
            Err(StoreError::NotFound { .. })
        ));
    }

    async fn test_pool() -> SqlitePool {
        let store = crate::Store::open_temporary().await.expect("store opens");
        store.pool().clone()
    }
}
