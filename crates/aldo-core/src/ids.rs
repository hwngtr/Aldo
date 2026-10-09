//! Identifier newtypes. A raw integer or string is never used as an identifier;
//! a distinct type per identifier makes a swapped argument a compile error.

use std::fmt::{self, Display, Formatter};

use serde::{Deserialize, Serialize};
use sha2::Digest as _;

/// Implements the identifier boilerplate: conversion both ways, a `Display`
/// that forwards to the inner value, and narrowing from a stored integer.
macro_rules! strong_id {
    ($name:ident, $inner:ty, $doc:literal) => {
        #[doc = $doc]
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(pub $inner);

        impl $name {
            pub fn get(self) -> $inner {
                self.0
            }
        }

        impl From<$inner> for $name {
            fn from(value: $inner) -> Self {
                Self(value)
            }
        }

        impl From<$name> for $inner {
            fn from(value: $name) -> Self {
                value.0
            }
        }

        impl FromStoredId for $name {
            fn from_stored(raw: i64) -> Option<Self> {
                <$inner>::try_from(raw).ok().map(Self)
            }
        }

        impl Display for $name {
            fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
                Display::fmt(&self.0, f)
            }
        }
    };
}

/// A `u32`-backed identifier. These convert straight to `i64` because SQLite
/// stores every id as a signed 64-bit integer.
macro_rules! strong_u32_id {
    ($name:ident, $doc:literal) => {
        strong_id!($name, u32, $doc);

        impl From<$name> for i64 {
            fn from(value: $name) -> Self {
                i64::from(value.0)
            }
        }
    };
}

strong_id!(UserId, i64, "Local user row identifier.");
strong_id!(ReleaseId, i64, "Local release row identifier.");
strong_id!(ReleaseTrackId, i64, "Local release track row identifier.");
strong_id!(MasterId, i64, "Local master (work) row identifier.");
strong_id!(ArtistId, i64, "Local artist row identifier.");
strong_id!(LabelId, i64, "Local label row identifier.");
strong_id!(CreditId, i64, "Local release credit row identifier.");
strong_id!(SessionId, i64, "Local search session row identifier.");
strong_id!(FanoutId, i64, "Local provider fanout row identifier.");
strong_id!(CandidateId, i64, "Local candidate row identifier.");
strong_id!(LocatorId, i64, "Local candidate locator row identifier.");
strong_id!(AcquisitionId, i64, "Local acquisition row identifier.");
strong_id!(AssetId, i64, "Local asset row identifier.");
strong_u32_id!(DiscogsReleaseId, "Discogs release identifier.");
strong_u32_id!(DiscogsMasterId, "Discogs master release identifier.");
strong_u32_id!(DiscogsArtistId, "Discogs artist identifier.");
strong_u32_id!(DiscogsLabelId, "Discogs label identifier.");

/// Implemented by every newtype that SQLite stores in a signed 64-bit column:
/// identifiers, `DurationMs` and `ByteSize`. A stored integer can be turned back
/// into the type without repeating the conversion at each call site, and a value
/// the type cannot represent is reported rather than truncated.
pub trait FromStoredId: Sized {
    /// Returns `None` when the stored value cannot represent this type, which
    /// means the row is corrupt.
    fn from_stored(raw: i64) -> Option<Self>;
}

/// Renders bytes as lowercase hex, two characters per byte.
///
/// The one place Aldo spells bytes as hex. A bearer token, a BitTorrent info hash
/// and a SHA-256 all travel as the same kind of string, so the encoder is shared
/// while the types around it stay distinct.
pub fn encode_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// Reads a hex string back into bytes.
///
/// Upper and lower case are both accepted; anything else is refused. The
/// failure carries *why*, because a 40-character info hash and a 64-character
/// SHA-256 reject the same string for different reasons.
///
/// # Errors
/// Returns [`InvalidHash::NotAscii`] for non-ASCII bytes and
/// [`InvalidHash::NotHex`] for a non-hexadecimal character. An odd number of
/// characters is refused rather than silently dropping its last one.
pub fn decode_hex(text: &str) -> Result<Vec<u8>, InvalidHash> {
    if !text.len().is_multiple_of(2) {
        return Err(InvalidHash::NotHex);
    }
    text.as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let pair = std::str::from_utf8(pair).map_err(|_| InvalidHash::NotAscii)?;
            u8::from_str_radix(pair, 16).map_err(|_| InvalidHash::NotHex)
        })
        .collect()
}

