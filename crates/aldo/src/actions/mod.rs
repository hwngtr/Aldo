//! What a command does, separated from how it is shown.
//!
//! The CLI and the TUI are two views of the same operations. Keeping the
//! orchestration here — the session row, the rate budget, the ranking, the
//! persistence — means the two front ends cannot drift: a search spends one
//! budget token whichever one asked, and the daily indexes the CLI's
//! `download N` resolves are the rows the TUI's result list wrote.

pub mod download;
pub mod reports;
pub mod search;
