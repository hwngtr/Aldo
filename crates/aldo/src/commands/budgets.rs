//! `aldo budgets`: what is left of each provider's rate allowance.
//!
//! The SoulSeek figure is the one that matters. Exceeding it is a thirty-minute
//! ban, and the failure looks like a search that returns nothing.

use std::process::ExitCode;

use crate::actions::reports::{BudgetLine, ReportsError, budgets};
use crate::settings::Settings;

pub async fn run(settings: &Settings) -> ExitCode {
    match show(settings).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("aldo: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn show(settings: &Settings) -> Result<(), ReportsError> {
    for line in budgets(settings).await? {
        crate::outln!("{}", render(&line));
    }
    Ok(())
}

/// One line: `soulseek     34 / 34   34 per 220s  (wait 12s)`.
fn render(line: &BudgetLine) -> String {
    let wait = if line.retry_after.is_zero() {
        String::new()
    } else {
        format!("  (wait {:?})", line.retry_after)
    };

    format!(
        "{provider:<12} {available:>3} / {capacity:<3}  {capacity} per {window}s{wait}",
        provider = line.provider,
        available = line.available,
        capacity = line.capacity,
        window = line.window.as_secs(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::time::Duration;

    #[test]
    fn a_budget_line_places_the_figures_under_their_headers() {
        let rendered = render(&BudgetLine {
            provider: "discogs",
            available: 7,
            capacity: 60,
            window: Duration::from_secs(60),
            retry_after: Duration::ZERO,
        });
        assert_eq!(rendered, "discogs        7 / 60   60 per 60s");
    }

    #[test]
    fn a_limited_budget_says_how_long_to_wait() {
        let rendered = render(&BudgetLine {
            provider: "soulseek",
            available: 0,
            capacity: 34,
            window: Duration::from_secs(220),
            retry_after: Duration::from_secs(12),
        });
        assert!(rendered.contains("(wait 12s)"), "{rendered}");
    }
}