/// Implements the hex-string plumbing shared by every fixed-length digest:
/// parsing, rendering, `Display` and serde, all as a lowercase hex string.
///
/// Only the plumbing is shared, and it comes from [`encode_hex`] and
/// [`decode_hex`]. The digest lengths stay distinct, because a 20-byte BitTorrent
/// info hash and a 32-byte SHA-256 are different values and must not be
/// interchangeable.
macro_rules! hex_digest {
    ($name:ident, $byte_len:literal, $doc:literal) => {
        #[doc = $doc]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(pub [u8; $byte_len]);

        impl $name {
            /// Length in bytes, and therefore twice as many hex characters.
            pub const BYTE_LEN: usize = $byte_len;

            pub fn from_hex(hex: &str) -> Result<Self, InvalidHash> {
                if hex.len() != Self::BYTE_LEN * 2 {
                    return Err(InvalidHash::WrongLength {
                        expected: Self::BYTE_LEN * 2,
                        found: hex.len(),
                    });
                }
                decode_hex(hex).map(|bytes| Self(bytes.try_into().expect("length checked above")))
            }

            pub fn to_hex(self) -> String {
                encode_hex(&self.0)
            }
        }

        impl Display for $name {
            fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
                f.write_str(&encode_hex(&self.0))
            }
        }

        impl Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.collect_str(self)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let text = String::deserialize(deserializer)?;
                <$name>::from_hex(&text).map_err(serde::de::Error::custom)
            }
        }

        impl FromStoredHex for $name {
            fn from_stored_hex(hex: &str) -> Result<Self, InvalidHash> {
                Self::from_hex(hex)
            }
        }
    };
}

/// Implemented by every digest stored in a hex `TEXT` column, so a read can name
/// the column in its failure without each call site restating the mapping from
/// `InvalidHash` onto a store error.
///
/// The digest types stay distinct — this is the plumbing they share, not the
/// values — so `aldo-store` can read a 40-character info hash and a 64-character
/// digest through one helper and still refuse each for its own reason.
pub trait FromStoredHex: Sized {
    fn from_stored_hex(hex: &str) -> Result<Self, InvalidHash>;
}

hex_digest!(
    InfoHash,
    20,
    "A BitTorrent info hash. Always twenty bytes, never a hex string in transit."
);

impl InfoHash {
    pub fn magnet_uri(self, tracker: &str) -> String {
        format!("magnet:?xt=urn:btih:{}&tr={tracker}", self.to_hex())
    }
}

hex_digest!(
    Sha256,
    32,
    "A SHA-256 digest, used for deduplicating identical audio across sources."
);

impl Sha256 {
    /// Hashes some bytes.
    ///
    /// The digest is an identity, not a security control: no secret is hashed
    /// anywhere in Aldo, and the value is only ever compared against another
    /// value this function produced. It is still a vetted implementation rather
    /// than a local one, because the result is persisted — two releases Aldo
    /// considers identical must keep hashing to the same key across versions,
    /// and a compression function that quietly stops matching produces
    /// deduplication that never fires, with nothing to report it.
    #[must_use]
    pub fn of(bytes: &[u8]) -> Self {
        let digest = sha2::Sha256::digest(bytes);
        let mut out = [0_u8; 32];
        out.copy_from_slice(&digest);
        Self(out)
    }
}

/// Which search source a candidate or an error came from.
///
/// Lives here rather than in `provider` because three modules name it:
/// `provider` (the trait returns it), `candidate` (a source carries one) and
/// `error` (a failure carries one). Defining it in `provider` made all three
/// mutually recursive; `ids` has no intra-crate dependency, so it is the layer
/// all three can agree on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderId {
    /// The SoulSeek peer network.
    SoulSeek,
    /// Tracker search. RutTracker and Nyaa.
    TrackerSearch,
    /// Resolving a magnet to its file list. Not a search: a resolution step.
    TorrentMeta,
    /// Discogs release metadata.
    Discogs,
    /// MusicBrainz release metadata.
    MusicBrainz,
}

/// Every provider, paired with the name it is stored and logged under.
///
/// One table for both directions of the mapping. `as_str` and `parse` written
/// as two exhaustive matches means a new variant fails to compile in one and
/// compiles happily in the other, after which every row stored under the new
/// name reads back as unknown.
pub const PROVIDER_NAMES: [(&str, ProviderId); 5] = [
    ("soulseek", ProviderId::SoulSeek),
    ("tracker", ProviderId::TrackerSearch),
    ("torrent-meta", ProviderId::TorrentMeta),
    ("discogs", ProviderId::Discogs),
    ("musicbrainz", ProviderId::MusicBrainz),
];

