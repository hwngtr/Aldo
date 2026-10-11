//! `aldo whoami`: what credentials a run would actually use, and where from.
//!
//! It never prints a secret, only whether one is set. The point is to make the
//! file-versus-environment precedence visible, which is the usual cause of a
//! login that "does not match".

use std::process::ExitCode;

use crate::credentials;
use crate::settings::Settings;

pub fn run(settings: &Settings) -> ExitCode {
    let saved = credentials::load();

    crate::outln!(
        "SoulSeek user:  {}  ({})",
        value(settings.slsk_username.as_deref()),
        source(settings.slsk_username.as_deref(), saved.slsk_username())
    );
    crate::outln!(
        "SoulSeek pass:  {}",
        if present(settings.slsk_password.as_deref()) {
            "set"
        } else {
            "not set"
        }
    );
    crate::outln!(
        "Discogs token:  {}  ({})",
        if present(settings.discogs_token.as_deref()) {
            "set"
        } else {
            "not set"
        },
        source(settings.discogs_token.as_deref(), saved.discogs_token())
    );

    match credentials::config_path() {
        Some(path) => crate::outln!(
            "config file:    {}  ({})",
            path.display(),
            if path.exists() { "exists" } else { "missing" }
        ),
        None => {
            crate::outln!(
                "config file:    none; set XDG_CONFIG_HOME or HOME so one can be written"
            );
        }
    }

    ExitCode::SUCCESS
}

/// Where an effective value comes from, given what the file holds.
fn source(effective: Option<&str>, saved: Option<&str>) -> &'static str {
    let effective = effective.filter(|text| !text.trim().is_empty());
    match (effective, saved) {
        (None, _) => "not set",
        (Some(_), None) => "from the environment",
        (Some(effective), Some(saved)) if effective == saved => "from the config file",
        (Some(_), Some(_)) => "from the environment, overriding the config file",
    }
}

fn present(value: Option<&str>) -> bool {
    value.is_some_and(|text| !text.trim().is_empty())
}

/// The value itself, or a dash so the line still lines up.
fn value(value: Option<&str>) -> &str {
    match value.filter(|text| !text.trim().is_empty()) {
        Some(value) => value,
        None => "-",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unset_value_says_so() {
        assert_eq!(source(None, None), "not set");
    }

    #[test]
    fn a_value_only_in_the_environment_is_named_as_such() {
        assert_eq!(source(Some("alice"), None), "from the environment");
    }

    #[test]
    fn a_value_matching_the_file_comes_from_the_file() {
        assert_eq!(source(Some("alice"), Some("alice")), "from the config file");
    }

    #[test]
    fn a_value_that_differs_from_the_file_comes_from_the_environment() {
        assert_eq!(
            source(Some("alice"), Some("bob")),
            "from the environment, overriding the config file"
        );
    }
}
