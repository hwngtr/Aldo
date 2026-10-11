//! `aldo login`: save credentials, so later runs need no flags.
//!
//! Two shapes of call:
//!
//! * No values given, at a terminal: a full sign-in. Every value is asked for,
//!   so the SoulSeek account can be changed.
//! * Any value given: a partial update. Only the values given are changed;
//!   anything already configured is left alone. This is how a Discogs token is
//!   added later, without retyping the SoulSeek account:
//!
//!   ```sh
//!   aldo login --token TOKEN
//!   ```
//!
//! The token may also arrive through the global `--discogs-token` option, which
//! carries the environment too. A value from there counts as an explicit request
//! only when it differs from the one already saved, so the environment cannot
//! masquerade as a deliberate update.
//!
//! Passwords and the token are read without echo, and the terminal is put back
//! on every path that takes it over, including a Ctrl-C.

use std::io::{IsTerminal as _, Write as _};
use std::process::ExitCode;

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};

use crate::cli::LoginArgs;
use crate::credentials::{self, Credentials};
use crate::settings::Settings;

pub fn run(settings: &Settings, args: &LoginArgs) -> ExitCode {
    match login(settings, args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("aldo: {error}");
            ExitCode::FAILURE
        }
    }
}

fn login(settings: &Settings, args: &LoginArgs) -> Result<(), LoginError> {
    let interactive = std::io::stdin().is_terminal();

    // The token can arrive through this command's `--token` or the global
    // `--discogs-token`. The latter is populated from the environment as well,
    // so it is an explicit request only when it changes what is saved.
    let requested_token = args
        .token
        .clone()
        .or_else(|| settings.discogs_token.clone());
    let saved = credentials::load();
    let token_is_new = requested_token.is_some() && requested_token != saved.discogs_token;

    let explicit =
        args.username.is_some() || args.password.is_some() || args.token.is_some() || token_is_new;

    let credentials = if interactive && !explicit {
        full_sign_in(requested_token)?
    } else {
        // Do not rewrite a token that only the environment asked for unchanged.
        let token = if args.token.is_some() || token_is_new {
            requested_token
        } else {
            None
        };
        partial_update(args, token, interactive)?
    };

    if credentials.slsk_username().is_none()
        && credentials.slsk_password().is_none()
        && credentials.discogs_token().is_none()
    {
        return Err(LoginError::NothingToDo);
    }

    let path = credentials::save(&credentials)?;
    crate::outln!("Saved credentials to {}.", path.display());

    if !configured(credentials::DISCOGS_TOKEN) && credentials.discogs_token().is_none() {
        crate::outln!(
            "No Discogs token saved; downloads are untagged. Add one later with \
             `aldo login --token TOKEN`."
        );
    }

    Ok(())
}

/// Asks for everything, so a misconfigured account can be replaced.
///
/// A token that is already known is kept rather than asked for again; pass
/// `--token` to replace it.
fn full_sign_in(requested_token: Option<String>) -> Result<Credentials, LoginError> {
    let username = require(prompt_line("SoulSeek username: ")?, "a SoulSeek username")?;
    let password = require(prompt_secret("SoulSeek password: ")?, "a SoulSeek password")?;

    let token = if let Some(token) = requested_token {
        Some(token)
    } else {
        let answer = prompt_secret("Discogs token (blank to skip): ")?;
        non_blank(Some(answer.as_str())).map(str::to_owned)
    };

    Ok(Credentials {
        slsk_username: Some(username),
        slsk_password: Some(password),
        discogs_token: token,
    })
}

/// Changes only what was named or is missing, keeping everything else.
fn partial_update(
    args: &LoginArgs,
    token: Option<String>,
    interactive: bool,
) -> Result<Credentials, LoginError> {
    let username = resolve(
        args.username.as_deref(),
        credentials::SLSK_USERNAME,
        "a SoulSeek username",
        interactive,
        || prompt_line("SoulSeek username: "),
    )?;
    let password = resolve(
        args.password.as_deref(),
        credentials::SLSK_PASSWORD,
        "a SoulSeek password",
        interactive,
        || prompt_secret("SoulSeek password: "),
    )?;

    Ok(Credentials {
        slsk_username: username,
        slsk_password: password,
        discogs_token: token,
    })
}

