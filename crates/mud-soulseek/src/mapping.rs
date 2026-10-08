//! Turning search responses into candidates.
//!
//! A search answers with *files*, not albums: a single `SearchResult` is one
//! peer's matching files. Grouping them by directory is a heuristic that turns
//! `Radiohead/OK Computer/01 - Airbag.flac` and its siblings into one
//! candidate. It is deliberately shallow: the Discogs release disambiguates
//! identity later, so a wrong guess here is correctable rather than fatal.

use std::collections::BTreeMap;

use mud_core::audio::{BitDepth, ByteSize, DurationMs, SampleRate};
use mud_core::candidate::{
    Candidate, CandidateDisplay, CandidateFile, DedupeKey, Locator, RejectionCounts,
    SoulSeekLocator,
};
use mud_core::ids::CandidateId;
use soulseek_rs::{File, SearchResult};

/// The attribute code for a file's duration, in seconds.
const ATTR_DURATION_SECONDS: u32 = 1;
/// The attribute code for a file's sample rate, in hertz.
const ATTR_SAMPLE_RATE_HZ: u32 = 4;
/// The attribute code for a file's bit depth, in bits.
const ATTR_BIT_DEPTH: u32 = 5;

/// Builds one candidate per directory of lossless files per peer.
///
/// Anything that is not a FLAC is counted into `rejected` rather than returned,
/// because a search that finds only lossy files must be able to say so.
#[must_use]
pub fn candidates_from(
    results: Vec<SearchResult>,
    rejected: &mut RejectionCounts,
) -> Vec<Candidate> {
    let mut candidates = Vec::new();

    for result in results {
        // `slots` is the peer's free upload slots: zero means a download would
        // wait in a queue behind everyone else.
        let has_free_slot = result.slots > 0;
        let upload_speed_bps = result.speed;

        let mut by_directory: BTreeMap<String, Vec<File>> = BTreeMap::new();
        for file in result.files {
            if !mud_core::is_flac(&file.name) {
                rejected.not_flac += 1;
                continue;
            }
            by_directory
                .entry(parent_of(&file.name))
                .or_default()
                .push(file);
        }

        for (directory, files) in by_directory {
            candidates.push(build(
                &result.username,
                has_free_slot,
                upload_speed_bps,
                directory,
                files,
            ));
        }
    }

    candidates
}

fn build(
    peer: &str,
    has_free_slot: bool,
    upload_speed_bps: u32,
    directory: String,
    files: Vec<File>,
) -> Candidate {
    let total_bytes: u64 = files.iter().map(|file| file.size).sum();
    let track_count = u16::try_from(files.len()).unwrap_or(u16::MAX);

    // The first file's claims describe the album closely enough for a
    // shortlist; per-file detail lives in the file list.
    let (sample_rate, bit_depth, duration) = files
        .first()
        .map_or((None, None, None), |file| attributes(&file.attribs));

    let display = infer_display(&directory);
    // A candidate is one source, not one album: two peers offering the same
    // album must stay two rows so "download the fastest source" can pick
    // between them. The release-level key still exists for when sources are
    // grouped, but search keys on the source.
    let dedupe_key = DedupeKey::for_source(peer, &directory);

    let candidate_files = files
        .into_iter()
        .map(|file| CandidateFile {
            path: file.name,
            size: ByteSize::new(file.size),
        })
        .collect();

    Candidate {
        candidate_id: CandidateId(0),
        dedupe_key,
        display: CandidateDisplay {
            track_count,
            ..display
        },
        locator: Locator::SoulSeek(SoulSeekLocator {
            peer: peer.to_owned(),
            // A grouped candidate is located by the directory its files share,
            // exactly as a torrent is located by its info hash; the files
            // themselves are listed separately.
            remote_path: directory,
            size: ByteSize::new(total_bytes),
            has_free_slot,
            queue_length: None,
            upload_speed_bps,
            sample_rate,
            bit_depth,
            duration,
        }),
        sources: Vec::new(),
        files: candidate_files,
        total_bytes: ByteSize::new(total_bytes),
    }
}

/// Reads the audio claims a peer attached to a file.
///
/// A claim outside the range MuD accepts is reported as absent rather than
/// believed: the values come from the peer, and a wrong sample rate would
/// poison the lossless filter.
fn attributes(
    attribs: &std::collections::HashMap<u32, u32>,
) -> (Option<SampleRate>, Option<BitDepth>, Option<DurationMs>) {
    let sample_rate = attribs
        .get(&ATTR_SAMPLE_RATE_HZ)
        .and_then(|hz| SampleRate::new(*hz));
    let bit_depth = attribs
        .get(&ATTR_BIT_DEPTH)
        .and_then(|bits| u8::try_from(*bits).ok())
        .and_then(BitDepth::new);
    let duration = attribs
        .get(&ATTR_DURATION_SECONDS)
        .map(|seconds| DurationMs::from_millis(u64::from(*seconds) * 1_000));

    (sample_rate, bit_depth, duration)
}

