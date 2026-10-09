use std::fmt;

/// A stable identifier for each kind of diagnostic.
///
/// Codes are permanent: once released, a code keeps its meaning and is never
/// reused. Its letter says what kind of problem it is, not which layer finds
/// it: `P` the source doesn't parse, `A` it parses but analysis rejects it,
/// `R` it fails while running, and `X` the compiler itself is wrong. Each
/// letter's `0000` is reserved for a generic error of that kind. A rule that
/// is broken at run time keeps its `R` code when the compiler catches it
/// early. Each code's explanation lives in `crates/diag/errors/<code>.md`.
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
    ChainedComparison,
    StrayCommentEnd,
    KeywordAsName,
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
    WrongOperandType,
    EmptyRemove,
    Overflow,
    DivisionByZero,
    OutOfMemory,
    Internal,
}

/// What kind of problem a code is, from its letter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeKind {
    /// `P`: the source doesn't parse.
    Parse,
    /// `A`: it parses, but isn't a valid program.
    Analysis,
    /// `R`: it fails while running.
    Run,
    /// `X`: a bug in the compiler, never the program's fault.
    Internal,
}

impl Code {
    pub const ALL: [Code; 44] = [
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
        Code::ChainedComparison,
        Code::StrayCommentEnd,
        Code::KeywordAsName,
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
        Code::WrongOperandType,
        Code::EmptyRemove,
        Code::Overflow,
        Code::DivisionByZero,
        Code::OutOfMemory,
        Code::Internal,
    ];

    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Code::UnexpectedCharacter => "P0001",
            Code::MissingToken => "P0002",
            Code::WrongDelimiter => "P0003",
            Code::ExpectedDefinition => "P0004",
            Code::AssignmentWithEq => "P0005",
            Code::UnterminatedText => "P0006",
            Code::InvalidEscape => "P0007",
            Code::InvalidInteger => "P0008",
            Code::InvalidRational => "P0009",
            Code::MiscasedKeyword => "P0010",
            Code::AttributeSeparator => "P0011",
            Code::UnterminatedComment => "P0012",
            Code::MissingCall => "P0013",
            Code::ForeignOperator => "P0014",
            Code::ChainedComparison => "P0015",
            Code::StrayCommentEnd => "P0016",
            Code::KeywordAsName => "P0017",
            Code::DuplicateName => "A0001",
            Code::MissingMain => "A0002",
            Code::UnknownType => "A0003",
            Code::TypeMismatch => "A0004",
            Code::IntegerOutOfRange => "A0005",
            Code::UntypedVariable => "A0006",
            Code::RationalOutOfRange => "A0007",
            Code::DuplicateAttribute => "A0008",
            Code::MissingAttribute => "A0009",
            Code::UnknownAttribute => "A0010",
            Code::UnknownRelationHeading => "A0011",
            Code::DuplicateTuple => "A0012",
            Code::BuiltinOutsidePrelude => "A0013",
            Code::AnyOutsideBuiltin => "A0014",
            Code::UnknownName => "A0015",
            Code::OperatorAsValue => "A0016",
            Code::NotAnOperator => "A0017",
            Code::PositionalArgument => "A0018",
            Code::MainWithParams => "A0019",
            Code::NameInSelector => "A0020",
            Code::CyclicInitialValues => "A0021",
            Code::WrongOperandType => "A0022",
            Code::EmptyRemove => "A0023",
            Code::Overflow => "R0001",
            Code::DivisionByZero => "R0002",
            Code::OutOfMemory => "R0003",
            Code::Internal => "X0000",
        }
    }

    /// The code with this id, such as `"P0002"`. Case-insensitive.
    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        Code::ALL
            .into_iter()
            .find(|c| c.id().eq_ignore_ascii_case(id))
    }

    #[must_use]
    pub fn kind(self) -> CodeKind {
        match self.id().as_bytes()[0] {
            b'P' => CodeKind::Parse,
            b'A' => CodeKind::Analysis,
            b'R' => CodeKind::Run,
            _ => CodeKind::Internal,
        }
    }

    /// Whether the compiler can report this code, so that its explanation
    /// can show a program that produces it. Run-time rules such as overflow
    /// are reported while compiling when the compiler can tell they'll be
    /// broken; running out of memory and internal errors never are.
    #[must_use]
    pub fn reported_when_compiling(self) -> bool {
        !matches!(self, Code::OutOfMemory | Code::Internal)
    }

    /// The Markdown explanation printed by `trendyc --explain`.
    #[must_use]
    pub fn explanation(self) -> &'static str {
        match self {
            Code::UnexpectedCharacter => include_str!("../errors/P0001.md"),
            Code::MissingToken => include_str!("../errors/P0002.md"),
            Code::WrongDelimiter => include_str!("../errors/P0003.md"),
            Code::ExpectedDefinition => include_str!("../errors/P0004.md"),
            Code::AssignmentWithEq => include_str!("../errors/P0005.md"),
            Code::UnterminatedText => include_str!("../errors/P0006.md"),
            Code::InvalidEscape => include_str!("../errors/P0007.md"),
            Code::InvalidInteger => include_str!("../errors/P0008.md"),
            Code::InvalidRational => include_str!("../errors/P0009.md"),
            Code::MiscasedKeyword => include_str!("../errors/P0010.md"),
            Code::AttributeSeparator => include_str!("../errors/P0011.md"),
            Code::UnterminatedComment => include_str!("../errors/P0012.md"),
            Code::MissingCall => include_str!("../errors/P0013.md"),
            Code::ForeignOperator => include_str!("../errors/P0014.md"),
            Code::ChainedComparison => include_str!("../errors/P0015.md"),
            Code::StrayCommentEnd => include_str!("../errors/P0016.md"),
            Code::KeywordAsName => include_str!("../errors/P0017.md"),
            Code::DuplicateName => include_str!("../errors/A0001.md"),
            Code::MissingMain => include_str!("../errors/A0002.md"),
            Code::UnknownType => include_str!("../errors/A0003.md"),
            Code::TypeMismatch => include_str!("../errors/A0004.md"),
            Code::IntegerOutOfRange => include_str!("../errors/A0005.md"),
            Code::UntypedVariable => include_str!("../errors/A0006.md"),
            Code::RationalOutOfRange => include_str!("../errors/A0007.md"),
            Code::DuplicateAttribute => include_str!("../errors/A0008.md"),
            Code::MissingAttribute => include_str!("../errors/A0009.md"),
            Code::UnknownAttribute => include_str!("../errors/A0010.md"),
            Code::UnknownRelationHeading => include_str!("../errors/A0011.md"),
            Code::DuplicateTuple => include_str!("../errors/A0012.md"),
            Code::BuiltinOutsidePrelude => include_str!("../errors/A0013.md"),
            Code::AnyOutsideBuiltin => include_str!("../errors/A0014.md"),
            Code::UnknownName => include_str!("../errors/A0015.md"),
            Code::OperatorAsValue => include_str!("../errors/A0016.md"),
            Code::NotAnOperator => include_str!("../errors/A0017.md"),
            Code::PositionalArgument => include_str!("../errors/A0018.md"),
            Code::MainWithParams => include_str!("../errors/A0019.md"),
            Code::NameInSelector => include_str!("../errors/A0020.md"),
            Code::CyclicInitialValues => include_str!("../errors/A0021.md"),
            Code::WrongOperandType => include_str!("../errors/A0022.md"),
            Code::EmptyRemove => include_str!("../errors/A0023.md"),
            Code::Overflow => include_str!("../errors/R0001.md"),
            Code::DivisionByZero => include_str!("../errors/R0002.md"),
            Code::OutOfMemory => include_str!("../errors/R0003.md"),
            Code::Internal => include_str!("../errors/X0000.md"),
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

    #[test]
    fn kinds_follow_the_letter() {
        assert_eq!(Code::MissingToken.kind(), CodeKind::Parse);
        assert_eq!(Code::TypeMismatch.kind(), CodeKind::Analysis);
        assert_eq!(Code::Overflow.kind(), CodeKind::Run);
        assert_eq!(Code::Internal.kind(), CodeKind::Internal);
        for code in Code::ALL {
            assert!(
                code.id().ends_with("0000") == (code == Code::Internal),
                "{code}: 0000 is reserved"
            );
        }
    }
}
