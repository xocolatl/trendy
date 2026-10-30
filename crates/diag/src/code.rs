use std::fmt;

/// Defines [`Code`] from one list of its variants, each with its id and,
/// in `errors/<id>.md`, its explanation.
macro_rules! codes {
    ($(#[$attr:meta])* $($code:ident => $id:literal,)*) => {
        $(#[$attr])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum Code {
            $($code,)*
        }

        impl Code {
            pub const ALL: [Code; [$($id),*].len()] = [$(Code::$code),*];

            #[must_use]
            pub fn id(self) -> &'static str {
                match self {
                    $(Code::$code => $id,)*
                }
            }

            /// The Markdown explanation printed by `trendyc --explain`.
            #[must_use]
            pub fn explanation(self) -> &'static str {
                match self {
                    $(Code::$code => include_str!(concat!("../errors/", $id, ".md")),)*
                }
            }
        }
    };
}

codes! {
    /// A stable identifier for each kind of diagnostic.
    ///
    /// Codes are permanent once the language is released: a code then keeps
    /// its meaning and is never reused. Until then, a code no longer
    /// reported is freed for the next new one. Its letter says what kind of
    /// problem it is, not which layer finds it: `P` the source doesn't
    /// parse, `A` it parses but analysis rejects it, `R` it fails while
    /// running, and `X` the compiler itself is wrong. Each letter's `0000`
    /// is reserved for a generic error of that kind. A rule that is broken
    /// at run time keeps its `R` code when the compiler catches it early.
    /// Each code's explanation lives in `crates/diag/errors/<code>.md`.
    UnexpectedCharacter => "P0001",
    MissingToken => "P0002",
    WrongDelimiter => "P0003",
    ExpectedDefinition => "P0004",
    AssignmentWithEq => "P0005",
    UnterminatedText => "P0006",
    InvalidEscape => "P0007",
    InvalidInteger => "P0008",
    InvalidApproximate => "P0009",
    MiscasedKeyword => "P0010",
    AttributeSeparator => "P0011",
    UnterminatedComment => "P0012",
    MissingCall => "P0013",
    ForeignOperator => "P0014",
    ChainedComparison => "P0015",
    StrayCommentEnd => "P0016",
    KeywordAsName => "P0017",
    RenameSeparator => "P0018",
    KeySpelling => "P0019",
    KeyBeforeValue => "P0020",
    SqlRelational => "P0021",
    UnorderedOperators => "P0022",
    DeleteWithoutWhere => "P0023",
    UpdateOrder => "P0024",
    CommaAssignment => "P0025",
    ResultSpelling => "P0026",
    TypeParameterSpelling => "P0027",
    ConditionalSpelling => "P0028",
    BranchBrackets => "P0029",
    LoadOrder => "P0030",
    BodyValue => "P0031",
    LoopSpelling => "P0032",
    HeadingAsType => "P0033",
    NestedDefinition => "P0034",
    ExpressionStatement => "P0035",
    CompoundAssignment => "P0036",
    NamedOperand => "P0037",
    ModeSpelling => "P0038",
    BareRepresentation => "P0039",
    ConstraintSpelling => "P0040",
    ConstraintInRepresentation => "P0041",
    DuplicateName => "A0001",
    MissingMain => "A0002",
    UnknownType => "A0003",
    TypeMismatch => "A0004",
    IntegerOutOfRange => "A0005",
    UntypedVariable => "A0006",
    ApproximateOutOfRange => "A0007",
    DuplicateAttribute => "A0008",
    MissingAttribute => "A0009",
    UnknownAttribute => "A0010",
    UnknownEmptyType => "A0011",
    DuplicateTuple => "A0012",
    BuiltinOutsidePrelude => "A0013",
    AnyOutsideParameter => "A0014",
    UnknownName => "A0015",
    OperatorAsValue => "A0016",
    NotAnOperator => "A0017",
    PositionalArgument => "A0018",
    MainSignature => "A0019",
    CyclicInitialValues => "A0021",
    WrongOperandType => "A0022",
    EmptyDoesNothing => "A0023",
    AttributeExists => "A0024",
    KeyNotRelation => "A0020",
    RedundantKey => "A0025",
    JoinTypeConflict => "A0027",
    CommonAttributes => "A0028",
    EmptyList => "A0029",
    ImageOutsideTuple => "A0030",
    HeadingMismatch => "A0031",
    NotAssignable => "A0026",
    EveryTuple => "A0032",
    NoChange => "A0033",
    ReadOnlyEffect => "A0034",
    Return => "A0035",
    Recursion => "A0036",
    CallOfReadOnly => "A0037",
    UndeterminedTypeParameter => "A0038",
    TypeParameterNamedOnce => "A0039",
    TypeParameterNotAny => "A0040",
    GrowingRecursion => "A0041",
    PerRelation => "A0042",
    PerAsValue => "A0043",
    ConstantPer => "A0044",
    LocalBeforeDeclaration => "A0045",
    PointlessCondition => "A0046",
    LoadMismatch => "A0047",
    AttributeAssigned => "A0048",
    CyclicTypes => "A0049",
    VariableInReadOnly => "A0050",
    MisplacedMode => "A0051",
    ArgumentMode => "A0052",
    Aliased => "A0053",
    UnassignedOut => "A0054",
    UnreadableAttribute => "A0055",
    UnchosenOperator => "A0056",
    SelectorName => "A0057",
    ConstantConstraint => "A0058",
    Overflow => "R0001",
    DivisionByZero => "R0002",
    OutOfMemory => "R0003",
    TupleMismatch => "R0004",
    NotOneTuple => "R0005",
    SharedTuple => "R0006",
    MissingTuple => "R0007",
    KeyViolated => "R0008",
    ConflictingChanges => "R0009",
    NoMinOrMax => "R0010",
    IndexOutOfRange => "R0011",
    UnreadableFile => "R0012",
    NotUtf8 => "R0013",
    Unparsable => "R0014",
    MalformedCsv => "R0015",
    CsvColumns => "R0016",
    BrokenConstraint => "R0017",
    Internal => "X0000",
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
    /// broken; running out of memory, reading a file, which is never read
    /// when compiling, and internal errors never are.
    #[must_use]
    pub fn reported_when_compiling(self) -> bool {
        !matches!(
            self,
            Code::OutOfMemory
                | Code::UnreadableFile
                | Code::NotUtf8
                | Code::MalformedCsv
                | Code::Internal
        )
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
