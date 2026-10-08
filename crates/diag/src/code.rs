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
    AssignmentWithEq,
    UnterminatedText,
    InvalidEscape,
    InvalidInteger,
    InvalidRational,
    MiscasedKeyword,
    AttributeSeparator,
    UnterminatedComment,
    MissingCall,
    ForeignOperator,
    DuplicateName,
    MissingMain,
    UnknownType,
    TypeMismatch,
    IntegerOutOfRange,
    UntypedVariable,
    RationalOutOfRange,
    DuplicateAttribute,
    MissingAttribute,
    UnknownAttribute,
    UnknownRelationHeading,
    DuplicateTuple,
    BuiltinOutsidePrelude,
    AnyOutsideBuiltin,
    UnknownName,
    OperatorAsValue,
    NotAnOperator,
    PositionalArgument,
    MainWithParams,
    NameInSelector,
    CyclicInitialValues,
    Overflow,
    DivisionByZero,
    WrongOperandType,
    ChainedComparison,
    StrayCommentEnd,
}

impl Code {
    pub const ALL: [Code; 40] = [
        Code::UnexpectedCharacter,
        Code::MissingToken,
        Code::WrongDelimiter,
        Code::ExpectedDefinition,
        Code::AssignmentWithEq,
        Code::UnterminatedText,
        Code::InvalidEscape,
        Code::InvalidInteger,
        Code::InvalidRational,
        Code::MiscasedKeyword,
        Code::AttributeSeparator,
        Code::UnterminatedComment,
        Code::MissingCall,
        Code::ForeignOperator,
        Code::DuplicateName,
        Code::MissingMain,
        Code::UnknownType,
        Code::TypeMismatch,
        Code::IntegerOutOfRange,
        Code::UntypedVariable,
        Code::RationalOutOfRange,
        Code::DuplicateAttribute,
        Code::MissingAttribute,
        Code::UnknownAttribute,
        Code::UnknownRelationHeading,
        Code::DuplicateTuple,
        Code::BuiltinOutsidePrelude,
        Code::AnyOutsideBuiltin,
        Code::UnknownName,
        Code::OperatorAsValue,
        Code::NotAnOperator,
        Code::PositionalArgument,
        Code::MainWithParams,
        Code::NameInSelector,
        Code::CyclicInitialValues,
        Code::Overflow,
        Code::DivisionByZero,
        Code::WrongOperandType,
        Code::ChainedComparison,
        Code::StrayCommentEnd,
    ];

    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Code::UnexpectedCharacter => "E0001",
            Code::MissingToken => "E0002",
            Code::WrongDelimiter => "E0003",
            Code::ExpectedDefinition => "E0004",
            Code::AssignmentWithEq => "E0005",
            Code::UnterminatedText => "E0006",
            Code::InvalidEscape => "E0007",
            Code::InvalidInteger => "E0008",
            Code::InvalidRational => "E0009",
            Code::MiscasedKeyword => "E0010",
            Code::AttributeSeparator => "E0011",
            Code::UnterminatedComment => "E0012",
            Code::MissingCall => "E0013",
            Code::ForeignOperator => "E0014",
            Code::DuplicateName => "E0101",
            Code::MissingMain => "E0102",
            Code::UnknownType => "E0103",
            Code::TypeMismatch => "E0104",
            Code::IntegerOutOfRange => "E0105",
            Code::UntypedVariable => "E0106",
            Code::RationalOutOfRange => "E0107",
            Code::DuplicateAttribute => "E0108",
            Code::MissingAttribute => "E0109",
            Code::UnknownAttribute => "E0110",
            Code::UnknownRelationHeading => "E0111",
            Code::DuplicateTuple => "E0112",
            Code::BuiltinOutsidePrelude => "E0113",
            Code::AnyOutsideBuiltin => "E0114",
            Code::UnknownName => "E0115",
            Code::OperatorAsValue => "E0116",
            Code::NotAnOperator => "E0117",
            Code::PositionalArgument => "E0118",
            Code::MainWithParams => "E0119",
            Code::NameInSelector => "E0120",
            Code::CyclicInitialValues => "E0121",
            Code::Overflow => "E0122",
            Code::DivisionByZero => "E0123",
            Code::WrongOperandType => "E0124",
            Code::ChainedComparison => "E0015",
            Code::StrayCommentEnd => "E0016",
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
            Code::AssignmentWithEq => include_str!("../errors/E0005.md"),
            Code::UnterminatedText => include_str!("../errors/E0006.md"),
            Code::InvalidEscape => include_str!("../errors/E0007.md"),
            Code::InvalidInteger => include_str!("../errors/E0008.md"),
            Code::InvalidRational => include_str!("../errors/E0009.md"),
            Code::MiscasedKeyword => include_str!("../errors/E0010.md"),
            Code::AttributeSeparator => include_str!("../errors/E0011.md"),
            Code::UnterminatedComment => include_str!("../errors/E0012.md"),
            Code::MissingCall => include_str!("../errors/E0013.md"),
            Code::ForeignOperator => include_str!("../errors/E0014.md"),
            Code::DuplicateName => include_str!("../errors/E0101.md"),
            Code::MissingMain => include_str!("../errors/E0102.md"),
            Code::UnknownType => include_str!("../errors/E0103.md"),
            Code::TypeMismatch => include_str!("../errors/E0104.md"),
            Code::IntegerOutOfRange => include_str!("../errors/E0105.md"),
            Code::UntypedVariable => include_str!("../errors/E0106.md"),
            Code::RationalOutOfRange => include_str!("../errors/E0107.md"),
            Code::DuplicateAttribute => include_str!("../errors/E0108.md"),
            Code::MissingAttribute => include_str!("../errors/E0109.md"),
            Code::UnknownAttribute => include_str!("../errors/E0110.md"),
            Code::UnknownRelationHeading => include_str!("../errors/E0111.md"),
            Code::DuplicateTuple => include_str!("../errors/E0112.md"),
            Code::BuiltinOutsidePrelude => include_str!("../errors/E0113.md"),
            Code::AnyOutsideBuiltin => include_str!("../errors/E0114.md"),
            Code::UnknownName => include_str!("../errors/E0115.md"),
            Code::OperatorAsValue => include_str!("../errors/E0116.md"),
            Code::NotAnOperator => include_str!("../errors/E0117.md"),
            Code::PositionalArgument => include_str!("../errors/E0118.md"),
            Code::MainWithParams => include_str!("../errors/E0119.md"),
            Code::NameInSelector => include_str!("../errors/E0120.md"),
            Code::CyclicInitialValues => include_str!("../errors/E0121.md"),
            Code::Overflow => include_str!("../errors/E0122.md"),
            Code::DivisionByZero => include_str!("../errors/E0123.md"),
            Code::WrongOperandType => include_str!("../errors/E0124.md"),
            Code::ChainedComparison => include_str!("../errors/E0015.md"),
            Code::StrayCommentEnd => include_str!("../errors/E0016.md"),
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
