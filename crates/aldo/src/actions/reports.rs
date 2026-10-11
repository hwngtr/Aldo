//! Small read-only reports the CLI and the TUI both show.

use std::time::Duration;

use aldo_core::ids::SessionId;
use aldo_store::RateBudget;
use aldo_store::repo::{BudgetStore, LOCAL_USER, SessionStore};

use crate::settings::{Settings, SettingsError};

/// Every budget the app maintains, in the order they are shown.
pub const BUDGETS: [(&str, RateBudget); 4] = [
    ("soulseek", RateBudget::SOULSEEK_SEARCH),
    ("discogs", RateBudget::DISCOGS_AUTHENTICATED),
    ("musicbrainz", RateBudget::MUSICBRAINZ),
    ("acoustid", RateBudget::ACOUSTID),
];

/// How many past searches to show.
pub const SESSION_LIMIT: u32 = 20;

/// One provider's remaining allowance.
#[derive(Debug, Clone, Copy)]
pub struct BudgetLine {
    pub provider: &'static str,
    pub available: u32,
    pub capacity: u32,
    /// The window the allowance refills over.
    pub window: Duration,
    /// Time until the next token, or zero when one is available now.
    pub retry_after: Duration,
}

impl BudgetLine {
    /// Whether the provider is refusing requests right now.
    #[must_use]
    pub fn is_limited(&self) -> bool {
        self.available == 0 && !self.retry_after.is_zero()
    }
}

/// Reads every budget, ensuring each row exists first.
///
/// # Errors
/// Fails when the store cannot be opened or a budget cannot be read.
pub async fn budgets(settings: &Settings) -> Result<Vec<BudgetLine>, ReportsError> {
    let store = settings.open_store().await?;
    let budgets = BudgetStore::new(store.pool().clone());
    let now = aldo_store::now_ms();

    let mut lines = Vec::with_capacity(BUDGETS.len());
    for (provider, budget) in BUDGETS {
        budgets.ensure(provider, budget, now).await?;
        let snapshot = budgets.snapshot(provider, now).await?;
        lines.push(BudgetLine {
            provider,
            available: snapshot.available,
            capacity: snapshot.capacity,
            window: budget.window,
            retry_after: snapshot.retry_after,
        });
    }
    Ok(lines)
}

/// One past search.
#[derive(Debug, Clone)]
pub struct SessionLine {
    pub session_id: SessionId,
    pub phrase: String,
    pub result_count: u32,
    pub created_at_ms: i64,
}

/// Lists recent searches, newest first.
///
/// # Errors
/// Fails when the store cannot be opened or the sessions cannot be read.
pub async fn sessions(settings: &Settings, limit: u32) -> Result<Vec<SessionLine>, ReportsError> {
    let store = settings.open_store().await?;
    let sessions = SessionStore::new(store.pool().clone());

    Ok(sessions
        .list(LOCAL_USER, limit)
        .await?
        .into_iter()
        .map(|row| SessionLine {
            session_id: row.session_id,
            phrase: row.query.phrase,
            result_count: row.result_count,
            created_at_ms: row.created_at,
        })
        .collect())
}

/// How long ago something happened, at a resolution worth reading.
#[must_use]
pub fn age(created_at_ms: i64, now_ms: i64) -> String {
    let seconds = (now_ms - created_at_ms).max(0) / 1_000;
    match seconds {
        0..=59 => format!("{seconds}s ago"),
        60..=3_599 => format!("{}m ago", seconds / 60),
        3_600..=86_399 => format!("{}h ago", seconds / 3_600),
        _ => format!("{}d ago", seconds / 86_400),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ReportsError {
    #[error(transparent)]
    Settings(#[from] SettingsError),

    #[error(transparent)]
    Store(#[from] aldo_store::StoreError),
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_700_000_000_000;

    #[test]
    fn every_provider_is_listed_once() {
        let mut seen: Vec<&str> = BUDGETS.iter().map(|(name, _)| *name).collect();
        let total = seen.len();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), total, "a provider is listed twice");
    }

    #[test]
    fn the_soulseek_budget_is_the_measured_ban_avoiding_one() {
        // If this drifts, searches start getting banned with no other symptom.
        let (_, soulseek) = BUDGETS[0];
        assert_eq!(soulseek.capacity, 34);
        assert_eq!(soulseek.window.as_secs(), 220);
    }

    #[test]
    fn a_search_from_just_now_reads_in_seconds() {
        assert_eq!(age(NOW, NOW), "0s ago");
        assert_eq!(age(NOW - 30_000, NOW), "30s ago");
    }

    #[test]
    fn a_search_from_minutes_hours_and_days_ago_reads_in_that_unit() {
        assert_eq!(age(NOW - 120_000, NOW), "2m ago");
        assert_eq!(age(NOW - 7_200_000, NOW), "2h ago");
        assert_eq!(age(NOW - 172_800_000, NOW), "2d ago");
    }

    #[test]
    fn a_clock_that_moved_backwards_does_not_print_a_negative_age() {
        // A stored timestamp in the future is a clock problem, not a search
        // from before the epoch.
        assert_eq!(age(NOW + 60_000, NOW), "0s ago");
    }
}
