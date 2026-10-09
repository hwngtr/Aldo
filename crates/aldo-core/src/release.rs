//! Release identity: the abstract work (master) and its individual pressings
//! (releases), as described by Discogs, plus the parsed tracklist and credits.
//!
//! Discogs reports the release type inside `formats[].descriptions`, not as a
//! dedicated field, and reports positions as opaque strings like `"A"` or
//! `"1-3"`. Both are parsed here once so no other module deals in raw strings.

use std::fmt::{self, Display, Formatter};

use serde::{Deserialize, Serialize};

use crate::audio::DurationMs;
use crate::ids::{
    ArtistId, CreditId, DiscogsArtistId, DiscogsLabelId, DiscogsMasterId, DiscogsReleaseId,
    LabelId, MasterId, MusicBrainzId, ReleaseId, ReleaseTrackId,
};

/// Declares a Discogs vocabulary enum together with the value it takes in a
/// `TEXT` column, and the parse that reads it back.
///
/// Both directions come from one table. `ReleaseType` spells `AudioDrama` as
/// `audiodrama` for Vorbis and as `audio_drama` for serde, so the pair cannot be
/// derived, but it is still a bijection: writing it as two matches means a
/// renamed value compiles cleanly in `as_vorbis_value`, stops reading back in
/// `from_vorbis_value`, and every release stored under the old spelling is then
/// reported as corrupt.
macro_rules! vorbis_table {
    (
        $(#[$attr:meta])*
        $name:ident => { $($variant:ident => $text:literal),+ $(,)? }
    ) => {
        $(#[$attr])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name {
            $($variant),+
        }

        impl $name {
            /// Every variant. A round trip is checked against this, so a variant
            /// added without a test entry cannot be missed.
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];

            /// The value written to the `RELEASETYPE` Vorbis comment, e.g.
            /// `album;compilation`.
            pub fn as_vorbis_value(self) -> &'static str {
                match self {
                    $(Self::$variant => $text),+
                }
            }

            /// Reads back what [`Self::as_vorbis_value`] writes.
            ///
            /// Deliberately not derived from serde: the enum's own `snake_case`
            /// form spells `AudioDrama` as `audio_drama`, which is not the
            /// Vorbis value.
            #[must_use]
            pub fn from_vorbis_value(value: &str) -> Option<Self> {
                match value {
                    $($text => Some(Self::$variant),)+
                    _ => None,
                }
            }
        }
    };
}

/// Declares a Discogs vocabulary enum that is also stored in a `TEXT` column,
/// generating the stored spelling, the parse back, and `ALL`.
///
/// The stored spelling coincides with the enum's serde `snake_case` name for
/// every variant below, so the two must not drift: `MediaFormat::DigitalMedia`
/// is `digital_media` on the wire and in the column because that is one word
/// spelled one way, and the table is where that stays true. Written as two
/// matches, a renamed variant compiles cleanly in `as_stored_str`, stops
/// reading back in `from_stored_str`, and every row stored under the old
/// spelling becomes corruption.
macro_rules! stored_table {
    (
        $(#[$attr:meta])*
        $name:ident => { $($variant:ident => $text:literal),+ $(,)? }
    ) => {
        $(#[$attr])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name {
            $($variant),+
        }

        impl $name {
            /// Every variant. A round trip is checked against this, so a
            /// variant added without a test entry cannot be missed.
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];

            /// The value stored in the `TEXT` column.
            pub fn as_stored_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $text),+
                }
            }

            /// Reads back what [`Self::as_stored_str`] writes. An unrecognised
            /// value is corruption in a row, never a variant to guess at.
            pub fn from_stored_str(text: &str) -> Option<Self> {
                match text {
                    $($text => Some(Self::$variant),)+
                    _ => None,
                }
            }
        }
    };
}

stored_table!(
    /// Discogs release status.
    /// The Discogs approval status of a release entry.
    ///
    /// This is about the Discogs data, not about the record. Whether a pressing
    /// is a promo, a bootleg or an official release is a separate fact that
    /// Discogs records in `formats[].descriptions`; it is not this field, and
    /// must not be derived from it.
    ReleaseStatus => {
        Accepted => "accepted",
        Draft => "draft",
        Deleted => "deleted",
        Rejected => "rejected",
        Other => "other",
    }
);

