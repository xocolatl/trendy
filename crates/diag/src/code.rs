use std::fmt;

/// A stable identifier for each kind of diagnostic.
///
/// Codes are permanent once the language is released: a code then keeps its
/// meaning and is never reused. Until then, a code no longer reported is
/// freed for the next new one. Its letter says what kind of problem it is,
/// not which layer finds it: `P` the source doesn't parse, `A` it parses
/// but analysis rejects it, `R` it fails while running, and `X` the
/// compiler itself is wrong. Each
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
    InvalidApproximate,
    MiscasedKeyword,
    AttributeSeparator,
    UnterminatedComment,
    MissingCall,
    ForeignOperator,
    ChainedComparison,
    StrayCommentEnd,
    KeywordAsName,
    RenameSeparator,
    KeySpelling,
    KeyBeforeValue,
    SqlRelational,
    UnorderedOperators,
    DeleteWithoutWhere,
    UpdateOrder,
    CommaAssignment,
    ResultSpelling,
    TypeParameterSpelling,
    ConditionalSpelling,
    BranchBrackets,
    LoadOrder,
    BodyValue,
    LoopSpelling,
    HeadingAsType,
    NestedDefinition,
    ExpressionStatement,
    DuplicateName,
    MissingMain,
    UnknownType,
    TypeMismatch,
    IntegerOutOfRange,
    UntypedVariable,
    ApproximateOutOfRange,
    DuplicateAttribute,
    MissingAttribute,
    UnknownAttribute,
    UnknownEmptyType,
    DuplicateTuple,
    BuiltinOutsidePrelude,
    AnyOutsideParameter,
    UnknownName,
    OperatorAsValue,
    NotAnOperator,
    PositionalArgument,
    MainSignature,
    CyclicInitialValues,
    WrongOperandType,
    EmptyDoesNothing,
    AttributeExists,
    KeyNotRelation,
    RedundantKey,
    JoinTypeConflict,
    CommonAttributes,
    EmptyList,
    ImageOutsideTuple,
    HeadingMismatch,
    NotAssignable,
    EveryTuple,
    NoChange,
    ReadOnlyEffect,
    Return,
    Recursion,
    CallOfReadOnly,
    UndeterminedTypeParameter,
    TypeParameterNamedOnce,
    TypeParameterNotAny,
    GrowingRecursion,
    PerRelation,
    PerAsValue,
    ConstantPer,
    LocalBeforeDeclaration,
    PointlessCondition,
    LoadMismatch,
    AttributeAssigned,
    CyclicHeadings,
    Overflow,
    DivisionByZero,
    OutOfMemory,
    TupleMismatch,
    NotOneTuple,
    SharedTuple,
    MissingTuple,
    KeyViolated,
    ConflictingChanges,
    NoMinOrMax,
    IndexOutOfRange,
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
    pub const ALL: [Code; 96] = [
        Code::UnexpectedCharacter,
        Code::MissingToken,
        Code::WrongDelimiter,
        Code::ExpectedDefinition,
        Code::AssignmentWithEq,
        Code::UnterminatedText,
        Code::InvalidEscape,
        Code::InvalidInteger,
        Code::InvalidApproximate,
        Code::MiscasedKeyword,
        Code::AttributeSeparator,
        Code::UnterminatedComment,
        Code::MissingCall,
        Code::ForeignOperator,
        Code::ChainedComparison,
        Code::StrayCommentEnd,
        Code::KeywordAsName,
        Code::RenameSeparator,
        Code::KeySpelling,
        Code::KeyBeforeValue,
        Code::SqlRelational,
        Code::UnorderedOperators,
        Code::DeleteWithoutWhere,
        Code::UpdateOrder,
        Code::CommaAssignment,
        Code::ResultSpelling,
        Code::TypeParameterSpelling,
        Code::ConditionalSpelling,
        Code::BranchBrackets,
        Code::LoadOrder,
        Code::BodyValue,
        Code::LoopSpelling,
        Code::HeadingAsType,
        Code::NestedDefinition,
        Code::ExpressionStatement,
        Code::DuplicateName,
        Code::MissingMain,
        Code::UnknownType,
        Code::TypeMismatch,
        Code::IntegerOutOfRange,
        Code::UntypedVariable,
        Code::ApproximateOutOfRange,
        Code::DuplicateAttribute,
        Code::MissingAttribute,
        Code::UnknownAttribute,
        Code::UnknownEmptyType,
        Code::DuplicateTuple,
        Code::BuiltinOutsidePrelude,
        Code::AnyOutsideParameter,
        Code::UnknownName,
        Code::OperatorAsValue,
        Code::NotAnOperator,
        Code::PositionalArgument,
        Code::MainSignature,
        Code::CyclicInitialValues,
        Code::WrongOperandType,
        Code::EmptyDoesNothing,
        Code::AttributeExists,
        Code::KeyNotRelation,
        Code::RedundantKey,
        Code::JoinTypeConflict,
        Code::CommonAttributes,
        Code::EmptyList,
        Code::ImageOutsideTuple,
        Code::HeadingMismatch,
        Code::NotAssignable,
        Code::EveryTuple,
        Code::NoChange,
        Code::ReadOnlyEffect,
        Code::Return,
        Code::Recursion,
        Code::CallOfReadOnly,
        Code::UndeterminedTypeParameter,
        Code::TypeParameterNamedOnce,
        Code::TypeParameterNotAny,
        Code::GrowingRecursion,
        Code::PerRelation,
        Code::PerAsValue,
        Code::ConstantPer,
        Code::LocalBeforeDeclaration,
        Code::PointlessCondition,
        Code::LoadMismatch,
        Code::AttributeAssigned,
        Code::CyclicHeadings,
        Code::Overflow,
        Code::DivisionByZero,
        Code::OutOfMemory,
        Code::TupleMismatch,
        Code::NotOneTuple,
        Code::SharedTuple,
        Code::MissingTuple,
        Code::KeyViolated,
        Code::ConflictingChanges,
        Code::NoMinOrMax,
        Code::IndexOutOfRange,
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
            Code::InvalidApproximate => "P0009",
            Code::MiscasedKeyword => "P0010",
            Code::AttributeSeparator => "P0011",
            Code::UnterminatedComment => "P0012",
            Code::MissingCall => "P0013",
            Code::ForeignOperator => "P0014",
            Code::ChainedComparison => "P0015",
            Code::StrayCommentEnd => "P0016",
            Code::KeywordAsName => "P0017",
            Code::RenameSeparator => "P0018",
            Code::KeySpelling => "P0019",
            Code::KeyBeforeValue => "P0020",
            Code::SqlRelational => "P0021",
            Code::UnorderedOperators => "P0022",
            Code::DeleteWithoutWhere => "P0023",
            Code::UpdateOrder => "P0024",
            Code::CommaAssignment => "P0025",
            Code::ResultSpelling => "P0026",
            Code::TypeParameterSpelling => "P0027",
            Code::ConditionalSpelling => "P0028",
            Code::BranchBrackets => "P0029",
            Code::LoadOrder => "P0030",
            Code::BodyValue => "P0031",
            Code::LoopSpelling => "P0032",
            Code::HeadingAsType => "P0033",
            Code::NestedDefinition => "P0034",
            Code::ExpressionStatement => "P0035",
            Code::DuplicateName => "A0001",
            Code::MissingMain => "A0002",
            Code::UnknownType => "A0003",
            Code::TypeMismatch => "A0004",
            Code::IntegerOutOfRange => "A0005",
            Code::UntypedVariable => "A0006",
            Code::ApproximateOutOfRange => "A0007",
            Code::DuplicateAttribute => "A0008",
            Code::MissingAttribute => "A0009",
            Code::UnknownAttribute => "A0010",
            Code::UnknownEmptyType => "A0011",
            Code::DuplicateTuple => "A0012",
            Code::BuiltinOutsidePrelude => "A0013",
            Code::AnyOutsideParameter => "A0014",
            Code::UnknownName => "A0015",
            Code::OperatorAsValue => "A0016",
            Code::NotAnOperator => "A0017",
            Code::PositionalArgument => "A0018",
            Code::MainSignature => "A0019",
            Code::CyclicInitialValues => "A0021",
            Code::WrongOperandType => "A0022",
            Code::EmptyDoesNothing => "A0023",
            Code::AttributeExists => "A0024",
            Code::KeyNotRelation => "A0020",
            Code::RedundantKey => "A0025",
            Code::JoinTypeConflict => "A0027",
            Code::CommonAttributes => "A0028",
            Code::EmptyList => "A0029",
            Code::ImageOutsideTuple => "A0030",
            Code::HeadingMismatch => "A0031",
            Code::NotAssignable => "A0026",
            Code::EveryTuple => "A0032",
            Code::NoChange => "A0033",
            Code::ReadOnlyEffect => "A0034",
            Code::Return => "A0035",
            Code::Recursion => "A0036",
            Code::CallOfReadOnly => "A0037",
            Code::UndeterminedTypeParameter => "A0038",
            Code::TypeParameterNamedOnce => "A0039",
            Code::TypeParameterNotAny => "A0040",
            Code::GrowingRecursion => "A0041",
            Code::PerRelation => "A0042",
            Code::PerAsValue => "A0043",
            Code::ConstantPer => "A0044",
            Code::LocalBeforeDeclaration => "A0045",
            Code::PointlessCondition => "A0046",
            Code::LoadMismatch => "A0047",
            Code::AttributeAssigned => "A0048",
            Code::CyclicHeadings => "A0049",
            Code::Overflow => "R0001",
            Code::DivisionByZero => "R0002",
            Code::OutOfMemory => "R0003",
            Code::TupleMismatch => "R0004",
            Code::NotOneTuple => "R0005",
            Code::SharedTuple => "R0006",
            Code::MissingTuple => "R0007",
            Code::KeyViolated => "R0008",
            Code::ConflictingChanges => "R0009",
            Code::NoMinOrMax => "R0010",
            Code::IndexOutOfRange => "R0011",
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
            Code::InvalidApproximate => include_str!("../errors/P0009.md"),
            Code::MiscasedKeyword => include_str!("../errors/P0010.md"),
            Code::AttributeSeparator => include_str!("../errors/P0011.md"),
            Code::UnterminatedComment => include_str!("../errors/P0012.md"),
            Code::MissingCall => include_str!("../errors/P0013.md"),
            Code::ForeignOperator => include_str!("../errors/P0014.md"),
            Code::ChainedComparison => include_str!("../errors/P0015.md"),
            Code::StrayCommentEnd => include_str!("../errors/P0016.md"),
            Code::KeywordAsName => include_str!("../errors/P0017.md"),
            Code::RenameSeparator => include_str!("../errors/P0018.md"),
            Code::KeySpelling => include_str!("../errors/P0019.md"),
            Code::KeyBeforeValue => include_str!("../errors/P0020.md"),
            Code::SqlRelational => include_str!("../errors/P0021.md"),
            Code::UnorderedOperators => include_str!("../errors/P0022.md"),
            Code::DeleteWithoutWhere => include_str!("../errors/P0023.md"),
            Code::UpdateOrder => include_str!("../errors/P0024.md"),
            Code::CommaAssignment => include_str!("../errors/P0025.md"),
            Code::ResultSpelling => include_str!("../errors/P0026.md"),
            Code::TypeParameterSpelling => include_str!("../errors/P0027.md"),
            Code::ConditionalSpelling => include_str!("../errors/P0028.md"),
            Code::BranchBrackets => include_str!("../errors/P0029.md"),
            Code::LoadOrder => include_str!("../errors/P0030.md"),
            Code::BodyValue => include_str!("../errors/P0031.md"),
            Code::LoopSpelling => include_str!("../errors/P0032.md"),
            Code::HeadingAsType => include_str!("../errors/P0033.md"),
            Code::NestedDefinition => include_str!("../errors/P0034.md"),
            Code::ExpressionStatement => include_str!("../errors/P0035.md"),
            Code::DuplicateName => include_str!("../errors/A0001.md"),
            Code::MissingMain => include_str!("../errors/A0002.md"),
            Code::UnknownType => include_str!("../errors/A0003.md"),
            Code::TypeMismatch => include_str!("../errors/A0004.md"),
            Code::IntegerOutOfRange => include_str!("../errors/A0005.md"),
            Code::UntypedVariable => include_str!("../errors/A0006.md"),
            Code::ApproximateOutOfRange => include_str!("../errors/A0007.md"),
            Code::DuplicateAttribute => include_str!("../errors/A0008.md"),
            Code::MissingAttribute => include_str!("../errors/A0009.md"),
            Code::UnknownAttribute => include_str!("../errors/A0010.md"),
            Code::UnknownEmptyType => include_str!("../errors/A0011.md"),
            Code::DuplicateTuple => include_str!("../errors/A0012.md"),
            Code::BuiltinOutsidePrelude => include_str!("../errors/A0013.md"),
            Code::AnyOutsideParameter => include_str!("../errors/A0014.md"),
            Code::UnknownName => include_str!("../errors/A0015.md"),
            Code::OperatorAsValue => include_str!("../errors/A0016.md"),
            Code::NotAnOperator => include_str!("../errors/A0017.md"),
            Code::PositionalArgument => include_str!("../errors/A0018.md"),
            Code::MainSignature => include_str!("../errors/A0019.md"),
            Code::CyclicInitialValues => include_str!("../errors/A0021.md"),
            Code::WrongOperandType => include_str!("../errors/A0022.md"),
            Code::EmptyDoesNothing => include_str!("../errors/A0023.md"),
            Code::AttributeExists => include_str!("../errors/A0024.md"),
            Code::KeyNotRelation => include_str!("../errors/A0020.md"),
            Code::RedundantKey => include_str!("../errors/A0025.md"),
            Code::JoinTypeConflict => include_str!("../errors/A0027.md"),
            Code::CommonAttributes => include_str!("../errors/A0028.md"),
            Code::EmptyList => include_str!("../errors/A0029.md"),
            Code::ImageOutsideTuple => include_str!("../errors/A0030.md"),
            Code::HeadingMismatch => include_str!("../errors/A0031.md"),
            Code::NotAssignable => include_str!("../errors/A0026.md"),
            Code::EveryTuple => include_str!("../errors/A0032.md"),
            Code::NoChange => include_str!("../errors/A0033.md"),
            Code::ReadOnlyEffect => include_str!("../errors/A0034.md"),
            Code::Return => include_str!("../errors/A0035.md"),
            Code::Recursion => include_str!("../errors/A0036.md"),
            Code::CallOfReadOnly => include_str!("../errors/A0037.md"),
            Code::UndeterminedTypeParameter => include_str!("../errors/A0038.md"),
            Code::TypeParameterNamedOnce => include_str!("../errors/A0039.md"),
            Code::TypeParameterNotAny => include_str!("../errors/A0040.md"),
            Code::GrowingRecursion => include_str!("../errors/A0041.md"),
            Code::PerRelation => include_str!("../errors/A0042.md"),
            Code::PerAsValue => include_str!("../errors/A0043.md"),
            Code::ConstantPer => include_str!("../errors/A0044.md"),
            Code::LocalBeforeDeclaration => include_str!("../errors/A0045.md"),
            Code::PointlessCondition => include_str!("../errors/A0046.md"),
            Code::LoadMismatch => include_str!("../errors/A0047.md"),
            Code::AttributeAssigned => include_str!("../errors/A0048.md"),
            Code::CyclicHeadings => include_str!("../errors/A0049.md"),
            Code::Overflow => include_str!("../errors/R0001.md"),
            Code::DivisionByZero => include_str!("../errors/R0002.md"),
            Code::OutOfMemory => include_str!("../errors/R0003.md"),
            Code::TupleMismatch => include_str!("../errors/R0004.md"),
            Code::NotOneTuple => include_str!("../errors/R0005.md"),
            Code::SharedTuple => include_str!("../errors/R0006.md"),
            Code::MissingTuple => include_str!("../errors/R0007.md"),
            Code::KeyViolated => include_str!("../errors/R0008.md"),
            Code::ConflictingChanges => include_str!("../errors/R0009.md"),
            Code::NoMinOrMax => include_str!("../errors/R0010.md"),
            Code::IndexOutOfRange => include_str!("../errors/R0011.md"),
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
