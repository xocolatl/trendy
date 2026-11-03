//! What literals mean. The lexer uses this module to report malformed
//! literals and sema uses it to decode them, so the rules live only here.

use std::ops::Range;

pub use trendy_text::SIMPLE_ESCAPES;

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

/// Why an integer literal token isn't one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntProblem {
    /// Like `0X1F`: a radix prefix in upper case, which is written in lower
    /// case, as `fixed` writes it.
    Uppercase { fixed: String },
    /// Like `1__000`, `1_` or `0x_1F`: an underscore that isn't between two
    /// digits, which `fixed` leaves out.
    Underscores { fixed: String },
    /// Like `0755`: a decimal number after a leading zero, which C reads
    /// as octal, `digits` without the zeros.
    LeadingZero { digits: String },
    /// Like `0b102` or `12ab`: `digit`, at byte `at` of the text, isn't
    /// one of the radix, named `radix`.
    Digit {
        digit: char,
        at: usize,
        radix: &'static str,
    },
    /// Like `0x`: a prefix with no digit after it.
    NoDigits,
}

/// The radix prefixes of integer literals, with their radixes and names.
pub const RADIXES: [(&str, u32, &str); 3] = [
    ("0x", 16, "hexadecimal"),
    ("0o", 8, "octal"),
    ("0b", 2, "binary"),
];

/// `digits`, a run of digits of `radix`, named `name`, with underscores,
/// at byte `start` of a literal, with each underscore that isn't between
/// two digits left out, or what is wrong if it has anything else, or no
/// digit.
fn separated(
    digits: &str,
    start: usize,
    (radix, name): (u32, &'static str),
) -> Result<String, IntProblem> {
    let mut fixed = String::new();
    for (i, c) in digits.char_indices() {
        match c {
            '_' if fixed.ends_with(|d: char| d.is_digit(radix)) => fixed.push('_'),
            '_' => {}
            c if c.is_digit(radix) => fixed.push(c),
            digit => {
                return Err(IntProblem::Digit {
                    digit,
                    at: start + i,
                    radix: name,
                });
            }
        }
    }
    let fixed = fixed.trim_end_matches('_').to_string();
    if fixed.is_empty() {
        Err(IntProblem::NoDigits)
    } else {
        Ok(fixed)
    }
}

/// Check an integer literal token's text: decimal digits, or a radix
/// prefix, `0x`, `0o` or `0b`, and hexadecimal, octal or binary ones, each
/// underscore between two digits. A decimal number has no leading zero,
/// as C reads one as octal.
///
/// # Errors
///
/// Returns what is wrong with a malformed literal.
pub fn check_int(text: &str) -> Result<(), IntProblem> {
    for (prefix, radix, name) in RADIXES {
        let upper = prefix.to_uppercase();
        let (body, uppercase) = if let Some(body) = text.strip_prefix(prefix) {
            (body, false)
        } else if let Some(body) = text.strip_prefix(upper.as_str()) {
            (body, true)
        } else {
            continue;
        };
        let fixed = separated(body, prefix.len(), (radix, name))?;
        return match (uppercase, fixed == body) {
            (false, true) => Ok(()),
            (true, _) => Err(IntProblem::Uppercase {
                fixed: format!("{prefix}{fixed}"),
            }),
            (false, false) => Err(IntProblem::Underscores {
                fixed: format!("{prefix}{fixed}"),
            }),
        };
    }
    let fixed = separated(text, 0, (10, "decimal"))?;
    let digits = fixed.replace('_', "");
    if digits.len() > 1 && digits.starts_with('0') {
        let trimmed = digits.trim_start_matches('0');
        return Err(IntProblem::LeadingZero {
            digits: if trimmed.is_empty() { "0" } else { trimmed }.to_string(),
        });
    }
    if fixed == text {
        Ok(())
    } else {
        Err(IntProblem::Underscores { fixed })
    }
}

/// The value of a well-formed integer literal, after a `-` if it is
/// negative, or `None` if it doesn't fit in `i64`.
#[must_use]
pub fn int_value(text: &str) -> Option<i64> {
    let (negative, unsigned) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text),
    };
    let (digits, radix) = RADIXES
        .iter()
        .find_map(|&(prefix, radix, _)| Some((unsigned.strip_prefix(prefix)?, radix)))
        .unwrap_or((unsigned, 10));
    let magnitude = i128::from_str_radix(&digits.replace('_', ""), radix).ok()?;
    i64::try_from(if negative { -magnitude } else { magnitude }).ok()
}

/// The letters that start an exponent in an approximate literal.
pub const EXPONENT_MARKERS: [char; 2] = ['e', 'E'];