impl ProviderId {
    /// Every variant. A round trip is checked against this, so a provider added
    /// without a test entry cannot be missed.
    pub const ALL: &'static [Self] = &[
        Self::SoulSeek,
        Self::TrackerSearch,
        Self::TorrentMeta,
        Self::Discogs,
        Self::MusicBrainz,
    ];

    pub fn as_str(self) -> &'static str {
        PROVIDER_NAMES
            .iter()
            .find_map(|(name, provider)| (*provider == self).then_some(*name))
            .unwrap_or_else(|| panic!("every ProviderId is listed in PROVIDER_NAMES"))
    }

    /// Reads the name `as_str` writes. Defined beside it rather than in the
    /// store, because the two spellings are one fact: a stored provider that
    /// does not come back is reported as a corrupt row, and the failure would
    /// otherwise sit two crates away from the definition.
    pub fn parse(text: &str) -> Option<Self> {
        PROVIDER_NAMES
            .iter()
            .find_map(|(name, provider)| (*name == text).then_some(*provider))
    }

    /// Whether failures are expected under normal operation. A rate-limited
    /// metadata source is not an error; an unreachable network is.
    pub fn is_resilient(self) -> bool {
        matches!(
            self,
            Self::SoulSeek | Self::TrackerSearch | Self::TorrentMeta
        )
    }
}

impl Display for ProviderId {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A MusicBrainz identifier. MusicBrainz serves these as UUIDs, but not every
/// MusicBrainz endpoint rejects a non-UUID string, so validity is checked at the
/// boundary rather than assumed.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MusicBrainzId(String);

impl MusicBrainzId {
    pub fn parse(text: &str) -> Result<Self, InvalidMusicBrainzId> {
        let valid = text.len() == 36
            && text.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
            && text.as_bytes()[8] == b'-'
            && text.as_bytes()[13] == b'-'
            && text.as_bytes()[18] == b'-'
            && text.as_bytes()[23] == b'-';
        if valid {
            Ok(Self(text.to_ascii_lowercase()))
        } else {
            Err(InvalidMusicBrainzId { found: text.len() })
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for MusicBrainzId {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        Display::fmt(&self.0, f)
    }
}

/// A hex string was not a well-formed digest. Carries the length the digest
/// type expects, because a 20-byte info hash and a 32-byte SHA-256 reject the
/// same string for different reasons.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InvalidHash {
    #[error("expected {expected} hex characters, found {found}")]
    WrongLength { expected: usize, found: usize },
    #[error("identifier contained non-ASCII bytes")]
    NotAscii,
    #[error("identifier contained a non-hexadecimal character")]
    NotHex,
}

/// A string was not a MusicBrainz UUID. A UUID is not a hex digest: it has
/// dashes at fixed offsets, so it needs its own error rather than being
/// reported as a malformed hash.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("expected a 36-character MusicBrainz UUID, found {found} characters")]
pub struct InvalidMusicBrainzId {
    pub found: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn info_hash_round_trips_through_hex() {
        let hex = "f43fa5f85f1c241db3819686bc56c4932e51fbc5";
        let hash = InfoHash::from_hex(hex).expect("valid hex");
        assert_eq!(hash.to_hex(), hex);
        assert_eq!(hash.to_string(), hex);
    }

    #[test]
    fn info_hash_accepts_uppercase_hex() {
        let hash =
            InfoHash::from_hex("F43FA5F85F1C241DB3819686BC56C4932E51FBC5").expect("valid hex");
        assert_eq!(hash.to_hex(), "f43fa5f85f1c241db3819686bc56c4932e51fbc5");
    }

    #[test]
    fn info_hash_rejects_wrong_length_and_non_hex() {
        assert_eq!(
            InfoHash::from_hex("abc"),
            Err(InvalidHash::WrongLength {
                expected: 40,
                found: 3
            })
        );
        let too_long = "z".repeat(40);
        assert_eq!(InfoHash::from_hex(&too_long), Err(InvalidHash::NotHex));
        // A 32-byte digest rejects a 20-byte one for a different reason, and
        // says so.
        assert_eq!(
            Sha256::from_hex(&"ab".repeat(20)).unwrap_err().to_string(),
            "expected 64 hex characters, found 40"
        );
    }

    #[test]
    fn sha256_round_trips_through_hex() {
        let hex = "a".repeat(64);
        let digest = Sha256::from_hex(&hex).expect("valid hex");
        assert_eq!(digest.to_hex(), hex);
        assert_eq!(digest.to_string(), hex);
    }

    #[test]
    fn musicbrainz_id_lowercases_and_validates() {
        let id = MusicBrainzId::parse("f4b1B5E6-1234-4C9A-9E1D-000000000000").expect("valid mbid");
        assert_eq!(id.as_str(), "f4b1b5e6-1234-4c9a-9e1d-000000000000");
        assert!(MusicBrainzId::parse("not-a-mbid").is_err());
    }

