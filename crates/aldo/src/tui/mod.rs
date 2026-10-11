//! The interactive interface.
//!
//! Terminal setup and the event loop live here. The state is in [`app`], the
//! drawing in [`ui`], and this module is the only place that knows both.
//!
//! Keys are read on a dedicated thread and posted to the same channel the
//! background tasks use, so the loop has one thing to wait on and no `select!`
//! over two runtime flavours.

mod app;
#[cfg(test)]
mod fixtures;
mod theme;
mod ui;
mod wordmark;

use std::io::{IsTerminal as _, stdout};
use std::process::ExitCode;
use std::time::Duration;

use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::cursor::{Hide, Show};
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyEventKind,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use tokio::sync::mpsc;

use crate::actions::search::{self, SearchPhase, SearchRun};
use crate::actions::{download, reports};
use crate::cli::TuiArgs;
use crate::credentials;
use crate::settings::Settings;

use app::{App, ConfigSummary, Confirm, Intent, Msg, Screen};

type Backend = CrosstermBackend<std::io::Stdout>;
type Term = Terminal<Backend>;

/// How often the spinner advances and transient messages expire.
const TICK: Duration = Duration::from_millis(100);

pub async fn run(settings: &mut Settings, args: &TuiArgs) -> ExitCode {
    if !stdout().is_terminal() {
        eprintln!(
            "aldo: the interactive interface needs a terminal; run `aldo search \"artist - album\"` instead"
        );
        return ExitCode::FAILURE;
    }

    let config = config_summary(settings);

    // A panic in raw mode leaves the terminal unusable, and the release profile
    // aborts, so the hook restores the screen before the process dies.
    install_panic_hook();

    let mut terminal = match setup() {
        Ok(terminal) => terminal,
        Err(error) => {
            eprintln!("aldo: could not take over the terminal: {error}");
            return ExitCode::FAILURE;
        }
    };
    let _guard = RestoreOnDrop;

    let outcome = event_loop(&mut terminal, settings, args, config).await;

    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("aldo: {error}");
            ExitCode::FAILURE
        }
    }
}

fn config_summary(settings: &Settings) -> ConfigSummary {
    let present =
        |value: &Option<String>| value.as_deref().is_some_and(|text| !text.trim().is_empty());

    ConfigSummary {
        library_root: settings.library_root.clone(),
        discogs: present(&settings.discogs_token),
        server: present(&settings.slsk_username) && present(&settings.slsk_password),
    }
}

async fn event_loop(
    terminal: &mut Term,
    settings: &mut Settings,
    args: &TuiArgs,
    config: ConfigSummary,
) -> std::io::Result<()> {
    let (messages, mut inbox) = mpsc::unbounded_channel::<Msg>();

    spawn_key_reader(messages.clone());

    let mut app = App::new(config, args.query.clone());
    // A query handed in on the command line waits behind the credential form.
    if app.screen != Screen::Setup
        && let Some(query) = app.take_pending_search()
    {
        dispatch(Intent::Search(query), settings, args.limit, &messages);
    }

    let mut ticker = tokio::time::interval(TICK);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        if app.needs_redraw() {
            terminal.draw(|frame| ui::draw(frame, &mut app))?;
        }

        tokio::select! {
            message = inbox.recv() => {
                let Some(message) = message else { break };

                // A finished setup save is the one message the event loop must
                // see before the app does: the settings and the config chips
                // belong to the loop, not to the pure state.
                let saved = match &message {
                    Msg::SetupSaved(result) => match &**result {
                        Ok(credentials) => Some(credentials.clone()),
                        Err(_) => None,
                    },
                    _ => None,
                };

                let intent = app.update(message);

                if let Some(credentials) = saved {
                    settings.apply_credentials(&credentials);
                    app.config = config_summary(settings);
                }

                let Some(intent) = intent else { continue };
                if matches!(intent, Intent::Quit) {
                    break;
                }
                dispatch(intent, settings, args.limit, &messages);
            }
            _ = ticker.tick() => app.on_tick(),
        }
    }

    Ok(())
}

