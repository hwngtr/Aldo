//! Small display helpers shared by the CLI and the TUI.
//!
//! Both front ends describe the same values — a peer's speed, a file's claimed
//! quality, a folder's name — so the formatting lives here once rather than
//! being re-derived, and drifting, in each renderer.

use std::time::Duration;

use aldo_core::audio::{BitDepth, SampleRate};
use aldo_core::candidate::Candidate;

/// Advertised transfer speed, in the largest unit that stays readable.
#[must_use]
pub fn speed(bytes_per_second: u32) -> String {
    if bytes_per_second >= 1_000_000 {
        format!("{:.1} MiB/s", f64::from(bytes_per_second) / 1_048_576.0)
    } else if bytes_per_second >= 1_000 {
        format!("{:.1} KiB/s", f64::from(bytes_per_second) / 1_024.0)
    } else {
        format!("{bytes_per_second} B/s")
    }
}

/// What a peer claims about a file, or `quality unknown` when it claims nothing.
///
/// A client that reports no attributes may still be sending a FLAC, so an
/// absent claim is reported as absent rather than as a low quality.
#[must_use]
pub fn quality(bit_depth: Option<BitDepth>, sample_rate: Option<SampleRate>) -> String {
    match (bit_depth, sample_rate) {
        (Some(depth), Some(rate)) => format!("{depth} {} kHz", rate.hz() / 1000),
        _ => "quality unknown".to_owned(),
    }
}

/// The folder a candidate came from, or a label when the peer shared its root.
#[must_use]
pub fn folder(candidate: &Candidate) -> String {
    if candidate.display.artist.is_empty() && candidate.display.album.is_empty() {
        return "(share root)".to_owned();
    }
    if candidate.display.artist.is_empty() {
        return candidate.display.album.clone();
    }
    format!("{}/{}", candidate.display.artist, candidate.display.album)
}

/// The final component of a path in either slash style.
#[must_use]
pub fn basename(path: &str) -> String {
    path.split(['/', '\\'])
        .next_back()
        .filter(|name| !name.is_empty())
        .unwrap_or(path)
        .to_owned()
}

/// A duration at a resolution worth reading: `12s`, `2m 30s`, `1h 05m`.
#[must_use]
pub fn short_duration(duration: Duration) -> String {
    let seconds = duration.as_secs();
    let hours = seconds / 3_600;
    let minutes = (seconds % 3_600) / 60;
    let seconds = seconds % 60;

    if hours > 0 {
        format!("{hours}h {minutes:02}m")
    } else if minutes > 0 {
        format!("{minutes}m {seconds:02}s")
    } else {
        format!("{seconds}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use aldo_core::audio::ByteSize;
    use aldo_core::candidate::{CandidateDisplay, DedupeKey, Locator, SoulSeekLocator};
    use aldo_core::ids::CandidateId;

    fn candidate(artist: &str, album: &str) -> Candidate {
        Candidate {
            candidate_id: CandidateId(0),
            dedupe_key: DedupeKey::from_release_parts(artist, album, None, 1, 1),
            display: CandidateDisplay {
                artist: artist.to_owned(),
                album: album.to_owned(),
                year: None,
                track_count: 1,
                disc_count: 1,
            },
            locator: Locator::SoulSeek(SoulSeekLocator {
                peer: "p".to_owned(),
                remote_path: "x".to_owned(),
                size: ByteSize::new(1),
                has_free_slot: true,
                queue_length: None,
                upload_speed_bps: 0,
                sample_rate: None,
                bit_depth: None,
                duration: None,
            }),
            sources: Vec::new(),
            files: Vec::new(),
            total_bytes: ByteSize::new(0),
        }
    }

    #[test]
    fn speeds_read_in_the_largest_unit_that_fits() {
        assert_eq!(speed(500), "500 B/s");
        assert_eq!(speed(1_500), "1.5 KiB/s");
        assert_eq!(speed(2_500_000), "2.4 MiB/s");
    }

    #[test]
    fn a_missing_quality_claim_is_said_not_guessed() {
        assert_eq!(quality(None, None), "quality unknown");
        assert_eq!(
            quality(BitDepth::new(16), SampleRate::new(44_100)),
            "16-bit 44 kHz"
        );
    }

    #[test]
    fn a_share_root_is_labelled_rather_than_left_blank() {
        assert_eq!(folder(&candidate("", "")), "(share root)");
        assert_eq!(folder(&candidate("", "Album")), "Album");
        assert_eq!(folder(&candidate("Artist", "Album")), "Artist/Album");
    }

    #[test]
    fn durations_drop_units_that_are_zero() {
        assert_eq!(short_duration(Duration::from_secs(12)), "12s");
        assert_eq!(short_duration(Duration::from_secs(150)), "2m 30s");
        assert_eq!(short_duration(Duration::from_mins(65)), "1h 05m");
    }

    #[test]
    fn a_basename_comes_back_from_either_slash_style() {
        assert_eq!(basename("a/b/c.flac"), "c.flac");
        assert_eq!(basename("a\\b\\c.flac"), "c.flac");
        assert_eq!(basename("c.flac"), "c.flac");
    }
}
