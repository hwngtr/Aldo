//! Command-line surface.
//!
//! Parsing is separated from execution so the argument rules can be tested
//! without opening a database.

use std::io::IsTerminal as _;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, CommandFactory as _, Parser, Subcommand};

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

    /// A missing subcommand is not an error: it opens the interface when there
    /// is a terminal, and prints this help when there is not.
    #[command(subcommand)]
    pub command: Option<Command>,
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

    /// SoulSeek account name. Absent until `aldo login` or the first-run setup
    /// saves one.
    #[arg(long, env = "ALDO_SLSK_USERNAME", global = true)]
    pub slsk_username: Option<String>,

    /// SoulSeek password. Absent until `aldo login` or the first-run setup
    /// saves one.
    #[arg(long, env = "ALDO_SLSK_PASSWORD", global = true)]
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
    /// Open the interactive interface.
    Tui(TuiArgs),
    /// Save credentials for Discogs and SoulSeek, so later runs need no flags.
    ///
    /// With no flags at a terminal it asks for everything. With any flag it
    /// changes only what is named, so `aldo login --token TOKEN` adds a Discogs
    /// token later without retyping the SoulSeek account.
    Login(LoginArgs),
    /// Forget the saved credentials, so the next run asks again.
    Logout,
    /// Show which credentials a run would use, and whether they come from the
    /// environment or the saved file. Secrets are never printed.
    Whoami,
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
pub struct TuiArgs {
    /// Pre-fill the search box with this query. Without it the box starts empty.
    #[arg(short, long)]
    pub query: Option<String>,

    /// How many Discogs release bodies to fetch per search. Each one spends rate
    /// budget, and a release is only ever fetched once.
    #[arg(long, default_value_t = aldo_catalog::DEFAULT_RELEASE_LIMIT)]
    pub limit: u32,
}

impl TuiArgs {
    /// The arguments a bare `aldo` uses.
    #[must_use]
    pub fn interactive() -> Self {
        Self {
            query: None,
            limit: aldo_catalog::DEFAULT_RELEASE_LIMIT,
        }
    }
}

#[derive(Debug, Args)]
pub struct LoginArgs {
    /// SoulSeek account name. Prompted for when omitted.
    #[arg(long)]
    pub username: Option<String>,

    /// SoulSeek password. Prompted for, without echo, when omitted.
    #[arg(long)]
    pub password: Option<String>,

    /// Discogs personal access token. Give it alone to add a token to an
    /// existing account: `aldo login --token TOKEN`.
    ///
    /// Deliberately `--token`, not `--discogs-token`: the latter is the global
    /// option, which reads the environment, and a value from there must not look
    /// like a deliberate update.
    #[arg(long)]
    pub token: Option<String>,
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

    /// List the files in the selected server folder without downloading.
    #[arg(long)]
    pub list: bool,

    /// Download without Discogs matching or metadata tagging.
    #[arg(long)]
    pub force: bool,
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

/// The user's music folder.
///
/// The same default the README documents. It is created on first use when it is
/// the default, so a new install does not have to mkdir it first, while a path
/// the user typed is still required to exist: a typo must fail loudly rather
/// than scatter an album into a new directory.
pub(crate) fn default_library_root() -> PathBuf {
    std::env::var_os("HOME").map_or_else(
        || std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")),
        |home| PathBuf::from(home).join("Music"),
    )
}

/// Dispatches a parsed command.
pub async fn run(cli: Cli) -> ExitCode {
    let mut settings = Settings::from(&cli.global);

    let Some(command) = cli.command else {
        return bare_invocation(&mut settings).await;
    };

    match command {
        Command::Tui(args) => crate::tui::run(&mut settings, &args).await,
        Command::Login(args) => commands::login::run(&settings, &args),
        Command::Logout => commands::logout::run(),
        Command::Whoami => commands::whoami::run(&settings),
        Command::Search(args) => commands::search::run(&settings, &args).await,
        Command::Download(args) => commands::download::run(&settings, &args).await,
        Command::Budgets => commands::budgets::run(&settings).await,
        Command::Sessions => commands::sessions::run(&settings).await,
        #[cfg(feature = "api")]
        Command::Serve(args) => crate::serve::run(&settings, &args).await,
    }
}

/// A bare `aldo`.
///
/// With a terminal, that is the interface, because it is the fastest way in.
/// Without one there is nothing to draw on, so the help is printed and the exit
/// code says the invocation was incomplete, which is what a script expects.
async fn bare_invocation(settings: &mut Settings) -> ExitCode {
    if !std::io::stdout().is_terminal() {
        let mut command = Cli::command();
        let _ = command.print_help();
        crate::outln!();
        return ExitCode::from(2);
    }

    crate::tui::run(settings, &TuiArgs::interactive()).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(args).expect("arguments parse")
    }

