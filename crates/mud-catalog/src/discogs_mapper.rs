//! Turns a Discogs release payload into MuD's domain types.
//!
//! Every Discogs quirk is handled exactly once, here: the release type hiding
//! inside `formats[].descriptions`, the year hiding inside the `released` date,
//! comma-separated credit roles, and credit scopes expressed as free text.

use mud_core::audio::DurationMs;
use mud_core::ids::{
    ArtistId, DiscogsArtistId, DiscogsLabelId, DiscogsMasterId, DiscogsReleaseId, ReleaseId,
    ReleaseTrackId,
};
use mud_core::release::{
    ArtistCredit, ArtistRef, MediaFormat, ReleaseCredit, ReleaseIdentifier, ReleaseIdentity,
    ReleaseStatus, ReleaseTrack, ReleaseType, TrackKind, TrackPosition,
};

use crate::discogs_wire::{ArtistCreditResponse, ReleaseResponse};

/// Maps a Discogs release onto the domain. Local ids are placeholders: a
/// release is identified by its Discogs id until it is stored.
#[must_use]
pub fn to_identity(release: &ReleaseResponse) -> ReleaseIdentity {
    let release_types = collect_release_types(release);
    let tracks = build_tracklist(release);
    let disc_count = tracks
        .iter()
        .map(|t| t.position.disc_number())
        .max()
        .unwrap_or(1);

    ReleaseIdentity {
        release_id: ReleaseId(0),
        discogs_release_id: DiscogsReleaseId(release.id),
        master_id: None,
        discogs_master_id: release.master_id.map(DiscogsMasterId),
        musicbrainz_release_id: None,
        title: release.title.clone(),
        artists: release
            .artists
            .iter()
            .enumerate()
            .map(|(index, artist)| ArtistCredit {
                artist: ArtistRef::from_discogs_credit(
                    ArtistId(0),
                    Some(DiscogsArtistId(artist.id)),
                    &artist.name,
                    artist.anv.as_deref(),
                ),
                join_phrase: artist.join.clone().filter(|j| !j.is_empty()),
                position: u8::try_from(index).unwrap_or(u8::MAX),
                is_album_artist: true,
            })
            .collect(),
        release_types,
        media: release
            .formats
            .first()
            .map(|format| MediaFormat::from_discogs_name(&format.name)),
        status: parse_status(release.status.as_deref()),
        year: released_year(release.released.as_deref()),
        country: release
            .country
            .clone()
            .filter(|c| !c.is_empty() && c != "-"),
        disc_count,
        duration: total_duration(&tracks),
        credits: build_credits(release),
        identifiers: build_identifiers(release),
        tracklist: tracks,
        genres: release.genres.clone(),
        styles: release.styles.clone(),
        cover_art_url: front_cover(release),
        popularity: release.community.as_ref().and_then(|c| c.have),
    }
}

/// The release type is not a field of its own: Discogs hides it inside
/// `formats[].descriptions`, mixed in with medium attributes such as `P/Mixed`.
/// The first recognised description wins; a release with none is inferred from
/// its playable track count.
fn collect_release_types(release: &ReleaseResponse) -> Vec<ReleaseType> {
    let mut found: Vec<ReleaseType> = release
        .formats
        .iter()
        .flat_map(|format| format.descriptions.iter())
        .filter_map(|description| ReleaseType::from_discogs_description(description))
        .collect();
    found.dedup();

    if found.is_empty() {
        let playable = release
            .tracklist
            .iter()
            .filter(|track| {
                track.track_type.as_deref() == Some("track") || track.track_type.is_none()
            })
            .count();
        found.push(infer_release_type(playable));
    }
    found
}

/// Discogs states no type at all for some releases, so fall back to the count
/// of playable tracks. An empty tracklist means the release data is unusable,
/// not that it is a zero-track album, so it lands on the album default.
fn infer_release_type(track_count: usize) -> ReleaseType {
    match track_count {
        1 | 2 => ReleaseType::Single,
        3..=5 => ReleaseType::Ep,
        _ => ReleaseType::Album,
    }
}

