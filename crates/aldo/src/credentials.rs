//! Where the user's credentials are saved, and where they are forgotten.
//!
//! Aldo reads credentials from the shell and from `~/.config/aldo/.env`. This
//! module is the only thing that writes that file: the first-run setup screen
//! and `aldo login` save through it, and `aldo logout` clears it. The file is
//! kept at mode 0600 inside a 0700 directory, because it holds a password.
//!
//! A save is additive: a field left out keeps whatever the file already had, so
//! signing in again cannot silently drop a Discogs token that was not retyped.
//! Removing credentials is a separate act, [`clear`].

use std::fmt;
use std::path::{Path, PathBuf};

/// The environment variable holding the SoulSeek account name.
pub const SLSK_USERNAME: &str = "ALDO_SLSK_USERNAME";
/// The environment variable holding the SoulSeek password.
pub const SLSK_PASSWORD: &str = "ALDO_SLSK_PASSWORD";
/// The environment variable holding the Discogs personal access token.
pub const DISCOGS_TOKEN: &str = "ALDO_DISCOGS_TOKEN";

/// The three keys this module owns, in the order they are written.
const KEYS: [&str; 3] = [SLSK_USERNAME, SLSK_PASSWORD, DISCOGS_TOKEN];

/// The credentials a user provided, any of which may be absent.
///
/// `Debug` is handwritten so a password cannot reach a log line through a stray
/// `{:?}`.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Credentials {
    pub slsk_username: Option<String>,
    pub slsk_password: Option<String>,
    pub discogs_token: Option<String>,
}

impl Credentials {
    /// The SoulSeek account name, trimmed, or `None` when blank.
    #[must_use]
    pub fn slsk_username(&self) -> Option<&str> {
        non_blank(self.slsk_username.as_deref())
    }

    /// The SoulSeek password, trimmed, or `None` when blank.
    #[must_use]
    pub fn slsk_password(&self) -> Option<&str> {
        non_blank(self.slsk_password.as_deref())
    }

    /// The Discogs token, trimmed, or `None` when blank.
    #[must_use]
    pub fn discogs_token(&self) -> Option<&str> {
        non_blank(self.discogs_token.as_deref())
    }

    /// The value of one of the three keys, or `None` when it is unset.
    fn get(&self, key: &str) -> Option<&str> {
        match key {
            SLSK_USERNAME => self.slsk_username(),
            SLSK_PASSWORD => self.slsk_password(),
            DISCOGS_TOKEN => self.discogs_token(),
            _ => None,
        }
    }
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let redacted = |value: &Option<String>| value.as_ref().map(|_| "redacted");
        f.debug_struct("Credentials")
            .field("slsk_username", &self.slsk_username)
            .field("slsk_password", &redacted(&self.slsk_password))
            .field("discogs_token", &redacted(&self.discogs_token))
            .finish()
    }
}

/// The user's config directory, or `None` when neither variable is set.
#[must_use]
pub fn config_dir() -> Option<PathBuf> {
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME").filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(xdg));
    }

    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config"))
}

/// The file Aldo writes credentials to, or `None` when there is no home.
#[must_use]
pub fn config_path() -> Option<PathBuf> {
    config_dir().map(|dir| dir.join("aldo").join(".env"))
}

/// Saves the given credentials, keeping any field that was not provided.
///
/// # Errors
/// Fails when there is no config directory, or the file cannot be written.
pub fn save(credentials: &Credentials) -> Result<PathBuf, CredentialsError> {
    let path = config_path().ok_or(CredentialsError::NoConfigDir)?;
    save_at(&path, credentials)?;
    Ok(path)
}

/// Removes every credential Aldo owns, leaving other keys in the file alone.
///
/// Returns the path that was touched, or `None` when there was nothing to do
/// because no credentials file exists yet.
///
/// # Errors
/// Fails when an existing file cannot be read or rewritten.
pub fn clear() -> Result<Option<PathBuf>, CredentialsError> {
    let Some(path) = config_path() else {
        return Ok(None);
    };
    Ok(clear_at(&path)?.then_some(path))
}