    fn search_args(cli: Cli) -> SearchArgs {
        match command(cli) {
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
            command(parse(&["aldo", "budgets"])),
            Command::Budgets
        ));
        assert!(matches!(
            command(parse(&["aldo", "sessions"])),
            Command::Sessions
        ));
    }

    #[test]
    fn a_bare_invocation_parses_as_no_subcommand() {
        // `run` decides what that means: the interface on a terminal, the help
        // off one. Parsing must not reject it first.
        assert!(parse(&["aldo"]).command.is_none());
    }

    fn command(cli: Cli) -> Command {
        cli.command.expect("a subcommand was given")
    }

    fn tui_args(cli: Cli) -> TuiArgs {
        match command(cli) {
            Command::Tui(args) => args,
            other => panic!("expected a tui command, got {other:?}"),
        }
    }

    #[test]
    fn the_tui_starts_with_an_empty_box_and_the_catalog_limit() {
        let args = tui_args(parse(&["aldo", "tui"]));
        assert!(args.query.is_none());
        assert_eq!(args.limit, aldo_catalog::DEFAULT_RELEASE_LIMIT);
    }

    #[test]
    fn the_tui_can_start_with_a_prefilled_query() {
        let args = tui_args(parse(&["aldo", "tui", "-q", "radiohead - ok computer"]));
        assert_eq!(args.query.as_deref(), Some("radiohead - ok computer"));
    }

    #[cfg(feature = "api")]
    #[test]
    fn serve_defaults_to_a_loopback_port() {
        let cli = parse(&["aldo", "serve"]);
        match command(cli) {
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
    fn logout_takes_no_arguments() {
        assert!(matches!(
            command(parse(&["aldo", "logout"])),
            Command::Logout
        ));
    }

    #[test]
    fn whoami_takes_no_arguments() {
        assert!(matches!(
            command(parse(&["aldo", "whoami"])),
            Command::Whoami
        ));
    }

    #[test]
    fn login_parses_its_optional_flags() {
        let cli = parse(&["aldo", "login"]);
        match command(cli) {
            Command::Login(args) => {
                assert!(args.username.is_none());
                assert!(args.password.is_none());
                assert!(args.token.is_none());
            }
            other => panic!("expected login, got {other:?}"),
        }

        let cli = parse(&[
            "aldo",
            "login",
            "--username",
            "alice",
            "--password",
            "secret",
            "--token",
            "tok",
        ]);
        match command(cli) {
            Command::Login(args) => {
                assert_eq!(args.username.as_deref(), Some("alice"));
                assert_eq!(args.password.as_deref(), Some("secret"));
                assert_eq!(args.token.as_deref(), Some("tok"));
            }
            other => panic!("expected login, got {other:?}"),
        }
    }

    #[test]
    fn soulseek_credentials_have_no_default_so_setup_can_be_detected() {
        // A default would make SoulSeek look configured before anyone signed in.
        if std::env::var_os("ALDO_SLSK_USERNAME").is_some()
            || std::env::var_os("ALDO_SLSK_PASSWORD").is_some()
        {
            return;
        }
        let cli = parse(&["aldo", "search", "a - b"]);
        assert!(cli.global.slsk_username.is_none());
        assert!(cli.global.slsk_password.is_none());
    }

    #[test]
    fn a_download_command_parses_its_daily_result_index() {
        let cli = parse(&["aldo", "download", "42"]);
        match command(cli) {
            Command::Download(args) => {
                assert_eq!(args.result_index, 42);
                assert!(!args.list);
                assert!(!args.force);
            }
            other => panic!("expected download, got {other:?}"),
        }
    }

    #[test]
    fn a_download_command_accepts_list_and_force() {
        let cli = parse(&["aldo", "download", "42", "--list"]);
        match command(cli) {
            Command::Download(args) => assert!(args.list),
            other => panic!("expected download, got {other:?}"),
        }

        let cli = parse(&["aldo", "download", "42", "--force"]);
        match command(cli) {
            Command::Download(args) => assert!(args.force),
            other => panic!("expected download, got {other:?}"),
        }
    }

    #[test]
    fn a_download_without_an_id_is_refused() {
        assert!(Cli::try_parse_from(["aldo", "download"]).is_err());
    }
}