stored_table!(
    /// Discogs medium format, taken from `formats[].name`.
    MediaFormat => {
        Cd => "cd",
        Vinyl => "vinyl",
        Cassette => "cassette",
        DigitalMedia => "digital_media",
        File => "file",
        Dvd => "dvd",
        Other => "other",
    }
);

stored_table!(
    /// Discogs `tracklist[].type_`.
    TrackKind => {
        Track => "track",
        Heading => "heading",
        Index => "index",
    }
);

vorbis_table!(
    /// Discogs primary and secondary release types. A release can carry several:
    /// an album that is also a compilation is `album;compilation`.
    ReleaseType => {
        Album => "album",
        Single => "single",
        Ep => "ep",
        Broadcast => "broadcast",
        AudioDrama => "audiodrama",
        Audiobook => "audiobook",
        Compilation => "compilation",
        Demo => "demo",
        DjMix => "djmix",
        FieldRecording => "fieldrecording",
        Interview => "interview",
        Live => "live",
        MixtapeStreet => "mixtape",
        Remix => "remix",
        Soundtrack => "soundtrack",
        Spokenword => "spokenword",
        Other => "other",
    }
);

impl ReleaseType {
    /// Maps one Discogs `formats[].descriptions[]` string to a release type.
    pub fn from_discogs_description(description: &str) -> Option<Self> {
        let normalised = description
            .trim()
            .to_ascii_lowercase()
            .replace([' ', '-'], "");
        match normalised.as_str() {
            "album" | "fullalbum" => Some(Self::Album),
            "single" => Some(Self::Single),
            "ep" => Some(Self::Ep),
            "compilation" => Some(Self::Compilation),
            "mixtape" | "mixtape/street" => Some(Self::MixtapeStreet),
            "djmix" | "djmix/mix" => Some(Self::DjMix),
            "live" => Some(Self::Live),
            "remix" => Some(Self::Remix),
            "soundtrack" => Some(Self::Soundtrack),
            "audiobook" => Some(Self::Audiobook),
            "audiodrama" | "dramacon" => Some(Self::AudioDrama),
            "demo" => Some(Self::Demo),
            "fieldrecording" => Some(Self::FieldRecording),
            "interview" => Some(Self::Interview),
            "spokenword" => Some(Self::Spokenword),
            "broadcast" => Some(Self::Broadcast),
            _ => None,
        }
    }
}

impl MediaFormat {
    pub fn from_discogs_name(name: &str) -> Self {
        match name.trim().to_ascii_lowercase().as_str() {
            "cd" => Self::Cd,
            "vinyl" | "12\" vinyl" => Self::Vinyl,
            "cassette" => Self::Cassette,
            "digital media" => Self::DigitalMedia,
            "file" | "flac" | "wav" => Self::File,
            "dvd" => Self::Dvd,
            _ => Self::Other,
        }
    }
}

/// A vinyl side letter. Discogs uses only `A` through `D`, so any other letter
/// is a position this crate cannot interpret rather than a fifth side.
///
/// Serialises as the bare letter, exactly as Discogs writes it, so a stored
/// side and a stored position string mean the same thing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Side {
    A,
    B,
    C,
    D,
}

impl Side {
    /// Parses a side letter, case-insensitively. Returns `None` for anything
    /// outside `A`-`D`.
    pub fn from_letter(letter: char) -> Option<Self> {
        match letter.to_ascii_uppercase() {
            'A' => Some(Self::A),
            'B' => Some(Self::B),
            'C' => Some(Self::C),
            'D' => Some(Self::D),
            _ => None,
        }
    }

    /// Parses a one-based side index, as Discogs writes in `"1-1-2"`.
    pub const fn from_index(index: u8) -> Option<Self> {
        match index {
            1 => Some(Self::A),
            2 => Some(Self::B),
            3 => Some(Self::C),
            4 => Some(Self::D),
            _ => None,
        }
    }