fn build_tracklist(release: &ReleaseResponse) -> Vec<ReleaseTrack> {
    release
        .tracklist
        .iter()
        .map(|track| ReleaseTrack {
            track_id: ReleaseTrackId(0),
            release_id: ReleaseId(0),
            // Kept verbatim, including when MuD cannot parse it: `position_raw`
            // is the only place the original Discogs string survives, and a
            // re-tag must be able to reproduce it.
            position: TrackPosition::parse(&track.position),
            title: track.title.clone(),
            kind: parse_track_kind(track.track_type.as_deref()),
            duration: parse_discogs_duration(track.duration.as_deref()),
            parent_position: None,
        })
        .collect()
}

/// Only `type_ == "track"` is playable. Discogs also uses `heading` for section
/// labels and `index` for nested groups.
///
/// Deliberately not [`TrackKind::from_stored_str`], which reads the same three
/// words back out of the database. Two rules differ and both matter: Discogs
/// omits `type_` for an ordinary track, so an absent or unrecognised value is a
/// track here rather than corruption; and a value MuD does not know is a
/// section label, not a reason to drop the entry. Reading the column has the
/// opposite duty — there, an unknown word means a row this crate cannot read.
fn parse_track_kind(text: Option<&str>) -> TrackKind {
    match text {
        Some("heading") => TrackKind::Heading,
        Some("index") => TrackKind::Index,
        _ => TrackKind::Track,
    }
}

/// Discogs reports status in title case with a hyphen (`"Pseudo-Release"`),
/// where the stored form is `snake_case`, so this is a different spelling and
/// not a copy of [`ReleaseStatus::from_stored_str`]. Unrecognised is `Other`
/// rather than an error for the same reason as `parse_track_kind`.
/// Discogs reports whether its own entry is approved, not what kind of release
/// this is. Anything unrecognised is `Other` rather than a guess.
fn parse_status(text: Option<&str>) -> ReleaseStatus {
    match text {
        Some("Accepted") => ReleaseStatus::Accepted,
        Some("Draft") => ReleaseStatus::Draft,
        Some("Deleted") => ReleaseStatus::Deleted,
        Some("Rejected") => ReleaseStatus::Rejected,
        _ => ReleaseStatus::Other,
    }
}

/// Discogs reports a full date; MuD stores the year. Exactly four leading
/// digits are required, and the year must fall in the music era.
#[must_use]
pub fn released_year(released: Option<&str>) -> Option<u16> {
    let text = released?.trim();
    let digits: String = text.chars().take_while(char::is_ascii_digit).collect();
    if digits.len() != 4 {
        return None;
    }
    digits
        .parse()
        .ok()
        .filter(|year| (1800..=2100).contains(year))
}

/// Durations arrive as `"4:38"` or `"1:02:03"`, and are frequently absent.
#[must_use]
pub fn parse_discogs_duration(text: Option<&str>) -> Option<DurationMs> {
    let text = text?.trim();
    let mut total_secs: u64 = 0;

    // `split` always yields one segment, and an empty segment fails to parse,
    // so the loop either returns `None` or leaves a real duration behind.
    for part in text.split(':') {
        let value: u64 = part.trim().parse().ok()?;
        total_secs = total_secs.checked_mul(60)?.checked_add(value)?;
    }

    Some(DurationMs::from_millis(total_secs * 1_000))
}

fn total_duration(tracks: &[ReleaseTrack]) -> Option<DurationMs> {
    let total: u64 = tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Track)
        .filter_map(|t| t.duration.map(DurationMs::as_millis))
        .sum();
    (total > 0).then_some(DurationMs::from_millis(total))
}

