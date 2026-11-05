//! Writes `src/character_tables.rs` from the data files of a version of Unicode:
//!
//! ```text
//! UPDATE_UNICODE=dir cargo test -p trendy_text --test character_tables
//! ```
//!
//! `dir` holds `GraphemeBreakProperty.txt`, `emoji-data.txt` and
//! `DerivedCoreProperties.txt`, from `https://www.unicode.org/Public/<version>/ucd/`,
//! and `unicode/GraphemeBreakTest.txt` must be replaced by that version's too, which
//! `characters.rs`'s tests pass, or the rules there have changed with it. It isn't a
//! check that the tables are up to date, as those files aren't kept here.

use std::fmt::Write;
use std::ops::RangeInclusive;
use std::path::PathBuf;

/// What each `Grapheme_Cluster_Break` is numbered, in a property's low bits, where
/// nothing is `Other`, and the name of its constant.
const CLASSES: [(&str, &str, u8); 13] = [
    ("CR", "CR", 1),
    ("LF", "LF", 2),
    ("Control", "CONTROL", 3),
    ("Extend", "EXTEND", 4),
    ("ZWJ", "ZWJ", 5),
    ("Regional_Indicator", "REGIONAL_INDICATOR", 6),
    ("Prepend", "PREPEND", 7),
    ("SpacingMark", "SPACING_MARK", 8),
    ("L", "L", 9),
    ("V", "V", 10),
    ("T", "T", 11),
    ("LV", "LV", 12),
    ("LVT", "LVT", 13),
];
/// What stands for `LV` and `LVT` in the tables, which alternate over the precomposed
/// syllables, every 28th being `LV`.
const HANGUL: u8 = 14;
const FIRST_HANGUL: u32 = 0xAC00;
const PICTOGRAPHIC: u8 = 1 << 4;
const INDIC: [(&str, &str, u8); 3] = [
    ("Linker", "INDIC_LINKER", 1 << 5),
    ("Consonant", "INDIC_CONSONANT", 2 << 5),
    ("Extend", "INDIC_EXTEND", 3 << 5),
];

#[test]
fn the_tables_are_generated() {
    let Some(dir) = std::env::var_os("UPDATE_UNICODE") else {
        return;
    };
    let dir = PathBuf::from(dir);
    let read = |name: &str| {
        std::fs::read_to_string(dir.join(name)).unwrap_or_else(|e| panic!("{name}: {e}"))
    };
    let breaks = read("GraphemeBreakProperty.txt");
    let version = breaks
        .lines()
        .next()
        .and_then(|l| l.strip_prefix("# GraphemeBreakProperty-"))
        .and_then(|l| l.strip_suffix(".txt"))
        .expect("the file says its version");
    let mut all = vec![0u8; 0x11_0000];
    let at = |c: u32| usize::try_from(c).expect("a code point");
    for (range, value, _) in entries(&breaks) {
        let class = number(&CLASSES, value);
        for c in range {
            let hangul = value == "LV" || value == "LVT";
            if hangul {
                assert_eq!(value == "LV", (c - FIRST_HANGUL).is_multiple_of(28));
            }
            all[at(c)] = if hangul { HANGUL } else { class };
        }
    }
    for (range, value, _) in entries(&read("emoji-data.txt")) {
        if value == "Extended_Pictographic" {
            for c in range {
                all[at(c)] |= PICTOGRAPHIC;
            }
        }
    }
    for (range, value, indic) in entries(&read("DerivedCoreProperties.txt")) {
        if value == "InCB" {
            let indic = number(&INDIC, indic);
            for c in range {
                all[at(c)] |= indic;
            }
        }
    }
    let mut runs: Vec<(usize, u8)> = Vec::new();
    for (c, &property) in all.iter().enumerate() {
        if runs.last().is_none_or(|&(_, last)| last != property) {
            runs.push((c, property));
        }
    }
    std::fs::write(
        concat!(env!("CARGO_MANIFEST_DIR"), "/src/character_tables.rs"),
        written(version, &runs),
    )
    .expect("the tables are written");
}

/// What the property value `value` is numbered among `values`.
fn number(values: &[(&str, &str, u8)], value: &str) -> u8 {
    values
        .iter()
        .find(|(name, ..)| *name == value)
        .unwrap_or_else(|| panic!("no number for {value}"))
        .2
}

/// The file of the tables of `runs`, each of code points of the same properties.
fn written(version: &str, runs: &[(usize, u8)]) -> String {
    let mut file = format!(
        "//! What the rules of what a character is read of each code point, generated from\n\
         //! the data files of Unicode {version}, (c) Unicode, Inc., whose terms are at\n\
         //! <https://www.unicode.org/terms_of_use.html>, by `tests/character_tables.rs`.\n\n\
         /// The version of Unicode these are of.\n\
         pub(crate) const TABLES_VERSION: &str = \"{version}\";\n\n\
         /// A code point's `Grapheme_Cluster_Break`, in a property's low bits, of which any\n\
         /// other than these is `Other`.\n\
         pub(crate) const BREAK: u8 = 0b0000_1111;\n"
    );
    for (_, name, number) in CLASSES {
        writeln!(file, "pub(crate) const {name}: u8 = {number};").expect("to a string");
    }
    write!(
        file,
        "/// A precomposed Hangul syllable: `LV` every 28th from the first, and otherwise\n\
         /// `LVT`, which as runs of their own would be a third of [`PROPERTIES`].\n\
         pub(crate) const HANGUL: u8 = {HANGUL};\n\
         pub(crate) const FIRST_HANGUL: u32 = 0x{FIRST_HANGUL:X};\n\n\
         /// Whether a code point is `Extended_Pictographic`.\n\
         pub(crate) const PICTOGRAPHIC: u8 = {PICTOGRAPHIC};\n\n\
         /// A code point's `Indic_Conjunct_Break`, in these two bits, none if they are zero.\n\
         pub(crate) const INDIC: u8 = 0b0110_0000;\n"
    )
    .expect("to a string");
    for (_, name, number) in INDIC {
        writeln!(file, "pub(crate) const {name}: u8 = {number};").expect("to a string");
    }
    write!(
        file,
        "\n/// Each run of code points of the same properties, in order: its first, in the\n\
         /// high bytes, and in the lowest its properties.\n\
         pub(crate) const PROPERTIES: [u32; {}] = [\n",
        runs.len()
    )
    .expect("to a string");
    for (first, property) in runs {
        let entry = (first << 8) | usize::from(*property);
        let (high, low) = (entry >> 16, entry & 0xFFFF);
        writeln!(file, "    0x{high:04X}_{low:04X},").expect("to a string");
    }
    file.push_str("];\n");
    file
}

/// The range, first field and any second of each line of a Unicode data file.
fn entries(file: &str) -> impl Iterator<Item = (RangeInclusive<u32>, &str, &str)> {
    file.lines().filter_map(|line| {
        let line = line.split('#').next()?.trim();
        if line.is_empty() {
            return None;
        }
        let mut fields = line.split(';').map(str::trim);
        let points = fields.next()?;
        let value = fields.next()?;
        let rest = fields.next().unwrap_or("");
        let hex = |p: &str| u32::from_str_radix(p, 16).expect("a code point");
        let range = match points.split_once("..") {
            Some((first, last)) => hex(first)..=hex(last),
            None => hex(points)..=hex(points),
        };
        Some((range, value, rest))
    })
}