/// Takes a value from the flag, the saved configuration, or a prompt.
///
/// `Ok(None)` means "already configured, leave it alone"; an absent and
/// unconfigured value with no terminal to ask is an error, not a silent skip.
fn resolve(
    flag: Option<&str>,
    key: &str,
    what: &'static str,
    interactive: bool,
    prompt: impl FnOnce() -> Result<String, LoginError>,
) -> Result<Option<String>, LoginError> {
    if let Some(value) = non_blank(flag) {
        return Ok(Some(value.to_owned()));
    }
    if configured(key) {
        return Ok(None);
    }
    if !interactive {
        return Err(LoginError::Missing { what });
    }
    Ok(Some(require(prompt()?, what)?))
}

/// Whether a value is already present in the environment, which includes the
/// saved file because `main` loads it before parsing.
fn configured(key: &str) -> bool {
    non_blank(std::env::var(key).ok().as_deref()).is_some()
}

/// Rejects an empty answer to a required question.
fn require(value: String, what: &'static str) -> Result<String, LoginError> {
    if value.is_empty() {
        Err(LoginError::Blank { what })
    } else {
        Ok(value)
    }
}

/// Reads a visible line from the terminal.
fn prompt_line(prompt: &str) -> Result<String, LoginError> {
    crate::out!("{prompt}");
    let _ = std::io::stdout().flush();

    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(line.trim().to_owned())
}

/// Reads a line with the characters hidden, ending at Enter. A paste arrives as
/// one `Paste` event and is inserted whole.
fn prompt_secret(prompt: &str) -> Result<String, LoginError> {
    crate::out!("{prompt}");
    let _ = std::io::stdout().flush();

    let guard = RawMode::enable()?;
    let mut text = String::new();
    loop {
        match event::read()? {
            Event::Key(key) if key.kind != KeyEventKind::Release => match key.code {
                KeyCode::Enter => break,
                KeyCode::Backspace => {
                    text.pop();
                }
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    drop(guard);
                    crate::out!("\r\n");
                    return Err(LoginError::Cancelled);
                }
                KeyCode::Char(ch) => text.push(ch),
                _ => {}
            },
            // A newline in a paste must not end the secret early.
            Event::Paste(pasted) => {
                text.extend(pasted.chars().filter(|ch| *ch != '\n' && *ch != '\r'));
            }
            _ => {}
        }
    }
    drop(guard);

    // Raw mode turns off output post-processing, so a bare `\n` would leave the
    // caret mid-line.
    crate::out!("\r\n");
    Ok(text)
}

/// Restores the terminal however the prompt ends.
struct RawMode;

impl RawMode {
    fn enable() -> Result<Self, LoginError> {
        ratatui::crossterm::terminal::enable_raw_mode()?;
        Ok(Self)
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        let _ = ratatui::crossterm::terminal::disable_raw_mode();
    }
}

fn non_blank(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|text| !text.is_empty())
}

#[derive(Debug, thiserror::Error)]
pub enum LoginError {
    #[error("{what} is required; pass it as a flag or run in a terminal")]
    Missing { what: &'static str },

    #[error("{what} cannot be blank")]
    Blank { what: &'static str },

    #[error("nothing to save: no change was requested")]
    NothingToDo,

    #[error("login cancelled")]
    Cancelled,

    #[error(transparent)]
    Credentials(#[from] credentials::CredentialsError),

    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A key no test or user sets, so `configured` is reliably false.
    const UNSET: &str = "ALDO_TEST_CREDENTIAL_NOT_SET";

    #[test]
    fn a_flag_is_used_and_a_configured_value_is_left_alone() {
        let value = resolve(Some("alice"), UNSET, "a SoulSeek username", false, || {
            unreachable!("a flag was given")
        })
        .expect("a flag resolves");
        assert_eq!(value.as_deref(), Some("alice"));
    }

    #[test]
    fn a_missing_value_without_a_terminal_is_an_error() {
        let error = resolve(None, UNSET, "a SoulSeek password", false, || {
            unreachable!("there is no terminal")
        })
        .expect_err("refused");
        assert!(matches!(error, LoginError::Missing { .. }));
    }

    #[test]
    fn a_blank_answer_to_a_prompt_is_refused() {
        let error = resolve(None, UNSET, "a SoulSeek username", true, || {
            Ok(String::new())
        })
        .expect_err("refused");
        assert!(matches!(error, LoginError::Blank { .. }));
    }

    #[test]
    fn a_prompted_value_is_taken() {
        let value = resolve(None, UNSET, "a SoulSeek username", true, || {
            Ok("alice".to_owned())
        })
        .expect("prompted");
        assert_eq!(value.as_deref(), Some("alice"));
    }

    #[test]
    fn blank_values_are_treated_as_absent() {
        assert_eq!(non_blank(None), None);
        assert_eq!(non_blank(Some("  ")), None);
        assert_eq!(non_blank(Some(" tok ")), Some("tok"));
    }
}
