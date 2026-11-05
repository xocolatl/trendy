//! What a character is: what a reader sees as one. A text is made of code points,
//! which are stable, but a flag is two of them, a family of four seven, and `é` one
//! or two. So a character is an extended grapheme cluster, by the default rules of
//! UAX #29 at [`UNICODE_VERSION`], without any locale's tailoring, so that it depends
//! on its code points alone: Slovak's `ch` is two.
//!
//! Those rules, and the properties they read, change between versions of Unicode, so
//! what is one character does too, and a type is a set of values. So the version is
//! pinned with the language, and is changed only by a change to the language.

use crate::character_tables::{
    BREAK, CONTROL, CR, EXTEND, FIRST_HANGUL, HANGUL, INDIC, INDIC_CONSONANT, INDIC_EXTEND,
    INDIC_LINKER, L, LF, LV, LVT, PICTOGRAPHIC, PREPEND, PROPERTIES, REGIONAL_INDICATOR,
    SPACING_MARK, T, TABLES_VERSION, V, ZWJ,
};

/// The version of Unicode whose rules say what a character is.
pub const UNICODE_VERSION: &str = TABLES_VERSION;

/// Below this, the first combining mark, no two code points but CR and LF are ever
/// joined, whatever is before them.
const FIRST_JOINING: u32 = 0x300;

/// The properties of the code point `c`.
fn properties(c: char) -> u8 {
    let c = u32::from(c);
    let after = PROPERTIES.partition_point(|entry| entry >> 8 <= c);
    let property = PROPERTIES[after - 1].to_le_bytes()[0];
    if property & BREAK == HANGUL {
        let class = if (c - FIRST_HANGUL).is_multiple_of(28) {
            LV
        } else {
            LVT
        };
        return property & !BREAK | class;
    }
    property
}

/// How far a character is into what `GB11` joins: `Extended_Pictographic Extend* ZWJ`.
#[derive(Clone, Copy, Default, PartialEq)]
enum Joining {
    #[default]
    No,
    /// The last is `Extended_Pictographic`, and any `Extend` after it.
    Pictograph,
    /// The last is a `ZWJ` after those.
    Joined,
}

/// What the rules with context remember of the code points of a character so far.
#[derive(Default)]
struct Context {
    /// `GB9c`: the last is `Indic_Conjunct_Break=Linker`, and any of its `Extend` after it.
    linked: bool,
    joining: Joining,
    /// `GB12` and `GB13`: the last is a regional indicator, of an odd number of them.
    odd_flag: bool,
}

impl Context {
    /// What it remembers once the code point with properties `added` is added.
    fn with(&self, added: u8) -> Context {
        let class = added & BREAK;
        Context {
            linked: match added & INDIC {
                INDIC_LINKER => true,
                INDIC_EXTEND => self.linked,
                _ => false,
            },
            joining: if added & PICTOGRAPHIC != 0
                || (self.joining == Joining::Pictograph && class == EXTEND)
            {
                Joining::Pictograph
            } else if self.joining == Joining::Pictograph && class == ZWJ {
                Joining::Joined
            } else {
                Joining::No
            },
            odd_flag: class == REGIONAL_INDICATOR && !self.odd_flag,
        }
    }

    /// Whether a character ends between code points with properties `before` and
    /// `after`, the rules of UAX #29's Table 1c in order, the first that applies
    /// deciding.
    fn breaks(&self, before: u8, after: u8) -> bool {
        let (left, right) = (before & BREAK, after & BREAK);
        match (left, right) {
            (CR, LF) => false,
            (CR | LF | CONTROL, _) | (_, CR | LF | CONTROL) => true,
            (L, L | V | LV | LVT)
            | (LV | V, V | T)
            | (LVT | T, T)
            | (_, EXTEND | ZWJ | SPACING_MARK)
            | (PREPEND, _) => false,
            _ if self.linked && after & INDIC == INDIC_CONSONANT => false,
            _ if self.joining == Joining::Joined && after & PICTOGRAPHIC != 0 => false,
            (REGIONAL_INDICATOR, REGIONAL_INDICATOR) => !self.odd_flag,
            _ => true,
        }
    }
}

/// How long the first character of `text` is, in bytes: nothing only if it is empty.
#[must_use]
pub fn first_character_len(text: &str) -> usize {
    let mut chars = text.char_indices();
    let Some((_, mut last)) = chars.next() else {
        return 0;
    };
    // The last code point's properties, and what its character remembers, once
    // something has needed them: most text has only code points that never join.
    let mut known: Option<(u8, Context)> = None;
    for (at, c) in chars {
        let low = u32::from(last) < FIRST_JOINING && u32::from(c) < FIRST_JOINING;
        if low && (last, c) != ('\r', '\n') {
            return at;
        }
        let (left, context) = known.get_or_insert_with(|| {
            let left = properties(last);
            (left, Context::default().with(left))
        });
        let right = properties(c);
        if context.breaks(*left, right) {
            return at;
        }
        let context = context.with(right);
        known = Some((right, context));
        last = c;
    }
    text.len()
}

