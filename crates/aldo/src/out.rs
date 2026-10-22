//! Writing to stdout without panicking when the reader goes away.
//!
//! `println!` panics if stdout is a pipe whose reader has closed, which is
//! exactly what `aldo search … | head` does. A closed pipe is the user's
//! business, not a bug in Aldo, so these macros drop the write and let the
//! process exit with the code it would have had anyway.
//!
//! The alternative is resetting `SIGPIPE` to its default disposition, which
//! needs `unsafe`, and this crate forbids it.

/// Escapes control characters before peer-controlled text reaches a terminal.
#[must_use]
pub fn terminal_safe(text: &str) -> String {
    text.chars()
        .map(|character| match character {
            '\t' => "\\t".to_owned(),
            '\n' => "\\n".to_owned(),
            '\r' => "\\r".to_owned(),
            character if character.is_control() => format!("\\u{{{:04X}}}", character as u32),
            character => character.to_string(),
        })
        .collect()
}

/// `println!`, minus the panic on a closed pipe.
#[macro_export]
macro_rules! outln {
    () => {
        $crate::outln!("")
    };
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        let _ = writeln!(std::io::stdout(), $($arg)*);
    }};
}

/// `print!`, minus the panic on a closed pipe.
#[macro_export]
macro_rules! out {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        let _ = write!(std::io::stdout(), $($arg)*);
    }};
}
