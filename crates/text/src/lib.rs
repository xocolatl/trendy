//! The rules for writing text values as trendy source, and for reading
//! values from text, shared by the compiler (which reads text literals, and
//! evaluates `parse_boolean` when compiling) and the runtime (which prints
//! text values, and evaluates `parse_boolean` when running), so that the
//! two can't disagree. It has no dependencies and doesn't need `std`, so
//! both can use it.

#![cfg_attr(not(test), no_std)]

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

/// Why a text isn't a value of a type, as an operator that reads one from
/// text, such as `parse_boolean`, reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unparsable {
    /// It isn't written as one.
    Malformed,
    /// It is, but beyond the type's range.
    OutOfRange,
}

/// The words for true, in any case: trendy's own, then those data writes
/// it with.
pub const TRUE_WORDS: [&str; 5] = ["true", "t", "yes", "y", "1"];

/// The words for false, in any case, as [`TRUE_WORDS`] are for true.
pub const FALSE_WORDS: [&str; 5] = ["false", "f", "no", "n", "0"];

/// The boolean `text` writes: true for one of [`TRUE_WORDS`] and false for
/// one of [`FALSE_WORDS`], in any case, and nothing else, not even spaces.
///
/// # Errors
///
/// Returns [`Unparsable::Malformed`] for other text.
pub fn parse_boolean(text: &str) -> Result<bool, Unparsable> {
    let among = |words: &[&str]| words.iter().any(|w| w.eq_ignore_ascii_case(text));
    if among(&TRUE_WORDS) {
        Ok(true)
    } else if among(&FALSE_WORDS) {
        Ok(false)
    } else {
        Err(Unparsable::Malformed)
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn booleans_are_read_from_their_words_in_any_case() {
        for word in [
            "true", "TRUE", "True", "t", "T", "yes", "YES", "Yes", "y", "Y", "1",
        ] {
            assert_eq!(parse_boolean(word), Ok(true), "{word:?}");
        }
        for word in [
            "false", "FALSE", "False", "f", "F", "no", "NO", "No", "n", "N", "0",
        ] {
            assert_eq!(parse_boolean(word), Ok(false), "{word:?}");
        }
        for other in [
            "",
            " true",
            "true ",
            "on",
            "off",
            "2",
            "01",
            "-1",
            "tru",
            "yess",
            "nope",
            "ｔｒｕｅ",
        ] {
            assert_eq!(
                parse_boolean(other),
                Err(Unparsable::Malformed),
                "{other:?}"
            );
        }
    }
}
