//! Command-line surface.
//!
//! Parsing is separated from execution so the argument rules can be tested
//! without opening a database.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};

use crate::commands;
use crate::settings::Settings;

#[derive(Debug, Parser)]
#[command(
    name = "aldo",
    about = "Search for lossless albums and tag them from Discogs",
    version
)]
pub struct Cli {
    #[command(flatten)]
    pub global: GlobalArgs,

    #[command(subcommand)]
    pub command: Command,
}

/// Options every command accepts, so they can be given before or after the
/// subcommand.
#[derive(Debug, Args)]
pub struct GlobalArgs {
    /// Where the database lives.
    #[arg(
        long,
        env = "ALDO_DATA_DIR",
        default_value_os_t = default_data_dir(),
        global = true
    )]
    pub data_dir: PathBuf,

    /// Where finished albums are written.
    #[arg(
        long,
        env = "ALDO_LIBRARY_ROOT",
        default_value_os_t = default_library_root(),
        global = true
    )]
    pub library_root: PathBuf,

    /// Discogs personal access token. Without one, searches that would use
    /// Discogs report it as not configured rather than failing.
    #[arg(long, env = "ALDO_DISCOGS_TOKEN", global = true)]
    pub discogs_token: Option<String>,

    /// SOCKS5 or HTTP proxy for the metadata sources. It does not affect
    /// SoulSeek, which owns its own socket.
    #[arg(long, env = "ALDO_PROXY", global = true)]
    pub proxy: Option<String>,

    /// SoulSeek account name. Defaults to the shared public account name.
    #[arg(
        long,
        env = "ALDO_SLSK_USERNAME",
        default_value = "aldouser",
        global = true
    )]
    pub slsk_username: Option<String>,

    /// SoulSeek password. Defaults to the shared public account password.
    #[arg(long, env = "ALDO_SLSK_PASSWORD", default_value = "123", global = true)]
    pub slsk_password: Option<String>,

    /// SoulSeek server, as `host:port`.
    #[arg(
        long,
        env = "ALDO_SLSK_SERVER",
        default_value = "server.slsknet.org:2416",
        global = true
    )]
    pub slsk_server: String,

    /// The port peers reach this client on. Forwarding it gives better results.
    #[arg(long, env = "ALDO_SLSK_PORT", default_value_t = 2234, global = true)]
    pub slsk_port: u16,

    /// A directory to share. Repeatable. Defaults to `~/Music`; set the
    /// environment variable to override it.
    #[arg(
        long = "slsk-share",
        env = "ALDO_SLSK_SHARED",
        default_values_os_t = default_shared_directories(),
        global = true
    )]
    pub slsk_shared: Vec<PathBuf>,

    /// How long a SoulSeek search collects responses, in seconds.
    #[arg(long, env = "ALDO_SLSK_TIMEOUT", default_value_t = 8, global = true)]
    pub slsk_timeout: u64,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Search for a release.
    Search(SearchArgs),
    /// Download a candidate found by search.
    Download(DownloadArgs),
    /// Show the remaining rate allowance for each provider.
    Budgets,
    /// Show recent searches, newest first.
    Sessions,
    /// Run the loopback HTTP daemon.
    #[cfg(feature = "api")]
    Serve(ServeArgs),
}

#[derive(Debug, Args)]
pub struct SearchArgs {
    /// The query. Quote it: `aldo search "radiohead - ok computer"`. An optional
    /// trailing year is understood, as in `"radiohead - ok computer - 1997"`.
    pub query: String,

    /// How many Discogs release bodies to fetch. Each one spends rate budget,
    /// and a release is only ever fetched once.
    #[arg(long, default_value_t = aldo_catalog::DEFAULT_RELEASE_LIMIT)]
    pub limit: u32,
}

#[derive(Debug, Args)]
pub struct DownloadArgs {
    /// The 1-based result number from the latest search today. Indexes reset at UTC midnight.
    pub result_index: i64,

    /// Override the Discogs release selected by the latest search.
    #[arg(long)]
    pub discogs_release: Option<u32>,
}

#[cfg(feature = "api")]
#[derive(Debug, Args)]
pub struct ServeArgs {
    /// Loopback port. Zero picks a free port.
    #[arg(long, env = "ALDO_PORT", default_value_t = 8137)]
    pub port: u16,
}

