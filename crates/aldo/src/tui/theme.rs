//! One palette for the whole interface.
//!
//! The accents are Anthropic's brand colours and are tuned for a dark terminal,
//! which is where a terminal UI is usually read. They stay legible on a light
//! one, but secondary text is dimmer there: a palette that adapted would need
//! the terminal's own background colour, which this does not query. Nothing here
//! sets a background except the selection highlight, because the user's terminal
//! already chose one and overriding it is how a TUI ends up unreadable.

use ratatui::style::{Color, Modifier, Style};

/// The warm accent, used for the wordmark, the active element and headings.
pub const ACCENT: Color = Color::Rgb(0xd9, 0x77, 0x57);
/// A cool accent, used for labels.
pub const LABEL: Color = Color::Rgb(0x6a, 0x9b, 0xcc);
/// A muted green, used for success and available capacity.
pub const GOOD: Color = Color::Rgb(0x78, 0x8c, 0x5d);
/// A muted red, used for failures. Derived, because the brand set has none.
pub const BAD: Color = Color::Rgb(0xc0, 0x5c, 0x5c);
/// Mid grey for anything secondary.
///
/// Picked for both backgrounds. One grey cannot reach WCAG's 4.5 against a light
/// terminal and a dark one at once, so this sits at the balance point: 3.4:1 on
/// a light background, against 2.1:1 for the paler grey it replaced, and 5.2:1
/// on a dark one.
pub const MUTED: Color = Color::Rgb(0x8a, 0x88, 0x80);
/// Near-black, paired with the accent as a highlight background.
pub const INK: Color = Color::Rgb(0x14, 0x14, 0x13);

pub fn accent() -> Style {
    Style::new().fg(ACCENT)
}

pub fn muted() -> Style {
    Style::new().fg(MUTED)
}

pub fn label() -> Style {
    Style::new().fg(LABEL).add_modifier(Modifier::BOLD)
}

pub fn good() -> Style {
    Style::new().fg(GOOD)
}

pub fn bad() -> Style {
    Style::new().fg(BAD)
}

pub fn warn() -> Style {
    Style::new().fg(ACCENT)
}

/// Bold with the terminal's own foreground, so it stays readable on a light
/// theme as well as a dark one.
pub fn strong() -> Style {
    Style::new().add_modifier(Modifier::BOLD)
}

pub fn wordmark() -> Style {
    Style::new()
        .fg(ACCENT)
        .add_modifier(Modifier::BOLD)
        .add_modifier(Modifier::ITALIC)
}

/// The active row. A filled accent block, which is legible whatever the
/// terminal's own colours are.
pub fn selected() -> Style {
    Style::new().bg(ACCENT).fg(INK).add_modifier(Modifier::BOLD)
}
