//! User search input and its parsed form.

use serde::{Deserialize, Serialize};

use crate::audio::{BitDepth, ByteSize};

/// What the user typed, before interpretation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawQuery {
    pub text: String,
}

impl RawQuery {
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into() }
    }

    pub fn as_str(&self) -> &str {
        self.text.trim()
    }

    pub fn is_blank(&self) -> bool {
        self.as_str().is_empty()
    }
}

/// The filters a search can carry. FLAC-only is not optional: every other
/// format is discarded before a result reaches the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SearchFilters {
    /// Reject anything that is not FLAC.
    pub lossless_only: bool,
    /// Require at least this bit depth. `None` accepts any width.
    pub minimum_bit_depth: Option<BitDepth>,
    /// Reject results larger than this. Guards against discography dumps.
    pub maximum_total_bytes: Option<ByteSize>,
    /// Drop sources with no free capacity or zero peers.
    pub require_available_source: bool,
}

impl Default for SearchFilters {
    fn default() -> Self {
        Self {
            lossless_only: true,
            minimum_bit_depth: BitDepth::new(16),
            maximum_total_bytes: None,
            require_available_source: true,
        }
    }
}

impl SearchFilters {
    /// The widest search the app permits: FLAC, any bit depth, any size.
    pub fn permissive() -> Self {
        Self {
            lossless_only: true,
            minimum_bit_depth: None,
            maximum_total_bytes: None,
            require_available_source: false,
        }
    }

    pub fn permits_bit_depth(&self, depth: BitDepth) -> bool {
        self.minimum_bit_depth.is_none_or(|min| depth >= min)
    }
}

/// A parsed query. `artist` and `album` stay optional because a bare phrase is
/// a legitimate search, and inventing a split would silently mislabel results.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchQuery {
    pub phrase: String,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub year: Option<u16>,
    pub filters: SearchFilters,
}

impl SearchQuery {
    /// Parses `"Artist - Album"`, `"Artist - Album - 1997"` and bare phrases.
    ///
    /// Deliberately narrow: an unrecognised shape yields a phrase-only query
    /// rather than a guessed split.
    pub fn parse(raw: &RawQuery, filters: SearchFilters) -> Option<Self> {
        let text = raw.as_str();
        if text.is_empty() {
            return None;
        }

        let segments = split_on_separator(text);
        let year = segments
            .iter()
            .find_map(|segment| match segment.trim().parse::<u16>() {
                Ok(year) if (1_800..=2_100).contains(&year) => Some(year),
                _ => None,
            });

        let remainder: Vec<&str> = segments
            .iter()
            .filter(|segment| segment.trim().parse::<u16>().is_err())
            .map(|segment| segment.trim())
            .filter(|segment| !segment.is_empty())
            .collect();

        let (artist, album) = match remainder.as_slice() {
            [] => (None, None),
            [only] => (None, Some((*only).to_owned())),
            [first, rest @ ..] => {
                let album = rest.join(" ");
                (Some((*first).to_owned()), Some(album))
            }
        };

        Some(Self {
            phrase: text.to_owned(),
            artist,
            album,
            year,
            filters,
        })
    }

    /// The best string to hand a metadata provider. Discogs handles either the
    /// structured or the free-form form, but the structured form disambiguates
    /// `artist` from `release_title`.
    pub fn discogs_release_title(&self) -> Option<&str> {
        self.album.as_deref().or(Some(self.phrase.as_str()))
    }

    /// The SoulSeek query string. The protocol has no field syntax, so tokens
    /// are plain words and no filter hints are leaked to peers.
    pub fn soulseek_query(&self) -> String {
        match (&self.artist, &self.album) {
            (Some(artist), Some(album)) => format!("{artist} {album}"),
            (None, Some(album)) => album.clone(),
            (Some(artist), None) => artist.clone(),
            (None, None) => self.phrase.clone(),
        }
    }
}

/// Splits on a hyphen surrounded by whitespace, which is how people write
/// `"Artist - Album"`. A hyphen inside a word, as in `Jay-Z`, is left alone.
fn split_on_separator(text: &str) -> Vec<&str> {
    let mut segments = Vec::new();
    let bytes = text.as_bytes();
    let mut start = 0usize;

    for index in 1..bytes.len().saturating_sub(1) {
        if bytes[index] == b'-'
            && bytes[index - 1].is_ascii_whitespace()
            && bytes[index + 1].is_ascii_whitespace()
        {
            segments.push(text[start..index].trim());
            start = index + 1;
        }
    }
    segments.push(text[start..].trim());
    segments.retain(|segment| !segment.is_empty());
    segments
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> SearchQuery {
        SearchQuery::parse(&RawQuery::new(text), SearchFilters::default()).expect("query")
    }

    #[test]
    fn rejects_blank_input() {
        assert!(SearchQuery::parse(&RawQuery::new("   "), SearchFilters::default()).is_none());
    }

    #[test]
    fn parses_artist_dash_album_dash_year() {
        let query = parse("Radiohead - OK Computer - 1997");
        assert_eq!(query.artist.as_deref(), Some("Radiohead"));
        assert_eq!(query.album.as_deref(), Some("OK Computer"));
        assert_eq!(query.year, Some(1997));
    }

    #[test]
    fn parses_artist_dash_album_without_year() {
        let query = parse("Miles Davis - Kind of Blue");
        assert_eq!(query.artist.as_deref(), Some("Miles Davis"));
        assert_eq!(query.album.as_deref(), Some("Kind of Blue"));
        assert_eq!(query.year, None);
    }

    #[test]
    fn treats_bare_phrase_as_album_only() {
        let query = parse("OK Computer");
        assert_eq!(query.artist, None);
        assert_eq!(query.album.as_deref(), Some("OK Computer"));
        assert_eq!(query.year, None);
    }

    #[test]
    fn does_not_split_on_hyphen_inside_a_word() {
        let query = parse("Jay-Z - The Black Album");
        assert_eq!(query.artist.as_deref(), Some("Jay-Z"));
        assert_eq!(query.album.as_deref(), Some("The Black Album"));
    }

    #[test]
    fn ignores_years_outside_the_music_era() {
        let query = parse("Artist - Album - 999");
        assert_eq!(query.year, None);
        assert_eq!(query.album.as_deref(), Some("Album"));
    }

    #[test]
    fn soulseek_query_is_plain_words_with_no_filter_tokens() {
        let query = parse("Radiohead - OK Computer");
        assert_eq!(query.soulseek_query(), "Radiohead OK Computer");
        assert!(!query.soulseek_query().contains(':'));
    }

    #[test]
    fn default_filters_are_lossless_and_hide_weak_sources() {
        let filters = SearchFilters::default();
        assert!(filters.lossless_only);
        assert!(filters.require_available_source);
        assert!(filters.permits_bit_depth(BitDepth::new(24).expect("valid")));
        assert!(!filters.permits_bit_depth(BitDepth::new(8).expect("valid")));
        assert!(SearchFilters::permissive().permits_bit_depth(BitDepth::new(8).expect("valid")));
    }

    #[test]
    fn the_minimum_width_default_is_the_cd_standard() {
        assert_eq!(
            SearchFilters::default().minimum_bit_depth,
            BitDepth::new(16)
        );
    }
}
