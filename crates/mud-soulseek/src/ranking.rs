//! Ordering sources by advertised download speed.
//!
//! A search returns sources, and the user downloads one. Ordering them means
//! the top of the list has the highest claimed upload speed.
//!
//! All the signals here are *claims*, not measurements: a peer's upload speed
//! is self-reported, and the true rate is only learned while downloading. The
//! order is therefore a best guess to start from, not a promise.

use std::cmp::Ordering;

use mud_core::candidate::{Candidate, Locator};

/// A floor under a peer's advertised speed, in bytes per second.
///
/// A peer that reports nothing claims zero, and dividing by zero would put it
/// at the top forever. The floor treats anything below a slow DSL line as
/// equally slow rather than infinitely slow.
const MIN_UPLOAD_SPEED: u64 = 100_000;

/// Sorts sources best-first, in place.
///
/// The order is: highest advertised speed first, then a peer with a free slot,
/// then higher audio quality and the shortest estimated transfer.
pub fn rank_sources(candidates: &mut [Candidate]) {
    candidates.sort_by(compare_sources);
}

/// A total order over two sources, for `sort_by`.
///
/// `Ordering::Less` means `a` sorts before `b`, i.e. `a` is preferred.
pub fn compare_sources(a: &Candidate, b: &Candidate) -> Ordering {
    // `sort_by` sorts ascending; reverse speed so the fastest source comes
    // first. Upload speed is the peer's claim, not a measured transfer rate.
    let by_speed = upload_speed(b).cmp(&upload_speed(a));
    if by_speed != Ordering::Equal {
        return by_speed;
    }

    let by_slot = has_free_slot(b).cmp(&has_free_slot(a));
    if by_slot != Ordering::Equal {
        return by_slot;
    }

    let by_quality = quality(b).cmp(&quality(a));
    if by_quality != Ordering::Equal {
        return by_quality;
    }

    estimated_seconds(a).cmp(&estimated_seconds(b))
}

fn upload_speed(candidate: &Candidate) -> u32 {
    match &candidate.locator {
        Locator::SoulSeek(locator) => locator.upload_speed_bps,
        Locator::Torrent(_) => 0,
    }
}

/// The claimed fidelity, as one number: bit depth times sample rate.
///
/// Unknown quality is `None`, which ranks below every known quality: "best
/// quality" cannot be claimed without evidence.
fn quality(candidate: &Candidate) -> Option<u64> {
    let Locator::SoulSeek(locator) = &candidate.locator else {
        return None;
    };
    match (locator.bit_depth, locator.sample_rate) {
        (Some(depth), Some(rate)) => Some(u64::from(depth.bits()) * u64::from(rate.hz())),
        _ => None,
    }
}

fn has_free_slot(candidate: &Candidate) -> bool {
    let Locator::SoulSeek(locator) = &candidate.locator else {
        return false;
    };
    locator.has_free_slot
}

/// Rough seconds to fetch the whole source, from its claimed speed and size.
fn estimated_seconds(candidate: &Candidate) -> u64 {
    let Locator::SoulSeek(locator) = &candidate.locator else {
        return u64::MAX;
    };
    let speed = u64::from(locator.upload_speed_bps).max(MIN_UPLOAD_SPEED);
    locator.size.as_u64() / speed
}

#[cfg(test)]
mod tests {
    use super::*;

    use mud_core::audio::{BitDepth, ByteSize, SampleRate};
    use mud_core::candidate::{CandidateDisplay, DedupeKey, SoulSeekLocator};

    fn candidate(
        peer: &str,
        depth: Option<u8>,
        rate: Option<u32>,
        free: bool,
        speed: u32,
        size: u64,
    ) -> Candidate {
        let locator = Locator::SoulSeek(SoulSeekLocator {
            peer: peer.to_owned(),
            remote_path: "Artist/Album".to_owned(),
            size: ByteSize::new(size),
            has_free_slot: free,
            queue_length: None,
            upload_speed_bps: speed,
            sample_rate: rate.and_then(SampleRate::new),
            bit_depth: depth.and_then(BitDepth::new),
            duration: None,
        });

        Candidate {
            candidate_id: mud_core::CandidateId(0),
            dedupe_key: DedupeKey::for_source(peer, "Artist/Album"),
            display: CandidateDisplay {
                artist: "Artist".to_owned(),
                album: "Album".to_owned(),
                year: None,
                track_count: 12,
                disc_count: 1,
            },
            locator,
            sources: Vec::new(),
            files: Vec::new(),
            total_bytes: ByteSize::new(size),
        }
    }

    fn ranked(candidates: &[Candidate]) -> Vec<String> {
        let mut copy = candidates.to_vec();
        rank_sources(&mut copy);
        copy.iter()
            .map(|c| match &c.locator {
                Locator::SoulSeek(locator) => locator.peer.clone(),
                Locator::Torrent(_) => "torrent".to_owned(),
            })
            .collect()
    }

    #[test]
    fn highest_advertised_speed_ranks_first_even_when_lower_quality() {
        let hi_res = candidate("hi-res", Some(24), Some(96_000), false, 1_000, 300_000_000);
        let cd = candidate("cd", Some(16), Some(44_100), true, 10_000_000, 100_000_000);

        assert_eq!(ranked(&[hi_res, cd]), vec!["cd", "hi-res"]);
    }

    #[test]
    fn equal_quality_prefers_a_free_slot_over_a_queued_peer() {
        let free = candidate("free", Some(16), Some(44_100), true, 1_000_000, 100_000_000);
        let queued = candidate(
            "queued",
            Some(16),
            Some(44_100),
            false,
            1_000_000,
            100_000_000,
        );

        assert_eq!(ranked(&[queued, free]), vec!["free", "queued"]);
    }

    #[test]
    fn equal_quality_and_slots_prefers_the_faster_peer() {
        let fast = candidate(
            "fast",
            Some(16),
            Some(44_100),
            true,
            10_000_000,
            100_000_000,
        );
        let slow = candidate("slow", Some(16), Some(44_100), true, 1_000_000, 100_000_000);

        assert_eq!(ranked(&[slow, fast]), vec!["fast", "slow"]);
    }

    #[test]
    fn known_quality_breaks_a_speed_and_slot_tie() {
        let unknown = candidate("unknown", None, None, true, 100_000_000, 100_000_000);
        let cd = candidate("cd", Some(16), Some(44_100), true, 100_000_000, 100_000_000);

        assert_eq!(ranked(&[unknown, cd]), vec!["cd", "unknown"]);
    }

    #[test]
    fn a_zero_speed_is_floored_rather_than_infinite() {
        // Without a floor, size / 0 would never return and would rank a dead
        // peer as if it were instant.
        let silent = candidate("silent", Some(16), Some(44_100), true, 0, 100_000_000);
        let slow = candidate("slow", Some(16), Some(44_100), true, 101_000, 100_000_000);

        assert_eq!(ranked(&[silent, slow]), vec!["slow", "silent"]);
    }

    #[test]
    fn the_order_is_stable_for_indistinguishable_sources() {
        // sort_by is stable, so two equal sources keep their input order rather
        // than swapping unpredictably.
        let a = candidate("a", Some(16), Some(44_100), true, 1_000_000, 100_000_000);
        let b = candidate("b", Some(16), Some(44_100), true, 1_000_000, 100_000_000);

        assert_eq!(ranked(&[a.clone(), b.clone()]), vec!["a", "b"]);
        assert_eq!(ranked(&[b, a]), vec!["b", "a"]);
    }
}
