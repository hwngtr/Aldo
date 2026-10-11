//! Test fixtures shared by the TUI's tests.
//!
//! One set of domain values keeps the state tests and the render tests looking
//! at the same search, so a change to what the screens show cannot pass one
//! suite and fail the other for the wrong reason.

use std::path::PathBuf;
use std::time::Duration;

use aldo_catalog::{FanoutReason, FanoutRecord};
use aldo_core::audio::{BitDepth, ByteSize, SampleRate};
use aldo_core::candidate::{
    Candidate, CandidateDisplay, CandidateFile, DedupeKey, Locator, SoulSeekLocator,
};
use aldo_core::ids::{CandidateId, DiscogsReleaseId, ReleaseId};
use aldo_core::release::TrackPosition;
use aldo_store::repo::{CatalogRow, FanoutStatus};

use crate::actions::download::{DownloadPlan, Prepared, TagMetadata};
use crate::actions::search::{SearchRun, SoulSeekFound, SoulSeekReport};
use crate::tui::app::{App, ConfigSummary, Screen};

pub fn config() -> ConfigSummary {
    ConfigSummary {
        library_root: PathBuf::from("/home/you/Music"),
        discogs: true,
        server: true,
    }
}

/// A fresh install: no SoulSeek account, no Discogs token.
pub fn needs_setup() -> ConfigSummary {
    ConfigSummary {
        library_root: PathBuf::from("/home/you/Music"),
        discogs: false,
        server: false,
    }
}

pub fn app() -> App {
    App::new(config(), None)
}

pub fn release() -> CatalogRow {
    CatalogRow {
        release_id: ReleaseId(1),
        discogs_release_id: DiscogsReleaseId(26_707_685),
        title: "Surf's Up".to_owned(),
        artist: Some("The Beach Boys".to_owned()),
        year: Some(1971),
        release_types: vec![aldo_core::ReleaseType::Album],
        track_count: 10,
        cover_art_url: None,
    }
}

pub fn source() -> Candidate {
    Candidate {
        candidate_id: CandidateId(6),
        dedupe_key: DedupeKey::from_release_parts("The Beach Boys", "Surf's Up", None, 10, 1),
        display: CandidateDisplay {
            artist: "The Beach Boys".to_owned(),
            album: "Surf's Up".to_owned(),
            year: None,
            track_count: 10,
            disc_count: 1,
        },
        locator: Locator::SoulSeek(SoulSeekLocator {
            peer: "alice".to_owned(),
            remote_path: "x".to_owned(),
            size: ByteSize::new(1),
            has_free_slot: true,
            queue_length: None,
            upload_speed_bps: 1_024,
            sample_rate: SampleRate::new(44_100),
            bit_depth: BitDepth::new(16),
            duration: None,
        }),
        sources: Vec::new(),
        files: vec![
            CandidateFile {
                path: "The Beach Boys - Surf's Up - 01 - Don't Go Near the Water.flac".to_owned(),
                size: ByteSize::new(30_000_000),
            },
            CandidateFile {
                path: "The Beach Boys - Surf's Up - 02 - Long Promised Road.flac".to_owned(),
                size: ByteSize::new(31_000_000),
            },
        ],
        total_bytes: ByteSize::new(61_000_000),
    }
}

pub fn plan() -> DownloadPlan {
    let source = source();
    DownloadPlan {
        candidate_id: source.candidate_id,
        artist: source.display.artist.clone(),
        album: source.display.album.clone(),
        locator: source.locator.clone(),
        files: source.files.clone(),
        total_bytes: source.total_bytes,
    }
}

pub fn prepared() -> Prepared {
    Prepared {
        plan: plan(),
        tags: Some(TagMetadata {
            discogs_release_id: 26_707_685,
            album: "Surf's Up".to_owned(),
            artist: "The Beach Boys".to_owned(),
            year: Some(1971),
            tracks: vec![(
                "Alone Again".to_owned(),
                TrackPosition::DiscTrack { disc: 1, track: 1 },
            )],
        }),
        warning: None,
    }
}

pub fn search_run() -> SearchRun {
    SearchRun {
        query: crate::actions::search::parse("The Beach Boys - Surf's Up").expect("query"),
        discogs: FanoutRecord {
            matches: vec![release()],
            reason: FanoutReason::Answered,
            status: FanoutStatus::Done,
            error: None,
            detail: "1 fetched, 0 cached, 50 reported".to_owned(),
            retry_after: None,
        },
        soulseek: SoulSeekReport::Answered(SoulSeekFound {
            candidates: vec![source()],
            raw_hits: 50,
            rejected: 3,
            shares_anything: true,
            shared_directories: vec!["/home/you/Music".to_owned()],
            search_timeout: Duration::from_secs(8),
            listen_port: 2234,
        }),
    }
}

/// An app sitting on the results of one search.
pub fn with_results() -> App {
    let mut app = app();
    app.search = Some(search_run());
    app.screen = Screen::Results;
    app
}
