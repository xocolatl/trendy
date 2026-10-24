//! The rules for writing text values as trendy source, shared by the
//! compiler (which reads text literals) and the runtime (which prints text
//! values). It has no dependencies and doesn't need `std`, so both can use
//! it.

#![no_std]

use core::fmt;

/// Escapes written as a backslash and one character, and what they stand for.
pub const SIMPLE_ESCAPES: &[(char, char)] = &[
    ('"', '"'),
    ('\\', '\\'),
    ('n', '\n'),
    ('r', '\r'),
    ('t', '\t'),
    ('0', '\0'),
];

/// Write the body of a text literal for `text`: quotes and backslashes are
/// escaped, as are control characters. The quotes around it are not
/// written.
///
/// # Errors
///
/// Returns an error if `out` does.
pub fn escape(text: &str, out: &mut impl fmt::Write) -> fmt::Result {
    escape_but(text, out, None)
}

/// Write `text` with nothing around it, as a table's cell shows it: as
/// [`escape`] writes a literal's body, so that a control character can't
/// break the line, but with quotes as they are, since none end it.
///
/// # Errors
///
/// Returns an error if `out` does.
pub fn escape_bare(text: &str, out: &mut impl fmt::Write) -> fmt::Result {
    escape_but(text, out, Some('"'))
}

/// As [`escape`], but with `kept` written as it is.
fn escape_but(text: &str, out: &mut impl fmt::Write, kept: Option<char>) -> fmt::Result {
    for c in text.chars() {
        if Some(c) == kept {
            out.write_char(c)?;
            continue;
        }
        match SIMPLE_ESCAPES.iter().find(|(_, value)| *value == c) {
            Some((name, _)) => {
                out.write_char('\\')?;
                out.write_char(*name)?;
            }
            None if c.is_control() => write!(out, "\\u{{{:X}}}", u32::from(c))?,
            None => out.write_char(c)?,
        }
    }
    Ok(())
}