    pub const fn letter(self) -> char {
        match self {
            Self::A => 'A',
            Self::B => 'B',
            Self::C => 'C',
            Self::D => 'D',
        }
    }

    /// The one-based index used in Discogs' `"disc-side-position"` form.
    pub const fn index(self) -> u8 {
        match self {
            Self::A => 1,
            Self::B => 2,
            Self::C => 3,
            Self::D => 4,
        }
    }
}

impl Display for Side {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::A => "A",
            Self::B => "B",
            Self::C => "C",
            Self::D => "D",
        })
    }
}

/// Where a track sits within a release, after parsing Discogs' opaque
/// `position` string.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TrackPosition {
    /// `"1-3"` — disc one, track three.
    DiscTrack { disc: u8, track: u8 },
    /// `"A"` or `"1-A"` — a vinyl side.
    Side { disc: u8, side: Side },
    /// Anything this parser does not recognise. Retained verbatim so a later
    /// re-tag does not silently drop the original value.
    Unparsed { raw: String },
}

impl TrackPosition {
    /// Parses Discogs' `position` field.
    ///
    /// Accepted shapes: `"3"` (track), `"A"` (side), `"2C"` (disc two, side C),
    /// `"1-3"` (disc one, track three), `"1-A"` (disc one, side A) and
    /// `"1-1-2"` (disc, side, position). Anything else is kept verbatim.
    pub fn parse(raw: &str) -> Self {
        let cleaned = raw.trim();
        if cleaned.is_empty() {
            return Self::Unparsed { raw: String::new() };
        }
        if let Some(compact) = Self::parse_compact(cleaned) {
            return compact;
        }

        let mut parts = cleaned.split('-');
        let first = parts.next().unwrap_or(cleaned);
        let second = parts.next();

        if let (Some(second), Some(third)) = (second, parts.next()) {
            // "disc-side-position", where the middle component is the side index.
            return match (
                first.parse::<u8>(),
                second.parse::<u8>(),
                third.parse::<u8>(),
            ) {
                (Ok(disc), Ok(side), Ok(_position)) if disc > 0 => match Side::from_index(side) {
                    Some(side) => Self::Side { disc, side },
                    None => Self::Unparsed {
                        raw: cleaned.into(),
                    },
                },
                _ => Self::Unparsed {
                    raw: cleaned.into(),
                },
            };
        }

        // A single component, or a "disc-track" / "disc-side" pair.
        let Some(second) = second else {
            return match first.parse::<u8>() {
                Ok(track) if track > 0 => Self::DiscTrack { disc: 1, track },
                _ => match single_side(first) {
                    Some(side) => Self::Side { disc: 1, side },
                    None => Self::Unparsed {
                        raw: cleaned.into(),
                    },
                },
            };
        };

        let disc = match first.parse::<u8>() {
            Ok(disc) if disc > 0 => disc,
            _ => {
                return Self::Unparsed {
                    raw: cleaned.into(),
                };
            }
        };

        match second.parse::<u8>() {
            Ok(track) if track > 0 => Self::DiscTrack { disc, track },
            _ => match single_side(second) {
                Some(side) => Self::Side { disc, side },
                None => Self::Unparsed {
                    raw: cleaned.into(),
                },
            },
        }
    }

    /// Parses the compact form Discogs uses when a disc number is glued to its
    /// side letter, e.g. `"2C"`. Returns `None` for anything else.
    pub fn parse_compact(raw: &str) -> Option<Self> {
        let cleaned = raw.trim();
        let (digits, rest) = cleaned.split_at(cleaned.len().checked_sub(1)?);
        if digits.is_empty() {
            return None;
        }
        let disc = digits.parse::<u8>().ok()?;
        if disc == 0 {
            return None;
        }
        let mut chars = rest.chars();
        match (chars.next(), chars.next()) {
            (Some(letter), None) => Some(Self::Side {
                disc,
                side: Side::from_letter(letter)?,
            }),
            _ => None,
        }
    }

    pub fn disc_number(&self) -> u8 {
        match self {
            Self::DiscTrack { disc, .. } | Self::Side { disc, .. } => *disc,
            Self::Unparsed { .. } => 1,
        }
    }

