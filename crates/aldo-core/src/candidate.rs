//! A provider-agnostic description of something downloadable, plus the typed
//! handles needed to actually fetch it.

use serde::{Deserialize, Serialize};

use crate::audio::{BitDepth, ByteSize, DurationMs, SampleRate};
use crate::ids::{CandidateId, FanoutId, InfoHash, ProviderId};

/// The identity a candidate is deduplicated on. Two providers reporting the
/// same album collapse to one candidate when this matches.
///
/// Deliberately excludes file sizes and bit rates: two rips of one album are
/// byte-different but the same candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DedupeKey(crate::ids::Sha256);

impl DedupeKey {
    /// Builds the key from the fields that identify a release, lowercased and
    /// with runs of whitespace and punctuation collapsed.
    pub fn from_release_parts(
        artist: &str,
        album: &str,
        year: Option<u16>,
        track_count: u16,
        disc_count: u8,
    ) -> Self {
        let mut canonical = String::with_capacity(album.len() + artist.len() + 24);
        canonical.push_str(&normalise(artist));
        canonical.push('\u{1}');
        canonical.push_str(&normalise(album));
        canonical.push('\u{1}');
        canonical.push_str(&year.map_or_else(String::new, |y| y.to_string()));
        canonical.push('\u{1}');
        canonical.push_str(&track_count.to_string());
        canonical.push('\u{1}');
        canonical.push_str(&disc_count.to_string());
        Self(crate::ids::Sha256::of(canonical.as_bytes()))
    }

    pub fn as_sha256(&self) -> crate::ids::Sha256 {
        self.0
    }

    /// Rebuilds a key from a stored digest.
    #[must_use]
    pub fn from_sha256(digest: crate::ids::Sha256) -> Self {
        Self(digest)
    }

    /// Builds the key for one *source*: a peer and the directory its files
    /// share.
    ///
    /// A search answers with sources, not albums, so the source is the unit the
    /// user picks and downloads. Two peers offering the same album are two
    /// sources and must not collapse, because "download the fastest source"
    /// needs them distinct.
    pub fn for_source(peer: &str, directory: &str) -> Self {
        let mut canonical = String::with_capacity(peer.len() + directory.len() + 1);
        canonical.push_str(&normalise(peer));
        canonical.push('\u{1}');
        canonical.push_str(&normalise(directory));
        Self(crate::ids::Sha256::of(canonical.as_bytes()))
    }
}

fn normalise(text: &str) -> String {
    let lowered = text.to_lowercase();
    let mut out = String::with_capacity(lowered.len());
    let mut last_was_sep = false;
    for ch in lowered.chars() {
        let is_sep = !ch.is_alphanumeric();
        if is_sep {
            if !last_was_sep && !out.is_empty() {
                out.push(' ');
            }
        } else {
            out.push(ch);
        }
        last_was_sep = is_sep;
    }
    out.trim_end().to_owned()
}

/// A SoulSeek peer's offer for one file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SoulSeekLocator {
    pub peer: String,
    /// The peer's remote path, relative to their share root. Untrusted input.
    pub remote_path: String,
    pub size: ByteSize,
    /// Whether the peer is serving uploads right now. This is the signal that
    /// decides whether a result is worth offering.
    pub has_free_slot: bool,
    /// How many peers are already waiting. `None` for a search result: the
    /// server reports a peer's upload slots but not its queue, which is only
    /// learned by asking to join it.
    pub queue_length: Option<u32>,
    /// Advertised upload speed, in bytes per second.
    pub upload_speed_bps: u32,
    /// What the peer claims about the file. Claims outside the ranges Aldo
    /// accepts are reported as absent rather than believed.
    pub sample_rate: Option<SampleRate>,
    pub bit_depth: Option<BitDepth>,
    pub duration: Option<DurationMs>,
}

/// A BitTorrent source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TorrentLocator {
    pub infohash: InfoHash,
    /// Index into the torrent's file list. `None` means the whole torrent.
    pub file_index: Option<u32>,
    pub tracker: Option<String>,
    pub seeders: Option<u32>,
    pub leechers: Option<u32>,
}

/// How to fetch a candidate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Locator {
    SoulSeek(SoulSeekLocator),
    Torrent(TorrentLocator),
}

