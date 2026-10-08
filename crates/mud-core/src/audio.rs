//! Typed audio facts. Raw integers are never passed around; each quantity has a
//! unit in its name and a validating constructor.

use std::fmt::{self, Display, Formatter};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::ids::FromStoredId;

/// Sample rate in hertz. Lossless sources in practice span 8 kHz to 384 kHz.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SampleRate(u32);

impl SampleRate {
    pub const MIN_HZ: u32 = 8_000;
    pub const MAX_HZ: u32 = 384_000;

    pub fn new(hz: u32) -> Option<Self> {
        (Self::MIN_HZ..=Self::MAX_HZ)
            .contains(&hz)
            .then_some(Self(hz))
    }

    pub fn hz(self) -> u32 {
        self.0
    }

    pub fn is_hi_res(self) -> bool {
        self.0 > 48_000
    }
}

impl Display for SampleRate {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{} Hz", self.0)
    }
}

/// Bits per sample. Only the widths that actually occur in released audio.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BitDepth(u8);

impl BitDepth {
    /// Every width that occurs in released audio. `new` reads this list, so a
    /// width is added in exactly one place.
    pub const SUPPORTED: [Self; 4] = [Self(8), Self(16), Self(24), Self(32)];

    pub fn new(bits: u8) -> Option<Self> {
        Self::SUPPORTED.into_iter().find(|depth| depth.0 == bits)
    }

    pub fn bits(self) -> u8 {
        self.0
    }

    /// Bits stored per sample per channel in a byte count.
    pub fn bits_per_channel(self) -> u32 {
        u32::from(self.0)
    }
}

impl Display for BitDepth {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{}-bit", self.0)
    }
}

/// Channel count.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Channels(u8);

impl Channels {
    pub fn new(count: u8) -> Option<Self> {
        (1..=8).contains(&count).then_some(Self(count))
    }

    pub fn count(self) -> u8 {
        self.0
    }

    pub fn is_stereo(self) -> bool {
        self.0 == 2
    }
}

impl Display for Channels {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self.0 {
            1 => write!(f, "mono"),
            2 => write!(f, "stereo"),
            other => write!(f, "{other} channels"),
        }
    }
}

/// Milliseconds. Wrapping `std::time::Duration` in a newtype keeps serde output
/// as a plain integer and forbids mixing up seconds with milliseconds. Also the
/// unit for every measured wait, such as a rate-limit retry delay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DurationMs(u64);

impl DurationMs {
    pub fn from_millis(ms: u64) -> Self {
        Self(ms)
    }

    pub fn as_millis(self) -> u64 {
        self.0
    }

    pub fn as_std(self) -> Duration {
        Duration::from_millis(self.0)
    }

    pub fn from_std(duration: Duration) -> Self {
        // `as_millis` is a `u128`; the saturate names the capacity limit of the
        // type rather than hiding a failure.
        Self(u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
    }
}

impl FromStoredId for DurationMs {
    fn from_stored(raw: i64) -> Option<Self> {
        u64::try_from(raw).ok().map(Self)
    }
}

impl TryFrom<DurationMs> for i64 {
    type Error = std::num::TryFromIntError;

    fn try_from(value: DurationMs) -> Result<Self, Self::Error> {
        i64::try_from(value.0)
    }
}

impl Display for DurationMs {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{} ms", self.0)
    }
}

/// A byte count. Distinct from `usize` so a size is never mistaken for a length
/// into a buffer, and distinct from `DurationMs` so a size is never mistaken for
/// a duration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ByteSize(u64);

impl ByteSize {
    pub fn new(bytes: u64) -> Self {
        Self(bytes)
    }

    pub fn as_u64(self) -> u64 {
        self.0
    }

    pub const ZERO: Self = Self(0);
}

impl FromStoredId for ByteSize {
    fn from_stored(raw: i64) -> Option<Self> {
        u64::try_from(raw).ok().map(Self)
    }
}

