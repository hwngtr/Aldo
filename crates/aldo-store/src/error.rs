#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("could not open the database: {0}")]
    Connect(String),

    #[error("migration failed: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),

    #[error("query failed: {0}")]
    Query(#[from] sqlx::Error),

    #[error("row not found in {table}")]
    NotFound { table: &'static str },

    #[error("stored value for {column} was corrupt: {detail}")]
    Corrupt {
        column: &'static str,
        detail: String,
    },

    #[error("{count} items cannot be numbered in {column}: too many")]
    TooManyItems { column: &'static str, count: usize },
}

impl StoreError {
    pub fn not_found(table: &'static str) -> Self {
        Self::NotFound { table }
    }

    pub fn corrupt(column: &'static str, detail: impl Into<String>) -> Self {
        Self::Corrupt {
            column,
            detail: detail.into(),
        }
    }
}

/// Turns a stored integer into a typed identifier, naming the column when the
/// value cannot represent one.
pub(crate) fn narrow_id<T: aldo_core::FromStoredId>(
    raw: i64,
    column: &'static str,
) -> Result<T, StoreError> {
    T::from_stored(raw).ok_or_else(|| StoreError::corrupt(column, format!("{raw} out of range")))
}

/// Narrows a stored integer to a smaller unsigned type. SQLite has no unsigned
/// columns and no small integer types, so every such read needs a checked
/// conversion rather than a cast.
macro_rules! narrow_uint {
    ($fn_name:ident, $ty:ty) => {
        /// # Errors
        /// Returns `Corrupt` when the stored value is negative or too large.
        pub(crate) fn $fn_name(raw: i64, column: &'static str) -> Result<$ty, StoreError> {
            <$ty>::try_from(raw)
                .map_err(|_| StoreError::corrupt(column, format!("{raw} out of range")))
        }
    };
}

narrow_uint!(narrow_u8, u8);
narrow_uint!(narrow_u16, u16);
narrow_uint!(narrow_u32, u32);
narrow_uint!(narrow_u64, u64);

/// Turns a byte count into the signed integer SQLite stores, refusing a value
/// too large to store. Saturating here would write a different, smaller byte
/// count than the caller measured.
pub(crate) fn widen_bytes(bytes: u64, column: &'static str) -> Result<i64, StoreError> {
    i64::try_from(bytes).map_err(|_| StoreError::corrupt(column, "too large to store"))
}

/// Turns a millisecond duration into the signed integer SQLite stores, refusing
/// a value too large to store rather than wrapping it negative.
///
/// Takes the column like [`widen_bytes`] because a duration is not only ever
/// stored in a column named `duration_ms`: `rate_budget.window_ms` and
/// `min_interval_ms` hold milliseconds too, and a failure naming the wrong one
/// sends whoever reads the error to a column that cannot be it.
pub(crate) fn widen_millis(millis: u64, column: &'static str) -> Result<i64, StoreError> {
    i64::try_from(millis).map_err(|_| StoreError::corrupt(column, "too large to store"))
}

/// Reads a digest back out of a hex `TEXT` column, naming the column when the
/// stored value is not one.
///
/// The mapping from a parse failure onto [`StoreError`] is one line repeated at
/// every hex column in the crate, and each site was free to name a different
/// column than the one it read. `InvalidHash` already carries the length the
/// type expected, so the detail is kept and only the column is added.
pub(crate) fn digest_from_stored<T: aldo_core::ids::FromStoredHex>(
    raw: &str,
    column: &'static str,
) -> Result<T, StoreError> {
    T::from_stored_hex(raw).map_err(|error| StoreError::corrupt(column, error.to_string()))
}

/// Reads a non-negative byte count from a column that SQLite types as signed.
pub(crate) fn bytes_from_stored(raw: i64, column: &'static str) -> Result<u64, StoreError> {
    narrow_u64(raw, column)
}

/// Reads a non-negative millisecond duration. A distinct name from
/// `bytes_from_stored` because the two convert identically but mean different
/// things, and a duration column read through a byte-count helper reads as a
/// unit bug that still type-checks.
pub(crate) fn millis_from_stored(raw: i64, column: &'static str) -> Result<u64, StoreError> {
    narrow_u64(raw, column)
}

/// Reads a SQLite `INTEGER` used as a flag. Only `0` and `1` are booleans: any
/// other stored value is corruption, not truth, and is reported rather than
/// folded into `true` or `false`.
pub(crate) fn bool_from_stored(raw: i64, column: &'static str) -> Result<bool, StoreError> {
    match raw {
        0 => Ok(false),
        1 => Ok(true),
        other => Err(StoreError::corrupt(
            column,
            format!("{other} is not a flag"),
        )),
    }
}

/// Narrows a list index to the `u32` the schema numbers rows with. A list too
/// long to number is refused rather than saturated, because a saturated index
/// collides with the real last index and silently overwrites its row.
pub(crate) fn ordinal_from_index(index: usize, column: &'static str) -> Result<u32, StoreError> {
    u32::try_from(index).map_err(|_| StoreError::TooManyItems {
        column,
        count: index,
    })
}

/// Milliseconds since the Unix epoch.
///
/// The return type is a bare `i64`, so nothing stops a timestamp from being
/// passed where a duration belongs. Every parameter and column that holds one
/// is named `*_at` or `now_ms`, which is a convention and not a guarantee.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;
    use aldo_core::{DiscogsReleaseId, ReleaseId};

    #[test]
    fn now_is_after_the_year_2020() {
        assert!(now_ms() > 1_577_836_800_000);
    }

    #[test]
    fn error_messages_name_the_offending_thing() {
        assert_eq!(
            StoreError::not_found("release").to_string(),
            "row not found in release"
        );
        assert_eq!(
            StoreError::corrupt("infohash", "wrong length").to_string(),
            "stored value for infohash was corrupt: wrong length"
        );
    }

    #[test]
    fn narrow_id_passes_a_valid_value_through() {
        let id = narrow_id::<ReleaseId>(42, "release_id").expect("valid");
        assert_eq!(id, ReleaseId(42));
    }

    #[test]
    fn narrow_id_names_the_column_when_the_value_is_impossible() {
        // A `u32`-backed id cannot represent a value above `u32::MAX`.
        let result = narrow_id::<DiscogsReleaseId>(i64::from(u32::MAX) + 1, "discogs_release_id");
        assert!(matches!(
            result,
            Err(StoreError::Corrupt {
                column: "discogs_release_id",
                ..
            })
        ));
    }

    #[test]
    fn a_byte_count_too_large_for_sqlite_is_reported_not_saturated() {
        assert_eq!(
            widen_bytes(1_000, "candidate.total_bytes").expect("storable"),
            1_000
        );
        // Saturating would store `i64::MAX`, which is a different and smaller
        // byte count than the one the caller measured.
        assert!(matches!(
            widen_bytes(u64::MAX, "candidate.total_bytes"),
            Err(StoreError::Corrupt {
                column: "candidate.total_bytes",
                ..
            })
        ));
    }

    #[test]
    fn a_duration_too_large_for_sqlite_is_reported_not_saturated() {
        assert_eq!(widen_millis(1_000, "duration_ms").expect("storable"), 1_000);
        assert!(matches!(
            widen_millis(u64::MAX, "duration_ms"),
            Err(StoreError::Corrupt {
                column: "duration_ms",
                ..
            })
        ));
        // A duration is not only ever stored in a column named `duration_ms`:
        // `rate_budget.window_ms` holds milliseconds too, and a failure naming
        // the wrong one sends the reader to a column that cannot be it.
        assert!(matches!(
            widen_millis(u64::MAX, "rate_budget.window_ms"),
            Err(StoreError::Corrupt {
                column: "rate_budget.window_ms",
                ..
            })
        ));
    }

    #[test]
    fn only_zero_and_one_are_flags() {
        assert!(!bool_from_stored(0, "release_artist.is_album_artist").expect("flag"));
        assert!(bool_from_stored(1, "release_artist.is_album_artist").expect("flag"));

        // `-1 != 0` would read as true, and `7 != 0` as true: both wrong.
        for raw in [-1, 2, 7] {
            assert!(
                matches!(
                    bool_from_stored(raw, "tag_application.reverted"),
                    Err(StoreError::Corrupt {
                        column: "tag_application.reverted",
                        ..
                    })
                ),
                "{raw} was accepted as a flag"
            );
        }
    }

    #[test]
    fn an_ordinal_above_the_storable_range_is_refused_not_saturated() {
        assert_eq!(
            ordinal_from_index(4_000_000_000, "asset.ordinal").expect("fits"),
            4_000_000_000
        );
        assert!(
            matches!(
                ordinal_from_index(usize::MAX, "asset.ordinal"),
                Err(StoreError::TooManyItems {
                    column: "asset.ordinal",
                    ..
                })
            ),
            "an unnumberable list index was saturated"
        );
    }

    #[test]
    fn a_stored_digest_is_read_through_one_helper_and_names_its_own_column() {
        use crate::error::digest_from_stored;
        use aldo_core::Sha256;

        let good: Sha256 =
            digest_from_stored(&"ab".repeat(32), "asset.sha256_audio").expect("valid");
        assert_eq!(good.to_hex(), "ab".repeat(32));

        // The failure names the column it was read from and keeps the reason the
        // digest type gave, because a 40-character info hash and a 64-character
        // digest reject the same string for different reasons.
        let error = digest_from_stored::<Sha256>(&"ab".repeat(20), "asset.sha256_audio")
            .expect_err("wrong length");
        assert!(matches!(
            error,
            StoreError::Corrupt {
                column: "asset.sha256_audio",
                ..
            }
        ));
        assert!(
            error.to_string().contains("expected 64 hex characters"),
            "the reason was dropped: {error}"
        );

        // The same helper serves a different digest type, which rejects the same
        // input for its own reason rather than being accepted as the other.
        let other =
            digest_from_stored::<aldo_core::InfoHash>(&"ab".repeat(20), "acquisition.qb_hash")
                .expect("valid info hash");
        assert_eq!(other.to_hex(), "ab".repeat(20));
        assert!(
            digest_from_stored::<aldo_core::InfoHash>(&"ab".repeat(32), "acquisition.qb_hash")
                .is_err()
        );
    }

    #[test]
    fn a_negative_duration_is_corruption() {
        assert_eq!(
            millis_from_stored(254_000, "release_track.duration_ms").expect("duration"),
            254_000
        );
        assert!(matches!(
            millis_from_stored(-1, "release_track.duration_ms"),
            Err(StoreError::Corrupt {
                column: "release_track.duration_ms",
                ..
            })
        ));
    }
}

/// Why a library root cannot be used.
///
/// The root is where finished albums are written. A mistyped one scatters files
/// across the filesystem, so it is checked before anything is stored. The rule
/// lives here because `aldo-store` owns the `app_user.library_root` column.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LibraryRootError {
    #[error("library root {path} must be an absolute path")]
    NotAbsolute { path: std::path::PathBuf },

    #[error("library root {path} does not exist or is not a directory")]
    Missing { path: std::path::PathBuf },
}

