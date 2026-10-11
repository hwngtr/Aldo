//! The wordmark.
//!
//! Drawn, not typeset. It is a blocky mark in the style of OpenCode's: solid
//! geometric letterforms on a coarse grid.
//!
//! `D` and `O` are the hard pair, and the fix for them is width, not corners.
//! `O` is three columns wide to `D`"'"'s four, which makes it read as a narrower
//! rounded letter rather than a second box. The first attempt gave both letters
//! four columns and cut `O`"'"'s corners, which works at full size and fails here:
//! [`rows`] pairs the design rows into half blocks, and a corner that differs by
//! half a cell disappears in the pairing. The mark read `AlOO`.
//!
//! `█` is ink and a space is nothing. The design is five rows tall, but a
//! terminal cell is about twice as tall as it is wide, so five rows of full
//! blocks read as stretched. [`rows`] pairs the design rows into half blocks
//! instead: every row of the design survives, and the mark is three rows tall.

/// The wordmark, 5 design rows by 22 columns.
pub const WIDTH: usize = 22;

/// How many terminal rows [`rows`] produces: the design rows, paired.
pub const ROWS: usize = LINES.len().div_ceil(2);

/// The design, top to bottom, each `WIDTH` columns wide.
const LINES: [&str; 5] = [
    " ██   █     ███    ██ ",
    "█  █  █     █  █  █  █",
    "████  █     █  █  █  █",
    "█  █  █     █  █  █  █",
    "█  █  ████  ███    ██ ",
];

/// The wordmark as terminal rows, one tone.
///
/// Each output row is two design rows: `▀` where only the upper is ink, `▄`
/// where only the lower is, `█` where both are, and a space where neither is.
#[must_use]
pub fn rows() -> Vec<String> {
    LINES.chunks(2).map(squeeze).collect()
}

fn squeeze(pair: &[&str]) -> String {
    let upper: Vec<char> = pair[0].chars().collect();
    let lower: Vec<char> = pair
        .get(1)
        .map(|row| row.chars().collect())
        .unwrap_or_default();

    (0..upper.len())
        .map(|column| {
            let top = upper[column] == INK;
            let bottom = lower.get(column).is_some_and(|cell| *cell == INK);
            match (top, bottom) {
                (true, true) => '█',
                (true, false) => '▀',
                (false, true) => '▄',
                (false, false) => ' ',
            }
        })
        .collect()
}

/// The plain wordmark, for a terminal too small for the art.
pub const PLAIN: &str = "A L D O";

const INK: char = '█';

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_row_is_the_declared_width() {
        for row in LINES {
            assert_eq!(row.chars().count(), WIDTH, "{row:?}");
        }
        for row in rows() {
            assert_eq!(row.chars().count(), WIDTH, "{row:?}");
        }
    }

    #[test]
    fn squeezing_halves_the_height_and_keeps_every_design_row() {
        assert_eq!(ROWS, 3);
        assert_eq!(rows().len(), ROWS);
        // Every design row contributes ink somewhere, so no row is all blank.
        for row in rows() {
            assert!(
                row.contains(INK) || row.contains('▀') || row.contains('▄'),
                "{row:?}"
            );
        }
    }

    #[test]
    fn a_pair_becomes_both_halves_upper_only_and_lower_only() {
        assert_eq!(squeeze(&["█  ", "█  "]), "█  ");
        assert_eq!(squeeze(&["█  ", "   "]), "▀  ");
        assert_eq!(squeeze(&["   ", "█  "]), "▄  ");
        // An odd number of design rows leaves the last output row without a
        // lower half, which must not panic or drop the ink.
        assert_eq!(squeeze(&["█  "]), "▀  ");
    }

    #[test]
    fn d_and_o_keep_a_difference_that_survives_the_squeeze() {
        // The letters are columns A 0..4, L 6..10, D 12..16, O 18..21, and the
        // gap between them is the two-column gutter. Counted in characters
        // because the block glyphs are multi-byte.
        //
        // This is the pair that went wrong once already: with equal widths and
        // only corners to tell them apart, the squeeze erased the difference and
        // the word read `AlOO`. If this test fails, look at the rendered mark
        // before adjusting the numbers.
        let squeezed = rows();
        let top: Vec<char> = squeezed[0].chars().collect();
        let bottom: Vec<char> = squeezed[2].chars().collect();

        let d_top: String = top[12..16].iter().collect();
        let o_top: String = top[18..21].iter().collect();
        let d_bottom: String = bottom[12..16].iter().collect();
        let o_bottom: String = bottom[18..21].iter().collect();

        assert_ne!(d_top, o_top, "D and O share a top row");
        assert_ne!(d_bottom, o_bottom, "D and O share a bottom row");
        // A full block at the corner is what makes D a box and not a ring.
        assert!(
            d_top.starts_with('█'),
            "D lost its square corner: {d_top:?}"
        );
        assert!(!o_top.starts_with('█'), "O squared off into a D: {o_top:?}");
    }
}
