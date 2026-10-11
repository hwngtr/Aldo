//! Domain types for Aldo. No I/O, no runtime, no network.
//!
//! Every identifier, byte count and audio quantity is a distinct type, so a
//! swapped argument or a unit mix-up is a compile error rather than a runtime
//! bug. Provider implementations live here as traits only; concrete adapters
//! are in `aldo-catalog` and `aldo-api`.

pub mod audio;
pub mod candidate;
pub mod error;
pub mod ids;
pub mod provider;
pub mod release;
pub mod search;

pub use audio::{BitDepth, ByteSize, Channels, DurationMs, SampleRate};
pub use candidate::{
    Candidate, CandidateDisplay, CandidateFile, CandidateSource, DedupeKey, Locator,
    RejectionCounts, SoulSeekLocator, TorrentLocator,
};
pub use error::{CatalogError, ProviderError, ValidationError};
pub use ids::{
    AcquisitionId, ArtistId, AssetId, CandidateId, CreditId, DiscogsArtistId, DiscogsLabelId,
    DiscogsMasterId, DiscogsReleaseId, FanoutId, FromStoredHex, FromStoredId, InfoHash,
    InvalidHash, InvalidMusicBrainzId, LabelId, LocatorId, MasterId, MusicBrainzId, ProviderId,
    ReleaseId, ReleaseTrackId, SessionId, Sha256, UserId, decode_hex, encode_hex,
};
pub use provider::{DiscoveryProvider, ProviderOutcome};
pub use release::{
    ArtistCredit, ArtistRef, MediaFormat, ReleaseCredit, ReleaseIdentifier, ReleaseIdentity,
    ReleaseStatus, ReleaseTrack, ReleaseType, Side, TrackKind, TrackPosition,
};
pub use search::{RawQuery, SearchFilters, SearchQuery};

/// The only FLAC extension Aldo accepts, in any casing.
pub const FLAC_EXTENSION: &str = "flac";

/// Returns the lowercase extension of a path-like string, or an empty string
/// when it has none.
pub fn extension_of(path: &str) -> String {
    let name = &path[path.rfind(['/', '\\']).map_or(0, |slash| slash + 1)..];
    name.rsplit_once('.')
        .map_or_else(String::new, |(_, ext)| ext.to_lowercase())
}

pub fn is_flac(path: &str) -> bool {
    extension_of(path) == FLAC_EXTENSION
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_is_case_insensitive() {
        assert_eq!(extension_of("Album/01 - Song.FLAC"), "flac");
        assert_eq!(extension_of("a.b.c.flac"), "flac");
        assert_eq!(extension_of("noextension"), "");
        assert_eq!(extension_of("trailingdot."), "");
    }

    #[test]
    fn is_flac_handles_windows_separators() {
        assert!(is_flac("Album\\disc1\\01.flac"));
        assert!(!is_flac("Album/cover.jpg"));
        assert!(!is_flac("album/notes.log"));
    }
}
