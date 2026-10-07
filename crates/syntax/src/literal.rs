//! What literals mean. The lexer uses this module to report malformed
//! literals and sema uses it to decode them, so the rules live only here.

use std::ops::Range;

/// Escapes written as a backslash and one character, and what they stand for.
pub const SIMPLE_ESCAPES: &[(char, char)] = &[
    ('"', '"'),
    ('\\', '\\'),
    ('n', '\n'),
    ('r', '\r'),
    ('t', '\t'),
    ('0', '\0'),
];

/// The written forms of every escape, for messages: `\" \\ \n \r \t \0 \u{…}`.
#[must_use]
pub fn escape_list() -> String {
    SIMPLE_ESCAPES
        .iter()
        .map(|(c, _)| format!("\\{c}"))
        .chain(std::iter::once("\\u{…}".to_string()))
        .collect::<Vec<_>>()
        .join(" ")
}

/// A problem inside a text literal. Ranges are byte offsets into the body,
/// the text between the quotes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextProblem {
    /// A backslash followed by a character that isn't an escape.
    UnknownEscape { range: Range<usize>, found: char },
    /// A malformed `\u{…}`; `reason` completes "invalid Unicode escape: …".
    InvalidUnicode {
        range: Range<usize>,
        reason: &'static str,
    },
}

/// Decode the body of a text literal. Problems are reported and skipped, so
/// the returned text is only meaningful when there are none.
#[must_use]
pub fn decode_text(body: &str) -> (String, Vec<TextProblem>) {
    let mut text = String::with_capacity(body.len());
    let mut problems = Vec::new();
    let mut chars = body.char_indices().peekable();

    while let Some((start, c)) = chars.next() {
        if c != '\\' {
            text.push(c);
            continue;
        }
        let Some((_, e)) = chars.next() else {
            // A trailing backslash means the closing quote was escaped, which
            // the lexer reports as an unterminated literal.
            break;
        };
        if let Some(&(_, value)) = SIMPLE_ESCAPES.iter().find(|(name, _)| *name == e) {
            text.push(value);
        } else if e == 'u' {
            match unicode_escape(body, start, &mut chars) {
                Ok(value) => text.push(value),
                Err(problem) => problems.push(problem),
            }
        } else {
            problems.push(TextProblem::UnknownEscape {
                range: start..start + 1 + e.len_utf8(),
                found: e,
            });
        }
    }
    (text, problems)
}

/// Parse the rest of `\u{…}` after the `u`; `start` is the backslash.
fn unicode_escape(
    body: &str,
    start: usize,
    chars: &mut std::iter::Peekable<std::str::CharIndices>,
) -> Result<char, TextProblem> {
    let invalid = |end: usize, reason| TextProblem::InvalidUnicode {
        range: start..end,
        reason,
    };
    let after_u = start + 2;
    if chars.next_if(|&(_, c)| c == '{').is_none() {
        return Err(invalid(after_u, "expected `{` after `\\u`"));
    }
    let digits_start = after_u + 1;
    let mut end = digits_start;
    while let Some((i, c)) = chars.next_if(|&(_, c)| c != '}' && c != '"' && c != '\\') {
        end = i + c.len_utf8();
    }
    if chars.next_if(|&(_, c)| c == '}').is_none() {
        return Err(invalid(end, "expected `}` to close it"));
    }
    let digits = &body[digits_start..end];
    let range_end = end + 1;
    if digits.is_empty() || digits.len() > 6 || !digits.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(invalid(range_end, "expected 1 to 6 hexadecimal digits"));
    }
    let value = u32::from_str_radix(digits, 16).expect("validated hex digits");
    char::from_u32(value).ok_or_else(|| {
        invalid(
            range_end,
            if (0xD800..=0xDFFF).contains(&value) {
                "surrogates are not characters"
            } else {
                "the largest character is U+10FFFF"
            },
        )
    })
}

/// Why an integer literal token isn't a plain decimal number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntProblem {
    /// Like `1_000`. The digits without underscores are valid.
    Underscores { digits: String },
    /// Like `0x1F`: a radix prefix, with the value if it fits in `i64`.
    Radix {
        name: &'static str,
        value: Option<i64>,
    },
    /// Anything else, like `12ab`.
    Invalid,
}