    /// The ordinal used for `TRACKNUMBER`. A side contributes its own index
    /// (A is 1, B is 2), which is the closest a bare side letter can get to a
    /// track number; `Unparsed` contributes none.
    pub fn track_number(&self) -> Option<u8> {
        match self {
            Self::DiscTrack { track, .. } => Some(*track),
            Self::Side { side, .. } => Some(side.index()),
            Self::Unparsed { .. } => None,
        }
    }
}

fn single_side(token: &str) -> Option<Side> {
    let mut chars = token.chars();
    match (chars.next(), chars.next()) {
        (Some(letter), None) => Side::from_letter(letter),
        _ => None,
    }
}

/// Who is credited, and the variation of that name as printed.
///
/// Shared by `ArtistCredit` and `ReleaseCredit`: both name an artist, both may
/// carry the Discogs artist id, and both may carry the printed variation, and
/// each of those means the same thing in both places. The two enclosing
/// records stay separate, because an ordered album-artist list and a
/// role-scoped credit are different things with different invariants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtistRef {
    /// The local row id. Zero until the artist is stored: a mapped release has
    /// no local ids yet.
    pub artist: ArtistId,
    pub discogs_artist_id: Option<DiscogsArtistId>,
    /// The canonical name Discogs publishes for this artist.
    ///
    /// Carried here because it is the name an `artist` row stores. Without it a
    /// mapping describes an artist it cannot name.
    pub name: String,
    /// The credit exactly as printed, taken from Discogs' `anv` when it differs.
    pub name_variation: Option<String>,
}

impl ArtistRef {
    /// Builds a reference from a Discogs credit. Discogs sends an empty `anv`
    /// for an artist whose name is printed normally, so an empty variation is
    /// absent rather than an empty string.
    pub fn from_discogs_credit(
        artist: ArtistId,
        discogs_artist_id: Option<DiscogsArtistId>,
        name: impl Into<String>,
        anv: Option<&str>,
    ) -> Self {
        Self {
            artist,
            discogs_artist_id,
            name: name.into(),
            name_variation: anv.map(str::to_owned).filter(|v| !v.is_empty()),
        }
    }
}

/// A release-level artist credit, or a track-level one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtistCredit {
    pub artist: ArtistRef,
    /// Join phrase joining this credit to the next, e.g. `" feat. "`.
    pub join_phrase: Option<String>,
    pub position: u8,
    pub is_album_artist: bool,
}

/// A credit such as producer or mixer, scoped either to the whole release or to
/// a subset of tracks. Discogs scopes these with a free-text `tracks` string
/// like `"3, 7-9"`; an empty string means the whole release.
///
/// The role stays a raw string because Discogs sends comma-separated values
/// such as `"Producer, Written-By"`, and this crate has no opinion about which
/// of them are taggable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseCredit {
    pub credit_id: Option<CreditId>,
    pub release_id: ReleaseId,
    /// The position of the tracklist entry this credit is nested in, when it is
    /// a track-level credit. A local `ReleaseTrackId` cannot be used: a release
    /// has no local ids until it is stored, exactly as for
    /// [`ReleaseTrack::parent_position`].
    pub track_position: Option<String>,
    pub artist: ArtistRef,
    pub role: String,
    /// The raw Discogs scope, kept verbatim for round-tripping. `None` and
    /// `Some("")` both mean the whole release.
    pub tracks_scope_raw: Option<String>,
}

/// One entry from a release tracklist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseTrack {
    pub track_id: ReleaseTrackId,
    pub release_id: ReleaseId,
    pub position: TrackPosition,
    pub title: String,
    pub kind: TrackKind,
    pub duration: Option<DurationMs>,
    /// Position of the `TrackKind::Index` entry that groups this track, when
    /// the release nests its tracklist. Referenced by position rather than by
    /// local id, because a release has no local ids until it is first stored.
    pub parent_position: Option<String>,
}

/// A label credit with its catalogue number or barcode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseIdentifier {
    pub label: Option<LabelId>,
    pub discogs_label_id: Option<DiscogsLabelId>,
    pub label_name: String,
    pub catalog_number: Option<String>,
    pub barcode: Option<String>,
}