/// Runs a requested operation off the UI thread and posts the result back.
fn dispatch(
    intent: Intent,
    settings: &Settings,
    limit: u32,
    messages: &mpsc::UnboundedSender<Msg>,
) {
    let settings = settings.clone();
    let messages = messages.clone();

    match intent {
        Intent::Quit => {}
        Intent::SaveCredentials(credentials) => {
            tokio::spawn(async move {
                let result = credentials::save(&credentials)
                    .map(|_| *credentials)
                    .map_err(|error| error.to_string());
                let _ = messages.send(Msg::SetupSaved(Box::new(result)));
            });
        }
        Intent::Search(query) => {
            tokio::spawn(async move {
                let result = run_search(&settings, &query, limit, messages.clone()).await;
                let _ = messages.send(Msg::Search(Box::new(result)));
            });
        }
        Intent::PrepareDownload(candidate_id) => {
            tokio::spawn(async move {
                let result = download::prepare(&settings, candidate_id, None)
                    .await
                    .map(|prepared| Confirm {
                        prepared,
                        library_root: settings.library_root.clone(),
                    })
                    .map_err(|error| error.to_string());
                let _ = messages.send(Msg::Prepared(Box::new(result)));
            });
        }
        Intent::ChooseRelease {
            candidate_id,
            release_id,
        } => {
            tokio::spawn(async move {
                let result = match download::choose_release(&settings, candidate_id, release_id)
                    .await
                {
                    Ok(()) => download::prepare(&settings, candidate_id, Some(release_id)).await,
                    Err(error) => Err(error),
                }
                .map(|prepared| Confirm {
                    prepared,
                    library_root: settings.library_root.clone(),
                })
                .map_err(|error| error.to_string());
                let _ = messages.send(Msg::Prepared(Box::new(result)));
            });
        }
        Intent::Download(prepared) => {
            tokio::spawn(async move {
                // The download reports in its own vocabulary; forward it into
                // the one channel the loop waits on.
                let (events, mut inbox) = mpsc::unbounded_channel::<download::DownloadEvent>();
                let forwarder = tokio::spawn({
                    let messages = messages.clone();
                    async move {
                        while let Some(event) = inbox.recv().await {
                            if messages.send(Msg::Download(event)).is_err() {
                                break;
                            }
                        }
                    }
                });

                let outcome = download::execute(
                    &settings,
                    &prepared.plan,
                    prepared.tags.as_ref(),
                    Some(&events),
                )
                .await
                .map(|_| ())
                .map_err(|error| error.to_string());

                drop(events);
                let _ = forwarder.await;
                let _ = messages.send(Msg::DownloadResult(outcome));
            });
        }
        Intent::ReloadBudgets => {
            tokio::spawn(async move {
                let result = reports::budgets(&settings)
                    .await
                    .map_err(|error| error.to_string());
                let _ = messages.send(Msg::Budgets(result));
            });
        }
        Intent::ReloadSessions => {
            tokio::spawn(async move {
                let result = reports::sessions(&settings, reports::SESSION_LIMIT)
                    .await
                    .map_err(|error| error.to_string());
                let _ = messages.send(Msg::Sessions(result));
            });
        }
    }
}

/// Runs one search, forwarding each provider phase to the header.
async fn run_search(
    settings: &Settings,
    query: &str,
    limit: u32,
    messages: mpsc::UnboundedSender<Msg>,
) -> Result<SearchRun, String> {
    let parsed = search::parse(query).map_err(|error| error.to_string())?;

    let (phases, mut phase_inbox) = mpsc::unbounded_channel::<SearchPhase>();
    let forwarder = tokio::spawn(async move {
        while let Some(phase) = phase_inbox.recv().await {
            if messages.send(Msg::Phase(phase)).is_err() {
                break;
            }
        }
    });

    let outcome = search::run(settings, &parsed, limit, Some(&phases))
        .await
        .map_err(|error| error.to_string());

    drop(phases);
    let _ = forwarder.await;
    outcome
}

/// Reads keys on a thread of its own, because `crossterm`'s reader blocks.
fn spawn_key_reader(messages: mpsc::UnboundedSender<Msg>) {
    std::thread::Builder::new()
        .name("aldo-tui-keys".to_owned())
        .spawn(move || {
            loop {
                match event::read() {
                    // Repeat and Press are keystrokes; Release would double
                    // every key on terminals that report it.
                    Ok(Event::Key(key)) if key.kind != KeyEventKind::Release => {
                        if messages.send(Msg::Key(key)).is_err() {
                            break;
                        }
                    }
                    Ok(Event::Paste(text)) => {
                        if messages.send(Msg::Paste(text)).is_err() {
                            break;
                        }
                    }
                    Ok(Event::Resize(..)) => {
                        if messages.send(Msg::Redraw).is_err() {
                            break;
                        }
                    }
                    Ok(_) => {}
                    // The terminal was closed under us.
                    Err(_) => break,
                }
            }
        })
        .ok();
}

fn setup() -> std::io::Result<Term> {
    enable_raw_mode()?;
    let mut out = stdout();
    execute!(out, EnterAlternateScreen, EnableBracketedPaste, Hide)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(out))?;
    terminal.clear()?;
    Ok(terminal)
}

/// Puts the terminal back, exactly once per path that took it over.
struct RestoreOnDrop;

impl Drop for RestoreOnDrop {
    fn drop(&mut self) {
        restore();
    }
}

fn restore() {
    let _ = disable_raw_mode();
    let _ = execute!(stdout(), DisableBracketedPaste, Show, LeaveAlternateScreen);
}

fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore();
        previous(info);
    }));
}
