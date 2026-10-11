//! `aldo logout`: forget the saved credentials.
//!
//! Only the file is touched. A variable exported in the shell still wins on the
//! next run, so the message says so rather than pretending the account is gone.

use std::process::ExitCode;

use crate::credentials;

pub fn run() -> ExitCode {
    // Read first: the keys left over are the ones that were actually removed.
    let before = credentials::load();

    match credentials::clear() {
        Ok(Some(path)) => {
            let removed = removed_keys(&before);
            if removed.is_empty() {
                crate::outln!(
                    "Nothing to sign out: {} holds no credentials.",
                    path.display()
                );
            } else {
                crate::outln!("Signed out: removed {}.", removed.join(", "));
            }
            crate::outln!(
                "Shell variables (ALDO_SLSK_*, ALDO_DISCOGS_TOKEN) are untouched; \
                 unset them if they are set."
            );
            ExitCode::SUCCESS
        }
        Ok(None) => {
            crate::outln!("Nothing to sign out: no saved credentials.");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("aldo: {error}");
            ExitCode::FAILURE
        }
    }
}

/// The names of the credential keys that were present before clearing.
fn removed_keys(before: &credentials::Credentials) -> Vec<&'static str> {
    let mut removed = Vec::new();
    if before.slsk_username().is_some() {
        removed.push(credentials::SLSK_USERNAME);
    }
    if before.slsk_password().is_some() {
        removed.push(credentials::SLSK_PASSWORD);
    }
    if before.discogs_token().is_some() {
        removed.push(credentials::DISCOGS_TOKEN);
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_keys_that_were_present_are_named() {
        let before = credentials::Credentials {
            slsk_username: Some("alice".to_owned()),
            slsk_password: Some("secret".to_owned()),
            discogs_token: None,
        };
        assert_eq!(
            removed_keys(&before),
            vec![credentials::SLSK_USERNAME, credentials::SLSK_PASSWORD]
        );
        assert_eq!(
            removed_keys(&credentials::Credentials::default()),
            Vec::<&'static str>::new()
        );
    }
}