/// Check an integer literal token's text: ASCII digits only.
///
/// # Errors
///
/// Returns what is wrong with a malformed literal.
pub fn check_int(text: &str) -> Result<(), IntProblem> {
    if text.bytes().all(|b| b.is_ascii_digit()) {
        return Ok(());
    }
    let prefixes = [
        ("0x", 16, "hexadecimal"),
        ("0o", 8, "octal"),
        ("0b", 2, "binary"),
    ];
    for (prefix, radix, name) in prefixes {
        if let Some(digits) = text
            .strip_prefix(prefix)
            .or_else(|| text.strip_prefix(&prefix.to_uppercase()))
        {
            let digits = digits.replace('_', "");
            if !digits.is_empty() && digits.chars().all(|c| c.is_digit(radix)) {
                let value = i64::from_str_radix(&digits, radix).ok();
                return Err(IntProblem::Radix { name, value });
            }
            return Err(IntProblem::Invalid);
        }
    }
    let digits = text.replace('_', "");
    if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) && !text.ends_with('_') {
        return Err(IntProblem::Underscores { digits });
    }
    Err(IntProblem::Invalid)
}

/// The value of a well-formed integer literal, or `None` if it doesn't fit
/// in `i64`.
#[must_use]
pub fn int_value(text: &str) -> Option<i64> {
    text.parse().ok()
}

/// The letters that start an exponent in a rational literal.
pub const EXPONENT_MARKERS: [char; 2] = ['e', 'E'];

/// Whether a number token is a rational literal: it has a decimal point or
/// an exponent. Radix-prefixed tokens like `0x1E` are integers, malformed.
#[must_use]
pub fn is_rational(text: &str) -> bool {
    let radix_prefixed = ["0x", "0o", "0b", "0X", "0O", "0B"]
        .iter()
        .any(|p| text.starts_with(p));
    !radix_prefixed && text.contains(|c| c == '.' || EXPONENT_MARKERS.contains(&c))
}