/// Whether `text` is exactly one character.
#[must_use]
pub fn is_one_character(text: &str) -> bool {
    !text.is_empty() && first_character_len(text) == text.len()
}

/// The characters of `text`, in order, each as the text of it: together, `text`.
#[must_use]
pub fn characters(text: &str) -> Characters<'_> {
    Characters { rest: text }
}

/// The iterator [`characters`] gives.
#[derive(Clone)]
pub struct Characters<'a> {
    rest: &'a str,
}

impl<'a> Iterator for Characters<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<&'a str> {
        if self.rest.is_empty() {
            return None;
        }
        let (first, rest) = self.rest.split_at(first_character_len(self.rest));
        self.rest = rest;
        Some(first)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The characters of `text`.
    fn split(text: &str) -> Vec<&str> {
        characters(text).collect()
    }

    #[test]
    fn what_a_reader_sees_as_one_is_one() {
        for one in [
            "a",
            "🇸🇪",
            "🏴\u{E0067}\u{E0062}\u{E0065}\u{E006E}\u{E0067}\u{E007F}",
            "👨\u{200D}👩\u{200D}👧\u{200D}👦",
            "\u{E9}",
            "e\u{301}",
            "\r\n",
            "한",
            "\u{1112}\u{1161}\u{11AB}",
            "👍\u{1F3FD}",
            // Devanagari `kṣa`, one since Unicode 15.1.
            "\u{915}\u{94D}\u{937}",
            // A combining mark with nothing to combine with.
            "\u{301}",
        ] {
            assert!(is_one_character(one), "{one:?}");
            assert_eq!(split(one), [one], "{one:?}");
        }
    }

    #[test]
    fn what_a_reader_sees_as_several_is_several() {
        assert_eq!(split("abc"), ["a", "b", "c"]);
        assert_eq!(split("🇸🇪🇫🇮"), ["🇸🇪", "🇫🇮"]);
        // A flag's half, after a whole one.
        assert_eq!(split("🇸🇪🇫"), ["🇸🇪", "🇫"]);
        assert_eq!(split("e\u{301}a"), ["e\u{301}", "a"]);
        assert_eq!(split("\n\r"), ["\n", "\r"]);
        assert_eq!(split("a\r\nb"), ["a", "\r\n", "b"]);
        assert_eq!(split("ch"), ["c", "h"]);
        for several in ["", "ab", "🇸🇪🇫🇮", "\n\r"] {
            assert!(!is_one_character(several), "{several:?}");
        }
        assert_eq!(first_character_len(""), 0);
        assert_eq!(split(""), Vec::<&str>::new());
    }

    /// Every line of Unicode's own test of these rules: a text's code points, with
    /// `÷` where it breaks and `×` where it doesn't.
    #[test]
    fn unicodes_own_tests_pass() {
        let tests = include_str!("../unicode/GraphemeBreakTest.txt");
        assert!(
            tests.starts_with(&format!("# GraphemeBreakTest-{UNICODE_VERSION}.txt")),
            "the tests are of another version"
        );
        let mut count = 0;
        for line in tests.lines() {
            let line = line.split('#').next().expect("a line has a first part");
            if line.trim().is_empty() {
                continue;
            }
            let (mut text, mut expected, mut character) =
                (String::new(), Vec::new(), String::new());
            for word in line.split_whitespace() {
                match word {
                    "×" => {}
                    "÷" => {
                        if !character.is_empty() {
                            expected.push(std::mem::take(&mut character));
                        }
                    }
                    point => {
                        let point = u32::from_str_radix(point, 16).expect("a code point");
                        let c = char::from_u32(point).expect("a scalar value");
                        text.push(c);
                        character.push(c);
                    }
                }
            }
            assert_eq!(split(&text), expected, "{line}");
            count += 1;
        }
        assert!(count > 800, "only {count} tests");
    }

    /// What lets [`first_character_len`] look at no properties for most text.
    #[test]
    fn nothing_below_the_first_combining_mark_joins() {
        for entry in PROPERTIES.iter().take_while(|e| *e >> 8 < FIRST_JOINING) {
            let property = entry.to_le_bytes()[0];
            assert!(
                matches!(property & BREAK, 0 | CR | LF | CONTROL),
                "{entry:#x}"
            );
            assert_eq!(property & INDIC, 0, "{entry:#x}");
        }
    }
}