/// A Discogs release: one physical or digital pressing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseIdentity {
    pub release_id: ReleaseId,
    pub discogs_release_id: DiscogsReleaseId,
    pub master_id: Option<MasterId>,
    pub discogs_master_id: Option<DiscogsMasterId>,
    pub musicbrainz_release_id: Option<MusicBrainzId>,
    pub title: String,
    pub artists: Vec<ArtistCredit>,
    pub release_types: Vec<ReleaseType>,
    pub media: Option<MediaFormat>,
    pub status: ReleaseStatus,
    pub year: Option<u16>,
    pub country: Option<String>,
    pub disc_count: u8,
    pub duration: Option<DurationMs>,
    pub tracklist: Vec<ReleaseTrack>,
    pub credits: Vec<ReleaseCredit>,
    pub identifiers: Vec<ReleaseIdentifier>,
    pub genres: Vec<String>,
    pub styles: Vec<String>,
    pub cover_art_url: Option<String>,
    pub popularity: Option<u32>,
}

impl ReleaseIdentity {
    /// Tracks that receive a `TRACKNUMBER`. Headings and index rows do not.
    pub fn playable_tracks(&self) -> impl Iterator<Item = &ReleaseTrack> {
        self.tracklist.iter().filter(|t| t.kind == TrackKind::Track)
    }

    pub fn is_compilation(&self) -> bool {
        self.release_types.contains(&ReleaseType::Compilation)
    }