/// The credentials currently in the saved file, ignoring the environment.
///
/// Best effort: a missing or unreadable file reads as empty, which is what a
/// caller comparing an incoming value against the stored one wants.
#[must_use]
pub fn load() -> Credentials {
    let Some(path) = config_path() else {
        return Credentials::default();
    };
    let text = std::fs::read_to_string(path).unwrap_or_default();

    let mut credentials = Credentials::default();
    for line in text.lines() {
        let (Some(key), Some(value)) = (line_key(line), line_value(line)) else {
            continue;
        };
        match key {
            SLSK_USERNAME => credentials.slsk_username = Some(value),
            SLSK_PASSWORD => credentials.slsk_password = Some(value),
            DISCOGS_TOKEN => credentials.discogs_token = Some(value),
            _ => {}
        }
    }
    credentials
}

/// Writes credentials to a specific file. Split out so the merge is testable
/// without touching the real home directory.
fn save_at(path: &Path, credentials: &Credentials) -> Result<(), CredentialsError> {
    let existing = read_if_exists(path)?;
    let merged = merge(&existing, credentials);
    write_private(path, &merged)
}

/// Removes the three keys from a specific file. `Ok(false)` means no file.
fn clear_at(path: &Path) -> Result<bool, CredentialsError> {
    if !path.exists() {
        return Ok(false);
    }
    let existing = read_if_exists(path)?;
    let stripped = strip(&existing);

    // Nothing left but credentials: remove the file rather than leave an empty
    // one that could be mistaken for a still-configured install.
    if stripped.trim().is_empty() {
        std::fs::remove_file(path).map_err(|source| CredentialsError::Remove {
            path: path.to_path_buf(),
            source,
        })?;
    } else {
        write_private(path, &stripped)?;
    }
    Ok(true)
}

/// Reads a file, treating a missing one as empty.
fn read_if_exists(path: &Path) -> Result<String, CredentialsError> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(source) => Err(CredentialsError::Read {
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// Creates the parent directory at 0700 and writes the file at 0600.
fn write_private(path: &Path, contents: &str) -> Result<(), CredentialsError> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir).map_err(|source| CredentialsError::CreateDir {
        path: dir.to_path_buf(),
        source,
    })?;
    set_mode(dir, 0o700)?;

    std::fs::write(path, contents).map_err(|source| CredentialsError::Write {
        path: path.to_path_buf(),
        source,
    })?;
    set_mode(path, 0o600)
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> Result<(), CredentialsError> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).map_err(|source| {
        CredentialsError::Permissions {
            path: path.to_path_buf(),
            source,
        }
    })
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> Result<(), CredentialsError> {
    Ok(())
}

/// Folds credentials into an existing file, keeping comments and foreign keys.
///
/// A provided field replaces its line in place; a later duplicate of the same
/// key is dropped. A field that is absent or blank is left exactly as it was.
fn merge(existing: &str, credentials: &Credentials) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut handled: Vec<&str> = Vec::new();

    for line in existing.lines() {
        if let Some(key) = line_key(line)
            && KEYS.contains(&key)
        {
            if handled.contains(&key) {
                // A duplicate of a key already written or dropped.
                continue;
            }
            handled.push(key);
            match credentials.get(key) {
                Some(value) => out.push(format!("{key}={}", format_value(value))),
                // A field that was not retyped keeps the line it already had.
                None => out.push(line.to_owned()),
            }
            continue;
        }
        out.push(line.to_owned());
    }

    for key in KEYS {
        if handled.contains(&key) {
            continue;
        }
        if let Some(value) = credentials.get(key) {
            out.push(format!("{key}={}", format_value(value)));
        }
    }

    finish(&out)
}

/// Removes every credential line, leaving comments and foreign keys alone.
fn strip(existing: &str) -> String {
    let out: Vec<String> = existing
        .lines()
        .filter(|line| !line_key(line).is_some_and(|key| KEYS.contains(&key)))
        .map(str::to_owned)
        .collect();
    finish(&out)
}

/// Joins lines with a trailing newline, so the file always ends cleanly.
fn finish(lines: &[String]) -> String {
    let mut text = lines.join("\n");
    if !text.is_empty() {
        text.push('\n');
    }
    text
}

/// The variable name on a `KEY=value` line, or `None` for anything else.
fn line_key(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    let trimmed = trimmed.strip_prefix("export ").unwrap_or(trimmed);
    let (key, _) = trimmed.split_once('=')?;
    let key = key.trim();
    if key.is_empty()
        || !key
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    {
        return None;
    }
    Some(key)
}