impl TryFrom<ByteSize> for i64 {
    type Error = std::num::TryFromIntError;

    fn try_from(value: ByteSize) -> Result<Self, Self::Error> {
        i64::try_from(value.0)
    }
}

impl Display for ByteSize {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        const KIB: u64 = 1024;
        const MIB: u64 = 1024 * KIB;
        const GIB: u64 = 1024 * MIB;
        match self.0 {
            b if b < KIB => write!(f, "{b} B"),
            b if b < MIB => write!(f, "{:.1} KiB", b as f64 / KIB as f64),
            b if b < GIB => write!(f, "{:.1} MiB", b as f64 / MIB as f64),
            b => write!(f, "{:.2} GiB", b as f64 / GIB as f64),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_rate_rejects_out_of_range_values() {
        assert_eq!(SampleRate::new(44_100).map(SampleRate::hz), Some(44_100));
        assert!(SampleRate::new(0).is_none());
        assert!(SampleRate::new(500_000).is_none());
        assert!(SampleRate::new(96_000).expect("valid").is_hi_res());
        assert!(!SampleRate::new(48_000).expect("valid").is_hi_res());
    }

    #[test]
    fn bit_depth_accepts_only_real_widths() {
        assert_eq!(BitDepth::new(16).map(BitDepth::bits), Some(16));
        assert!(BitDepth::new(0).is_none());
        assert!(BitDepth::new(20).is_none());
        assert!(BitDepth::new(255).is_none());
    }

    #[test]
    fn channels_reject_zero_and_extremes() {
        assert!(Channels::new(0).is_none());
        assert!(Channels::new(2).expect("valid").is_stereo());
        assert!(Channels::new(9).is_none());
    }

    #[test]
    fn byte_size_formats_in_binary_units() {
        assert_eq!(ByteSize::new(512).to_string(), "512 B");
        assert_eq!(ByteSize::new(2048).to_string(), "2.0 KiB");
        assert_eq!(ByteSize::new(3 * 1024 * 1024).to_string(), "3.0 MiB");
        assert_eq!(
            ByteSize::new(2 * 1024 * 1024 * 1024).to_string(),
            "2.00 GiB"
        );
    }

    #[test]
    fn duration_round_trips_through_std() {
        let duration = Duration::from_millis(4_500);
        assert_eq!(DurationMs::from_std(duration).as_std(), duration);
    }

    #[test]
    fn a_duration_and_a_byte_count_do_not_convert_into_each_other() {
        // Both wrap a `u64` and both `Display` with a unit suffix, so this is
        // the pair most likely to be confused. There is no conversion between
        // them, by design.
        let duration = DurationMs::from_millis(4_096);
        let size = ByteSize::new(4_096);
        assert_eq!(duration.as_millis(), size.as_u64());
        assert_eq!(duration.to_string(), "4096 ms");
        assert_eq!(size.to_string(), "4.0 KiB");
    }

    #[test]
    fn stored_quantities_reject_a_negative_column() {
        // SQLite has no unsigned columns, so every read of a non-negative
        // column is a checked conversion.
        assert_eq!(
            DurationMs::from_stored(1_000),
            Some(DurationMs::from_millis(1_000))
        );
        assert_eq!(DurationMs::from_stored(-1), None);
        assert_eq!(ByteSize::from_stored(4_096), Some(ByteSize::new(4_096)));
        assert_eq!(ByteSize::from_stored(-1), None);
    }

    #[test]
    fn a_quantity_too_large_for_sqlite_is_refused_not_wrapped() {
        assert!(i64::try_from(ByteSize::new(u64::MAX)).is_err());
        assert_eq!(i64::try_from(ByteSize::new(4_096)), Ok(4_096));
        assert!(i64::try_from(DurationMs::from_millis(u64::MAX)).is_err());
        assert_eq!(i64::try_from(DurationMs::from_millis(4_096)), Ok(4_096));
    }
}
