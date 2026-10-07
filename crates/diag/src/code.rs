use std::fmt;

/// A stable identifier for each kind of diagnostic.
///
/// Codes are permanent: once released, a code keeps its meaning and is never
/// reused. `E00xx` are syntax errors and `E01xx` are semantic errors. Each
/// code's explanation lives in `crates/diag/errors/<code>.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Code {
    UnexpectedCharacter,
    MissingToken,
    WrongDelimiter,
    ExpectedDefinition,
    DuplicateOperator,
    MissingMain,
}

impl Code {
    pub const ALL: [Code; 6] = [
        Code::UnexpectedCharacter,
        Code::MissingToken,
        Code::WrongDelimiter,
        Code::ExpectedDefinition,
        Code::DuplicateOperator,
        Code::MissingMain,
    ];

    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Code::UnexpectedCharacter => "E0001",
            Code::MissingToken => "E0002",
            Code::WrongDelimiter => "E0003",
            Code::ExpectedDefinition => "E0004",
            Code::DuplicateOperator => "E0101",
            Code::MissingMain => "E0102",
        }
    }

    /// The code with this id, such as `"E0002"`. Case-insensitive.
    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        Code::ALL
            .into_iter()
            .find(|c| c.id().eq_ignore_ascii_case(id))
    }

    /// The Markdown explanation printed by `trendyc --explain`.
    #[must_use]
    pub fn explanation(self) -> &'static str {
        match self {
            Code::UnexpectedCharacter => include_str!("../errors/E0001.md"),
            Code::MissingToken => include_str!("../errors/E0002.md"),
            Code::WrongDelimiter => include_str!("../errors/E0003.md"),
            Code::ExpectedDefinition => include_str!("../errors/E0004.md"),
            Code::DuplicateOperator => include_str!("../errors/E0101.md"),
            Code::MissingMain => include_str!("../errors/E0102.md"),
        }
    }
}

impl fmt::Display for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.id())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_round_trip() {
        let mut ids: Vec<_> = Code::ALL.iter().map(|c| c.id()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), Code::ALL.len());
        for code in Code::ALL {
            assert_eq!(Code::from_id(code.id()), Some(code));
            assert_eq!(Code::from_id(&code.id().to_lowercase()), Some(code));
        }
    }

    #[test]
    fn explanations_start_with_their_id() {
        for code in Code::ALL {
            let title = code.explanation().lines().next().unwrap();
            assert!(title.starts_with(&format!("# {}: ", code.id())), "{title}");
        }
    }

    #[test]
    fn every_explanation_file_has_a_code() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/errors");
        for entry in std::fs::read_dir(dir).unwrap() {
            let name = entry.unwrap().file_name().into_string().unwrap();
            let id = name.strip_suffix(".md").unwrap();
            assert!(Code::from_id(id).is_some(), "{name} has no Code");
        }
    }
}