/// Why a rational literal token isn't in canonical form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RationalProblem {
    /// Close to a valid literal: `fixed` is the valid form, and `reason`
    /// completes "this literal …", as in "has digit separators".
    Fixable { fixed: String, reason: &'static str },
    /// Not repairable mechanically; `reason` completes "this literal …".
    Invalid { reason: &'static str },
}

/// Check a rational literal token's text: digits, then an optional `.` and
/// digits, then an optional exponent (`e` or `E`, an optional sign, and
/// digits), with at least a point or an exponent.
///
/// # Errors
///
/// Returns what is wrong with a malformed literal, and the fix if there is
/// one.
pub fn check_rational(text: &str) -> Result<(), RationalProblem> {
    if is_canonical_rational(text) {
        return Ok(());
    }
    // Repairs, in order; the first one that applies gives the reason.
    let mut fixed = text.replace('_', "");
    let mut reason = (fixed.len() != text.len()).then_some("has digit separators");
    if fixed.starts_with('.') {
        fixed.insert(0, '0');
        reason.get_or_insert("has no digits before the decimal point");
    }
    if let Some(point) = fixed.find('.')
        && !fixed[point + 1..].starts_with(|c: char| c.is_ascii_digit())
    {
        fixed.insert(point + 1, '0');
        reason.get_or_insert("has no digits after the decimal point");
    }
    match reason {
        Some(reason) if is_canonical_rational(&fixed) => {
            Err(RationalProblem::Fixable { fixed, reason })
        }
        _ => Err(RationalProblem::Invalid {
            reason: if fixed
                .trim_end_matches(['+', '-'])
                .ends_with(EXPONENT_MARKERS)
            {
                "has an exponent with no digits"
            } else {
                "is not a decimal number"
            },
        }),
    }
}

fn is_canonical_rational(text: &str) -> bool {
    fn digits(s: &str) -> usize {
        s.bytes().take_while(u8::is_ascii_digit).count()
    }
    let whole = digits(text);
    if whole == 0 {
        return false;
    }
    let mut rest = &text[whole..];
    let mut has_point = false;
    if let Some(after) = rest.strip_prefix('.') {
        let fraction = digits(after);
        if fraction == 0 {
            return false;
        }
        has_point = true;
        rest = &after[fraction..];
    }
    if rest.is_empty() {
        return has_point;
    }
    let Some(exponent) = rest.strip_prefix(EXPONENT_MARKERS) else {
        return false;
    };
    let exponent = exponent.strip_prefix(['+', '-']).unwrap_or(exponent);
    !exponent.is_empty() && digits(exponent) == exponent.len()
}

/// The value of a well-formed rational literal, or `None` if it is too
/// large to represent.
#[must_use]
pub fn rational_value(text: &str) -> Option<f64> {
    text.parse::<f64>().ok().filter(|v| v.is_finite())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_escapes() {
        assert_eq!(
            decode_text(r#"a\"b\\c\nd\re\tf\0g"#),
            ("a\"b\\c\nd\re\tf\0g".to_string(), vec![])
        );
    }

    #[test]
    fn unicode_escapes() {
        assert_eq!(decode_text(r"\u{1F600}\u{e9}"), ("😀é".to_string(), vec![]));
    }

    #[test]
    fn unknown_escape() {
        let (_, problems) = decode_text(r"it\'s");
        assert_eq!(
            problems,
            [TextProblem::UnknownEscape {
                range: 2..4,
                found: '\''
            }]
        );
    }

    #[test]
    fn invalid_unicode_escapes() {
        let reason = |body: &str| match &decode_text(body).1[..] {
            [TextProblem::InvalidUnicode { reason, .. }] => *reason,
            other => panic!("{body}: {other:?}"),
        };
        assert_eq!(reason(r"\u41"), "expected `{` after `\\u`");
        assert_eq!(reason(r"\u{41"), "expected `}` to close it");
        assert_eq!(reason(r"\u{}"), "expected 1 to 6 hexadecimal digits");
        assert_eq!(reason(r"\u{xyz}"), "expected 1 to 6 hexadecimal digits");
        assert_eq!(reason(r"\u{D800}"), "surrogates are not characters");
        assert_eq!(reason(r"\u{110000}"), "the largest character is U+10FFFF");
    }

    #[test]
    fn invalid_unicode_escape_range_covers_the_escape() {
        let (_, problems) = decode_text(r"ab\u{D800}cd");
        assert_eq!(
            problems,
            [TextProblem::InvalidUnicode {
                range: 2..10,
                reason: "surrogates are not characters"
            }]
        );
    }

    #[test]
    fn integers() {
        assert_eq!(check_int("0"), Ok(()));
        assert_eq!(check_int("007"), Ok(()));
        assert_eq!(
            check_int("1_000"),
            Err(IntProblem::Underscores {
                digits: "1000".to_string()
            })
        );
        assert_eq!(
            check_int("0x1F"),
            Err(IntProblem::Radix {
                name: "hexadecimal",
                value: Some(31)
            })
        );
        assert_eq!(
            check_int("0b101"),
            Err(IntProblem::Radix {
                name: "binary",
                value: Some(5)
            })
        );
        assert_eq!(
            check_int("0xFFFFFFFFFFFFFFFFFF"),
            Err(IntProblem::Radix {
                name: "hexadecimal",
                value: None
            })
        );
        assert_eq!(check_int("12ab"), Err(IntProblem::Invalid));
        assert_eq!(check_int("1_"), Err(IntProblem::Invalid));
        assert_eq!(int_value("9223372036854775807"), Some(i64::MAX));
        assert_eq!(int_value("9223372036854775808"), None);
    }

    #[test]
    fn rationals() {
        for valid in [
            "0.0", "5.0", "17.5", "0e0", "1E10", "1.5e-3", "2.5E+2", "007.5",
        ] {
            assert_eq!(check_rational(valid), Ok(()), "{valid}");
            assert!(is_rational(valid), "{valid}");
        }
        assert!(!is_rational("42"));
        assert!(!is_rational("0x1E"));
    }

    #[test]
    fn rational_fixes() {
        let fixed = |text: &str| match check_rational(text) {
            Err(RationalProblem::Fixable { fixed, reason }) => (fixed, reason),
            other => panic!("{text}: {other:?}"),
        };
        assert_eq!(
            fixed("5."),
            ("5.0".to_string(), "has no digits after the decimal point")
        );
        assert_eq!(
            fixed(".5"),
            ("0.5".to_string(), "has no digits before the decimal point")
        );
        assert_eq!(
            fixed("1_000.5"),
            ("1000.5".to_string(), "has digit separators")
        );
        assert_eq!(
            fixed("5.e3"),
            ("5.0e3".to_string(), "has no digits after the decimal point")
        );
    }

    #[test]
    fn invalid_rationals() {
        let reason = |text: &str| match check_rational(text) {
            Err(RationalProblem::Invalid { reason }) => reason,
            other => panic!("{text}: {other:?}"),
        };
        assert_eq!(reason("1e"), "has an exponent with no digits");
        assert_eq!(reason("1.5e+"), "has an exponent with no digits");
        assert_eq!(reason("1.5x"), "is not a decimal number");
        assert_eq!(reason("1.2.3"), "is not a decimal number");
    }

    #[test]
    fn rational_values() {
        assert_eq!(rational_value("17.5"), Some(17.5));
        assert_eq!(rational_value("1.5e-3"), Some(0.0015));
        assert_eq!(rational_value("1e400"), None);
    }

    #[test]
    fn escape_list_names_every_escape() {
        assert_eq!(escape_list(), r#"\" \\ \n \r \t \0 \u{…}"#);
    }
}