/// Checks that a library root is absolute and already a directory.
///
/// The absolute-path check runs first because it is a property of the input
/// rather than of the filesystem, so it reports the same way whatever the
/// working directory happens to be.
pub fn validate_library_root(path: &std::path::Path) -> Result<(), LibraryRootError> {
    if !path.is_absolute() {
        return Err(LibraryRootError::NotAbsolute {
            path: path.to_path_buf(),
        });
    }
    if !path.is_dir() {
        return Err(LibraryRootError::Missing {
            path: path.to_path_buf(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod library_root_tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn an_existing_absolute_directory_is_accepted() {
        assert!(validate_library_root(Path::new("/tmp")).is_ok());
    }

    #[test]
    fn a_relative_path_is_refused_before_the_filesystem_is_consulted() {
        let error = validate_library_root(Path::new("relative/path")).expect_err("refused");
        assert!(matches!(error, LibraryRootError::NotAbsolute { .. }));
    }

    #[test]
    fn a_path_that_is_not_a_directory_is_refused() {
        let file = std::env::temp_dir().join(format!("aldo-libroot-{}", std::process::id()));
        std::fs::write(&file, b"x").expect("fixture file");

        let error = validate_library_root(&file).expect_err("refused");
        assert!(matches!(error, LibraryRootError::Missing { .. }));

        std::fs::remove_file(&file).expect("cleanup");
    }

    #[test]
    fn a_directory_that_does_not_exist_is_refused() {
        let error = validate_library_root(Path::new("/no/such/library/root")).expect_err("refused");
        assert!(matches!(error, LibraryRootError::Missing { .. }));
    }
}
