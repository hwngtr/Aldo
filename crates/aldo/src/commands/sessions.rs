//! `aldo sessions`: the searches this install has run, newest first.

use std::process::ExitCode;

use crate::actions::reports::{ReportsError, SESSION_LIMIT, age, sessions};
use crate::settings::Settings;

pub async fn run(settings: &Settings) -> ExitCode {
    match list(settings).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("aldo: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn list(settings: &Settings) -> Result<(), ReportsError> {
    let rows = sessions(settings, SESSION_LIMIT).await?;
    if rows.is_empty() {
        crate::outln!("no searches yet");
        return Ok(());
    }

    let now = aldo_store::now_ms();
    for row in rows {
        crate::outln!(
            "#{:<4} {:>10}  {:>4} found  {}",
            row.session_id,
            age(row.created_at_ms, now),
            row.result_count,
            row.phrase
        );
    }

    Ok(())
}