/// Reads `artist` and `album` out of a directory path.
///
/// `.../Radiohead/OK Computer` yields `Radiohead` and `OK Computer`. A path with
/// fewer components yields empty strings, which the caller renders as unknown.
fn infer_display(directory: &str) -> CandidateDisplay {
    let parts: Vec<&str> = directory
        .split(['/', '\\'])
        .filter(|part| !part.trim().is_empty())
        .collect();

    let album = parts.last().copied().unwrap_or_default().trim().to_owned();
    let artist = parts
        .len()
        .checked_sub(2)
        .and_then(|index| parts.get(index))
        .copied()
        .unwrap_or_default()
        .trim()
        .to_owned();

    CandidateDisplay {
        artist,
        album,
        year: None,
        track_count: 0,
        disc_count: 1,
    }
}

/// The directory a file sits in, or empty when it sits at a share root.
fn parent_of(path: &str) -> String {
    match path.rfind(['/', '\\']) {
        Some(index) => path[..index].to_owned(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use soulseek_rs::File;

    fn file(path: &str, size: u64, attrs: &[(u32, u32)]) -> File {
        File {
            username: "peer".to_owned(),
            name: path.to_owned(),
            size,
            attribs: attrs.iter().copied().collect(),
        }
    }

    /// A 16-bit 44.1 kHz FLAC, as a SoulseekQt client reports one.
    fn lossless(path: &str, size: u64) -> File {
        file(
            path,
            size,
            &[
                (ATTR_DURATION_SECONDS, 254),
                (ATTR_SAMPLE_RATE_HZ, 44_100),
                (ATTR_BIT_DEPTH, 16),
            ],
        )
    }

    fn result(username: &str, slots: u8, files: Vec<File>) -> SearchResult {
        SearchResult {
            token: 1,
            files,
            slots,
            speed: 1_000_000,
            username: username.to_owned(),
        }
    }

    fn convert(results: Vec<SearchResult>) -> (Vec<Candidate>, RejectionCounts) {
        let mut rejected = RejectionCounts::default();
        let candidates = candidates_from(results, &mut rejected);
        (candidates, rejected)
    }

    #[test]
    fn files_in_one_directory_become_one_candidate() {
        let (candidates, rejected) = convert(vec![result(
            "alice",
            1,
            vec![
                lossless("Radiohead/OK Computer/01 - Airbag.flac", 40_000_000),
                lossless(
                    "Radiohead/OK Computer/02 - Paranoid Android.flac",
                    42_000_000,
                ),
            ],
        )]);

        assert_eq!(candidates.len(), 1);
        assert_eq!(rejected.total(), 0);

        let candidate = &candidates[0];
        assert_eq!(candidate.display.artist, "Radiohead");
        assert_eq!(candidate.display.album, "OK Computer");
        assert_eq!(candidate.display.track_count, 2);
        assert_eq!(candidate.total_bytes, ByteSize::new(82_000_000));
        assert_eq!(candidate.files.len(), 2);
    }

    #[test]
    fn two_albums_in_one_response_stay_separate() {
        let (candidates, _) = convert(vec![result(
            "alice",
            1,
            vec![
                lossless("Radiohead/OK Computer/01.flac", 1),
                lossless("Radiohead/Kid A/01.flac", 1),
            ],
        )]);

        assert_eq!(candidates.len(), 2);
    }

    #[test]
    fn the_same_album_from_two_peers_is_two_distinct_sources() {
        // A search answers with sources; two peers offering one album must stay
        // two rows, or "download the fastest source" cannot tell them apart.
        let (candidates, _) = convert(vec![
            result(
                "alice",
                1,
                vec![lossless("Radiohead/OK Computer/01.flac", 10)],
            ),
            result(
                "bob",
                1,
                vec![lossless("Radiohead/OK Computer/01.flac", 20)],
            ),
        ]);

        assert_eq!(candidates.len(), 2);
        assert_ne!(
            candidates[0].dedupe_key, candidates[1].dedupe_key,
            "two peers offering one album must not deduplicate"
        );
    }

    #[test]
    fn the_same_source_reported_twice_shares_a_key() {
        let (candidates, _) = convert(vec![
            result(
                "alice",
                1,
                vec![lossless("Radiohead/OK Computer/01.flac", 10)],
            ),
            result(
                "alice",
                1,
                vec![lossless("Radiohead/OK Computer/01.flac", 10)],
            ),
        ]);

        assert_eq!(
            candidates[0].dedupe_key, candidates[1].dedupe_key,
            "one peer and folder is one source"
        );
    }

    #[test]
    fn a_non_flac_file_is_rejected_and_counted() {
        let (candidates, rejected) = convert(vec![result(
            "alice",
            1,
            vec![
                lossless("A/B/song.flac", 1),
                file("A/B/song.mp3", 1, &[(0, 320)]),
                file("A/B/notes.txt", 1, &[]),
            ],
        )]);

        assert_eq!(candidates.len(), 1);
        assert_eq!(rejected.not_flac, 2);
    }

    #[test]
    fn a_flac_without_attributes_is_still_accepted() {
        // The extension is the claim; attributes are a bonus. Rejecting a
        // `.flac` because a client sent no attributes would drop real files.
        let (candidates, rejected) = convert(vec![result(
            "alice",
            1,
            vec![file("A/B/no-attrs.flac", 1, &[])],
        )]);

        assert_eq!(candidates.len(), 1);
        assert_eq!(rejected.not_flac, 0);

        match &candidates[0].locator {
            Locator::SoulSeek(locator) => {
                assert_eq!(locator.sample_rate, None);
                assert_eq!(locator.bit_depth, None);
                assert_eq!(locator.duration, None);
            }
            Locator::Torrent(_) => panic!("a soulseek result produced a torrent locator"),
        }
    }

    #[test]
    fn a_claim_outside_the_accepted_range_is_absent_not_believed() {
        let (candidates, _) = convert(vec![result(
            "alice",
            1,
            vec![file(
                "A/B/odd.flac",
                1,
                &[(ATTR_SAMPLE_RATE_HZ, 7_000), (ATTR_BIT_DEPTH, 20)],
            )],
        )]);

        match &candidates[0].locator {
            Locator::SoulSeek(locator) => {
                assert_eq!(locator.sample_rate, None, "7 kHz is not a real rate");
                assert_eq!(locator.bit_depth, None, "20-bit is not a real width");
            }
            Locator::Torrent(_) => panic!("a soulseek result produced a torrent locator"),
        }
    }

    #[test]
    fn a_valid_attribute_set_is_read_into_typed_values() {
        let (candidates, _) = convert(vec![result(
            "alice",
            1,
            vec![file(
                "A/B/24-96.flac",
                1,
                &[
                    (ATTR_DURATION_SECONDS, 300),
                    (ATTR_SAMPLE_RATE_HZ, 96_000),
                    (ATTR_BIT_DEPTH, 24),
                ],
            )],
        )]);

        match &candidates[0].locator {
            Locator::SoulSeek(locator) => {
                assert_eq!(locator.sample_rate, SampleRate::new(96_000));
                assert_eq!(locator.bit_depth, BitDepth::new(24));
                assert_eq!(locator.duration, Some(DurationMs::from_millis(300_000)));
            }
            Locator::Torrent(_) => panic!("a soulseek result produced a torrent locator"),
        }
    }

    #[test]
    fn a_peer_with_no_free_slot_produces_an_unavailable_candidate() {
        let (candidates, _) = convert(vec![result("alice", 0, vec![lossless("A/B/1.flac", 1)])]);

        assert!(!candidates[0].locator.is_available());

        let (free, _) = convert(vec![result("alice", 3, vec![lossless("A/B/1.flac", 1)])]);
        assert!(free[0].locator.is_available());
    }

    #[test]
    fn windows_style_separators_are_understood() {
        let (candidates, _) = convert(vec![result(
            "alice",
            1,
            vec![lossless("Radiohead\\OK Computer\\01.flac", 1)],
        )]);

        assert_eq!(candidates[0].display.artist, "Radiohead");
        assert_eq!(candidates[0].display.album, "OK Computer");
    }

    #[test]
    fn a_file_at_a_share_root_yields_an_unknown_album() {
        let (candidates, _) = convert(vec![result("alice", 1, vec![lossless("lonely.flac", 1)])]);

        assert_eq!(candidates[0].display.album, "");
        assert_eq!(candidates[0].display.artist, "");
        assert_eq!(candidates[0].display.track_count, 1);
    }

    #[test]
    fn the_peer_reported_speed_is_carried_through() {
        let (candidates, _) = convert(vec![result("alice", 1, vec![lossless("A/B/1.flac", 1)])]);

        match &candidates[0].locator {
            Locator::SoulSeek(locator) => assert_eq!(locator.upload_speed_bps, 1_000_000),
            Locator::Torrent(_) => panic!("a soulseek result produced a torrent locator"),
        }
    }

    #[test]
    fn parent_of_handles_both_separators_and_a_bare_name() {
        assert_eq!(parent_of("a/b/c.flac"), "a/b");
        assert_eq!(parent_of("a\\b\\c.flac"), "a\\b");
        assert_eq!(parent_of("c.flac"), "");
    }
}
