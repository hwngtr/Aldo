//! `aldo download`: fetch the files a search result points at, then print it.
//!
//! The operation itself lives in [`crate::actions::download`], because the TUI
//! runs the same download and must not diverge from it. This module only turns
//! the events it emits into terminal output.

use std::io::Write as _;
use std::process::ExitCode;

use aldo_core::audio::ByteSize;
use tokio::sync::mpsc;

use crate::actions::download::{self, DownloadError, DownloadEvent, DownloadPlan, DownloadReport};
use crate::cli::DownloadArgs;
use crate::format::speed;
use crate::settings::Settings;

pub async fn run(settings: &Settings, args: &DownloadArgs) -> ExitCode {
    match download(settings, args).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("aldo: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn download(settings: &Settings, args: &DownloadArgs) -> Result<(), DownloadError> {
    let candidate_id = download::resolve_daily_index(settings, args.result_index).await?;
    let plan = download::plan(settings, candidate_id).await?;

    if args.list {
        print_file_list(args.result_index, &plan);
        return Ok(());
    }

    let tags = download::metadata(settings, &plan, args.discogs_release, args.force).await?;
    if tags.is_none() {
        crate::outln!("No matching Discogs release was stored; downloading without metadata.");
    }
    print_header(args.result_index, &plan);

    // Progress is printed from a task so the transfer task is never blocked on
    // stdout, which a slow terminal can make surprisingly slow.
    let (events, mut inbox) = mpsc::unbounded_channel();
    let printer = tokio::spawn(async move {
        while let Some(event) = inbox.recv().await {
            print_event(&event);
        }
    });

    let outcome = download::execute(settings, &plan, tags.as_ref(), Some(&events)).await;

    drop(events);
    let _ = printer.await;
    outcome.map(|_| ())
}

fn print_header(id: i64, plan: &DownloadPlan) {
    crate::outln!(
        "Candidate #{id}: {} - {} ({} files, {}) from peer '{}'",
        crate::out::terminal_safe(or_unknown(&plan.artist, "Unknown Artist")),
        crate::out::terminal_safe(or_unknown(&plan.album, "Unknown Album")),
        plan.files.len(),
        plan.total_bytes,
        crate::out::terminal_safe(plan.peer().unwrap_or("unknown"))
    );
}

fn print_file_list(result_index: i64, plan: &DownloadPlan) {
    crate::outln!(
        "Candidate #{}: {} files from peer '{}'",
        result_index,
        plan.files.len(),
        crate::out::terminal_safe(plan.peer().unwrap_or("unknown"))
    );
    for (index, file) in plan.files.iter().enumerate() {
        crate::outln!(
            "  {:>2}. {} ({})",
            index + 1,
            crate::out::terminal_safe(&file.path),
            file.size
        );
    }
}

fn print_event(event: &DownloadEvent) {
    match event {
        DownloadEvent::Connecting { username } => {
            crate::outln!(
                "Connecting to server as {}...",
                crate::out::terminal_safe(username)
            );
        }
        DownloadEvent::FileSkipped { index, total, name } => {
            crate::outln!(
                "  [{index}/{total}] {} (already complete, skipping)",
                crate::out::terminal_safe(name)
            );
        }
        DownloadEvent::FileStarted {
            index,
            total,
            name,
            size,
        } => {
            crate::outln!(
                "  [{index}/{total}] {} ({size})...",
                crate::out::terminal_safe(name)
            );
        }
        DownloadEvent::Progress {
            bytes_done,
            bytes_total,
            speed_bps,
            queued,
            ..
        } => {
            if *queued {
                crate::out!("\r    waiting in peer queue...                 ");
            } else {
                let done = (*bytes_done).min(*bytes_total);
                let percent = percentage(done, *bytes_total);
                crate::out!(
                    "\r    {} {percent:5.1}% ({}/{}) @ {}       ",
                    progress_bar(done, *bytes_total),
                    ByteSize::new(*bytes_done),
                    ByteSize::new(*bytes_total),
                    speed(*speed_bps),
                );
            }
            let _ = std::io::stdout().flush();
        }
        DownloadEvent::FileDone { .. } => {
            crate::outln!("\r    100% completed.                           ");
        }
        DownloadEvent::Finished(report) => print_finished(report),
    }
}

fn print_finished(report: &DownloadReport) {
    crate::outln!(
        "\nDownload complete: {} files saved to {}{}",
        report.files,
        report.target_dir.display(),
        if report.tagged {
            " (Discogs tags written)"
        } else {
            ""
        }
    );
}

fn or_unknown<'a>(value: &'a str, fallback: &'a str) -> &'a str {
    if value.is_empty() { fallback } else { value }
}

fn percentage(done: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        (done as f64 / total as f64) * 100.0
    }
}

/// A compact inline bar for the active file. The terminal line updates in
/// place, so a multi-file download stays readable without drawing a UI.
fn progress_bar(completed: u64, total: u64) -> String {
    const WIDTH: usize = 20;
    let filled = if total == 0 {
        0
    } else {
        let cells = (u128::from(completed.min(total)) * WIDTH as u128) / u128::from(total);
        usize::try_from(cells).unwrap_or(WIDTH)
    };

    format!("[{}{}]", "=".repeat(filled), "-".repeat(WIDTH - filled))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_progress_bar_is_full_at_the_end_and_empty_at_the_start() {
        assert_eq!(progress_bar(0, 100), format!("[{}]", "-".repeat(20)));
        assert_eq!(progress_bar(100, 100), format!("[{}]", "=".repeat(20)));
        assert_eq!(
            progress_bar(50, 100),
            format!("[{}{}]", "=".repeat(10), "-".repeat(10))
        );
    }

    #[test]
    fn a_bar_for_an_unknown_total_is_not_a_division_by_zero() {
        assert_eq!(progress_bar(5, 0), format!("[{}]", "-".repeat(20)));
        assert!(percentage(5, 0).abs() < f64::EPSILON);
    }
}
