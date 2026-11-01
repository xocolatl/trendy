//! The rules for writing text values as trendy source, and for reading
//! values from text, shared by the compiler (which reads text literals, and
//! evaluates `parse_boolean` when compiling) and the runtime (which prints
//! text values, and evaluates `parse_boolean` when running), so that the
//! two can't disagree, and so are the words both write in an error they
//! can each report. It has no dependencies and doesn't need `std`, so both
//! can use it.

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

/// The characters that change the order text is shown in, the embeddings,
/// overrides and isolates of Unicode's bidirectional algorithm (UAX #9):
/// not control characters, but invisible, and able to make what text
/// shows differ from what it holds, as CVE-2021-42574, "Trojan Source",
/// shows of source code. They are rustc's
/// `text_direction_codepoint_in_literal`'s.
pub const BIDI_CONTROLS: [char; 9] = [
    '\u{202A}', '\u{202B}', '\u{202C}', '\u{202D}', '\u{202E}', '\u{2066}', '\u{2067}', '\u{2068}',
    '\u{2069}',
];

/// Write the body of a text literal for `text`: quotes and backslashes are
/// escaped, as are control characters and [`BIDI_CONTROLS`], so that what
/// is written shows what the text holds, in its order, and can't break the
/// line it is on or change the terminal it is written to. The quotes
/// around it are not written.
///
/// # Errors
///
/// Returns an error if `out` does.
pub fn escape(text: &str, out: &mut impl fmt::Write) -> fmt::Result {
    escape_but(text, out, None)
}

/// Write `text` with nothing around it, as a table's cell shows it: as
/// [`escape`] writes a literal's body, so that a control character can't
/// break the line or change the terminal, nor a bidirectional control
/// reorder it, but with quotes as they are, since none end it.
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

/// Write how an error names the relation that is the component
/// `component` of a value of the type named `ty`, when compiling and when
/// running: `` a `polygon`'s `vertices` ``, or `an` before a name that
/// starts with a vowel.
///
/// # Errors
///
/// Returns an error if `out` does.
pub fn write_component(ty: &str, component: &str, out: &mut impl fmt::Write) -> fmt::Result {
    write!(out, "{} `{ty}`'s `{component}`", article(ty))
}

/// The indefinite article before `word`, in an error: `an` before one that
/// starts with a vowel, and otherwise `a`.
#[must_use]
pub fn article(word: &str) -> &'static str {
    if word.starts_with(['a', 'e', 'i', 'o', 'u']) {
        "an"
    } else {
        "a"
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
            None if c.is_control() || BIDI_CONTROLS.contains(&c) => {
                write!(out, "\\u{{{:X}}}", u32::from(c))?;
            }
            None => out.write_char(c)?,
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_could_change_a_terminal_or_reorder_text_is_escaped() {
        let escaped = |text: &str, bare: bool| {
            let mut out = String::new();
            if bare {
                escape_bare(text, &mut out).unwrap();
            } else {
                escape(text, &mut out).unwrap();
            }
            out
        };
        // An escape sequence, a line break, and an override that shows
        // what follows it backwards.
        let text = "a\u{1B}[31m\"b\"\nc\u{202E}d\u{2066}";
        assert_eq!(
            escaped(text, false),
            "a\\u{1B}[31m\\\"b\\\"\\nc\\u{202E}d\\u{2066}"
        );
        assert_eq!(
            escaped(text, true),
            "a\\u{1B}[31m\"b\"\\nc\\u{202E}d\\u{2066}"
        );
        // Other characters, right-to-left ones too, are as they are.
        assert_eq!(escaped("שלום, 西安", false), "שלום, 西安");
    }

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