/// The value on a `KEY=value` line, unquoted and trimmed.
fn line_value(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    let trimmed = trimmed.strip_prefix("export ").unwrap_or(trimmed);
    let (_, value) = trimmed.split_once('=')?;
    Some(unquote(value.trim()))
}

/// Undoes the quoting [`format_value`] adds.
fn unquote(value: &str) -> String {
    if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
        return value[1..value.len() - 1]
            .replace("\\\"", "\"")
            .replace("\\\\", "\\");
    }
    if value.len() >= 2 && value.starts_with('\'') && value.ends_with('\'') {
        return value[1..value.len() - 1].to_owned();
    }
    value.to_owned()
}

/// Quotes a value when it holds anything dotenv would otherwise misread.
fn format_value(value: &str) -> String {
    let needs_quotes = value.is_empty()
        || value
            .chars()
            .any(|ch| ch.is_whitespace() || matches!(ch, '#' | '"' | '\''));
    if needs_quotes {
        let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
        format!("\"{escaped}\"")
    } else {
        value.to_owned()
    }
}

/// Treats a whitespace-only value as absent.
fn non_blank(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|text| !text.is_empty())
}

#[derive(Debug, thiserror::Error)]
pub enum CredentialsError {
    #[error("could not find a config directory: set XDG_CONFIG_HOME or HOME")]
    NoConfigDir,