    #[test]
    fn a_bad_musicbrainz_id_is_reported_as_a_uuid_failure() {
        assert_eq!(
            MusicBrainzId::parse("not-a-mbid").unwrap_err().to_string(),
            "expected a 36-character MusicBrainz UUID, found 10 characters"
        );
    }

    #[test]
    fn every_digest_length_distinguishes_itself() {
        assert_eq!(InfoHash::BYTE_LEN, 20);
        assert_eq!(Sha256::BYTE_LEN, 32);
    }

    #[test]
    fn magnet_uri_embeds_hex_and_tracker() {
        let hash = InfoHash::from_hex(&"ab".repeat(20)).expect("valid hex");
        let uri = hash.magnet_uri("http%3A%2F%2Ftracker%2Fannounce");
        assert!(uri.starts_with("magnet:?xt=urn:btih:abab"));
        assert!(uri.contains("tr=http%3A%2F%2Ftracker%2Fannounce"));
    }

    #[test]
    fn a_provider_name_round_trips_through_its_stored_form() {
        for provider in ProviderId::ALL {
            assert_eq!(
                ProviderId::parse(provider.as_str()),
                Some(*provider),
                "{provider} does not read back from its own name"
            );
        }
        assert_eq!(ProviderId::parse("napster"), None);
    }

    /// The store persists providers under these names, so the table has to
    /// cover every variant. A provider missing from `PROVIDER_NAMES` would pass
    /// the round-trip test above and still have nowhere to be stored.
    #[test]
    fn every_provider_has_exactly_one_stored_name() {
        assert_eq!(
            PROVIDER_NAMES.len(),
            ProviderId::ALL.len(),
            "{:?} is missing from PROVIDER_NAMES",
            ProviderId::ALL
        );
        for provider in ProviderId::ALL {
            assert_eq!(
                ProviderId::parse(provider.as_str()),
                Some(*provider),
                "{provider} has no name of its own"
            );
        }
    }

    #[test]
    fn hex_codec_round_trips_arbitrary_bytes() {
        let bytes = [0x00_u8, 0x01, 0x7f, 0x80, 0xab, 0xcd, 0xef, 0xff];
        let text = encode_hex(&bytes);
        assert_eq!(text, "00017f80abcdefff");
        assert_eq!(decode_hex(&text).expect("decodes"), bytes);
        assert_eq!(
            decode_hex(&text.to_uppercase()).expect("case-insensitive"),
            bytes
        );
    }

    #[test]
    fn hex_codec_refuses_an_odd_length_string_rather_than_dropping_a_byte() {
        // A truncated digest must not decode to a shorter digest.
        assert_eq!(decode_hex("abc"), Err(InvalidHash::NotHex));
        assert_eq!(decode_hex("ab c"), Err(InvalidHash::NotHex));
        assert_eq!(decode_hex("zz"), Err(InvalidHash::NotHex));
        // A two-byte chunk that splits a multi-byte character is not a hex
        // character, and is reported as the encoding problem it is rather than
        // as bad hex.
        let split = String::from_utf8(vec![0xe0, 0xa0, 0x80, 0xe0, 0xa0, 0x80]).expect("valid");
        assert_eq!(decode_hex(&split), Err(InvalidHash::NotAscii));
        assert_eq!(encode_hex(&[]), "");
        assert_eq!(decode_hex("").expect("empty"), Vec::<u8>::new());
    }

    #[test]
    fn a_digest_can_be_hashed_and_read_back() {
        let digest = Sha256::of(b"abc");
        assert_eq!(
            digest.to_hex(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            Sha256::from_hex(&digest.to_hex()).expect("round trip"),
            digest
        );
    }

    #[test]
    fn the_hash_matches_the_published_vectors() {
        // Aldo hashes audio content and release identities, so the vectors stay
        // pinned: a digests change is a silent break of every stored dedupe key.
        let hex_of = |bytes: &[u8]| Sha256::of(bytes).to_hex();

        assert_eq!(
            hex_of(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            hex_of(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            hex_of(b"The quick brown fox jumps over the lazy dog"),
            "d7a8fbb307d7809469ca9abcb0082e4f8d5651e46d3cdb762d02d0bf37c9e592"
        );
    }

    #[test]
    fn the_hash_handles_input_spanning_several_blocks() {
        // 1000 bytes is 15 whole blocks plus one, so this covers both the
        // multi-block loop and the length padding.
        assert_eq!(
            Sha256::of(&[b'a'; 1000]).to_hex(),
            "41edece42d63e8d9bf515a9ba6932e1c20cbc9f5a5d134645adb5db1b9737ea3"
        );
    }
}