fn build_credits(release: &ReleaseResponse) -> Vec<ReleaseCredit> {
    let mut credits = Vec::new();

    // A role on a release-level artist entry is a release-wide credit.
    for artist in &release.artists {
        let Some(role) = artist.role.clone().filter(|r| !r.is_empty()) else {
            continue;
        };
        credits.push(credit_from(artist, &role, None, artist.tracks.as_deref()));
    }

    // A credit nested in a tracklist entry belongs to that entry, so the entry's
    // position is the local scope. Discogs' own `tracks` field is kept verbatim
    // alongside it, because a scope like `"3, 7-9"` names several tracks and
    // fits no single one.
    for track in &release.tracklist {
        let position = (!track.position.is_empty()).then(|| track.position.clone());
        for artist in &track.extraartists {
            let Some(role) = artist.role.clone().filter(|r| !r.is_empty()) else {
                continue;
            };
            credits.push(credit_from(
                artist,
                &role,
                position.clone(),
                artist.tracks.as_deref(),
            ));
        }
    }

    credits
}

fn credit_from(
    artist: &ArtistCreditResponse,
    role: &str,
    track_position: Option<String>,
    scope: Option<&str>,
) -> ReleaseCredit {
    ReleaseCredit {
        credit_id: None,
        release_id: ReleaseId(0),
        track_position,
        artist: ArtistRef::from_discogs_credit(
            ArtistId(0),
            Some(DiscogsArtistId(artist.id)),
            &artist.name,
            artist.anv.as_deref(),
        ),
        role: role.to_owned(),
        tracks_scope_raw: scope.filter(|s| !s.is_empty()).map(str::to_owned),
    }
}

fn build_identifiers(release: &ReleaseResponse) -> Vec<ReleaseIdentifier> {
    let barcodes: Vec<String> = release
        .identifiers
        .iter()
        .filter(|identifier| identifier.identifier_type == "Barcode")
        .map(|identifier| identifier.value.clone())
        .collect();

    release
        .labels
        .iter()
        .map(|label| ReleaseIdentifier {
            label: None,
            discogs_label_id: Some(DiscogsLabelId(label.id)),
            label_name: label.name.clone(),
            catalog_number: label.catno.clone().filter(|c| !c.is_empty()),
            barcode: barcodes.first().cloned(),
        })
        .collect()
}

