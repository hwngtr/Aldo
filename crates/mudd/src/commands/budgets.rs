//! `mud budgets`: what is left of each provider's rate allowance.
//!
//! The SoulSeek figure is the one that matters. Exceeding it is a thirty-minute
//! ban, and the failure looks like a search that returns nothing.

use std::process::ExitCode;

use mud_store::RateBudget;
use mud_store::repo::BudgetStore;

use crate::settings::Settings;

/// Every budget the app maintains, in the order they are shown.
const BUDGETS: [(&str, RateBudget); 4] = [
    ("soulseek", RateBudget::SOULSEEK_SEARCH),
    ("discogs", RateBudget::DISCOGS_AUTHENTICATED),
    ("musicbrainz", RateBudget::MUSICBRAINZ),
    ("acoustid", RateBudget::ACOUSTID),
];

pub async fn run(settings: &Settings) -> ExitCode {
    match show(settings).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("mud: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn show(settings: &Settings) -> Result<(), Box<dyn std::error::Error>> {
    let store = settings.open_store().await?;
    let budgets = BudgetStore::new(store.pool().clone());
    let now = mud_store::now_ms();

    for (name, budget) in BUDGETS {
        budgets.ensure(name, budget, now).await?;
        let snapshot = budgets.snapshot(name, now).await?;

        let window = budget.window.as_secs();
        let wait = if snapshot.retry_after.is_zero() {
            String::new()
        } else {
            format!("  (wait {:?})", snapshot.retry_after)
        };

        println!(
            "{name:<12} {:>3} / {:<3}  {capacity} per {window}s{wait}",
            snapshot.available,
            snapshot.capacity,
            capacity = snapshot.capacity,
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
