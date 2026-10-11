//! The mapping and persistence path, exercised against a real Discogs body.
//!
//! The unit tests use hand-written fixtures, which can only ever confirm the
//! assumptions behind them. This one uses a response captured from the live
//! API, so a field that Discogs spells differently from the fixture fails here.
//!
//! The body is anonymous: `GET /releases/{id}` needs no token, which is why it
//! could be captured without credentials.

use aldo_catalog::identity_from_payload;
use aldo_store::Store;
use aldo_store::repo::CatalogStore;

/// A real response for `GET /releases/249504`.
const REAL_RELEASE: &str = include_str!("fixtures/discogs_release_249504.json");

#[tokio::test]
async fn a_real_discogs_release_survives_the_mapping_and_the_store() {
    let store = Store::open_temporary().await.expect("store opens");
    let catalog = CatalogStore::new(store.pool().clone());

    let identity = identity_from_payload(REAL_RELEASE).expect("the real body maps");
    let release_id = catalog.upsert_release(&identity, 0).await.expect("stored");
    let row = catalog.get(release_id).await.expect("read back");

    assert_eq!(row.discogs_release_id, aldo_core::DiscogsReleaseId(249_504));
    assert_eq!(row.title, "Never Gonna Give You Up");
    assert_eq!(
        row.artist.as_deref(),
        Some("Rick Astley"),
        "the album artist must come through the real body"
    );
    assert_eq!(row.year, Some(1987));
    assert_eq!(row.track_count, 2);
    assert!(row.cover_art_url.is_some(), "the real body carries images");

    // Discogs reports its own approval status, which is `Accepted` here. It is
    // not the MusicBrainz official/promo/bootleg vocabulary.
    assert_eq!(identity.status, aldo_core::ReleaseStatus::Accepted);

    // A 7" single, so the positions are vinyl sides rather than track numbers.
    let tracks = catalog.tracks(release_id).await.expect("tracks");
    assert_eq!(tracks.len(), 2);
    assert!(matches!(
        tracks[0].position,
        aldo_core::TrackPosition::Side {
            side: aldo_core::Side::A,
            ..
        }
    ));
    assert!(matches!(
        tracks[1].position,
        aldo_core::TrackPosition::Side {
            side: aldo_core::Side::B,
            ..
        }
    ));
}

#[tokio::test]
async fn a_real_release_is_identified_and_its_identifiers_are_kept() {
    let store = Store::open_temporary().await.expect("store opens");
    let catalog = CatalogStore::new(store.pool().clone());

    let identity = identity_from_payload(REAL_RELEASE).expect("maps");
    assert_eq!(
        identity.discogs_master_id,
        Some(aldo_core::DiscogsMasterId(96_559))
    );
    assert_eq!(identity.media, Some(aldo_core::MediaFormat::Vinyl));
    assert_eq!(identity.country.as_deref(), Some("UK"));

    let release_id = catalog.upsert_release(&identity, 0).await.expect("stored");
    let row = catalog.get(release_id).await.expect("read back");
    assert_eq!(row.release_types, vec![aldo_core::ReleaseType::Single]);
}