/// Whether a number token is an approximate literal: it has a decimal point
/// or an exponent. One with a point but no exponent, such as `1.5`, is a
/// malformed approximate literal. Radix-prefixed tokens like `0x1E` are
/// integers, malformed.
#[must_use]
pub fn is_approximate(text: &str) -> bool {
    let radix_prefixed = ["0x", "0o", "0b", "0X", "0O", "0B"]
        .iter()
        .any(|p| text.starts_with(p));
    !radix_prefixed && text.contains(|c| c == '.' || EXPONENT_MARKERS.contains(&c))
}

/// Why an approximate literal token isn't in canonical form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApproximateProblem {
    /// Close to a valid literal: `fixed` is the valid form, and `reason`
    /// completes "this literal …", as in "has digit separators".
    Fixable { fixed: String, reason: &'static str },
    /// Not repairable mechanically; `reason` completes "this literal …".
    Invalid { reason: &'static str },
}

/// Check an approximate literal token's text: digits, then an optional `.`
/// and digits, then an exponent (`e` or `E`, an optional sign, and digits).
/// The exponent is required: a decimal number without one, such as `1.5`,
/// is reserved for an exact type.
///
/// # Errors
///
/// Returns what is wrong with a malformed literal, and the fix if there is
/// one.
pub fn check_approximate(text: &str) -> Result<(), ApproximateProblem> {
    if is_canonical_approximate(text) {
        return Ok(());
    }
    // Repairs, in order; the first one that applies gives the reason.
    let mut fixed = without_stray_underscores(text);
    let mut reason =
        (fixed.len() != text.len()).then_some("has an underscore not between two digits");
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
    if !fixed.contains(EXPONENT_MARKERS) {
        // `1.0` is `1e0`, and `1.5` is `1.5e0`.
        if let Some(point) = fixed.find('.')
            && fixed[point + 1..].bytes().all(|b| b == b'0')
        {
            fixed.truncate(point);
        }
        fixed.push_str("e0");
        reason.get_or_insert("has no exponent");
    }
    match reason {
        Some(reason) if is_canonical_approximate(&fixed) => {
            Err(ApproximateProblem::Fixable { fixed, reason })
        }
        _ => Err(ApproximateProblem::Invalid {
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

/// `text` with each run of underscores between two digits made one, and
/// any other left out.
fn without_stray_underscores(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut fixed = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '_' {
            fixed.push(chars[i]);
            i += 1;
            continue;
        }
        let end = (i..chars.len())
            .find(|&j| chars[j] != '_')
            .unwrap_or(chars.len());
        let between = i > 0
            && chars[i - 1].is_ascii_digit()
            && chars.get(end).is_some_and(char::is_ascii_digit);
        if between {
            fixed.push('_');
        }
        i = end;
    }
    fixed
}

fn is_canonical_approximate(text: &str) -> bool {
    /// How long the run of decimal digits `s` starts with is, each
    /// underscore in it between two digits.
    fn digits(s: &str) -> usize {
        let bytes = s.as_bytes();
        let mut len = 0;
        while len < bytes.len() {
            let digit = bytes[len].is_ascii_digit();
            let separator = bytes[len] == b'_'
                && len > 0
                && bytes[len - 1].is_ascii_digit()
                && bytes.get(len + 1).is_some_and(u8::is_ascii_digit);
            if !digit && !separator {
                break;
            }
            len += 1;
        }
        len
    }
    let whole = digits(text);
    if whole == 0 {
        return false;
    }
    let mut rest = &text[whole..];
    if let Some(after) = rest.strip_prefix('.') {
        let fraction = digits(after);
        if fraction == 0 {
            return false;
        }
        rest = &after[fraction..];
    }
    let Some(exponent) = rest.strip_prefix(EXPONENT_MARKERS) else {
        return false;
    };
    let exponent = exponent.strip_prefix(['+', '-']).unwrap_or(exponent);
    !exponent.is_empty() && digits(exponent) == exponent.len()
}

/// The value of a well-formed approximate literal, or `None` if it is too
/// large to represent.
#[must_use]
pub fn approximate_value(text: &str) -> Option<f64> {
    text.replace('_', "")
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite())
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
    fn escape_is_the_inverse_of_decode() {
        let escape = |text: &str| {
            let mut body = String::new();
            trendy_text::escape(text, &mut body).unwrap();
            body
        };
        for text in ["", "plain", "a\"b\\c\nd\re\tf\0g", "\u{7}bell", "é😀"] {
            let body = escape(text);
            assert_eq!(decode_text(&body), (text.to_string(), vec![]), "{body}");
        }
        assert_eq!(escape("say \"hi\""), r#"say \"hi\""#);
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
        for valid in [
            "0",
            "42",
            "1_000",
            "0x1F",
            "0xff_FF",
            "0o755",
            "0b1010_0101",
        ] {
            assert_eq!(check_int(valid), Ok(()), "{valid}");
        }
        let fixed = |fixed: &str| fixed.to_string();
        assert_eq!(
            check_int("0X1F"),
            Err(IntProblem::Uppercase {
                fixed: fixed("0x1F")
            })
        );
        assert_eq!(
            check_int("0B_1__0_"),
            Err(IntProblem::Uppercase {
                fixed: fixed("0b1_0")
            })
        );
        for (written, expected) in [("1__000", "1_000"), ("1_", "1"), ("0x_1F", "0x1F")] {
            assert_eq!(
                check_int(written),
                Err(IntProblem::Underscores {
                    fixed: fixed(expected)
                }),
                "{written}"
            );
        }
        // C's octal, which a decimal number would silently differ from.
        assert_eq!(
            check_int("0755"),
            Err(IntProblem::LeadingZero {
                digits: fixed("755")
            })
        );
        assert_eq!(
            check_int("00"),
            Err(IntProblem::LeadingZero { digits: fixed("0") })
        );
        let digit = |digit, at, radix| Err(IntProblem::Digit { digit, at, radix });
        assert_eq!(check_int("12ab"), digit('a', 2, "decimal"));
        assert_eq!(check_int("0b1_02"), digit('2', 5, "binary"));
        assert_eq!(check_int("0o8"), digit('8', 2, "octal"));
        assert_eq!(check_int("0xFG"), digit('G', 3, "hexadecimal"));
        for empty in ["0x", "0x_"] {
            assert_eq!(check_int(empty), Err(IntProblem::NoDigits), "{empty}");
        }
        assert_eq!(int_value("9223372036854775807"), Some(i64::MAX));
        assert_eq!(int_value("9223372036854775808"), None);
        assert_eq!(int_value("-9_223_372_036_854_775_808"), Some(i64::MIN));
        assert_eq!(int_value("0xFF"), Some(255));
        assert_eq!(int_value("0o755"), Some(493));
        assert_eq!(int_value("0b1010_0101"), Some(165));
        assert_eq!(int_value("-0x8000_0000_0000_0000"), Some(i64::MIN));
        assert_eq!(int_value("0x8000_0000_0000_0000"), None);
    }

    #[test]
    fn approximate_numbers() {
        for valid in [
            "0e0",
            "5e0",
            "1.75e1",
            "1E10",
            "1.5e-3",
            "2.5E+2",
            "007.5e0",
            "1_000.25e1_0",
        ] {
            assert_eq!(check_approximate(valid), Ok(()), "{valid}");
            assert!(is_approximate(valid), "{valid}");
        }
        assert!(!is_approximate("42"));
        assert!(!is_approximate("0x1E"));
    }

    #[test]
    fn approximate_fixes() {
        let fixed = |text: &str| match check_approximate(text) {
            Err(ApproximateProblem::Fixable { fixed, reason }) => (fixed, reason),
            other => panic!("{text}: {other:?}"),
        };
        assert_eq!(fixed("1.5"), ("1.5e0".to_string(), "has no exponent"));
        assert_eq!(fixed("1.0"), ("1e0".to_string(), "has no exponent"));
        assert_eq!(fixed("0.00"), ("0e0".to_string(), "has no exponent"));
        assert_eq!(
            fixed("5."),
            ("5e0".to_string(), "has no digits after the decimal point")
        );
        assert_eq!(
            fixed(".5"),
            (
                "0.5e0".to_string(),
                "has no digits before the decimal point"
            )
        );
        assert_eq!(
            fixed("1_000.5"),
            ("1_000.5e0".to_string(), "has no exponent")
        );
        assert_eq!(
            fixed("1__000.5_e0"),
            (
                "1_000.5e0".to_string(),
                "has an underscore not between two digits"
            )
        );
        assert_eq!(
            fixed("5.e3"),
            ("5.0e3".to_string(), "has no digits after the decimal point")
        );
    }

    #[test]
    fn invalid_approximates() {
        let reason = |text: &str| match check_approximate(text) {
            Err(ApproximateProblem::Invalid { reason }) => reason,
            other => panic!("{text}: {other:?}"),
        };
        assert_eq!(reason("1e"), "has an exponent with no digits");
        assert_eq!(reason("1.5e+"), "has an exponent with no digits");
        assert_eq!(reason("1.5x"), "is not a decimal number");
        assert_eq!(reason("1.2.3"), "is not a decimal number");
    }

    #[test]
    fn approximate_values() {
        assert_eq!(approximate_value("1.75e1"), Some(17.5));
        assert_eq!(approximate_value("1.5e-3"), Some(0.0015));
        assert_eq!(approximate_value("1e400"), None);
        assert_eq!(approximate_value("1_000.5e0"), Some(1000.5));
    }

    #[test]
    fn escape_list_names_every_escape() {
        assert_eq!(escape_list(), r#"\" \\ \n \r \t \0 \u{…}"#);
    }
}
