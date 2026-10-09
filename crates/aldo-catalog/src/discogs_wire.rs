//! Discogs wire types.
//!
//! Field names mirror the API exactly, including `type_`, because serde maps
//! them without a rename attribute. Anything Aldo does not use is omitted rather
//! than deserialised and thrown away.

use serde::{Deserialize, Serialize};

/// Discogs has returned `year` as both a JSON number and a numeric string.
/// Keep the wire type strict while accepting both representations.
fn deserialize_flexible_year<'de, D>(deserializer: D) -> Result<Option<u16>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Year {
        Number(u16),
        Text(String),
    }

    Option::<Year>::deserialize(deserializer)?.map_or(Ok(None), |year| match year {
        Year::Number(value) => Ok(Some(value)),
        Year::Text(value) => value.parse().map(Some).map_err(serde::de::Error::custom),
    })
}

/// A token that authorises database searches. A plain string would be
/// accidentally printable or loggable; the wrapper carries that intent.
#[derive(Clone, PartialEq, Eq)]
pub struct DiscogsToken(String);

impl std::fmt::Debug for DiscogsToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DiscogsToken(redacted)")
    }
}

impl DiscogsToken {
    pub fn new(token: impl Into<String>) -> Self {
        Self(token.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

/// `/database/search` response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResponse {
    pub results: Vec<SearchResult>,
    /// Absent on some responses. `items` then reads as `None`, not as zero.
    #[serde(default)]
    pub pagination: PaginationResponse,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub id: u32,
    #[serde(default)]
    pub title: String,
    #[serde(default, deserialize_with = "deserialize_flexible_year")]
    pub year: Option<u16>,
    #[serde(default)]
    pub format: Vec<String>,
    #[serde(default)]
    pub label: Vec<String>,
    #[serde(default)]
    pub catno: Option<String>,
    #[serde(default)]
    pub country: Option<String>,
    #[serde(default)]
    pub genre: Vec<String>,
    #[serde(default)]
    pub style: Vec<String>,
    #[serde(default)]
    pub cover_image: Option<String>,
    #[serde(default)]
    pub master_id: Option<u32>,
    #[serde(rename = "type", default)]
    pub result_type: String,
}

impl SearchResult {
    /// The artist portion of a `"Artist - Album"` title.
    #[must_use]
    pub fn artist(&self) -> Option<&str> {
        self.title
            .split_once(" - ")
            .map(|(artist, _)| artist.trim())
    }

    #[must_use]
    pub fn album(&self) -> Option<&str> {
        self.title.split_once(" - ").map(|(_, album)| album.trim())
    }

    /// Release type tokens from the `format` strings, which look like
    /// `"CD, Album, P/Mixed"`.
    #[must_use]
    pub fn release_types(&self) -> Vec<aldo_core::ReleaseType> {
        self.format
            .iter()
            .flat_map(|entry| entry.split(','))
            .filter_map(aldo_core::ReleaseType::from_discogs_description)
            .collect()
    }

    /// True when the pressing is on a physical or digital medium rather than a
    /// bare file listing, which is what a FLAC rip corresponds to.
    #[must_use]
    pub fn is_physical_medium(&self) -> bool {
        self.format.iter().any(|entry| {
            // An entry with no comma is its own first part, so this needs no
            // fallback. An empty entry is a bare file listing, not a pressing.
            let first = entry
                .split_once(',')
                .map_or(entry.as_str(), |(first, _)| first)
                .trim();
            !first.is_empty()
                && !first.eq_ignore_ascii_case("file")
                && !first.eq_ignore_ascii_case("flac")
        })
    }
}

/// `/releases/{id}` response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseResponse {
    pub id: u32,
    #[serde(default)]
    pub master_id: Option<u32>,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub artists: Vec<ArtistCreditResponse>,
    #[serde(default)]
    pub tracklist: Vec<TrackResponse>,
    #[serde(default)]
    pub labels: Vec<LabelResponse>,
    #[serde(default)]
    pub identifiers: Vec<IdentifierResponse>,
    #[serde(default)]
    pub companies: Vec<CompanyResponse>,
    #[serde(default)]
    pub genres: Vec<String>,
    #[serde(default)]
    pub styles: Vec<String>,
    #[serde(default)]
    pub formats: Vec<FormatResponse>,
    #[serde(default)]
    pub status: Option<String>,
    /// A date such as `"1997-05-21"`, which is the only year Discogs reports
    /// on a release.
    #[serde(default)]
    pub released: Option<String>,
    #[serde(default)]
    pub country: Option<String>,
    #[serde(default)]
    pub images: Vec<ImageResponse>,
    #[serde(default)]
    pub community: Option<CommunityResponse>,
    #[serde(default)]
    pub data_quality: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtistCreditResponse {
    pub id: u32,
    pub name: String,
    #[serde(default)]
    pub anv: Option<String>,
    #[serde(default)]
    pub join: Option<String>,
    #[serde(default)]
    pub role: Option<String>,
    /// Which tracks a credit applies to: empty means the whole release, `"3,
    /// 7-9"` means those tracks, `"A"` means that side.
    #[serde(default)]
    pub tracks: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackResponse {
    pub position: String,
    pub title: String,
    #[serde(default)]
    pub duration: Option<String>,
    /// Discogs spells this with a trailing underscore to avoid its own
    /// reserved word.
    #[serde(rename = "type_", default)]
    pub track_type: Option<String>,
    #[serde(default)]
    pub extraartists: Vec<ArtistCreditResponse>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LabelResponse {
    pub id: u32,
    pub name: String,
    #[serde(default)]
    pub catno: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdentifierResponse {
    #[serde(rename = "type", default)]
    pub identifier_type: String,
    #[serde(default)]
    pub value: String,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompanyResponse {
    pub id: u32,
    pub name: String,
    #[serde(default)]
    pub catno: Option<String>,
    #[serde(default)]
    pub entity_type_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FormatResponse {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub qty: Option<String>,
    /// Release-type tokens such as `Album` are mixed in with medium attributes
    /// such as `P/Mixed` here. There is no separate type field.
    #[serde(default)]
    pub descriptions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageResponse {
    #[serde(rename = "type", default)]
    pub image_type: String,
    #[serde(default)]
    pub uri: Option<String>,
    #[serde(default)]
    pub resource_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommunityResponse {
    #[serde(default)]
    pub have: Option<u32>,
    #[serde(default)]
    pub want: Option<u32>,
}

/// Result counts for one `/database/search` call. `items` is the total match
/// count, not the length of the returned page.
///
/// `items` is absent rather than defaulted: a response without it has not said
/// how many matches exist, and reading the omission as zero would tell a caller
/// paging over results that the album does not exist.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PaginationResponse {
    #[serde(default)]
    pub page: u32,
    #[serde(default)]
    pub per_page: u32,
    #[serde(default)]
    pub items: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn search_result(title: &str, format: &[&str]) -> SearchResult {
        SearchResult {
            id: 1,
            title: title.to_owned(),
            year: None,
            format: format.iter().map(|f| (*f).to_owned()).collect(),
            label: vec![],
            catno: None,
            country: None,
            genre: vec![],
            style: vec![],
            cover_image: None,
            master_id: None,
            result_type: "release".to_owned(),
        }
    }

    #[test]
    fn splits_a_discogs_title_into_artist_and_album() {
        let result = search_result("Radiohead - OK Computer", &["CD"]);
        assert_eq!(result.artist(), Some("Radiohead"));
        assert_eq!(result.album(), Some("OK Computer"));
    }

    #[test]
    fn a_title_without_a_separator_is_whole() {
        let result = search_result("OK Computer", &["CD"]);
        assert_eq!(result.artist(), None);
        assert_eq!(result.album(), None);
    }

    #[test]
    fn release_types_come_from_the_format_strings() {
        let result = search_result(
            "Various - A Compilation",
            &["CD, Album, P/Mixed", "Compilation"],
        );
        let types = result.release_types();
        assert!(types.contains(&aldo_core::ReleaseType::Album));
        assert!(types.contains(&aldo_core::ReleaseType::Compilation));
    }

    #[test]
    fn recognises_a_physical_medium() {
        assert!(search_result("A - B", &["CD, Album"]).is_physical_medium());
        assert!(search_result("A - B", &["Vinyl, LP, Album"]).is_physical_medium());
        assert!(
            !search_result("A - B", &["File, FLAC"]).is_physical_medium(),
            "a bare file listing is not a pressing"
        );
        // `format` is `#[serde(default)]`, so an entry can arrive empty.
        assert!(!search_result("A - B", &[""]).is_physical_medium());
    }

    #[test]
    fn a_token_never_prints_its_value() {
        let token = DiscogsToken::new("secret-token");
        assert_eq!(format!("{token:?}"), "DiscogsToken(redacted)");
        assert_eq!(token.expose(), "secret-token");
    }

    #[test]
    fn deserialises_a_track_type_written_as_type_underscore() {
        let track: TrackResponse = serde_json::from_str(
            r#"{"position":"1","title":"Airbag","duration":"4:38","type_":"track"}"#,
        )
        .expect("parsed");
        assert_eq!(track.track_type.as_deref(), Some("track"));
        assert_eq!(track.duration.as_deref(), Some("4:38"));
    }

    #[test]
    fn tolerates_a_release_missing_optional_sections() {
        let release: ReleaseResponse =
            serde_json::from_str(r#"{"id":5,"title":"Minimal"}"#).expect("parsed");
        assert_eq!(release.id, 5);
        assert!(release.tracklist.is_empty());
        assert!(release.formats.is_empty());
        assert!(release.released.is_none());
    }

    #[test]
    fn a_search_response_reports_the_total_match_count_not_the_page_length() {
        let response: SearchResponse = serde_json::from_str(
            r#"{"results":[{"id":1,"title":"A - B"}],"pagination":{"page":2,"per_page":50,"items":417}}"#,
        )
        .expect("parsed");

        assert_eq!(response.results.len(), 1);
        assert_eq!(response.pagination.items, Some(417));
        assert_eq!(response.pagination.page, 2);
    }

    #[test]
    fn accepts_numeric_and_string_years_and_null() {
        for (year, expected) in [
            (r#"1997"#, Some(1997)),
            (r#""1997""#, Some(1997)),
            ("null", None),
        ] {
            let body = format!(r#"{{"id":1,"title":"A - B","year":{year}}}"#);
            let result: SearchResult = serde_json::from_str(&body).expect("parsed");
            assert_eq!(result.year, expected, "{body}");
        }
    }

    #[test]
    fn a_missing_match_count_is_absent_rather_than_zero() {
        // Reading an omitted `items` as zero tells a caller paging over the
        // results that no such release exists, which is a claim Discogs never
        // made. The page it did send still lists one hit.
        for body in [
            r#"{"results":[{"id":1,"title":"A - B"}]}"#,
            r#"{"results":[{"id":1,"title":"A - B"}],"pagination":{}}"#,
            r#"{"results":[{"id":1,"title":"A - B"}],"pagination":{"page":2,"per_page":50}}"#,
        ] {
            let response: SearchResponse = serde_json::from_str(body).expect("parsed");

            assert_eq!(response.pagination.items, None, "{body}");
            assert_eq!(response.results.len(), 1, "{body}");
        }
    }

    #[test]
    fn every_identifier_in_a_release_survives_deserialisation() {
        let release: ReleaseResponse = serde_json::from_str(
            r#"{"id":5,"title":"X","identifiers":[{"type":"Barcode","value":"0079243511322"}]}"#,
        )
        .expect("parsed");
        assert_eq!(release.identifiers.len(), 1);
        assert_eq!(release.identifiers[0].identifier_type, "Barcode");
        assert_eq!(release.identifiers[0].value, "0079243511322");
    }
}