fn front_cover(release: &ReleaseResponse) -> Option<String> {
    release
        .images
        .iter()
        .find(|image| image.image_type == "primary")
        .or_else(|| release.images.first())
        .and_then(|image| image.resource_url.clone().or_else(|| image.uri.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discogs_wire::{ArtistCreditResponse, FormatResponse, TrackResponse};

    fn release(tracks: Vec<TrackResponse>) -> ReleaseResponse {
        ReleaseResponse {
            id: 1,
            master_id: Some(9),
            title: "OK Computer".into(),
            artists: vec![ArtistCreditResponse {
                id: 100,
                name: "Radiohead".into(),
                anv: None,
                join: None,
                role: None,
                tracks: None,
            }],
            tracklist: tracks,
            labels: vec![],
            identifiers: vec![],
            companies: vec![],
            genres: vec![],
            styles: vec![],
            formats: vec![],
            status: Some("Official".into()),
            released: Some("1997-05-21".into()),
            country: Some("GB".into()),
            images: vec![],
            community: None,
            data_quality: None,
            notes: None,
        }
    }

    fn track(position: &str, title: &str) -> TrackResponse {
        TrackResponse {
            position: position.into(),
            title: title.into(),
            duration: None,
            track_type: Some("track".into()),
            extraartists: vec![],
        }
    }

    #[test]
    fn reads_the_year_out_of_a_full_date() {
        assert_eq!(released_year(Some("1997-05-21")), Some(1997));
        assert_eq!(released_year(Some("1997")), Some(1997));
        assert_eq!(released_year(Some("n/a")), None);
        assert_eq!(released_year(Some("99-12-31")), None);
        assert_eq!(released_year(None), None);
    }

    #[test]
    fn rejects_a_year_outside_the_music_era() {
        assert_eq!(released_year(Some("1200-01-01")), None);
        assert_eq!(released_year(Some("2999-01-01")), None);
    }

    #[test]
    fn parses_durations_in_both_shapes_and_refuses_nonsense() {
        assert_eq!(
            parse_discogs_duration(Some("4:38")),
            Some(DurationMs::from_millis(278_000))
        );
        assert_eq!(
            parse_discogs_duration(Some("1:02:03")),
            Some(DurationMs::from_millis(3_723_000))
        );
        assert_eq!(parse_discogs_duration(Some("None")), None);
        assert_eq!(parse_discogs_duration(Some("")), None);
        assert_eq!(parse_discogs_duration(None), None);
    }

    #[test]
    fn extracts_the_release_type_from_a_format_description() {
        let mut subject = release(vec![track("1", "Airbag")]);
        subject.formats = vec![FormatResponse {
            name: "CD".into(),
            qty: Some("1".into()),
            descriptions: vec!["Album".into(), "P/Mixed".into()],
        }];

        let identity = to_identity(&subject);
        assert_eq!(identity.release_types, vec![ReleaseType::Album]);
        assert_eq!(identity.media, Some(MediaFormat::Cd));
    }

    #[test]
    fn infers_a_release_type_from_track_count_when_discogs_states_none() {
        let single = to_identity(&release(vec![track("1", "Only")]));
        assert_eq!(single.release_types, vec![ReleaseType::Single]);

        let ep = to_identity(&release(vec![
            track("1", "A"),
            track("2", "B"),
            track("3", "C"),
        ]));
        assert_eq!(ep.release_types, vec![ReleaseType::Ep]);

        let album = to_identity(&release(
            (1..=6).map(|n| track(&n.to_string(), "T")).collect(),
        ));
        assert_eq!(album.release_types, vec![ReleaseType::Album]);
    }

    #[test]
    fn headings_do_not_count_towards_an_inferred_album_length() {
        let mut tracks: Vec<TrackResponse> = (1..=6).map(|n| track(&n.to_string(), "T")).collect();
        tracks.push(TrackResponse {
            position: String::new(),
            title: "Bonus Tracks".into(),
            duration: None,
            track_type: Some("heading".into()),
            extraartists: vec![],
        });

        let identity = to_identity(&release(tracks));
        assert_eq!(identity.tracklist.len(), 7);
        assert_eq!(identity.playable_tracks().count(), 6);
        assert_eq!(identity.release_types, vec![ReleaseType::Album]);
    }

    #[test]
    fn disc_count_follows_the_highest_disc_in_the_tracklist() {
        let identity = to_identity(&release(vec![
            track("1-1", "One"),
            track("1-2", "Two"),
            track("2-1", "Three"),
        ]));
        assert_eq!(identity.disc_count, 2);
    }

    #[test]
    fn an_unparseable_position_is_kept_verbatim_and_invents_no_track_number() {
        // Discogs sends positions MuD does not recognise. Replacing one with the
        // tracklist index would overwrite the only copy of the original string
        // and assert a track number Discogs never gave.
        let broken = track("Bonus", "Unpositioned");
        let identity = to_identity(&release(vec![track("1", "First"), broken]));

        let unpositioned = &identity.tracklist[1];
        assert_eq!(
            unpositioned.position,
            TrackPosition::Unparsed {
                raw: "Bonus".into()
            },
            "the original position string was discarded"
        );
        assert_eq!(
            unpositioned.position.track_number(),
            None,
            "a track number was invented for an unparseable position"
        );
        assert_eq!(identity.tracklist[0].position.track_number(), Some(1));
    }

    #[test]
    fn an_empty_position_is_kept_empty_and_invents_no_track_number() {
        let mut empty = track("", "Unpositioned");
        empty.position = String::new();
        let identity = to_identity(&release(vec![track("1", "First"), empty]));

        assert_eq!(
            identity.tracklist[1].position,
            TrackPosition::Unparsed { raw: String::new() }
        );
        assert_eq!(identity.tracklist[1].position.track_number(), None);
    }

    #[test]
    fn prefers_the_printed_credit_when_discogs_supplies_one() {
        let mut subject = release(vec![track("1", "Airbag")]);
        subject.artists[0].anv = Some("RADIOHEAD".into());

        let identity = to_identity(&subject);
        assert_eq!(
            identity.artists[0].artist.name_variation.as_deref(),
            Some("RADIOHEAD")
        );
        // The printed variation is not a substitute for the canonical name: an
        // `artist` row stores the canonical one.
        assert_eq!(identity.artists[0].artist.name, "Radiohead");
    }

    #[test]
    fn a_credit_nested_in_a_tracklist_entry_is_scoped_to_that_position() {
        let mut entry = track("2", "Paranoid Android");
        entry.extraartists = vec![ArtistCreditResponse {
            id: 9,
            name: "Nigel Godrich".into(),
            anv: None,
            join: None,
            role: Some("Producer".into()),
            tracks: None,
        }];
        let subject = release(vec![track("1", "Airbag"), entry]);

        let identity = to_identity(&subject);
        let producer = identity
            .credits
            .iter()
            .find(|credit| credit.role == "Producer")
            .expect("producer credit");

        assert_eq!(producer.track_position.as_deref(), Some("2"));
        assert_eq!(producer.artist.name, "Nigel Godrich");
    }

    #[test]
    fn a_release_level_credit_carries_no_track_scope() {
        let mut subject = release(vec![track("1", "Airbag")]);
        subject.artists[0].role = Some("Producer".into());

        let identity = to_identity(&subject);
        let producer = identity
            .credits
            .iter()
            .find(|credit| credit.role == "Producer")
            .expect("producer credit");

        assert_eq!(producer.track_position, None);
    }

    #[test]
    fn attaches_the_barcode_to_every_label_of_the_release() {
        let mut subject = release(vec![track("1", "A")]);
        subject.labels = vec![
            crate::discogs_wire::LabelResponse {
                id: 5,
                name: "Parlophone".into(),
                catno: Some("7243 8 55229 2 9".into()),
            },
            crate::discogs_wire::LabelResponse {
                id: 6,
                name: "Capitol".into(),
                catno: None,
            },
        ];
        subject.identifiers = vec![crate::discogs_wire::IdentifierResponse {
            identifier_type: "Barcode".into(),
            value: "724385522929".into(),
            description: None,
        }];

        let identifiers = to_identity(&subject).identifiers;
        assert_eq!(identifiers.len(), 2);
        assert!(
            identifiers
                .iter()
                .all(|i| i.barcode.as_deref() == Some("724385522929"))
        );
        assert_eq!(
            identifiers[0].catalog_number.as_deref(),
            Some("7243 8 55229 2 9")
        );
        assert_eq!(identifiers[1].catalog_number, None);
    }

    #[test]
    fn treats_a_placeholder_country_as_absent() {
        let mut subject = release(vec![track("1", "A")]);
        subject.country = Some("-".into());
        assert_eq!(to_identity(&subject).country, None);
    }

    #[test]
    fn sums_only_playable_track_durations() {
        let mut tracks: Vec<TrackResponse> = (1..=2).map(|n| track(&n.to_string(), "T")).collect();
        tracks[0].duration = Some("1:00".into());
        tracks[1].duration = Some("2:00".into());
        tracks.push(TrackResponse {
            position: String::new(),
            title: "Notes".into(),
            duration: Some("9:00".into()),
            track_type: Some("heading".into()),
            extraartists: vec![],
        });

        assert_eq!(
            to_identity(&release(tracks)).duration,
            Some(DurationMs::from_millis(180_000))
        );
    }
}
