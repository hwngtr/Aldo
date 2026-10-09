//! One repository module per bounded area. Every query lives here so SQL
//! binding is impossible to forget and row mapping is type-checked.

/// Declares an enum whose values live in a `TEXT` column, together with the
/// spelling used on the way in and the way back out.
///
/// The stored spelling is one fact with three consumers: the `CHECK` constraint
/// in the migration, the value bound into a statement, and the parse on the way
/// out. Writing the mapping twice as two exhaustive matches does not keep them
/// honest — adding a variant makes `as_str` fail to compile while `parse`
/// compiles happily and then reports every stored row as corrupt. One table
/// makes both directions come from the same line, and `ALL` lets a round-trip
/// test cover every variant without restating them.
macro_rules! stored_enum {
    (
        $(#[$attr:meta])*
        $name:ident, $column:literal => {
            $($(#[$variant_attr:meta])* $variant:ident => $text:literal),+ $(,)?
        }
    ) => {
        $(#[$attr])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum $name {
            $($(#[$variant_attr])* $variant),+
        }

        impl $name {
            /// Every variant. A round trip is checked against this so a
            /// variant added without a test entry cannot be missed.
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];

            pub fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $text),+
                }
            }

            /// # Errors
            /// Returns [`StoreError::Corrupt`] naming the column when the stored
            /// value is not one of the known spellings. An unknown value is
            /// corruption in a row, never a variant to guess at.
            pub fn parse(text: &str) -> Result<Self, StoreError> {
                match text {
                    $($text => Ok(Self::$variant),)+
                    other => Err(StoreError::corrupt($column, other)),
                }
            }
        }
    };
}

pub mod acquisition;
pub mod budget;
pub mod candidate;
pub mod catalog;
pub mod session;
pub mod user;

pub use acquisition::{
    AcquisitionRow, AcquisitionStatus, AcquisitionStore, AssetRow, AssetState, Engine, Ratio,
    UnitInterval,
};
pub use budget::{BudgetSnapshot, BudgetStore, RateBudget};
pub use candidate::{CandidateRow, CandidateStore, LoadedCandidate};
pub use catalog::{CachedMetainfo, CatalogRow, CatalogStore};
pub use session::{FanoutRow, FanoutStatus, SessionRow, SessionStore};
pub use user::{LOCAL_USER, ensure_local_user};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::StoreError;

    /// Every variant must survive the trip to storage and back, and must own
    /// its stored spelling: `AcquisitionStatus::Partial` and `AssetState::Partial`
    /// are both `"partial"`, in different tables, and a copy-paste that merged
    /// two variants of one enum would make the second unreachable.
    macro_rules! assert_stored_enum_is_total {
        ($name:ident) => {
            for variant in $name::ALL {
                let text = variant.as_str();
                assert_eq!(
                    $name::parse(text).ok(),
                    Some(*variant),
                    "{} stored as {text} does not read back",
                    stringify!($name)
                );
            }

            let mut seen: Vec<&str> = $name::ALL.iter().map(|variant| variant.as_str()).collect();
            let total = seen.len();
            seen.sort_unstable();
            seen.dedup();
            assert_eq!(
                seen.len(),
                total,
                "{} stores two variants under one spelling",
                stringify!($name)
            );
        };
    }

    #[test]
    fn every_stored_enum_round_trips_through_one_spelling_per_variant() {
        assert_stored_enum_is_total!(FanoutStatus);
        assert_stored_enum_is_total!(Engine);
        assert_stored_enum_is_total!(AcquisitionStatus);
        assert_stored_enum_is_total!(AssetState);
    }

    /// Asserts an unknown stored value is reported against the column it came
    /// from, rather than mapped onto a variant that happens to fit.
    fn assert_rejected<T: std::fmt::Debug>(result: Result<T, StoreError>, column: &str) {
        match result {
            Err(StoreError::Corrupt { column: named, .. }) => assert_eq!(named, column),
            other => panic!("a stored value outside the table was accepted: {other:?}"),
        }
    }

    #[test]
    fn a_stored_spelling_outside_the_table_is_reported_against_its_own_column() {
        assert_rejected(FanoutStatus::parse("banana"), "provider_fanout.status");
        assert_rejected(Engine::parse("aria2"), "acquisition.engine");
        assert_rejected(AcquisitionStatus::parse("paused"), "acquisition.status");
        assert_rejected(AssetState::parse("corrupt"), "asset.state");
    }
}