/// The user's data directory.
///
/// `~` is not expanded by the shell here, so it is resolved from the
/// environment rather than written literally into a path.
fn default_data_dir() -> PathBuf {
    if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
        return PathBuf::from(xdg).join("aldo");
    }

    std::env::var_os("HOME").map_or_else(
        || PathBuf::from("aldo"),
        |home| PathBuf::from(home).join(".local/share/aldo"),
    )
}

fn default_shared_directories() -> Vec<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| vec![home.join("Music")])
        .unwrap_or_default()
}

/// Use the current directory as the default library root, but resolve it now.
/// Store setup requires an existing absolute path, while Clap's literal `.`
/// default fails that check on every first run.
fn default_library_root() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"))
}

/// Dispatches a parsed command.
pub async fn run(cli: Cli) -> ExitCode {
    let settings = Settings::from(&cli.global);

    match cli.command {
        Command::Search(args) => commands::search::run(&settings, &args).await,
        Command::Download(args) => commands::download::run(&settings, &args).await,
        Command::Budgets => commands::budgets::run(&settings).await,
        Command::Sessions => commands::sessions::run(&settings).await,
        #[cfg(feature = "api")]
        Command::Serve(args) => crate::serve::run(&settings, &args).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use clap::CommandFactory as _;

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(args).expect("arguments parse")
    }

    fn search_args(cli: Cli) -> SearchArgs {
        match cli.command {
            Command::Search(args) => args,
            other => panic!("expected a search command, got {other:?}"),
        }
    }

    #[test]
    fn the_command_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn a_quoted_query_is_taken_whole() {
        let args = search_args(parse(&["aldo", "search", "radiohead - ok computer"]));
        assert_eq!(args.query, "radiohead - ok computer");
    }

    #[test]
    fn global_options_may_follow_the_subcommand() {
        let cli = parse(&["aldo", "search", "a - b", "--library-root", "/music"]);
        assert_eq!(cli.global.library_root, PathBuf::from("/music"));
    }

    #[test]
    fn global_options_may_precede_the_subcommand() {
        let cli = parse(&["aldo", "--library-root", "/music", "search", "a - b"]);
        assert_eq!(cli.global.library_root, PathBuf::from("/music"));
    }

    #[test]
    fn the_release_limit_defaults_to_the_catalog_default() {
        let args = search_args(parse(&["aldo", "search", "a - b"]));
        assert_eq!(args.limit, aldo_catalog::DEFAULT_RELEASE_LIMIT);
    }

    #[test]
    fn a_search_without_a_query_is_refused() {
        assert!(Cli::try_parse_from(["aldo", "search"]).is_err());
    }

    #[test]
    fn the_data_dir_is_absolute_after_defaulting() {
        // A relative default would scatter the database wherever the command
        // happens to be run from.
        let dir = default_data_dir();
        assert!(
            dir.is_absolute() || std::env::var_os("HOME").is_none(),
            "{dir:?} is not absolute"
        );
    }

    #[test]
    fn the_default_library_root_is_absolute() {
        assert!(default_library_root().is_absolute());
    }

    #[test]
    fn budgets_and_sessions_take_no_arguments() {
        assert!(matches!(
            parse(&["aldo", "budgets"]).command,
            Command::Budgets
        ));
        assert!(matches!(
            parse(&["aldo", "sessions"]).command,
            Command::Sessions
        ));
    }

    #[cfg(feature = "api")]
    #[test]
    fn serve_defaults_to_a_loopback_port() {
        let cli = parse(&["aldo", "serve"]);
        match cli.command {
            Command::Serve(args) => assert_eq!(args.port, 8137),
            other => panic!("expected serve, got {other:?}"),
        }
    }

    #[cfg(feature = "api")]
    #[test]
    fn no_flag_exists_that_could_bind_a_routable_address() {
        // The daemon holds a Discogs token, so there is deliberately no
        // `--host` or `--bind`.
        assert!(Cli::try_parse_from(["aldo", "serve", "--host", "0.0.0.0"]).is_err());
    }

    #[test]
    fn a_download_command_parses_its_daily_result_index() {
        let cli = parse(&["aldo", "download", "42"]);
        match cli.command {
            Command::Download(args) => assert_eq!(args.result_index, 42),
            other => panic!("expected download, got {other:?}"),
        }
    }

    #[test]
    fn a_download_without_an_id_is_refused() {
        assert!(Cli::try_parse_from(["aldo", "download"]).is_err());
    }
}