    #[error("could not read {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("could not create {path}: {source}")]
    CreateDir {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("could not write {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("could not remove {path}: {source}")]
    Remove {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("could not set permissions on {path}: {source}")]
    Permissions {
        path: PathBuf,
        source: std::io::Error,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn credentials(username: &str, password: &str, token: Option<&str>) -> Credentials {
        Credentials {
            slsk_username: Some(username.to_owned()),
            slsk_password: Some(password.to_owned()),
            discogs_token: token.map(str::to_owned),
        }
    }

    #[test]
    fn merging_into_an_empty_file_writes_only_what_was_given() {
        let merged = merge("", &credentials("alice", "secret", None));
        assert_eq!(
            merged,
            "ALDO_SLSK_USERNAME=alice\nALDO_SLSK_PASSWORD=secret\n"
        );
    }

    #[test]
    fn merging_keeps_comments_and_foreign_keys() {
        let existing =
            "# my settings\nALDO_PROXY=socks5://127.0.0.1:9050\nALDO_SLSK_USERNAME=old\n";
        let merged = merge(existing, &credentials("alice", "secret", None));

        assert!(merged.contains("# my settings"), "{merged}");
        assert!(
            merged.contains("ALDO_PROXY=socks5://127.0.0.1:9050"),
            "{merged}"
        );
        assert!(merged.contains("ALDO_SLSK_USERNAME=alice"), "{merged}");
        assert!(!merged.contains("ALDO_SLSK_USERNAME=old"), "{merged}");
    }

    #[test]
    fn a_field_left_out_is_not_touched() {
        // Signing in without retyping the token must not erase it.
        let existing = "ALDO_DISCOGS_TOKEN=keep-me\n";
        let merged = merge(existing, &credentials("alice", "secret", None));
        assert!(merged.contains("ALDO_DISCOGS_TOKEN=keep-me"), "{merged}");
    }

    #[test]
    fn duplicate_lines_for_a_key_are_collapsed() {
        let existing = "ALDO_SLSK_USERNAME=one\nALDO_SLSK_USERNAME=two\n";
        let merged = merge(existing, &credentials("alice", "secret", None));
        assert_eq!(merged.matches("ALDO_SLSK_USERNAME=").count(), 1, "{merged}");
        assert!(merged.contains("ALDO_SLSK_USERNAME=alice"), "{merged}");
    }

    #[test]
    fn a_value_with_spaces_or_hashes_is_quoted() {
        let merged = merge("", &credentials("alice smith", "p#ss word", None));
        assert!(
            merged.contains("ALDO_SLSK_USERNAME=\"alice smith\""),
            "{merged}"
        );
        assert!(
            merged.contains("ALDO_SLSK_PASSWORD=\"p#ss word\""),
            "{merged}"
        );
    }

    #[test]
    fn a_written_value_reads_back_unchanged() {
        assert_eq!(
            line_value("ALDO_SLSK_USERNAME=\"alice smith\"").as_deref(),
            Some("alice smith")
        );
        assert_eq!(
            line_value("export ALDO_SLSK_PASSWORD=secret").as_deref(),
            Some("secret")
        );
        assert_eq!(line_value("# a comment"), None);
        assert_eq!(unquote("\"a\\\"b\""), "a\"b");
    }

    #[test]
    fn clearing_removes_only_the_three_keys() {
        let existing = "ALDO_PROXY=direct\nALDO_SLSK_USERNAME=alice\nALDO_SLSK_PASSWORD=secret\nALDO_DISCOGS_TOKEN=t\n";
        let cleared = strip(existing);

        assert!(cleared.contains("ALDO_PROXY=direct"), "{cleared}");
        assert!(!cleared.contains("ALDO_SLSK_USERNAME"), "{cleared}");
        assert!(!cleared.contains("ALDO_SLSK_PASSWORD"), "{cleared}");
        assert!(!cleared.contains("ALDO_DISCOGS_TOKEN"), "{cleared}");
    }

    #[test]
    fn blank_fields_are_never_written() {
        let blank = Credentials {
            slsk_username: Some("   ".to_owned()),
            slsk_password: None,
            discogs_token: Some(String::new()),
        };
        assert_eq!(merge("", &blank), "");
    }

    #[test]
    fn the_debug_form_never_prints_a_secret() {
        let text = format!("{:?}", credentials("alice", "hunter2", Some("xyzzy")));
        assert!(!text.contains("hunter2"), "{text}");
        assert!(!text.contains("xyzzy"), "{text}");
        assert!(text.contains("alice"), "{text}");
    }

    #[test]
    fn saving_creates_the_file_and_clearing_removes_the_keys() {
        let dir = std::env::temp_dir().join(format!("aldo-credentials-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("aldo").join(".env");

        save_at(&path, &credentials("alice", "secret", Some("tok"))).expect("saved");
        let text = std::fs::read_to_string(&path).expect("file exists");
        assert!(text.contains("ALDO_SLSK_USERNAME=alice"), "{text}");
        assert!(text.contains("ALDO_DISCOGS_TOKEN=tok"), "{text}");

        assert!(clear_at(&path).expect("cleared"));
        assert!(!path.exists(), "an empty credentials file was left behind");

        // Clearing again reports nothing, because there is no file left.
        assert!(!clear_at(&path).expect("nothing to clear"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn clearing_keeps_a_file_that_still_holds_other_settings() {
        let dir = std::env::temp_dir().join(format!("aldo-keep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("aldo").join(".env");
        std::fs::create_dir_all(path.parent().expect("dir")).expect("mkdir");
        std::fs::write(
            &path,
            "ALDO_SLSK_USERNAME=alice\nALDO_PROXY=socks5://127.0.0.1:9050\n",
        )
        .expect("seed");

        assert!(clear_at(&path).expect("cleared"));
        let text = std::fs::read_to_string(&path).expect("the file is kept");
        assert!(!text.contains("ALDO_SLSK_USERNAME"), "{text}");
        assert!(
            text.contains("ALDO_PROXY=socks5://127.0.0.1:9050"),
            "{text}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn clearing_a_missing_file_does_nothing() {
        let path = std::env::temp_dir().join(format!("aldo-absent-{}.env", std::process::id()));
        let _ = std::fs::remove_file(&path);
        assert!(!clear_at(&path).expect("no file"));
    }

    #[cfg(unix)]
    #[test]
    fn the_file_is_private_and_the_directory_is_private() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = std::env::temp_dir().join(format!("aldo-perms-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("aldo").join(".env");

        save_at(&path, &credentials("alice", "secret", None)).expect("saved");

        let file_mode = std::fs::metadata(&path).expect("file").permissions().mode();
        assert_eq!(file_mode & 0o777, 0o600, "the file is not private");

        let dir_mode = std::fs::metadata(path.parent().expect("dir"))
            .expect("dir")
            .permissions()
            .mode();
        assert_eq!(dir_mode & 0o777, 0o700, "the directory is not private");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