    /// The value Picard writes to `RELEASETYPE`: primary type first, then the
    /// rest alphabetically.
    pub fn release_type_vorbis_value(&self) -> String {
        let mut types = self.release_types.clone();
        if types.is_empty() {
            return ReleaseType::Other.as_vorbis_value().to_owned();
        }
        let primary = types.remove(0);
        let mut rest: Vec<&str> = types
            .iter()
            .filter(|t| **t != primary)
            .map(|t| t.as_vorbis_value())
            .collect();
        rest.sort_unstable();
        rest.dedup();
        let mut value = String::from(primary.as_vorbis_value());
        for secondary in rest {
            value.push(';');
            value.push_str(secondary);
        }
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_single_numeric_position() {
        assert_eq!(
            TrackPosition::parse("3"),
            TrackPosition::DiscTrack { disc: 1, track: 3 }
        );
    }

    #[test]
    fn parses_vinyl_side_position() {
        assert_eq!(
            TrackPosition::parse("A"),
            TrackPosition::Side {
                disc: 1,
                side: Side::A
            }
        );
        assert_eq!(
            TrackPosition::parse("2C"),
            TrackPosition::Side {
                disc: 2,
                side: Side::C
            }
        );
    }

    #[test]
    fn parses_disc_side_position() {
        assert_eq!(
            TrackPosition::parse("1-A"),
            TrackPosition::Side {
                disc: 1,
                side: Side::A
            }
        );
        assert_eq!(
            TrackPosition::parse("2-2-1"),
            TrackPosition::Side {
                disc: 2,
                side: Side::B
            }
        );
    }

    #[test]
    fn a_letter_outside_a_to_d_is_not_a_side() {
        // Discogs has no side E. Retaining it as a side would make
        // `track_number` report 4 for both D and E.
        assert_eq!(
            TrackPosition::parse("2Z"),
            TrackPosition::Unparsed { raw: "2Z".into() }
        );
        assert_eq!(
            TrackPosition::parse("Z"),
            TrackPosition::Unparsed { raw: "Z".into() }
        );
        assert_eq!(
            TrackPosition::parse("1-5-1"),
            TrackPosition::Unparsed {
                raw: "1-5-1".into()
            }
        );
        assert_eq!(Side::from_letter('a'), Some(Side::A));
        assert_eq!(Side::from_letter('Z'), None);
        assert_eq!(Side::D.index(), 4);
        assert_eq!(Side::from_index(4), Some(Side::D));
        assert_eq!(Side::from_index(5), None);
    }

    #[test]
    fn a_side_prints_as_the_letter_discogs_wrote() {
        // The round trip back to a stored position string must not change shape.
        assert_eq!(Side::A.to_string(), "A");
        assert_eq!(format!("2{}", Side::C), "2C");
        assert_eq!(format!("1-{}", Side::B), "1-B");
    }

    #[test]
    fn parses_disc_qualified_track_position() {
        assert_eq!(
            TrackPosition::parse("1-3"),
            TrackPosition::DiscTrack { disc: 1, track: 3 }
        );
        assert_eq!(
            TrackPosition::parse("2-11"),
            TrackPosition::DiscTrack { disc: 2, track: 11 }
        );
    }

    #[test]
    fn retains_unrecognised_positions_verbatim() {
        assert_eq!(
            TrackPosition::parse("Bonus"),
            TrackPosition::Unparsed {
                raw: "Bonus".into()
            }
        );
        assert_eq!(
            TrackPosition::parse("0-2"),
            TrackPosition::Unparsed { raw: "0-2".into() }
        );
        assert_eq!(
            TrackPosition::parse(""),
            TrackPosition::Unparsed { raw: String::new() }
        );
    }

    #[test]
    fn maps_release_types_from_discogs_descriptions() {
        assert_eq!(
            ReleaseType::from_discogs_description("Album"),
            Some(ReleaseType::Album)
        );
        assert_eq!(ReleaseType::from_discogs_description("P/Mixed"), None);
        assert_eq!(
            ReleaseType::from_discogs_description("DJ-mix"),
            Some(ReleaseType::DjMix)
        );
        assert_eq!(ReleaseType::from_discogs_description("2 x LP"), None);
    }

    #[test]
    fn release_type_vorbis_value_puts_primary_first_and_sorts_rest() {
        let release = ReleaseIdentity {
            release_id: ReleaseId(1),
            discogs_release_id: DiscogsReleaseId(1),
            master_id: None,
            discogs_master_id: None,
            musicbrainz_release_id: None,
            title: "t".into(),
            artists: vec![],
            release_types: vec![
                ReleaseType::Album,
                ReleaseType::Soundtrack,
                ReleaseType::Compilation,
            ],
            media: None,
            status: ReleaseStatus::Accepted,
            year: None,
            country: None,
            disc_count: 1,
            duration: None,
            tracklist: vec![],
            credits: vec![],
            identifiers: vec![],
            genres: vec![],
            styles: vec![],
            cover_art_url: None,
            popularity: None,
        };
        assert_eq!(
            release.release_type_vorbis_value(),
            "album;compilation;soundtrack"
        );
        assert!(release.is_compilation());
    }

    #[test]
    fn media_format_maps_unknown_names_to_other() {
        assert_eq!(MediaFormat::from_discogs_name("CD"), MediaFormat::Cd);
        assert_eq!(
            MediaFormat::from_discogs_name("12\" Vinyl"),
            MediaFormat::Vinyl
        );
        assert_eq!(
            MediaFormat::from_discogs_name("Betamax"),
            MediaFormat::Other
        );
    }

    #[test]
    fn a_release_medium_attribute_is_not_a_release_type() {
        // `P/Mixed` and `2 x LP` sit in the same descriptions list as `Album`.
        assert_eq!(ReleaseType::from_discogs_description("P/Mixed"), None);
        assert_eq!(ReleaseType::from_discogs_description("2 x LP"), None);
        assert_eq!(
            ReleaseType::from_discogs_description("Album"),
            Some(ReleaseType::Album)
        );
    }

    #[test]
    fn every_release_type_reads_back_from_its_vorbis_value() {
        for release_type in ReleaseType::ALL {
            assert_eq!(
                ReleaseType::from_vorbis_value(release_type.as_vorbis_value()),
                Some(*release_type),
                "{release_type:?} does not read back from its own Vorbis value"
            );
        }
    }

    #[test]
    fn no_two_release_types_share_a_vorbis_value() {
        // Two variants under one spelling would make the second unreachable
        // on the way back out, and the round trip above would pass anyway.
        let mut seen: Vec<&str> = ReleaseType::ALL
            .iter()
            .map(|t| t.as_vorbis_value())
            .collect();
        let total = seen.len();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(
            seen.len(),
            total,
            "two release types share one Vorbis value"
        );
    }

    #[test]
    fn the_vorbis_value_is_not_the_serde_name() {
        // The two spellings differ, which is why `from_vorbis_value` exists
        // instead of a serde round trip.
        assert_eq!(ReleaseType::AudioDrama.as_vorbis_value(), "audiodrama");
        assert_eq!(ReleaseType::from_vorbis_value("audio_drama"), None);
        assert_eq!(ReleaseType::from_vorbis_value("hologram"), None);
    }

    /// The `stored_table!` enums are serialised as `snake_case` and written to a
    /// `TEXT` column under the same spelling. That is one word spelled one way,
    /// so a variant that drifts between the two is a bug the table should catch.
    ///
    /// Driven from the real serialisation rather than a restated list, so it
    /// cannot drift from the types themselves.
    macro_rules! assert_stored_spelling {
        ($($name:ident),+ $(,)?) => {$(
            for variant in $name::ALL {
                let text = variant.as_stored_str();
                let wire = serde_json::to_string(variant).expect("serialises");
                assert_eq!(
                    wire,
                    format!("\"{text}\""),
                    "{text} is stored under one spelling and serialised as another"
                );
                assert_eq!(
                    $name::from_stored_str(text),
                    Some(*variant),
                    "{text} does not read back from its own spelling"
                );
            }
        )+};
    }

    #[test]
    fn every_stored_discogs_enum_spells_one_word_one_way() {
        assert_stored_spelling!(ReleaseStatus, MediaFormat, TrackKind);
    }

    #[test]
    fn a_stored_vocabulary_value_outside_the_table_is_absent_not_guessed() {
        // A `CHECK` constraint makes these unreachable through the typed write
        // path, but the read still has to refuse rather than fall back.
        for text in ["banana", "", "TRACK", "pseudo release"] {
            assert_eq!(TrackKind::from_stored_str(text), None, "accepted {text:?}");
            assert_eq!(
                MediaFormat::from_stored_str(text),
                None,
                "accepted {text:?}"
            );
            assert_eq!(
                ReleaseStatus::from_stored_str(text),
                None,
                "accepted {text:?}"
            );
        }
    }

    /// `release_track.kind` is checked against these three names in the
    /// migration, so a fourth variant needs the constraint widened at the same
    /// time as this table.
    #[test]
    fn the_track_kind_table_is_the_one_the_schema_allows() {
        let stored: Vec<&str> = TrackKind::ALL.iter().map(|k| k.as_stored_str()).collect();
        assert_eq!(stored, vec!["track", "heading", "index"]);
    }

    #[test]
    fn artist_ref_and_release_credit_agree_on_a_shared_reference() {
        let artist = ArtistRef::from_discogs_credit(
            ArtistId(3),
            Some(DiscogsArtistId(9)),
            "Nina Kraviz",
            Some("NK 2"),
        );
        let credit = ReleaseCredit {
            credit_id: None,
            release_id: ReleaseId(1),
            track_position: None,
            artist: artist.clone(),
            role: "Producer".into(),
            tracks_scope_raw: None,
        };
        assert_eq!(credit.artist, artist);
        assert_eq!(credit.artist.name, "Nina Kraviz");
        assert_eq!(credit.artist.name_variation.as_deref(), Some("NK 2"));
    }

    #[test]
    fn an_artist_ref_keeps_the_canonical_name_separate_from_the_printed_variation() {
        // The canonical name is what an `artist` row stores; the variation is
        // what the release printed. Losing either makes a mapping unusable.
        let plain = ArtistRef::from_discogs_credit(ArtistId(0), None, "Radiohead", None);
        assert_eq!(plain.name, "Radiohead");
        assert_eq!(plain.name_variation, None, "an empty `anv` is absent");

        let varied = ArtistRef::from_discogs_credit(ArtistId(0), None, "Radiohead", Some(""));
        assert_eq!(varied.name_variation, None);

        let printed =
            ArtistRef::from_discogs_credit(ArtistId(0), None, "Radiohead", Some("RADIOHEAD"));
        assert_eq!(printed.name, "Radiohead");
        assert_eq!(printed.name_variation.as_deref(), Some("RADIOHEAD"));
    }
}