impl Locator {
    pub fn provider(&self) -> ProviderId {
        match self {
            Self::SoulSeek(_) => ProviderId::SoulSeek,
            Self::Torrent(_) => ProviderId::TorrentMeta,
        }
    }

    /// Whether the source is worth offering right now. A source with a queued
    /// peer and no free slot will not finish, and a torrent whose seeder count
    /// the tracker did not report cannot be shown to have capacity, so it is not
    /// offered either.
    pub fn is_available(&self) -> bool {
        match self {
            // A free slot is what makes a peer downloadable. The queue length
            // is reported when known, but an unknown one must not read as a
            // full queue.
            Self::SoulSeek(locator) => locator.has_free_slot,
            Self::Torrent(locator) => matches!(locator.seeders, Some(seeders) if seeders > 0),
        }
    }
}

/// One provider's contribution to a candidate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateSource {
    pub fanout_id: FanoutId,
    pub provider: ProviderId,
    pub locator: Locator,
}

/// One file inside a candidate, as advertised by the source before download.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateFile {
    pub path: String,
    pub size: ByteSize,
}

/// What the user sees for a candidate, before it is matched to a release.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateDisplay {
    pub artist: String,
    pub album: String,
    pub year: Option<u16>,
    pub track_count: u16,
    pub disc_count: u8,
}

/// A single downloadable thing, deduplicated across providers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Candidate {
    pub candidate_id: CandidateId,
    pub dedupe_key: DedupeKey,
    pub display: CandidateDisplay,
    pub locator: Locator,
    pub sources: Vec<CandidateSource>,
    pub files: Vec<CandidateFile>,
    pub total_bytes: ByteSize,
}

impl Candidate {
    /// Adds a source, promoting it to the primary locator when the existing one
    /// is unavailable and the newcomer is not.
    pub fn absorb(&mut self, source: CandidateSource) {
        if !self.locator.is_available() && source.locator.is_available() {
            self.locator = source.locator.clone();
        }
        self.sources.push(source);
    }

    pub fn provider_count(&self) -> usize {
        self.sources.len()
    }
}

/// Why a raw hit was discarded. Surfaced in the UI so a search returning
/// nothing is explainable.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RejectionCounts {
    pub not_flac: u32,
    pub bit_depth_too_low: u32,
    pub too_large: u32,
    pub no_available_source: u32,
    pub unparsable: u32,
}

