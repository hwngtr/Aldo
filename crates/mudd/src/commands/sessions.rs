//! `mud sessions`: the searches this install has run, newest first.

use std::process::ExitCode;

use mud_store::repo::{LOCAL_USER, SessionStore};

use crate::settings::Settings;

/// How many past searches to show.
const LIMIT: u32 = 20;

pub async fn run(settings: &Settings) -> ExitCode {
    match list(settings).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("mud: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn list(settings: &Settings) -> Result<(), Box<dyn std::error::Error>> {
    let store = settings.open_store().await?;
    let sessions = SessionStore::new(store.pool().clone());
    let now = mud_store::now_ms();

    let rows = sessions.list(LOCAL_USER, LIMIT).await?;
    if rows.is_empty() {
        println!("no searches yet");
        return Ok(());
    }

    for row in rows {
        println!(
            "#{:<4} {:>10}  {:>4} found  {}",
            row.session_id,
            age(row.created_at, now),
            row.result_count,
            row.query.phrase
        );
    }

    Ok(())
}

/// How long ago a search ran, at a resolution worth reading.
fn age(created_at_ms: i64, now_ms: i64) -> String {
    let seconds = (now_ms - created_at_ms).max(0) / 1_000;
    match seconds {
        0..=59 => format!("{seconds}s ago"),
        60..=3_599 => format!("{}m ago", seconds / 60),
        3_600..=86_399 => format!("{}h ago", seconds / 3_600),
        _ => format!("{}d ago", seconds / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_700_000_000_000;

    fn ago(millis: i64) -> String {
        age(NOW - millis, NOW)
    }

    #[test]
    fn a_search_from_just_now_reads_in_seconds() {
        assert_eq!(ago(0), "0s ago");
        assert_eq!(ago(30_000), "30s ago");
    }

    #[test]
    fn a_search_from_minutes_hours_and_days_ago_reads_in_that_unit() {
        assert_eq!(ago(120_000), "2m ago");
        assert_eq!(ago(7_200_000), "2h ago");
        assert_eq!(ago(172_800_000), "2d ago");
    }

    #[test]
    fn a_clock_that_moved_backwards_does_not_print_a_negative_age() {
        // A stored timestamp in the future is a clock problem, not a search
        // from before the epoch.
        assert_eq!(age(NOW + 60_000, NOW), "0s ago");
    }
}