impl RejectionCounts {
    pub fn total(&self) -> u32 {
        // Five independently accumulated counters; saturating because a
        // provider must never be able to panic the orchestrator by counting.
        self.not_flac
            .saturating_add(self.bit_depth_too_low)
            .saturating_add(self.too_large)
            .saturating_add(self.no_available_source)
            .saturating_add(self.unparsable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parts(artist: &str, album: &str, year: Option<u16>, tracks: u16) -> DedupeKey {
        DedupeKey::from_release_parts(artist, album, year, tracks, 1)
    }

    #[test]
    fn dedupe_key_ignores_case_and_punctuation_spacing() {
        assert_eq!(
            parts("Radiohead", "OK Computer", Some(1997), 12),
            parts("radiohead", "ok   computer", Some(1997), 12)
        );
        assert_eq!(
            parts("Travis", "The Man Who", Some(1999), 12),
            parts("TRAVIS", "The-Man-Who", Some(1999), 12)
        );
    }

    #[test]
    fn dedupe_key_separates_different_releases() {
        assert_ne!(
            parts("Radiohead", "OK Computer", Some(1997), 12),
            parts("Radiohead", "Kid A", Some(2000), 10)
        );
        assert_ne!(
            parts("Artist", "Album", Some(1997), 12),
            parts("Artist", "Album", Some(1998), 12)
        );
    }

    #[test]
    fn soulseek_locator_availability_follows_the_upload_slot() {
        let free = Locator::SoulSeek(SoulSeekLocator {
            peer: "alice".into(),
            remote_path: "a.flac".into(),
            size: ByteSize::new(1),
            has_free_slot: true,
            queue_length: None,
            upload_speed_bps: 1_000,
            sample_rate: SampleRate::new(44_100),
            bit_depth: BitDepth::new(16),
            duration: Some(DurationMs::from_millis(200_000)),
        });
        assert!(free.is_available());

        let queued = Locator::SoulSeek(SoulSeekLocator {
            has_free_slot: false,
            queue_length: Some(4),
            ..match free.clone() {
                Locator::SoulSeek(inner) => inner,
                Locator::Torrent(_) => unreachable!(),
            }
        });
        assert!(!queued.is_available());

        // A free slot with an unknown queue is still downloadable: the search
        // result simply does not carry the queue length.
        let unknown_queue = Locator::SoulSeek(SoulSeekLocator {
            has_free_slot: true,
            queue_length: None,
            ..match free {
                Locator::SoulSeek(inner) => inner,
                Locator::Torrent(_) => unreachable!(),
            }
        });
        assert!(unknown_queue.is_available());
    }

    #[test]
    fn a_peer_claim_outside_the_accepted_range_is_absent_not_a_number() {
        let locator = SoulSeekLocator {
            peer: "bob".into(),
            remote_path: "b.flac".into(),
            size: ByteSize::new(1),
            has_free_slot: true,
            queue_length: None,
            upload_speed_bps: 0,
            sample_rate: SampleRate::new(7_000),
            bit_depth: BitDepth::new(20),
            duration: Some(DurationMs::from_millis(1)),
        };
        assert_eq!(locator.sample_rate, None);
        assert_eq!(locator.bit_depth, None);
    }

    #[test]
    fn torrent_locator_requires_a_seeder() {
        let dead = Locator::Torrent(TorrentLocator {
            infohash: InfoHash::from_hex(&"00".repeat(20)).expect("valid"),
            file_index: None,
            tracker: None,
            seeders: Some(0),
            leechers: Some(10),
        });
        assert!(!dead.is_available());

        let live = Locator::Torrent(TorrentLocator {
            seeders: Some(3),
            ..match dead {
                Locator::Torrent(inner) => inner,
                Locator::SoulSeek(_) => unreachable!(),
            }
        });
        assert!(live.is_available());
    }

    #[test]
    fn an_unreported_seeder_count_is_absent_not_zero() {
        // A tracker that omits the count has not said "no seeders". Reading it
        // as zero would report a dead torrent for a live one, so the count stays
        // absent and the source is not offered.
        let unstated = Locator::Torrent(TorrentLocator {
            infohash: InfoHash::from_hex(&"00".repeat(20)).expect("valid"),
            file_index: None,
            tracker: None,
            seeders: None,
            leechers: None,
        });

        assert!(
            !unstated.is_available(),
            "an unverified seeder count was offered as available"
        );
        let Locator::Torrent(locator) = &unstated else {
            unreachable!()
        };
        assert_eq!(
            locator.seeders, None,
            "an unreported seeder count was turned into a number"
        );
    }

    #[test]
    fn absorbing_an_available_source_promotes_it_to_primary() {
        let unavailable = Locator::Torrent(TorrentLocator {
            infohash: InfoHash::from_hex(&"11".repeat(20)).expect("valid"),
            file_index: None,
            tracker: None,
            seeders: Some(0),
            leechers: None,
        });
        let available = Locator::Torrent(TorrentLocator {
            infohash: InfoHash::from_hex(&"22".repeat(20)).expect("valid"),
            file_index: None,
            tracker: None,
            seeders: Some(9),
            leechers: None,
        });

        let mut candidate = Candidate {
            candidate_id: CandidateId(1),
            dedupe_key: parts("A", "B", None, 1),
            display: CandidateDisplay {
                artist: "A".into(),
                album: "B".into(),
                year: None,
                track_count: 1,
                disc_count: 1,
            },
            locator: unavailable.clone(),
            sources: vec![CandidateSource {
                fanout_id: FanoutId(1),
                provider: ProviderId::TorrentMeta,
                locator: unavailable,
            }],
            files: vec![],
            total_bytes: ByteSize::new(0),
        };

        candidate.absorb(CandidateSource {
            fanout_id: FanoutId(2),
            provider: ProviderId::TorrentMeta,
            locator: available.clone(),
        });

        assert_eq!(candidate.locator, available);
        assert_eq!(candidate.provider_count(), 2);
    }

    #[test]
    fn rejection_counts_sum_to_a_total() {
        let counts = RejectionCounts {
            not_flac: 5,
            bit_depth_too_low: 2,
            too_large: 1,
            no_available_source: 8,
            unparsable: 0,
        };
        assert_eq!(counts.total(), 16);
        assert_eq!(RejectionCounts::default().total(), 0);
    }
}
