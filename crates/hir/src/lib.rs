//! High-level IR: the checked program handed from sema onward.
//!
//! Tree-shaped like the source, but every name is resolved and every
//! invariant sema promises holds. This is where relational algebra lives,
//! and where the optimizer rewrites it. Nothing here refers to syntax.
//!
//! Operators, variables and types each have a namespace of their own.

use std::collections::{BTreeMap, BTreeSet};

use trendy_diag::Code;

pub use trendy_text::Unparsable;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Program {
    /// Every operator: the types' constraints and conversions, then the
    /// built-ins, then those written in trendy and the types' selectors,
    /// the prelude's first.
    pub operators: Vec<Operator>,
    pub variables: Vec<Variable>,
    /// Each type's immediate subtypes, by its name: a union type's, what
    /// a value of it can be, of which the largest decides how much room it
    /// takes, and a regular type's, defined by constraints, which decide
    /// which a value of it is of (see [`Defined::specialization`]).
    pub subtypes: BTreeMap<String, Vec<Type>>,
    /// The operator the executable starts in: a user operator with no
    /// parameters.
    pub entry: OperatorId,
    /// The prelude's `message{error: error}: text`, which writes the
    /// message of an error, a value of the prelude's union type `error`,
    /// that the program fails with (see [`Expr::Fail`]).
    pub message: OperatorId,
}

impl Program {
    #[must_use]
    pub fn operator(&self, id: OperatorId) -> &Operator {
        &self.operators[id.0]
    }

    #[must_use]
    pub fn variable(&self, id: VariableId) -> &Variable {
        &self.variables[id.0]
    }

    /// The program's variables that `block` assigns, or that the update
    /// operators it calls do, at any depth, and their versions, each once:
    /// directly, or by passing them to a `var` or `out` parameter.
    #[must_use]
    pub fn variables_assigned(&self, block: &Block) -> Vec<VariableId> {
        let mut variables = Vec::new();
        let mut add = |targets: Vec<Target>| {
            for target in targets {
                if let Target::Variable(id) = target
                    && !variables.contains(&id)
                {
                    variables.push(id);
                }
            }
        };
        add(block.assigned());
        let (mut seen, mut next) = (Vec::new(), block.called());
        while let Some(id) = next.pop() {
            if seen.contains(&id) {
                continue;
            }
            seen.push(id);
            let op = self.operator(id);
            next.extend(op.versions.iter().map(|v| v.operator));
            if let OperatorKind::Update { body, .. } = &op.kind {
                add(body.assigned());
                next.extend(body.called());
            }
        }
        variables
    }

    /// The type of `error`, a subtype of `error` the prelude declares, as
    /// sema checks.
    ///
    /// # Panics
    ///
    /// If the prelude doesn't declare it, which sema rules out.
    #[must_use]
    pub fn language_error(&self, error: LanguageError) -> &Defined {
        self.subtypes["error"]
            .iter()
            .find_map(|ty| match ty {
                Type::Defined(defined) if defined.name == error.name() => Some(&**defined),
                _ => None,
            })
            .expect("the prelude declares the errors the language makes")
    }

    /// The type of `expr`, inside an operator with the parameters
    /// `params` and the local variables `locals`: one of the program's, or
    /// one instantiated from it.
    ///
    /// # Panics
    ///
    /// If `expr` names a parameter that isn't in `params`, or a local
    /// variable that isn't in `locals`, or calls an update operator, which
    /// sema never produces.
    #[must_use]
    pub fn type_of(
        &self,
        expr: &Expr,
        params: &BTreeMap<String, Parameter>,
        locals: &[Local],
    ) -> Type {
        expr.type_with(&|leaf| match leaf {
            Expr::Variable(id) => self.variable(*id).ty.clone(),
            Expr::Parameter(name) => params[name].ty.clone(),
            Expr::Local(id) => locals[id.0].ty.clone(),
            Expr::Call(call) => self.result(call),
            _ => unreachable!("only variables, parameters, locals and calls are leaves"),
        })
    }

    /// The type of the value of `call`, a call of a read-only operator.
    ///
    /// # Panics
    ///
    /// If the operator isn't read-only, which sema rules out.
    #[must_use]
    pub fn result(&self, call: &Call) -> Type {
        let result = match &self.operator(call.operator).kind {
            OperatorKind::ReadOnly { result, .. } => Some(result.clone()),
            OperatorKind::Specification { result } => result.clone(),
            OperatorKind::Builtin(builtin) => builtin.result(),
            OperatorKind::Update { .. } => None,
        };
        result
            .expect("only read-only operators are called in expressions")
            .instantiated(&call.types)
    }
}

/// Index into [`Program::operators`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OperatorId(pub usize);

/// Index into [`Program::variables`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VariableId(pub usize);

/// Index into an update operator's local variables
/// ([`OperatorKind::Update`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LocalId(pub usize);

/// A local variable of an operator, purely local to it (TTM RM Prescription
/// 3): in scope in the whole block it is declared in, and assigned its
/// initial value where it is declared, which is before any use. In an update operator, each is
/// assigned as a variable is; in a read-only one, its declaration and each
/// assignment are a [`Expr::Let`] of a local variable of its own, which has
/// the value from there on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Local {
    /// Unique among the operator's parameters and local variables.
    pub name: String,
    pub ty: Type,
    /// A relation's keys, as a variable's ([`Variable::keys`]).
    pub keys: Vec<Key>,
}

impl Local {
    /// The local variable, in an operator instantiated with `types`.
    #[must_use]
    pub fn instantiated(&self, types: &[Type]) -> Local {
        Local {
            name: self.name.clone(),
            ty: self.ty.instantiated(types),
            keys: self.keys.clone(),
        }
    }
}

/// An operator: an update operator, which returns nothing and is invoked
/// by `call`, or a read-only operator, whose call is a value (TTM RM
/// Prescription 3).
///
/// An operator with type parameters is generic: its parameters' types,
/// its result type and its body refer to them, and each call gives each
/// one a type. Lowering compiles it once for each list of types it is
/// called with, instantiated with them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Operator {
    /// Its name, which other operators can have, with parameters of other
    /// names.
    pub name: String,
    /// The type parameters, the `i`th with index `i`. Each is in some
    /// parameter's type, so a call's arguments determine it.
    pub types: Vec<TypeParameter>,
    /// The parameters, by name. A call's arguments are a tuple of them.
    pub params: BTreeMap<String, Parameter>,
    pub kind: OperatorKind,
    /// Its versions (*Type Inheritance and Relational Theory*, IM
    /// Prescription 17), of subtypes of its parameters' union types, each
    /// before those it is more specific than: a call computes the first
    /// whose parameters its arguments' values are of, and otherwise its own
    /// value or body. None for most operators.
    pub versions: Vec<Version>,
}

/// A version of an operator: another operator, with the same parameters,
/// each of a subtype of the operator's, which a call computes when its
/// arguments' values are of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    pub operator: OperatorId,
    /// Its parameters' types, of those that are narrower than the
    /// operator's: what its arguments' values must be of.
    pub params: BTreeMap<String, Type>,
}

/// A parameter of an operator: its type, and how it takes its argument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parameter {
    pub ty: Type,
    pub mode: Mode,
}

impl Parameter {
    /// A parameter of type `ty` that takes its argument's value.
    #[must_use]
    pub fn constant(ty: Type) -> Self {
        Parameter {
            ty,
            mode: Mode::Const,
        }
    }
}

/// How a parameter takes its argument (TTM RM Prescription 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mode {
    /// The argument's value, which the operator only reads: a parameter's
    /// mode unless it says otherwise.
    Const,
    /// A variable, which an update operator can read and assign, and whose
    /// value is the parameter's when the operator ends: the parameter is
    /// subject to update.
    Var,
    /// A variable, as for [`Mode::Var`], whose value before the call is
    /// never read: the operator assigns the parameter on every path, and
    /// reads it only once it has.
    Out,
}

impl Mode {
    /// The word for it, as written before a parameter's name.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Mode::Const => "const",
            Mode::Var => "var",
            Mode::Out => "out",
        }
    }
}

impl Operator {
    /// The operator a call that gives the type parameters `types` calls:
    /// this one with each type parameter replaced by its type, so it has
    /// none.
    #[must_use]
    pub fn instantiated(&self, types: &[Type]) -> Operator {
        let kind = match &self.kind {
            OperatorKind::Update { locals, body } => OperatorKind::Update {
                locals: locals.iter().map(|l| l.instantiated(types)).collect(),
                body: body.instantiated(types),
            },
            OperatorKind::ReadOnly {
                result,
                locals,
                value,
            } => OperatorKind::ReadOnly {
                result: result.instantiated(types),
                locals: locals.iter().map(|l| l.instantiated(types)).collect(),
                value: value.instantiated(types),
            },
            OperatorKind::Builtin(builtin) => OperatorKind::Builtin(*builtin),
            OperatorKind::Specification { result } => OperatorKind::Specification {
                result: result.as_ref().map(|r| r.instantiated(types)),
            },
        };
        Operator {
            name: self.name.clone(),
            types: Vec::new(),
            params: self
                .params
                .iter()
                .map(|(name, p)| {
                    let parameter = Parameter {
                        ty: p.ty.instantiated(types),
                        mode: p.mode,
                    };
                    (name.clone(), parameter)
                })
                .collect(),
            kind,
            versions: self.versions.clone(),
        }
    }
}

/// A type parameter of an operator: any type of what it is, which each
/// call gives it from its arguments.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TypeParameter {
    /// Its place among the operator's type parameters, which a call's
    /// types follow.
    pub index: usize,
    /// The name it is declared with, in `types { T: any type }`, or `None`
    /// for one written as a parameter's type, as in `v: any type`.
    pub name: Option<String>,
    pub any: Any,
}

/// What a type parameter can be: `any type`, every type; `any tuple`,
/// `any relation` or `any array`, every type that type generator makes; or
/// any type of a kind, such as `any ordered`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Any {
    Type,
    Tuple,
    Relation,
    Array,
    Kind(Kind),
}

impl Any {
    /// Whether a type parameter of this can be used as `requirement` says,
    /// because every type it can be is of a kind that requires it.
    #[must_use]
    pub fn requires(&self, requirement: Requirement) -> bool {
        matches!(self, Any::Kind(kind) if kind.requires.contains(&requirement))
    }
}

/// A kind: a set of types, each of which declares it, and what each must
/// have, which values of a type parameter of the kind can be used for.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Kind {
    pub name: String,
    pub requires: BTreeSet<Requirement>,
}

/// Something a kind's types must have.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Requirement {
    /// `<`, a total order, and so `<=`, `>` and `>=` with it.
    Order,
    /// `+`, addition, and so prefix `+`, which gives its operand.
    Addition,
    /// `*`, multiplication.
    Multiplication,
}

impl Requirement {
    /// Source text: the operator, as a kind's declaration writes it.
    #[must_use]
    pub fn text(self) -> &'static str {
        match self {
            Requirement::Order => "<",
            Requirement::Addition => "+",
            Requirement::Multiplication => "*",
        }
    }
}

/// Source text: `any type`, `any tuple`, `any relation`, or `any` and the
/// kind, as in `any ordered`.
impl std::fmt::Display for Any {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Any::Type => f.write_str("any type"),
            Any::Tuple => f.write_str("any tuple"),
            Any::Relation => f.write_str("any relation"),
            Any::Array => f.write_str("any array"),
            Any::Kind(kind) => write!(f, "any {}", kind.name),
        }
    }
}

/// Its name, or, without one, what it can be: `any type`.
impl std::fmt::Display for TypeParameter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.name {
            Some(name) => f.write_str(name),
            None => write!(f, "{}", self.any),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperatorKind {
    /// An update operator defined in the program, with its local variables,
    /// every block's, and its body.
    Update { locals: Vec<Local>, body: Block },
    /// A read-only operator defined in the program: its value, of type
    /// `result`, computed from its parameters, with its local variables. It
    /// changes nothing else, and calls no operator but read-only ones, none
    /// of which calls itself.
    ReadOnly {
        result: Type,
        locals: Vec<Local>,
        value: Expr,
    },
    /// Declared in the prelude and implemented by the compiler: an update
    /// operator, or a read-only one, with a [`Builtin::result`].
    Builtin(Builtin),
    /// Declared without a body, as an operator of a union type's values,
    /// which its versions compute, one for each type under it: read-only,
    /// with this result type, or, without one, an update operator.
    Specification { result: Option<Type> },
}

/// The operators the compiler implements. Each is declared in the prelude,
/// and sema checks the declaration against [`Builtin::params`] and
/// [`Builtin::result`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Builtin {
    ReadOnly(ReadOnlyBuiltin),
    Update(UpdateBuiltin),
}

/// The read-only operators the compiler implements, each a function of its
/// arguments: a call with the same arguments has the same value, whenever
/// it is made (TTM, RM Prescription 3), so [`ReadOnlyBuiltin::evaluate`]
/// computes any call whose arguments are known when compiling, as running
/// it would. One that would read input, the clock or randomness, or change
/// anything outside the program, is an [`UpdateBuiltin`] instead, which
/// gives its result through an `out` parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReadOnlyBuiltin {
    /// The number of tuples of a relation.
    Count,
    /// The number of elements of an array.
    Length,
    /// The least of a value computed for each tuple of a relation, by `<`.
    Min,
    /// The greatest of a value computed for each tuple of a relation, by
    /// `<`.
    Max,
    /// The sum of a number computed for each tuple of a relation: exact,
    /// then rounded once, so that no order of adding them matters.
    Sum,
    /// The product of a number computed for each tuple of a relation,
    /// exact, then rounded once.
    Product,
    /// A value as trendy source, the text `debug_value` writes.
    ToText,
    /// The approximate number nearest an integer.
    ToApproximate,
    /// The integer a text writes.
    ParseInteger,
    /// The approximate number nearest the decimal number a text writes.
    ParseApproximate,
    /// The boolean a text writes.
    ParseBoolean,
    /// The square root of an approximate number, correctly rounded, which
    /// a negative one hasn't.
    Sqrt,
    /// The sine of an angle in radians.
    Sin,
    /// The cosine of an angle in radians.
    Cos,
    /// The angle, in radians, from the positive x axis to a point, from
    /// -π, exclusive, to π.
    Atan2,
}

/// The update operators the compiler implements, which `call` invokes:
/// those that write output, or read input, which they give through an
/// `out` parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UpdateBuiltin {
    /// Print a value to stderr as one line of trendy source.
    DebugValue,
    /// Print a relation to stderr as a table.
    DebugTable,
    /// Read the lines of a text file, in order, into its `out` parameter,
    /// when the program runs: reading the same file twice can give
    /// different lines.
    LoadTextFile,
    /// As [`UpdateBuiltin::LoadTextFile`], but a file it can't read gives
    /// its error in the `out` parameter `errors` rather than stopping the
    /// program: an `unreadable_file` or a `not_utf8` ([`LanguageError`]).
    LoadTextFileWithErrors,
    /// The records of a CSV file, as tuples whose attributes are its
    /// columns of their names, read when the program runs, as
    /// [`UpdateBuiltin::LoadTextFile`] reads a file's lines.
    LoadCsv,
    /// As [`UpdateBuiltin::LoadCsv`], but a file it can't read gives its
    /// error in the `out` parameter `errors`, as does each record it
    /// can't, which it skips, rather than stopping the program: the
    /// [`LanguageError`]s of reading a file.
    LoadCsvWithErrors,
}

impl UpdateBuiltin {
    /// Whether it reads input or writes output, which a transaction's
    /// statements do none of, at any depth of their calls, since nothing
    /// undoes it: `debug_value` and `debug_table` write for the
    /// programmer, so they are exempt.
    #[must_use]
    pub fn does_io(self) -> bool {
        match self {
            UpdateBuiltin::DebugValue | UpdateBuiltin::DebugTable => false,
            UpdateBuiltin::LoadTextFile
            | UpdateBuiltin::LoadTextFileWithErrors
            | UpdateBuiltin::LoadCsv
            | UpdateBuiltin::LoadCsvWithErrors => true,
        }
    }

    /// Whether it reads a CSV file, as `load_csv`, with its `columns`.
    #[must_use]
    pub fn reads_csv(self) -> bool {
        matches!(
            self,
            UpdateBuiltin::LoadCsv | UpdateBuiltin::LoadCsvWithErrors
        )
    }
}

/// The heading of what a transaction collects, the prelude's
/// `transaction_collection`, which sema checks it declares: each error,
/// `error`, of the prelude's union type `error`, `error_type`, that a
/// statement in the transaction failed or warned with, and whether it
/// failed, `failed`.
#[must_use]
pub fn transaction_collection(error_type: Type) -> Heading {
    Heading::new(
        [
            ("error".to_string(), error_type),
            ("failed".to_string(), Type::Scalar(Scalar::Boolean)),
        ]
        .into(),
    )
}

/// An error the language makes: one a built-in gives in its `out`
/// parameter `errors`, rather than stopping the program, or one a failure
/// of the program's values fails with, which a transaction takes. A value
/// of a subtype of the prelude's union type `error`, which the prelude
/// declares with these components, as sema checks, and which the runtime
/// makes, knowing each by its place in [`LanguageError::ALL`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LanguageError {
    /// A file can't be read, for the system's reason (R0012).
    UnreadableFile,
    /// A file isn't UTF-8 text, from a line on (R0013).
    NotUtf8,
    /// A CSV file isn't CSV on a line, for a problem in words (R0015).
    MalformedCsv,
    /// An attribute is read from a column a CSV file hasn't (R0016).
    MissingColumn,
    /// A CSV file's field isn't a value of its attribute's type (R0014).
    UnreadableField,
    /// A value a CSV file's record gives breaks its type's constraint
    /// (R0017).
    BrokenConstraintInFile,
    /// A value a CSV file's record gives has a relation-valued component
    /// with two tuples that share their values for one of its keys
    /// (R0008).
    ComponentKeyViolatedInFile,
    /// A value a CSV file's record gives is of two subtypes of its type
    /// defined by constraints, but a value has one most specific type
    /// (R0020).
    OverlappingSubtypesInFile,
    /// An operation's result is out of its type's range (R0001).
    Overflow,
    /// An operation divides by zero (R0002).
    DivisionByZero,
    /// A relation hasn't one tuple, where its only one is taken (R0005).
    NotOneTuple,
    /// An array has no element at a position (R0011).
    IndexOutOfRange,
    /// Tuples being joined disagree on a common attribute (R0004).
    TupleMismatch,
    /// A disjoint union adds a tuple that is already there (R0006).
    SharedTuple,
    /// An included minus removes a tuple that isn't there (R0007).
    MissingTuple,
    /// Two tuples of a variable's relation have the same values for one of
    /// its keys (R0008).
    KeyViolated,
    /// Two tuples of a value's relation-valued component have the same
    /// values for one of its keys (R0008).
    ComponentKeyViolated,
    /// Two changes in an `assign` block remove the same tuple (R0009).
    TupleRemovedTwice,
    /// Two changes in an `assign` block set the same array element, or one
    /// an element in the other's (R0009).
    ElementSetTwice,
    /// A relation has no tuples, where the least or greatest value of an
    /// attribute is taken (R0010).
    NoMinOrMax,
    /// A text isn't a value of the type it is parsed as (R0014).
    Unparsable,
    /// A value selected breaks its type's constraint (R0017).
    BrokenConstraint,
    /// A negative number has no square root (R0018).
    NoSquareRoot,
    /// A value treated as one of a type isn't one (R0019).
    NotOfType,
    /// A value is of two subtypes of its type defined by constraints, but a
    /// value has one most specific type (R0020).
    OverlappingSubtypes,
    /// A load's `columns` gives two attributes one column (R0016).
    ColumnShared,
    /// A load's `columns` gives an attribute a column below 1 (R0016).
    ColumnBelowOne,
}

impl LanguageError {
    pub const ALL: [LanguageError; 27] = [
        LanguageError::UnreadableFile,
        LanguageError::NotUtf8,
        LanguageError::MalformedCsv,
        LanguageError::MissingColumn,
        LanguageError::UnreadableField,
        LanguageError::BrokenConstraintInFile,
        LanguageError::ComponentKeyViolatedInFile,
        LanguageError::OverlappingSubtypesInFile,
        LanguageError::Overflow,
        LanguageError::DivisionByZero,
        LanguageError::NotOneTuple,
        LanguageError::IndexOutOfRange,
        LanguageError::TupleMismatch,
        LanguageError::SharedTuple,
        LanguageError::MissingTuple,
        LanguageError::KeyViolated,
        LanguageError::ComponentKeyViolated,
        LanguageError::TupleRemovedTwice,
        LanguageError::ElementSetTwice,
        LanguageError::NoMinOrMax,
        LanguageError::Unparsable,
        LanguageError::BrokenConstraint,
        LanguageError::NoSquareRoot,
        LanguageError::NotOfType,
        LanguageError::OverlappingSubtypes,
        LanguageError::ColumnShared,
        LanguageError::ColumnBelowOne,
    ];

    /// The code a failure with it is written with, when nothing takes it.
    #[must_use]
    pub fn code(self) -> Code {
        match self {
            LanguageError::UnreadableFile => Code::UnreadableFile,
            LanguageError::NotUtf8 => Code::NotUtf8,
            LanguageError::MalformedCsv => Code::MalformedCsv,
            LanguageError::MissingColumn
            | LanguageError::ColumnShared
            | LanguageError::ColumnBelowOne => Code::CsvColumns,
            LanguageError::UnreadableField | LanguageError::Unparsable => Code::Unparsable,
            LanguageError::BrokenConstraintInFile | LanguageError::BrokenConstraint => {
                Code::BrokenConstraint
            }
            LanguageError::ComponentKeyViolatedInFile
            | LanguageError::KeyViolated
            | LanguageError::ComponentKeyViolated => Code::KeyViolated,
            LanguageError::OverlappingSubtypesInFile | LanguageError::OverlappingSubtypes => {
                Code::OverlappingSubtypes
            }
            LanguageError::Overflow => Code::Overflow,
            LanguageError::DivisionByZero => Code::DivisionByZero,
            LanguageError::NotOneTuple => Code::NotOneTuple,
            LanguageError::IndexOutOfRange => Code::IndexOutOfRange,
            LanguageError::TupleMismatch => Code::TupleMismatch,
            LanguageError::SharedTuple => Code::SharedTuple,
            LanguageError::MissingTuple => Code::MissingTuple,
            LanguageError::TupleRemovedTwice | LanguageError::ElementSetTwice => {
                Code::ConflictingChanges
            }
            LanguageError::NoMinOrMax => Code::NoMinOrMax,
            LanguageError::NoSquareRoot => Code::NoResult,
            LanguageError::NotOfType => Code::NotOfType,
        }
    }

    /// The name of its type.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            LanguageError::UnreadableFile => "unreadable_file",
            LanguageError::NotUtf8 => "not_utf8",
            LanguageError::MalformedCsv => "malformed_csv",
            LanguageError::MissingColumn => "missing_column",
            LanguageError::UnreadableField => "unreadable_field",
            LanguageError::BrokenConstraintInFile => "broken_constraint_in_file",
            LanguageError::ComponentKeyViolatedInFile => "component_key_violated_in_file",
            LanguageError::OverlappingSubtypesInFile => "overlapping_subtypes_in_file",
            LanguageError::Overflow => "overflow",
            LanguageError::DivisionByZero => "division_by_zero",
            LanguageError::NotOneTuple => "not_one_tuple",
            LanguageError::IndexOutOfRange => "index_out_of_range",
            LanguageError::TupleMismatch => "tuple_mismatch",
            LanguageError::SharedTuple => "shared_tuple",
            LanguageError::MissingTuple => "missing_tuple",
            LanguageError::KeyViolated => "key_violated",
            LanguageError::ComponentKeyViolated => "component_key_violated",
            LanguageError::TupleRemovedTwice => "tuple_removed_twice",
            LanguageError::ElementSetTwice => "element_set_twice",
            LanguageError::NoMinOrMax => "no_min_or_max",
            LanguageError::Unparsable => "unparsable",
            LanguageError::BrokenConstraint => "broken_constraint",
            LanguageError::NoSquareRoot => "no_square_root",
            LanguageError::NotOfType => "not_of_type",
            LanguageError::OverlappingSubtypes => "overlapping_subtypes",
            LanguageError::ColumnShared => "column_shared",
            LanguageError::ColumnBelowOne => "column_below_one",
        }
    }

    /// Its type's components.
    #[must_use]
    pub fn components(self) -> Heading {
        let (text, integer) = (Scalar::Text, Scalar::Integer);
        let components: &[(&str, Scalar)] = match self {
            LanguageError::UnreadableFile => &[("path", text), ("reason", text)],
            LanguageError::NotUtf8 => &[("path", text), ("line", integer)],
            LanguageError::MalformedCsv => &[("path", text), ("line", integer), ("problem", text)],
            LanguageError::MissingColumn => &[
                ("path", text),
                ("attribute", text),
                ("column", integer),
                ("columns", integer),
            ],
            LanguageError::UnreadableField => &[
                ("path", text),
                ("line", integer),
                ("column", integer),
                ("attribute", text),
                ("type", text),
                ("text", text),
                ("out_of_range", Scalar::Boolean),
            ],
            LanguageError::BrokenConstraintInFile => &[
                ("path", text),
                ("line", integer),
                ("attribute", text),
                ("value", text),
                ("type", text),
                ("constraint", text),
            ],
            LanguageError::ComponentKeyViolatedInFile => &[
                ("path", text),
                ("line", integer),
                ("attribute", text),
                ("type", text),
                ("component", text),
                ("key", text),
                ("first", text),
                ("second", text),
            ],
            LanguageError::OverlappingSubtypesInFile => &[
                ("path", text),
                ("line", integer),
                ("attribute", text),
                ("value", text),
                ("first", text),
                ("second", text),
            ],
            LanguageError::Overflow | LanguageError::DivisionByZero => &[("operation", text)],
            LanguageError::NotOneTuple => &[("count", integer)],
            LanguageError::IndexOutOfRange => &[("position", integer), ("length", integer)],
            LanguageError::TupleMismatch => {
                &[("attribute", text), ("first", text), ("second", text)]
            }
            LanguageError::SharedTuple
            | LanguageError::MissingTuple
            | LanguageError::TupleRemovedTwice => &[("value", text)],
            LanguageError::KeyViolated => &[
                ("variable", text),
                ("key", text),
                ("first", text),
                ("second", text),
            ],
            LanguageError::ComponentKeyViolated => &[
                ("type", text),
                ("component", text),
                ("key", text),
                ("first", text),
                ("second", text),
            ],
            LanguageError::ElementSetTwice => &[("element", text), ("within", Scalar::Boolean)],
            LanguageError::NoMinOrMax => &[("aggregate", text)],
            LanguageError::Unparsable => &[
                ("text", text),
                ("type", text),
                ("out_of_range", Scalar::Boolean),
            ],
            LanguageError::BrokenConstraint => {
                &[("value", text), ("type", text), ("constraint", text)]
            }
            LanguageError::NoSquareRoot => &[("of", Scalar::Approximate)],
            LanguageError::NotOfType => &[("value", text), ("type", text)],
            LanguageError::OverlappingSubtypes => {
                &[("value", text), ("first", text), ("second", text)]
            }
            LanguageError::ColumnShared => {
                &[("first", text), ("second", text), ("column", integer)]
            }
            LanguageError::ColumnBelowOne => &[("attribute", text), ("column", integer)],
        };
        Heading::new(
            components
                .iter()
                .map(|&(name, scalar)| (name.to_string(), Type::Scalar(scalar)))
                .collect(),
        )
    }

    /// The prelude's union type `error`, a root, whose subtypes these are.
    #[must_use]
    pub fn union() -> Type {
        Type::Dummy(Box::new(Dummy {
            name: "error".to_string(),
            supertypes: Vec::new(),
        }))
    }

    /// The type of a built-in's `errors`: `relation { error: error }`.
    #[must_use]
    pub fn relation() -> Type {
        Type::Relation(Heading::new(
            [("error".to_string(), LanguageError::union())].into(),
        ))
    }
}

impl Builtin {
    pub const ALL: [Builtin; 21] = [
        Builtin::Update(UpdateBuiltin::DebugValue),
        Builtin::Update(UpdateBuiltin::DebugTable),
        Builtin::ReadOnly(ReadOnlyBuiltin::Count),
        Builtin::ReadOnly(ReadOnlyBuiltin::Length),
        Builtin::ReadOnly(ReadOnlyBuiltin::Min),
        Builtin::ReadOnly(ReadOnlyBuiltin::Max),
        Builtin::ReadOnly(ReadOnlyBuiltin::Sum),
        Builtin::ReadOnly(ReadOnlyBuiltin::Product),
        Builtin::ReadOnly(ReadOnlyBuiltin::ToText),
        Builtin::ReadOnly(ReadOnlyBuiltin::ToApproximate),
        Builtin::ReadOnly(ReadOnlyBuiltin::ParseInteger),
        Builtin::ReadOnly(ReadOnlyBuiltin::ParseApproximate),
        Builtin::ReadOnly(ReadOnlyBuiltin::ParseBoolean),
        Builtin::ReadOnly(ReadOnlyBuiltin::Sqrt),
        Builtin::ReadOnly(ReadOnlyBuiltin::Sin),
        Builtin::ReadOnly(ReadOnlyBuiltin::Cos),
        Builtin::ReadOnly(ReadOnlyBuiltin::Atan2),
        Builtin::Update(UpdateBuiltin::LoadTextFile),
        Builtin::Update(UpdateBuiltin::LoadTextFileWithErrors),
        Builtin::Update(UpdateBuiltin::LoadCsv),
        Builtin::Update(UpdateBuiltin::LoadCsvWithErrors),
    ];

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Builtin::Update(UpdateBuiltin::DebugValue) => "debug_value",
            Builtin::Update(UpdateBuiltin::DebugTable) => "debug_table",
            Builtin::Update(
                UpdateBuiltin::LoadTextFile | UpdateBuiltin::LoadTextFileWithErrors,
            ) => "load_text_file",
            Builtin::Update(UpdateBuiltin::LoadCsv | UpdateBuiltin::LoadCsvWithErrors) => {
                "load_csv"
            }
            Builtin::ReadOnly(builtin) => builtin.name(),
        }
    }

    /// The type parameters the compiler's implementation takes: for `min`
    /// and `max`, `T`, of the kind `ordered`, and for `sum` and `product`,
    /// of `numeric`, and the tuples of `r`, whose `per` parameter `of` is.
    #[must_use]
    pub fn types(self) -> Vec<TypeParameter> {
        let unnamed = |index, any| TypeParameter {
            index,
            name: None,
            any,
        };
        // `T`, of the kind named, which requires `requires`, and the tuples.
        let of_kind = |name: &str, requires: &[Requirement]| {
            vec![
                TypeParameter {
                    index: 0,
                    name: Some("T".to_string()),
                    any: Any::Kind(Kind {
                        name: name.to_string(),
                        requires: requires.iter().copied().collect(),
                    }),
                },
                unnamed(1, Any::Tuple),
            ]
        };
        match self {
            Builtin::Update(UpdateBuiltin::DebugValue)
            | Builtin::ReadOnly(ReadOnlyBuiltin::ToText) => vec![unnamed(0, Any::Type)],
            Builtin::Update(UpdateBuiltin::DebugTable)
            | Builtin::ReadOnly(ReadOnlyBuiltin::Count) => vec![unnamed(0, Any::Relation)],
            Builtin::ReadOnly(ReadOnlyBuiltin::Length) => vec![unnamed(0, Any::Array)],
            // `C`, the columns of the tuples' attributes, and `T`, the
            // tuples, numbered in their names' order, as sema numbers
            // declared type parameters.
            Builtin::Update(UpdateBuiltin::LoadCsv | UpdateBuiltin::LoadCsvWithErrors) => {
                ["C", "T"]
                    .into_iter()
                    .enumerate()
                    .map(|(index, name)| TypeParameter {
                        index,
                        name: Some(name.to_string()),
                        any: Any::Tuple,
                    })
                    .collect()
            }
            Builtin::ReadOnly(
                ReadOnlyBuiltin::ToApproximate
                | ReadOnlyBuiltin::ParseInteger
                | ReadOnlyBuiltin::ParseApproximate
                | ReadOnlyBuiltin::ParseBoolean
                | ReadOnlyBuiltin::Sqrt
                | ReadOnlyBuiltin::Sin
                | ReadOnlyBuiltin::Cos
                | ReadOnlyBuiltin::Atan2,
            )
            | Builtin::Update(
                UpdateBuiltin::LoadTextFile | UpdateBuiltin::LoadTextFileWithErrors,
            ) => Vec::new(),
            Builtin::ReadOnly(ReadOnlyBuiltin::Min | ReadOnlyBuiltin::Max) => {
                of_kind("ordered", &[Requirement::Order])
            }
            Builtin::ReadOnly(ReadOnlyBuiltin::Sum | ReadOnlyBuiltin::Product) => of_kind(
                "numeric",
                &[Requirement::Addition, Requirement::Multiplication],
            ),
        }
    }

    /// The parameters the compiler's implementation takes, as it sees them:
    /// a `per` parameter is an attribute of its relation's tuples.
    #[must_use]
    pub fn params(self) -> BTreeMap<String, Parameter> {
        let types = self.types();
        let parameter = |index: usize| Type::Parameter(types[index].clone());
        let constant = |name: &str, ty: Type| (name.to_string(), Parameter::constant(ty));
        let out = |name: &str, ty: Type| {
            (
                name.to_string(),
                Parameter {
                    ty,
                    mode: Mode::Out,
                },
            )
        };
        match self {
            Builtin::Update(UpdateBuiltin::DebugValue) => [constant("v", parameter(0))].into(),
            Builtin::Update(UpdateBuiltin::DebugTable)
            | Builtin::ReadOnly(ReadOnlyBuiltin::Count) => [constant("r", parameter(0))].into(),
            Builtin::ReadOnly(ReadOnlyBuiltin::Length) => [constant("a", parameter(0))].into(),
            Builtin::ReadOnly(ReadOnlyBuiltin::ToText) => [constant("from", parameter(0))].into(),
            Builtin::ReadOnly(ReadOnlyBuiltin::ToApproximate) => {
                [constant("from", Type::Scalar(Scalar::Integer))].into()
            }
            Builtin::ReadOnly(
                ReadOnlyBuiltin::ParseInteger
                | ReadOnlyBuiltin::ParseApproximate
                | ReadOnlyBuiltin::ParseBoolean,
            ) => [constant("from", Type::Scalar(Scalar::Text))].into(),
            Builtin::ReadOnly(
                ReadOnlyBuiltin::Sqrt | ReadOnlyBuiltin::Sin | ReadOnlyBuiltin::Cos,
            ) => [constant("of", Type::Scalar(Scalar::Approximate))].into(),
            Builtin::ReadOnly(ReadOnlyBuiltin::Atan2) => [
                constant("y", Type::Scalar(Scalar::Approximate)),
                constant("x", Type::Scalar(Scalar::Approximate)),
            ]
            .into(),
            Builtin::Update(UpdateBuiltin::LoadTextFile) => [
                constant("path", Type::Scalar(Scalar::Text)),
                out("lines", Type::Array(Box::new(Type::Scalar(Scalar::Text)))),
            ]
            .into(),
            Builtin::Update(UpdateBuiltin::LoadTextFileWithErrors) => [
                constant("path", Type::Scalar(Scalar::Text)),
                out("lines", Type::Array(Box::new(Type::Scalar(Scalar::Text)))),
                out("errors", LanguageError::relation()),
            ]
            .into(),
            Builtin::Update(UpdateBuiltin::LoadCsv) => [
                constant("path", Type::Scalar(Scalar::Text)),
                constant("header", Type::Scalar(Scalar::Boolean)),
                constant("columns", parameter(0)),
                out("rows", Type::Array(Box::new(parameter(1)))),
            ]
            .into(),
            Builtin::Update(UpdateBuiltin::LoadCsvWithErrors) => [
                constant("path", Type::Scalar(Scalar::Text)),
                constant("header", Type::Scalar(Scalar::Boolean)),
                constant("columns", parameter(0)),
                out("rows", Type::Array(Box::new(parameter(1)))),
                out("errors", LanguageError::relation()),
            ]
            .into(),
            Builtin::ReadOnly(
                ReadOnlyBuiltin::Min
                | ReadOnlyBuiltin::Max
                | ReadOnlyBuiltin::Sum
                | ReadOnlyBuiltin::Product,
            ) => {
                let tuples = Heading::new(
                    [
                        ("of".to_string(), parameter(0)),
                        (wrapped("r"), parameter(1)),
                    ]
                    .into(),
                );
                [constant("r", Type::Relation(tuples))].into()
            }
        }
    }

    /// The built-in, if it is a read-only one.
    #[must_use]
    pub fn read_only(self) -> Option<ReadOnlyBuiltin> {
        match self {
            Builtin::ReadOnly(builtin) => Some(builtin),
            Builtin::Update(_) => None,
        }
    }

    /// The result type of the compiler's implementation, for a read-only
    /// built-in, whose call is a value.
    #[must_use]
    pub fn result(self) -> Option<Type> {
        match self {
            Builtin::ReadOnly(builtin) => Some(builtin.result()),
            Builtin::Update(_) => None,
        }
    }
}

impl ReadOnlyBuiltin {
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            ReadOnlyBuiltin::Count => "count",
            ReadOnlyBuiltin::Length => "length",
            ReadOnlyBuiltin::Min => "min",
            ReadOnlyBuiltin::Max => "max",
            ReadOnlyBuiltin::Sum => "sum",
            ReadOnlyBuiltin::Product => "product",
            ReadOnlyBuiltin::ToText => "to_text",
            ReadOnlyBuiltin::ToApproximate => "to_approximate",
            ReadOnlyBuiltin::ParseInteger => "parse_integer",
            ReadOnlyBuiltin::ParseApproximate => "parse_approximate",
            ReadOnlyBuiltin::ParseBoolean => "parse_boolean",
            ReadOnlyBuiltin::Sqrt => "sqrt",
            ReadOnlyBuiltin::Sin => "sin",
            ReadOnlyBuiltin::Cos => "cos",
            ReadOnlyBuiltin::Atan2 => "atan2",
        }
    }

    /// The type of the value of a call, which every read-only operator's
    /// call is.
    #[must_use]
    pub fn result(self) -> Type {
        match self {
            ReadOnlyBuiltin::Count | ReadOnlyBuiltin::Length | ReadOnlyBuiltin::ParseInteger => {
                Type::Scalar(Scalar::Integer)
            }
            ReadOnlyBuiltin::Min
            | ReadOnlyBuiltin::Max
            | ReadOnlyBuiltin::Sum
            | ReadOnlyBuiltin::Product => {
                Type::Parameter(Builtin::ReadOnly(self).types()[0].clone())
            }
            ReadOnlyBuiltin::ToText => Type::Scalar(Scalar::Text),
            ReadOnlyBuiltin::ToApproximate
            | ReadOnlyBuiltin::ParseApproximate
            | ReadOnlyBuiltin::Sqrt
            | ReadOnlyBuiltin::Sin
            | ReadOnlyBuiltin::Cos
            | ReadOnlyBuiltin::Atan2 => Type::Scalar(Scalar::Approximate),
            ReadOnlyBuiltin::ParseBoolean => Type::Scalar(Scalar::Boolean),
        }
    }

    /// The value of a call with the arguments `args`, known when compiling,
    /// by parameter name: what running the call gives, as it is a function
    /// of its arguments. `min` and `max` compare values of types of the
    /// program's own with the components in `converted` (see [`compare`]).
    ///
    /// # Errors
    ///
    /// Returns why there is no value: `min` or `max` of a relation without
    /// tuples, which has neither, a `sum` or `product` out of its type's
    /// range, `parse_integer`, `parse_approximate` or `parse_boolean` of
    /// text that isn't a value of its type, or `sqrt` of a negative number.
    ///
    /// # Panics
    ///
    /// If the arguments aren't those of [`Builtin::params`], which sema
    /// rules out.
    pub fn evaluate(
        self,
        args: &BTreeMap<String, Value>,
        converted: &Converted,
    ) -> Result<Value, BuiltinFailure> {
        let relation = || match &args["r"] {
            Value::Relation(r) => r,
            other => panic!("{} of {other:?}", self.name()),
        };
        let approximate = |name: &str| match args[name] {
            Value::Approximate(x) => x,
            ref other => panic!("{} of {other:?}", self.name()),
        };
        let computed = |x: f64| Value::Approximate(Approximate::new(x));
        let tuples = || &relation().tuples;
        let values = || {
            tuples()
                .iter()
                .map(|t| t.get("of").expect("`of` is an attribute"))
        };
        match self {
            ReadOnlyBuiltin::Count => {
                let count = i64::try_from(tuples().len()).expect("a relation's tuples fit");
                Ok(Value::Integer(count))
            }
            ReadOnlyBuiltin::Length => match &args["a"] {
                Value::Array(array) => {
                    let length =
                        i64::try_from(array.elements.len()).expect("an array's elements fit");
                    Ok(Value::Integer(length))
                }
                other => panic!("`length` of {other:?}"),
            },
            ReadOnlyBuiltin::Min => values()
                .min_by(|a, b| compare(a, b, converted))
                .cloned()
                .ok_or(BuiltinFailure::NoTuples),
            ReadOnlyBuiltin::Max => values()
                .max_by(|a, b| compare(a, b, converted))
                .cloned()
                .ok_or(BuiltinFailure::NoTuples),
            ReadOnlyBuiltin::Sum | ReadOnlyBuiltin::Product => {
                let of = relation().heading.get("of").expect("`of` is an attribute");
                let folded = if self == ReadOnlyBuiltin::Sum {
                    sum(of, values())
                } else {
                    product(of, values())
                };
                folded.map_err(BuiltinFailure::Failure)
            }
            ReadOnlyBuiltin::ToText => Ok(Value::Text(args["from"].to_string())),
            ReadOnlyBuiltin::ToApproximate => match args["from"] {
                Value::Integer(n) => Ok(Value::Approximate(Approximate::nearest(n))),
                ref other => panic!("`to_approximate` of {other:?}"),
            },
            ReadOnlyBuiltin::ParseInteger => self.parsed(args, |text| {
                trendy_numeric::parse_integer(text).map(Value::Integer)
            }),
            ReadOnlyBuiltin::ParseApproximate => self.parsed(args, |text| {
                trendy_numeric::parse_approximate(text)
                    .map(|x| Value::Approximate(Approximate::new(x)))
            }),
            ReadOnlyBuiltin::ParseBoolean => self.parsed(args, |text| {
                trendy_text::parse_boolean(text).map(Value::Boolean)
            }),
            ReadOnlyBuiltin::Sqrt => {
                let x = approximate("of");
                trendy_numeric::sqrt(x.get())
                    .map(|root| Value::Approximate(Approximate::new(root)))
                    .ok_or(BuiltinFailure::Failure(Failure::NegativeRoot(x)))
            }
            ReadOnlyBuiltin::Sin => Ok(computed(trendy_numeric::sin(approximate("of").get()))),
            ReadOnlyBuiltin::Cos => Ok(computed(trendy_numeric::cos(approximate("of").get()))),
            ReadOnlyBuiltin::Atan2 => Ok(computed(trendy_numeric::atan2(
                approximate("y").get(),
                approximate("x").get(),
            ))),
        }
    }

    /// The value of a call of this built-in, which reads a value from the
    /// text `args` give `from` with `read`.
    fn parsed(
        self,
        args: &BTreeMap<String, Value>,
        read: impl Fn(&str) -> Result<Value, Unparsable>,
    ) -> Result<Value, BuiltinFailure> {
        let Value::Text(text) = &args["from"] else {
            panic!("`{}` of {:?}", self.name(), args["from"]);
        };
        read(text).map_err(BuiltinFailure::Unparsable)
    }
}

/// Why a call of a read-only built-in has no value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinFailure {
    /// `min` or `max` of a relation without tuples, which has neither.
    NoTuples,
    /// An operator's failure, such as a `sum` out of its type's range.
    Failure(Failure),
    /// `parse_integer`, `parse_approximate` or `parse_boolean` of text
    /// that isn't a value of its type.
    Unparsable(Unparsable),
}

/// The exact sum of `values`, of the numeric type `ty`, rounded once.
fn sum<'a>(ty: &Type, values: impl Iterator<Item = &'a Value>) -> Result<Value, Failure> {
    match ty {
        Type::Scalar(Scalar::Integer) => {
            let mut sum = trendy_numeric::IntegerSum::default();
            integers(values).for_each(|n| sum.add(n));
            let total = sum.total().ok_or(Failure::Overflow(Scalar::Integer))?;
            Ok(Value::Integer(total))
        }
        Type::Scalar(Scalar::Approximate) => {
            let mut sum = trendy_numeric::ApproximateSum::default();
            approximates(values).for_each(|x| sum.add(x));
            let total = sum.total().ok_or(Failure::Overflow(Scalar::Approximate))?;
            Ok(Value::Approximate(Approximate::new(total)))
        }
        other => panic!("a sum of {other}"),
    }
}

/// The exact product of `values`, of the numeric type `ty`, rounded once.
fn product<'a>(
    ty: &Type,
    values: impl Iterator<Item = &'a Value> + Clone,
) -> Result<Value, Failure> {
    match ty {
        Type::Scalar(Scalar::Integer) => {
            let mut product = trendy_numeric::IntegerProduct::default();
            integers(values).for_each(|n| product.multiply(n));
            let total = product.total().ok_or(Failure::Overflow(Scalar::Integer))?;
            Ok(Value::Integer(total))
        }
        Type::Scalar(Scalar::Approximate) => {
            let mut product = trendy_numeric::ApproximateProduct::default();
            approximates(values.clone()).for_each(|x| product.multiply(x));
            let total = match product.total() {
                trendy_numeric::Rounded::Nearest(total) => total,
                trendy_numeric::Rounded::Exact(exact) => {
                    exact.total(approximates(values), &mut vec![0; exact.limbs()])
                }
            };
            let total = total.ok_or(Failure::Overflow(Scalar::Approximate))?;
            Ok(Value::Approximate(Approximate::new(total)))
        }
        other => panic!("a product of {other}"),
    }
}

/// `values`, integers.
fn integers<'a>(values: impl Iterator<Item = &'a Value>) -> impl Iterator<Item = i64> {
    values.map(|value| match value {
        Value::Integer(n) => *n,
        other => panic!("{other:?} as an integer"),
    })
}

/// `values`, approximate numbers.
fn approximates<'a>(values: impl Iterator<Item = &'a Value>) -> impl Iterator<Item = f64> {
    values.map(|value| match value {
        Value::Approximate(x) => x.get(),
        other => panic!("{other:?} as an approximate number"),
    })
}

/// A call of an operator: of an update operator, as a statement, or of a
/// read-only one, as an expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Call {
    pub operator: OperatorId,
    /// The type of each of the operator's type parameters, by index, which
    /// the arguments determine.
    pub types: Vec<Type>,
    /// The arguments for the operator's `const` parameters, by name: their
    /// values.
    pub args: BTreeMap<String, Expr>,
    /// The variables for its `var` and `out` parameters, by name, which the
    /// call updates: each a different one. A read-only operator has none.
    pub updated: BTreeMap<String, Target>,
}

impl Call {
    /// The call, in an operator instantiated with `types`.
    #[must_use]
    pub fn instantiated(&self, types: &[Type]) -> Call {
        Call {
            operator: self.operator,
            types: self.types.iter().map(|t| t.instantiated(types)).collect(),
            args: self
                .args
                .iter()
                .map(|(name, arg)| (name.clone(), arg.instantiated(types)))
                .collect(),
            updated: self.updated.clone(),
        }
    }
}

/// A statement in an operator's body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Statement {
    /// `call name{arguments};`.
    Call(Call),
    /// Assignments made at once (TTM RM Prescription 21), each to a
    /// different variable, or local variable, which a declaration assigns
    /// its initial value, the shorthands, such as `insert`, expanded, and
    /// an `assign` block's changes to one variable merged. Every value is
    /// computed from the variables as they were before any changes, and
    /// the variables' keys are checked once all the values are, before any
    /// is assigned.
    Assign(Vec<Assignment>),
    /// `if condition then [ … ] else [ … ];`: the first block's statements
    /// if the condition, a boolean, is true, and else the second's, which
    /// is empty without an `else`.
    If(Expr, Block, Block),
    /// The block's statements run while the condition, a boolean, is true,
    /// checked before each run, `while condition loop [ … ];`, or after,
    /// `loop [ … ] while condition;`, which runs them at least once.
    Loop(Expr, Block, Check),
    /// `for x in a loop [ … ];`: the block's statements run for each
    /// element of the array, computed once, in order, with the local
    /// variable, which the block lists, the element's value. Neither it nor
    /// a variable the array is computed from changes while the loop runs.
    For(LocalId, Expr, Block),
    /// `fail e;`: the program fails with the error that the expression is,
    /// as [`Expr::Fail`] does.
    Fail(Expr),
    /// `warn e;`: the program warns with the error that the expression is,
    /// and carries on: the innermost transaction with `collecting` running
    /// collects it, as it does a failure, or, with none, the program writes
    /// its message.
    Warn(Expr),
    /// `transaction [ … ];`, or `transaction collecting e [ … ];`, with the
    /// variable `e`, a `relation { error: error }`: the block's statements,
    /// which, if one fails, are undone, all of them, every variable they
    /// assigned given back its value from before the transaction (TTM OO
    /// Prescriptions 4 and 5). Then, with a variable, the failure's error
    /// is added to it, and the statements after the transaction run;
    /// without, the failure goes on, to the transaction around it, or the
    /// program's end. Transactions nest.
    Transaction(Option<Target>, Block),
}

/// When a loop checks its condition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Check {
    /// Before each run of its block: `while c loop [ … ];`.
    Before,
    /// After each run of its block: `loop [ … ] while c;`.
    After,
}

impl Statement {
    /// The blocks in the statement: an `if`'s two, a loop's, or a
    /// transaction's.
    #[must_use]
    pub fn blocks(&self) -> Vec<&Block> {
        match self {
            Statement::If(_, then, otherwise) => vec![then, otherwise],
            Statement::Loop(_, body, _)
            | Statement::For(_, _, body)
            | Statement::Transaction(_, body) => vec![body],
            Statement::Call(_) | Statement::Assign(_) | Statement::Fail(_) | Statement::Warn(_) => {
                Vec::new()
            }
        }
    }

    /// The statement, in an operator instantiated with `types`.
    #[must_use]
    pub fn instantiated(&self, types: &[Type]) -> Statement {
        match self {
            Statement::Call(call) => Statement::Call(call.instantiated(types)),
            Statement::Assign(assignments) => Statement::Assign(
                assignments
                    .iter()
                    .map(|a| Assignment {
                        target: a.target.clone(),
                        value: a.value.instantiated(types),
                    })
                    .collect(),
            ),
            Statement::If(condition, then, otherwise) => Statement::If(
                condition.instantiated(types),
                then.instantiated(types),
                otherwise.instantiated(types),
            ),
            Statement::Loop(condition, body, check) => Statement::Loop(
                condition.instantiated(types),
                body.instantiated(types),
                *check,
            ),
            Statement::For(variable, array, body) => Statement::For(
                *variable,
                array.instantiated(types),
                body.instantiated(types),
            ),
            Statement::Fail(error) => Statement::Fail(error.instantiated(types)),
            Statement::Warn(error) => Statement::Warn(error.instantiated(types)),
            Statement::Transaction(collecting, body) => {
                Statement::Transaction(collecting.clone(), body.instantiated(types))
            }
        }
    }
}

/// Where the program is in its source while it runs: a statement's place,
/// or a value's, which a failure there says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Site {
    pub file: File,
    /// Counting from 1.
    pub line: usize,
    /// Counting from 1, in characters, as diagnostics count them.
    pub column: usize,
}

/// The source a [`Site`] is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum File {
    /// The program's own.
    Program,
    /// The prelude's, whose operators written in trendy are in every
    /// program.
    Prelude,
}

/// Statements run in order, an update operator's body or a branch of an
/// `if` statement, with the local variables declared among them, which are
/// in scope from their declaration to its end.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Block {
    /// Each statement, with where it is.
    pub statements: Vec<(Site, Statement)>,
    /// The local variables declared in it, not in a block within it: their
    /// memory is released at its end.
    pub locals: Vec<LocalId>,
}

impl Block {
    /// The block, in an operator instantiated with `types`.
    #[must_use]
    pub fn instantiated(&self, types: &[Type]) -> Block {
        Block {
            statements: self
                .statements
                .iter()
                .map(|(site, s)| (*site, s.instantiated(types)))
                .collect(),
            locals: self.locals.clone(),
        }
    }

    /// Call `f` with each of the block's statements, and those in the
    /// blocks within them, each before those within it.
    pub fn each_statement(&self, f: &mut dyn FnMut(&Statement)) {
        for (_, statement) in &self.statements {
            f(statement);
            for block in statement.blocks() {
                block.each_statement(f);
            }
        }
    }

    /// What the block's statements assign, in the blocks within them too,
    /// each once, in the order they first do: each assignment's target,
    /// each variable a call passes to a `var` or `out` parameter, and each
    /// a transaction adds its errors to.
    #[must_use]
    pub fn assigned(&self) -> Vec<Target> {
        let mut assigned = Vec::new();
        self.each_statement(&mut |statement| {
            let targets: Vec<&Target> = match statement {
                Statement::Assign(assignments) => assignments.iter().map(|a| &a.target).collect(),
                Statement::Call(call) => call.updated.values().collect(),
                Statement::Transaction(collecting, _) => collecting.iter().collect(),
                _ => Vec::new(),
            };
            for target in targets {
                if !assigned.contains(target) {
                    assigned.push(target.clone());
                }
            }
        });
        assigned
    }

    /// The local variables declared in the block, and in the blocks
    /// within it: each block's own, and each `for` loop's variable.
    #[must_use]
    pub fn declared(&self) -> Vec<LocalId> {
        let mut declared = self.locals.clone();
        self.each_statement(&mut |statement| {
            if let Statement::For(variable, ..) = statement {
                declared.push(*variable);
            }
            for block in statement.blocks() {
                declared.extend(&block.locals);
            }
        });
        declared
    }

    /// The operators the block's statements call, in the blocks within
    /// them too, each once, in the order they first do.
    #[must_use]
    pub fn called(&self) -> Vec<OperatorId> {
        let mut called = Vec::new();
        self.each_statement(&mut |statement| {
            if let Statement::Call(call) = statement
                && !called.contains(&call.operator)
            {
                called.push(call.operator);
            }
        });
        called
    }
}

/// `target := value`, in an assignment statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assignment {
    pub target: Target,
    pub value: Expr,
}

/// What an assignment assigns: a variable of the program, a local
/// variable of the operator it is in, or a `var` parameter of it, whose
/// argument's variable it updates.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Target {
    Variable(VariableId),
    Local(LocalId),
    Parameter(String),
}

impl Target {
    /// The variable, as its name reads it.
    #[must_use]
    pub fn read(&self) -> Expr {
        match self {
            Target::Variable(id) => Expr::Variable(*id),
            Target::Local(id) => Expr::Local(*id),
            Target::Parameter(name) => Expr::Parameter(name.clone()),
        }
    }
}

/// A change to an element of an array, as in `a[i][j] := x`: the element
/// at `path`, its positions, from the outermost array in, counting from 1,
/// given `value`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ElementChange {
    pub path: Vec<Expr>,
    pub value: Expr,
}

impl ElementChange {
    /// Whether this change and `other` overlap, one's path being the
    /// other's or the start of it, as far as their positions show now:
    /// `Some(true)` if every position the two paths have in common is
    /// known to be the same, the same constant or expression, `Some(false)`
    /// if one is known to differ, and `None` if that is known only when
    /// running.
    #[must_use]
    pub fn overlaps(&self, other: &ElementChange) -> Option<bool> {
        let mut same = true;
        for (p, q) in self.path.iter().zip(&other.path) {
            match (p, q) {
                (Expr::Constant(p), Expr::Constant(q)) if p != q => return Some(false),
                _ if p == q => {}
                _ => same = false,
            }
        }
        same.then_some(true)
    }

    /// The change, in an operator instantiated with `types`.
    #[must_use]
    pub fn instantiated(&self, types: &[Type]) -> ElementChange {
        ElementChange {
            path: self.path.iter().map(|p| p.instantiated(types)).collect(),
            value: self.value.instantiated(types),
        }
    }
}

/// An attribute a `load` orders a relation's tuples by, or a component a
/// type of the program's own orders its values by, and in which
/// direction: `asc a` or `desc a`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OrderItem {
    pub attribute: String,
    pub descending: bool,
}

/// What an array loaded from a relation holds: each of its tuples, less
/// the attributes removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Loaded {
    /// The tuples.
    Tuples,
    /// The values of the tuples' only attribute.
    Values,
}

/// How a `load` makes a relation of an array (see [`Expr::Unload`]): each
/// element a tuple, its own if it is one, with the attribute `named`, if
/// there is one, whose value it is, and `numbered`, if there is one, whose
/// value is its position, counting from 1.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Unloading {
    pub named: Option<String>,
    pub numbered: Option<String>,
}

impl Unloading {
    /// The heading of the relation made of an array of `element`s.
    ///
    /// # Panics
    ///
    /// If an element without a name isn't a tuple, or the number's name is
    /// one of its attributes', which sema rules out.
    #[must_use]
    pub fn heading(&self, element: &Type) -> Heading {
        let mut attributes: BTreeMap<String, Type> = match (&self.named, element) {
            (Some(name), _) => [(name.clone(), element.clone())].into(),
            (None, Type::Tuple(heading)) => heading
                .attributes()
                .map(|(a, ty)| (a.to_string(), ty.clone()))
                .collect(),
            (None, other) => panic!("the tuples of an array of {other}"),
        };
        if let Some(numbered) = &self.numbered {
            let clash = attributes.insert(numbered.clone(), Type::Scalar(Scalar::Integer));
            assert!(
                clash.is_none(),
                "`{numbered}` numbers elements that have it"
            );
        }
        Heading::new(attributes)
    }

    /// The tuple the element `value`, at `position`, counting from 1,
    /// becomes.
    ///
    /// # Panics
    ///
    /// If an element without a name isn't a tuple, which sema rules out.
    #[must_use]
    pub fn tuple(&self, value: &Value, position: usize) -> TupleValue {
        let mut tuple = match (&self.named, value) {
            (Some(name), _) => TupleValue::new([(name.clone(), value.clone())].into()),
            (None, Value::Tuple(tuple)) => tuple.clone(),
            (None, other) => panic!("the tuple of {other:?}"),
        };
        if let Some(numbered) = &self.numbered {
            let position = i64::try_from(position).expect("an array's positions fit");
            tuple = tuple.with([(numbered.clone(), Value::Integer(position))]);
        }
        tuple
    }
}

/// How a `load` makes an array of a relation (see [`Expr::Load`]).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Loading {
    /// The items the tuples are ordered by: at least one, each a different
    /// attribute of the relation, of an ordered type, by the first, then,
    /// among tuples equal on it, by the next, and so on.
    pub order: Vec<OrderItem>,
    /// The attributes left out of each element, once the tuples are
    /// ordered: equal elements are each still there.
    pub removed: BTreeSet<String>,
    pub loaded: Loaded,
}

impl Loading {
    /// The type of the elements of an array loaded from a relation with
    /// `heading`.
    ///
    /// # Panics
    ///
    /// For [`Loaded::Values`], if the heading hasn't exactly one attribute
    /// besides those removed, which sema rules out.
    #[must_use]
    pub fn element(&self, heading: &Heading) -> Type {
        let kept = heading.without(&self.removed);
        match self.loaded {
            Loaded::Tuples => Type::Tuple(kept),
            Loaded::Values => match kept.attributes().collect::<Vec<_>>()[..] {
                [(_, ty)] => ty.clone(),
                _ => panic!("the values of {kept:?}, which hasn't one attribute"),
            },
        }
    }
}

/// An expression whose value is known only when the program runs. Parts
/// known at compile time are already evaluated to constants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    /// A value known at compile time.
    Constant(Value),
    Variable(VariableId),
    /// A parameter of the operator the expression is in.
    Parameter(String),
    /// A local variable of the operator the expression is in.
    Local(LocalId),
    /// The last expression, with the local variable holding the value of
    /// the first, computed once, in it: a read-only operator's local
    /// variable, from its declaration or an assignment on.
    Let(LocalId, Box<Expr>, Box<Expr>),
    /// The expression, computed at the site: a value block's local
    /// variable's value, or the value it ends with, which a failure in it
    /// says it is at (see [`Expr::at`]).
    At(Site, Box<Expr>),
    /// A monadic operator: `not x` or `-x`.
    Unary(UnaryOp, Box<Expr>),
    /// A dyadic operator, such as `x + y` or `x < y`.
    Binary(BinaryOp, Box<Expr>, Box<Expr>),
    /// `and`, `or` or `xor` of its operands, booleans, of which there are
    /// at least two. Sema keeps them in a normal form: an operand is never
    /// a constant, nor the same connective, as a chain of them, however it
    /// is grouped, is one; the operands of `and` and `or` are distinct; and
    /// a `not` is in them only where it can't go further in, which is never
    /// on an operand of an `xor`, but on the `xor`, if it has more than two
    /// (see [`Connective`]). `not (a xor b)` is `a = b`, which is never an
    /// operand of an `xor` either.
    Connective(Connective, Vec<Expr>),
    /// The value of a tuple's attribute: `t.a`. The attribute is one of the
    /// tuple's.
    Extract(Box<Expr>, String),
    /// A tuple selector whose attribute values aren't all known when
    /// compiling.
    Tuple(BTreeMap<String, Expr>),
    /// A relation selector with this heading whose tuples aren't all known
    /// when compiling: each tuple of the heading's tuple type. Tuples that
    /// turn out equal become one when the program runs.
    Relation(Heading, Vec<Expr>),
    /// A tuple or relation without the named attributes (Algebra A's
    /// REMOVE). A projection is remove of the attributes not named. The names
    /// are attributes of the operand, and there is at least one.
    Remove(Box<Expr>, BTreeSet<String>),
    /// A tuple or relation with attributes renamed (Algebra A's RENAME),
    /// all at once, from each key to its value. The keys are attributes of
    /// the operand, and the result's names are distinct; no renaming is to
    /// the same name, and there is at least one.
    Rename(Box<Expr>, BTreeMap<String, String>),
    /// The relation of every tuple made of one tuple of each operand that
    /// agree on their common attributes (Algebra A's AND, of relations):
    /// what `join`, `times` and `compose` all are. Of tuples, the tuple of
    /// both operands' attributes (tuple join, or tuple union), which fails
    /// if they disagree on a common attribute, since no tuple would do. The
    /// operands are both relations or both tuples, whose common attributes
    /// have overlapping types, and are of the least specific common
    /// subtype of them in the result (see [`Heading::and`]). Values of the
    /// types in a hierarchy are laid out alike, as its root's, so they
    /// compare as they are.
    And(Box<Expr>, Box<Expr>),
    /// Two relations of the same attributes combined, as the `Combination`
    /// says: the tuples of either, or of the left that aren't in the right,
    /// where the checked ones fail if a tuple breaks their condition. Each
    /// attribute's types overlap, for a minus, or, for a union, have a
    /// common supertype, and the result's heading is as
    /// [`Combination::heading`] says. `intersect` is an `And`, of
    /// relations with all their attributes in common.
    Combine(Combination, Box<Expr>, Box<Expr>),
    /// The tuples of the left relation that have a match in the right, on
    /// their common attributes (`matching`, the semijoin): the result has
    /// the left's heading. The operands' common attributes have
    /// overlapping types, as for `And`.
    Semijoin(Box<Expr>, Box<Expr>),
    /// The tuples of the left relation that have no match in the right
    /// (`not matching`, the antijoin), otherwise as for `Semijoin`.
    Antijoin(Box<Expr>, Box<Expr>),
    /// A tuple, or each tuple of a relation, with attributes computed, each
    /// by its expression from the tuple at hand, which is `Current(0, …)` in
    /// it. An attribute the operand hasn't is added (`extend`); one it has
    /// gets a new value, computed from the old ones, all at once, of the
    /// same type for `replace`, or of another for `wrap`, which replaces an
    /// attribute it wraps with the tuple of them. This is Tutorial D
    /// Version 2's `EXTEND`, which does both. The heading is the computed attributes': an empty relation
    /// has no tuple to compute them from, but the result still has them.
    Extend(Box<Expr>, Heading, BTreeMap<String, Expr>),
    /// A relation with relation-valued attributes replaced by their
    /// attributes (`ungroup`): for each tuple, one tuple for each
    /// combination of a tuple of each named attribute's relation, so a tuple
    /// with an empty one has none. The names are relation-valued attributes
    /// of the operand, at least one, and no two attributes of the result
    /// share a name. Not Algebra A's: Appendix A defines it with an infinite
    /// relation, which no program can hold.
    Ungroup(Box<Expr>, BTreeSet<String>),
    /// The tuples of a relation for which the condition, a boolean computed
    /// from the tuple at hand, `Current(0, …)` in it, is true (`where`).
    /// Not Algebra A's AND with the relation of the tuples that satisfy
    /// the condition, which is usually infinite.
    Restrict(Box<Expr>, Box<Expr>),
    /// An array selector: its elements, of this type, in order.
    Array(Type, Vec<Expr>),
    /// The element of the array at the position the second expression
    /// gives, counting from 1, which fails if it has none there.
    Index(Box<Expr>, Box<Expr>),
    /// The array with an element changed for each change, at its path:
    /// what `a[i] := x` assigns `a`, as TTM's pseudovariables are shorthand
    /// for an assignment of the whole variable (RM Prescription 5). Every
    /// position and value is computed from the array as it was. It fails if
    /// a position is out of range, as `Index` does, or if two changes'
    /// paths overlap, one's being the other's or the start of it, since no
    /// order of the two would be the right one. The changes are at least
    /// one.
    WithElements(Box<Expr>, Vec<ElementChange>),
    /// The second expression's value if the first, a boolean, is true, and
    /// else the third's: only that one is computed, so the other's failure
    /// never happens.
    If(Box<Expr>, Box<Expr>, Box<Expr>),
    /// The array a `load` makes of a relation: its tuples, in the order the
    /// `Loading` says, each without the attributes it removes, or the
    /// value of the only attribute left. Tuples equal on every item come in
    /// canonical order, which the language leaves undefined.
    Load(Box<Expr>, Loading),
    /// The relation a `load` makes of an array: each element a tuple, as the
    /// `Unloading` says, and those that turn out equal one.
    Unload(Box<Expr>, Unloading),
    /// The only tuple of a relation (`tuple from`), which fails if it has
    /// none or several. `value from` is the extraction of its only
    /// attribute.
    Only(Box<Expr>),
    /// The value of the program's own type `Defined` whose components, in
    /// its possible representation, are the tuple: what its selector
    /// computes, as the privileged operator Date has a selector written
    /// with sets a value's physical representation (*Type Inheritance and
    /// Relational Theory*, chapter 2). It fails unless each of the type's
    /// constraints, in order, is true of the components, since a type is
    /// the values its constraints are true of (TTM RM Prescription 23).
    Select(Box<Defined>, Box<Expr>),
    /// The components of a value of one of the program's own types, in its
    /// possible representation, as a tuple: what its `THE_` operators
    /// extract from, as `v.c` is `Extract(Components(v), c)`.
    Components(Box<Expr>),
    /// The value of the expression, as a value of the type, which it is
    /// known to be of: TREAT (*Type Inheritance and Relational Theory*, IM
    /// Prescription 14) to a supertype of the expression's type, which
    /// gives a value its declared type where a value of a subtype is
    /// substituted for one of it (IM Prescription 16), or where what is
    /// known when compiling is narrower than what is declared, as a
    /// variable's value can be; or to the relation a restriction's
    /// condition shows its tuples' attributes to be of. A value of a type
    /// in a hierarchy is laid out as every value of its root's, so nothing
    /// changes when running, and it can't fail.
    Treat(Box<Type>, Box<Expr>),
    /// The value of the expression, as a value of the type, a subtype of
    /// its type: TREAT down (IM Prescription 14), which fails unless the
    /// value is of the type. A name narrowed by a test, `x is t`, is one,
    /// which the guard rule computes only once the test is true (see
    /// [`Connective::guarded`]).
    TreatAs(Box<Type>, Box<Expr>),
    /// Whether the value of the expression is of the type, or one of its
    /// subtypes (IM Prescription 15).
    Is(Box<Expr>, Box<Type>),
    /// `fail e`: the program fails with the error that the expression is,
    /// a value of the prelude's union type `error`, writing its message
    /// ([`Program::message`]), so it has no value. The type is the one a
    /// value would have where it is written, as one branch of an `if`
    /// takes the other's. It fails only if it is computed, as a branch not
    /// taken isn't.
    Fail(Box<Expr>, Box<Type>),
    /// The tuple at hand, with this heading: the one an `Extend` is computing
    /// attributes for, or a `Restrict` a condition, `depth` of them out from
    /// here, counting the innermost as 0. Its attributes are in scope by
    /// name in the source, and are extracted from it here.
    Current(usize, Heading),
    /// A call of a read-only operator, whose value it is. Sema evaluates
    /// every call it can, so one remains only where an argument, or a
    /// variable the operator reads, is known only when the program runs.
    Call(Call),
}

impl Expr {
    /// `expr`, computed at `site`, so that a failure in it says so; one
    /// that can't fail needs no site.
    #[must_use]
    pub fn at(site: Site, expr: Expr) -> Expr {
        if expr.can_fail() {
            Expr::At(site, Box::new(expr))
        } else {
            expr
        }
    }

    /// The expression's type, where `leaf` gives the type of each variable,
    /// parameter and call in it.
    ///
    /// # Panics
    ///
    /// If the expression isn't well typed, which sema rules out.
    #[must_use]
    pub fn type_with(&self, leaf: &dyn Fn(&Expr) -> Type) -> Type {
        match self {
            Expr::Constant(value) => value.ty(),
            Expr::Variable(_) | Expr::Parameter(_) | Expr::Local(_) | Expr::Call(_) => leaf(self),
            Expr::Let(_, _, body) | Expr::At(_, body) => body.type_with(leaf),
            Expr::Array(element, _) => Type::Array(Box::new(element.clone())),
            Expr::Index(array, _) => match array.type_with(leaf) {
                Type::Array(element) => *element,
                other => panic!("an element of {other}"),
            },
            Expr::WithElements(array, _) => array.type_with(leaf),
            Expr::If(_, then, _) => then.type_with(leaf),
            Expr::Unload(array, unloading) => match array.type_with(leaf) {
                Type::Array(element) => Type::Relation(unloading.heading(&element)),
                other => panic!("unloading {other}"),
            },
            Expr::Load(relation, loading) => match relation.type_with(leaf) {
                Type::Relation(heading) => Type::Array(Box::new(loading.element(&heading))),
                other => panic!("loading {other}"),
            },
            // Arithmetic on a type parameter's values has their type.
            Expr::Unary(op, operand) => op
                .operand()
                .map_or_else(|| operand.type_with(leaf), Type::Scalar),
            Expr::Binary(op, lhs, _) => op
                .result()
                .map_or_else(|| lhs.type_with(leaf), Type::Scalar),
            Expr::Connective(..) | Expr::Is(..) => Type::Scalar(Scalar::Boolean),
            Expr::Extract(tuple, name) => match tuple.type_with(leaf) {
                Type::Tuple(heading) => heading
                    .get(name)
                    .cloned()
                    .expect("sema extracts only attributes the tuple has"),
                other => panic!("extracting `{name}` from {other}"),
            },
            Expr::Tuple(attributes) => Type::Tuple(Heading::new(
                attributes
                    .iter()
                    .map(|(name, e)| (name.clone(), e.type_with(leaf)))
                    .collect(),
            )),
            Expr::Relation(heading, _) => Type::Relation(heading.clone()),
            Expr::Remove(operand, names) => operand.type_with(leaf).without(names),
            Expr::Rename(operand, renamings) => operand.type_with(leaf).renamed(renamings),
            Expr::And(lhs, rhs) => lhs.type_with(leaf).and(&rhs.type_with(leaf)),
            Expr::Extend(operand, computed, _) => operand.type_with(leaf).with(computed),
            Expr::Current(_, heading) => Type::Tuple(heading.clone()),
            Expr::Ungroup(operand, names) => operand.type_with(leaf).ungrouped(names),
            Expr::Combine(combination, lhs, rhs) => {
                combination.combined(&lhs.type_with(leaf), &rhs.type_with(leaf))
            }
            Expr::Restrict(operand, _)
            | Expr::Semijoin(operand, _)
            | Expr::Antijoin(operand, _) => operand.type_with(leaf),
            Expr::Only(operand) => match operand.type_with(leaf) {
                Type::Relation(heading) => Type::Tuple(heading),
                other => panic!("the only tuple of {other}"),
            },
            Expr::Select(defined, ..) => Type::Defined(defined.clone()),
            Expr::Components(value) => match value.type_with(leaf) {
                Type::Defined(defined) => Type::Tuple(defined.components),
                other => panic!("the components of {other}"),
            },
            Expr::Treat(ty, _) | Expr::TreatAs(ty, _) | Expr::Fail(_, ty) => (**ty).clone(),
        }
    }
}

impl Expr {
    /// Whether computing the expression can fail when the program runs:
    /// an integer overflow, a division by zero, running out of memory, or
    /// a failure in an operator it calls. This is part of the language: of
    /// the operands of `and` and `or`, those that can fail are computed
    /// only when those that can't don't decide the result (see
    /// [`Connective::And`]).
    #[must_use]
    pub fn can_fail(&self) -> bool {
        match self {
            Expr::Constant(_)
            | Expr::Variable(_)
            | Expr::Parameter(_)
            | Expr::Local(_)
            | Expr::Current(..) => false,
            // Negating the smallest integer overflows, and building a relation,
            // removing, renaming or adding attributes, joining or ungrouping it,
            // allocates the result, as building or loading an array does; adding
            // attributes computes them, which can fail too, as can a call,
            // which computes its operator's value, and an index can be out of
            // range, and a value treated as one of a subtype of its type
            // needn't be one.
            Expr::Unary(UnaryOp::Negate(Numeric::Integer), _)
            | Expr::Fail(..)
            | Expr::TreatAs(..)
            | Expr::Call(..)
            | Expr::Remove(..)
            | Expr::Rename(..)
            | Expr::And(..)
            | Expr::Ungroup(..)
            | Expr::Only(..)
            | Expr::Restrict(..)
            | Expr::Semijoin(..)
            | Expr::Antijoin(..)
            | Expr::Combine(..)
            | Expr::Extend(..)
            | Expr::Relation(..)
            | Expr::Array(..)
            | Expr::Load(..)
            | Expr::Unload(..)
            | Expr::Index(..)
            | Expr::WithElements(..) => true,
            // A value breaks a constraint, or a constraint's operator
            // fails.
            Expr::Select(defined, operand) => !defined.constraints.is_empty() || operand.can_fail(),
            // A value is a value of its type's supertypes.
            Expr::Unary(_, operand)
            | Expr::At(_, operand)
            | Expr::Components(operand)
            | Expr::Treat(_, operand)
            | Expr::Is(operand, _) => operand.can_fail(),
            Expr::Tuple(attributes) => attributes.values().any(Expr::can_fail),
            Expr::Connective(_, operands) => operands.iter().any(Expr::can_fail),
            Expr::Extract(tuple, _) => tuple.can_fail(),
            Expr::Binary(op, lhs, rhs) => {
                // Arithmetic and concatenation can fail themselves;
                // comparisons can't.
                let itself = !matches!(op, BinaryOp::Compare(..));
                itself || lhs.can_fail() || rhs.can_fail()
            }
            Expr::Let(_, value, body) => value.can_fail() || body.can_fail(),
            Expr::If(condition, then, otherwise) => {
                condition.can_fail() || then.can_fail() || otherwise.can_fail()
            }
        }
    }

    /// The expression, whose type is a subtype of `ty`, as one of `ty`: a
    /// relation or an array known when compiling takes `ty`'s heading or
    /// elements' type, which its value carries, a constant of `ty` stays
    /// as it is, and anything else is treated as a value of `ty` (see
    /// [`Expr::Treat`]), once.
    #[must_use]
    pub fn treated(self, ty: &Type) -> Expr {
        match (self, ty) {
            (Expr::Constant(Value::Relation(relation)), Type::Relation(heading)) => {
                Expr::Constant(Value::Relation(RelationValue {
                    heading: heading.clone(),
                    tuples: relation.tuples,
                }))
            }
            (Expr::Constant(Value::Array(array)), Type::Array(element)) => {
                Expr::Constant(Value::Array(ArrayValue {
                    element: (**element).clone(),
                    elements: array.elements,
                }))
            }
            (Expr::Constant(value), _) if value.ty() == *ty => Expr::Constant(value),
            (Expr::Treat(_, inner), _) => inner.treated(ty),
            (expr, _) => Expr::Treat(Box::new(ty.clone()), Box::new(expr)),
        }
    }

    /// `not self`, taken as far into it as it goes, so that a `not` is
    /// only where it can't go further: `not not a` is `a`, `not (a and b)`
    /// is `not a or not b` and `not (a or b)` is `not a and not b` (De
    /// Morgan), `not (a < b)` is `a >= b`, and `not (a xor b)` is `a = b`.
    /// None of these changes which operands can fail, nor which guard
    /// which, as `and` and `or` swap together with false and true (see
    /// [`Connective::And`]). It stops at an `xor` of more than two, and at
    /// a comparison of relations by inclusion (see [`Comparison::negated`]).
    #[must_use]
    pub fn negated(self) -> Expr {
        match self {
            Expr::Constant(Value::Boolean(value)) => Expr::Constant(Value::Boolean(!value)),
            Expr::Unary(UnaryOp::Not, inner) => *inner,
            Expr::Connective(Connective::Xor, operands) => match <[Expr; 2]>::try_from(operands) {
                Ok([lhs, rhs]) => Expr::Binary(
                    BinaryOp::Compare(Comparison::Equal, Compared::Scalar(Scalar::Boolean)),
                    Box::new(lhs),
                    Box::new(rhs),
                ),
                Err(operands) => Expr::Unary(
                    UnaryOp::Not,
                    Box::new(Expr::Connective(Connective::Xor, operands)),
                ),
            },
            Expr::Connective(c @ (Connective::And | Connective::Or), operands) => {
                let swapped = if c == Connective::And {
                    Connective::Or
                } else {
                    Connective::And
                };
                swapped.of(operands.into_iter().map(Expr::negated).collect())
            }
            Expr::Binary(BinaryOp::Compare(comparison, compared), lhs, rhs)
                if matches!(comparison, Comparison::Equal | Comparison::NotEqual)
                    || matches!(compared, Compared::Scalar(_)) =>
            {
                Expr::compared(comparison.negated(), compared, *lhs, *rhs)
            }
            operand => Expr::Unary(UnaryOp::Not, Box::new(operand)),
        }
    }

    /// `lhs` compared with `rhs`, in normal form: `<>` of booleans is
    /// `xor`, and `=` of them `not xor`, which is `=` again unless the
    /// operands make it more or less than an `xor` of two, as `a = true` is
    /// `a`.
    #[must_use]
    pub fn compared(comparison: Comparison, compared: Compared, lhs: Expr, rhs: Expr) -> Expr {
        match (comparison, compared) {
            (Comparison::NotEqual, Compared::Scalar(Scalar::Boolean)) => {
                Connective::Xor.of(vec![lhs, rhs])
            }
            (Comparison::Equal, Compared::Scalar(Scalar::Boolean)) => {
                Connective::Xor.of(vec![lhs, rhs]).negated()
            }
            _ => Expr::Binary(
                BinaryOp::Compare(comparison, compared),
                Box::new(lhs),
                Box::new(rhs),
            ),
        }
    }

    /// The conjuncts of the expression, a boolean: the operands of an
    /// `and`, or else the expression itself.
    #[must_use]
    pub fn conjuncts(&self) -> &[Expr] {
        match self {
            Expr::Connective(Connective::And, operands) => operands,
            _ => std::slice::from_ref(self),
        }
    }

    /// The expression with each expression directly in it replaced by what
    /// `f` makes of it, given how many tuples at hand further in that one
    /// is: one for a `Restrict`'s condition and an `Extend`'s values, whose
    /// tuple at hand is `Current(0, …)` in them, and none for the others.
    #[must_use]
    pub fn map_children(self, mut f: impl FnMut(Expr, usize) -> Expr) -> Expr {
        let all = |exprs: Vec<Expr>, f: &mut dyn FnMut(Expr, usize) -> Expr| {
            exprs.into_iter().map(|e| f(e, 0)).collect()
        };
        match self {
            Expr::Constant(_)
            | Expr::Variable(_)
            | Expr::Parameter(_)
            | Expr::Local(_)
            | Expr::Current(..) => self,
            Expr::Let(local, value, body) => {
                Expr::Let(local, Box::new(f(*value, 0)), Box::new(f(*body, 0)))
            }
            Expr::At(site, expr) => Expr::At(site, Box::new(f(*expr, 0))),
            Expr::Unary(op, operand) => Expr::Unary(op, Box::new(f(*operand, 0))),
            Expr::Binary(op, lhs, rhs) => {
                Expr::Binary(op, Box::new(f(*lhs, 0)), Box::new(f(*rhs, 0)))
            }
            Expr::Connective(connective, operands) => {
                Expr::Connective(connective, all(operands, &mut f))
            }
            Expr::Extract(tuple, name) => Expr::Extract(Box::new(f(*tuple, 0)), name),
            Expr::Tuple(attributes) => Expr::Tuple(
                attributes
                    .into_iter()
                    .map(|(name, e)| (name, f(e, 0)))
                    .collect(),
            ),
            Expr::Relation(heading, tuples) => Expr::Relation(heading, all(tuples, &mut f)),
            Expr::Remove(operand, names) => Expr::Remove(Box::new(f(*operand, 0)), names),
            Expr::Rename(operand, renamings) => Expr::Rename(Box::new(f(*operand, 0)), renamings),
            Expr::And(lhs, rhs) => Expr::And(Box::new(f(*lhs, 0)), Box::new(f(*rhs, 0))),
            Expr::Combine(combination, lhs, rhs) => {
                Expr::Combine(combination, Box::new(f(*lhs, 0)), Box::new(f(*rhs, 0)))
            }
            Expr::Semijoin(lhs, rhs) => Expr::Semijoin(Box::new(f(*lhs, 0)), Box::new(f(*rhs, 0))),
            Expr::Antijoin(lhs, rhs) => Expr::Antijoin(Box::new(f(*lhs, 0)), Box::new(f(*rhs, 0))),
            Expr::Extend(operand, computed, attributes) => Expr::Extend(
                Box::new(f(*operand, 0)),
                computed,
                attributes
                    .into_iter()
                    .map(|(name, e)| (name, f(e, 1)))
                    .collect(),
            ),
            Expr::Ungroup(operand, names) => Expr::Ungroup(Box::new(f(*operand, 0)), names),
            Expr::Restrict(operand, condition) => {
                Expr::Restrict(Box::new(f(*operand, 0)), Box::new(f(*condition, 1)))
            }
            Expr::Array(element, elements) => Expr::Array(element, all(elements, &mut f)),
            Expr::Index(array, position) => {
                Expr::Index(Box::new(f(*array, 0)), Box::new(f(*position, 0)))
            }
            Expr::WithElements(array, changes) => Expr::WithElements(
                Box::new(f(*array, 0)),
                changes
                    .into_iter()
                    .map(|change| ElementChange {
                        path: all(change.path, &mut f),
                        value: f(change.value, 0),
                    })
                    .collect(),
            ),
            Expr::If(condition, then, otherwise) => Expr::If(
                Box::new(f(*condition, 0)),
                Box::new(f(*then, 0)),
                Box::new(f(*otherwise, 0)),
            ),
            Expr::Load(relation, loading) => Expr::Load(Box::new(f(*relation, 0)), loading),
            Expr::Unload(array, unloading) => Expr::Unload(Box::new(f(*array, 0)), unloading),
            Expr::Only(operand) => Expr::Only(Box::new(f(*operand, 0))),
            Expr::Select(defined, components) => Expr::Select(defined, Box::new(f(*components, 0))),
            Expr::Components(value) => Expr::Components(Box::new(f(*value, 0))),
            Expr::Treat(ty, value) => Expr::Treat(ty, Box::new(f(*value, 0))),
            Expr::TreatAs(ty, value) => Expr::TreatAs(ty, Box::new(f(*value, 0))),
            Expr::Is(value, ty) => Expr::Is(Box::new(f(*value, 0)), ty),
            Expr::Fail(error, ty) => Expr::Fail(Box::new(f(*error, 0)), ty),
            Expr::Call(call) => Expr::Call(Call {
                args: call
                    .args
                    .into_iter()
                    .map(|(name, e)| (name, f(e, 0)))
                    .collect(),
                ..call
            }),
        }
    }

    /// `f` of each expression directly in this one, given how many tuples
    /// at hand further in it is, as for [`Expr::map_children`].
    pub fn for_each_child(&self, mut f: impl FnMut(&Expr, usize)) {
        match self {
            Expr::Constant(_)
            | Expr::Variable(_)
            | Expr::Parameter(_)
            | Expr::Local(_)
            | Expr::Current(..) => {}
            Expr::Unary(_, operand)
            | Expr::At(_, operand)
            | Expr::Extract(operand, _)
            | Expr::Remove(operand, _)
            | Expr::Rename(operand, _)
            | Expr::Ungroup(operand, _)
            | Expr::Only(operand)
            | Expr::Select(_, operand)
            | Expr::Components(operand)
            | Expr::Treat(_, operand)
            | Expr::TreatAs(_, operand)
            | Expr::Is(operand, _)
            | Expr::Fail(operand, _)
            | Expr::Load(operand, ..)
            | Expr::Unload(operand, ..) => f(operand, 0),
            Expr::Let(_, lhs, rhs)
            | Expr::Binary(_, lhs, rhs)
            | Expr::And(lhs, rhs)
            | Expr::Combine(_, lhs, rhs)
            | Expr::Semijoin(lhs, rhs)
            | Expr::Antijoin(lhs, rhs)
            | Expr::Index(lhs, rhs) => {
                f(lhs, 0);
                f(rhs, 0);
            }
            Expr::Connective(_, exprs) | Expr::Relation(_, exprs) | Expr::Array(_, exprs) => {
                for e in exprs {
                    f(e, 0);
                }
            }
            Expr::Tuple(attributes) => {
                for e in attributes.values() {
                    f(e, 0);
                }
            }
            Expr::Extend(operand, _, attributes) => {
                f(operand, 0);
                for e in attributes.values() {
                    f(e, 1);
                }
            }
            Expr::Restrict(operand, condition) => {
                f(operand, 0);
                f(condition, 1);
            }
            Expr::WithElements(array, changes) => {
                f(array, 0);
                for change in changes {
                    for position in &change.path {
                        f(position, 0);
                    }
                    f(&change.value, 0);
                }
            }
            Expr::If(condition, then, otherwise) => {
                f(condition, 0);
                f(then, 0);
                f(otherwise, 0);
            }
            Expr::Call(call) => {
                for e in call.args.values() {
                    f(e, 0);
                }
            }
        }
    }

    /// What the expression reads of the tuple at hand `depth` of them out
    /// from it (see [`Expr::Current`]).
    #[must_use]
    pub fn tuple_read(&self, depth: usize) -> TupleRead {
        fn walk(expr: &Expr, depth: usize, read: &mut TupleRead) {
            match expr {
                Expr::Extract(tuple, name) if matches!(**tuple, Expr::Current(d, _) if d == depth) =>
                {
                    read.attributes.insert(name.clone());
                }
                Expr::Current(d, _) => read.whole |= *d == depth,
                _ => expr.for_each_child(|child, deeper| walk(child, depth + deeper, read)),
            }
        }
        let mut read = TupleRead::default();
        walk(self, depth, &mut read);
        read
    }

    /// The expression, in an operator instantiated with `types`: each type
    /// parameter in it replaced by its type, by index.
    #[must_use]
    pub fn instantiated(&self, types: &[Type]) -> Expr {
        let inst = |e: &Expr| Box::new(e.instantiated(types));
        let all = |attributes: &BTreeMap<String, Expr>| {
            attributes
                .iter()
                .map(|(name, e)| (name.clone(), e.instantiated(types)))
                .collect()
        };
        match self {
            Expr::Constant(value) => Expr::Constant(value.instantiated(types)),
            Expr::Variable(_) | Expr::Parameter(_) | Expr::Local(_) => self.clone(),
            Expr::Let(local, value, body) => Expr::Let(*local, inst(value), inst(body)),
            Expr::At(site, expr) => Expr::At(*site, inst(expr)),
            Expr::Array(element, elements) => Expr::Array(
                element.instantiated(types),
                elements.iter().map(|e| e.instantiated(types)).collect(),
            ),
            Expr::Index(array, index) => Expr::Index(inst(array), inst(index)),
            Expr::WithElements(array, changes) => Expr::WithElements(
                inst(array),
                changes.iter().map(|c| c.instantiated(types)).collect(),
            ),
            Expr::If(condition, then, otherwise) => {
                Expr::If(inst(condition), inst(then), inst(otherwise))
            }
            // A load names only attributes, which no type parameter changes.
            Expr::Load(relation, loading) => Expr::Load(inst(relation), loading.clone()),
            Expr::Unload(array, unloading) => Expr::Unload(inst(array), unloading.clone()),
            // A type parameter's comparisons and `not`s of them are kept in
            // normal form when it becomes a boolean, or a scalar.
            Expr::Unary(UnaryOp::Not, operand) => operand.instantiated(types).negated(),
            Expr::Unary(op, operand) => Expr::Unary(op.instantiated(types), inst(operand)),
            Expr::Binary(op, lhs, rhs) => match op.instantiated(types) {
                BinaryOp::Compare(comparison, compared) => Expr::compared(
                    comparison,
                    compared,
                    lhs.instantiated(types),
                    rhs.instantiated(types),
                ),
                op => Expr::Binary(op, inst(lhs), inst(rhs)),
            },
            Expr::Connective(connective, operands) => {
                connective.of(operands.iter().map(|e| e.instantiated(types)).collect())
            }
            Expr::Extract(tuple, name) => Expr::Extract(inst(tuple), name.clone()),
            Expr::Tuple(attributes) => Expr::Tuple(all(attributes)),
            Expr::Relation(heading, tuples) => Expr::Relation(
                heading.instantiated(types),
                tuples.iter().map(|t| t.instantiated(types)).collect(),
            ),
            Expr::Remove(operand, names) => Expr::Remove(inst(operand), names.clone()),
            Expr::Rename(operand, renamings) => Expr::Rename(inst(operand), renamings.clone()),
            Expr::And(lhs, rhs) => Expr::And(inst(lhs), inst(rhs)),
            Expr::Combine(combination, lhs, rhs) => {
                Expr::Combine(*combination, inst(lhs), inst(rhs))
            }
            Expr::Semijoin(lhs, rhs) => Expr::Semijoin(inst(lhs), inst(rhs)),
            Expr::Antijoin(lhs, rhs) => Expr::Antijoin(inst(lhs), inst(rhs)),
            Expr::Extend(operand, computed, attributes) => {
                Expr::Extend(inst(operand), computed.instantiated(types), all(attributes))
            }
            Expr::Ungroup(operand, names) => Expr::Ungroup(inst(operand), names.clone()),
            Expr::Restrict(operand, condition) => Expr::Restrict(inst(operand), inst(condition)),
            Expr::Only(operand) => Expr::Only(inst(operand)),
            // A type of the program's own has no type parameters in it.
            Expr::Select(defined, components) => Expr::Select(defined.clone(), inst(components)),
            Expr::Components(value) => Expr::Components(inst(value)),
            Expr::Treat(ty, value) => Expr::Treat(Box::new(ty.instantiated(types)), inst(value)),
            Expr::TreatAs(ty, value) => {
                Expr::TreatAs(Box::new(ty.instantiated(types)), inst(value))
            }
            Expr::Is(value, ty) => Expr::Is(inst(value), Box::new(ty.instantiated(types))),
            Expr::Fail(error, ty) => Expr::Fail(inst(error), Box::new(ty.instantiated(types))),
            Expr::Current(depth, heading) => Expr::Current(*depth, heading.instantiated(types)),
            Expr::Call(call) => Expr::Call(call.instantiated(types)),
        }
    }
}

/// What an expression reads of a tuple at hand ([`Expr::tuple_read`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TupleRead {
    /// The attributes it reads, each by name.
    pub attributes: BTreeSet<String>,
    /// Whether it reads the tuple other than an attribute at a time, as an
    /// image relation does.
    pub whole: bool,
}

/// Why an operator has no result: it is out of its type's range (for an
/// approximate number, beyond the largest finite magnitude), or it divides by
/// zero.
/// Approximate numbers are therefore always finite: no operator produces an
/// infinity or a NaN.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Failure {
    /// The result is out of the range of this type.
    Overflow(Scalar),
    DivisionByZero,
    /// The square root of this negative number, which has none, as
    /// approximate numbers have no NaN.
    NegativeRoot(Approximate),
    /// An array has no element at the position `index`, which, counting
    /// from 1, is below 1 or beyond its `length`, if that is known.
    OutOfRange {
        index: i64,
        length: Option<usize>,
    },
}

/// Whether relations `lhs` and `rhs` compare true by `comparison`, an
/// ordering one, as sets: `<=` is subset, `<` proper subset, and `>=` and
/// `>` the same the other way around.
///
/// # Panics
///
/// If either value isn't a relation, or the comparison is `=` or `<>`.
fn contains(comparison: Comparison, lhs: &Value, rhs: &Value) -> bool {
    let (Value::Relation(lhs), Value::Relation(rhs)) = (lhs, rhs) else {
        panic!("comparing {lhs:?} and {rhs:?} as sets");
    };
    let (small, large, proper) = match comparison {
        Comparison::LessOrEqual => (lhs, rhs, false),
        Comparison::Less => (lhs, rhs, true),
        Comparison::GreaterOrEqual => (rhs, lhs, false),
        Comparison::Greater => (rhs, lhs, true),
        Comparison::Equal | Comparison::NotEqual => panic!("equality isn't containment"),
    };
    small.tuples.is_subset(&large.tuples) && (!proper || small.tuples.len() < large.tuples.len())
}

/// How relations of the same attributes are combined, by
/// [`Expr::Combine`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Combination {
    /// The tuples of either (`union`, Algebra A's OR).
    Union,
    /// The tuples of either, which must have none in common (`d_union`).
    DisjointUnion,
    /// The tuples of exactly one (`xunion`, the symmetric difference, as
    /// `xor` is of booleans).
    ExclusiveUnion,
    /// The tuples of the left that aren't in the right (`minus`, Algebra
    /// A's AND with the NOT of the right).
    Minus,
    /// The tuples of the left that aren't in the right, all of whose tuples
    /// must be in the left (`i_minus`).
    IncludedMinus,
    /// The tuples of either, which must have none in common: the tuples
    /// that changes to one variable in an `assign` block remove, which
    /// conflict if two remove the same tuple.
    DisjointChanges,
}

impl Combination {
    /// The heading of relations of headings `lhs` and `rhs` combined (*Type
    /// Inheritance and Relational Theory*, IM Prescription 13): of a union,
    /// each attribute of the most specific common supertype of its types,
    /// since it has the values of both, and of a minus, the left's, since
    /// its tuples are the left's.
    ///
    /// # Panics
    ///
    /// If a union's operands' attributes have no common supertype, which
    /// sema rules out.
    #[must_use]
    pub fn heading(self, lhs: &Heading, rhs: &Heading) -> Heading {
        match self {
            Combination::Union
            | Combination::DisjointUnion
            | Combination::ExclusiveUnion
            | Combination::DisjointChanges => lhs
                .common_supertype(rhs)
                .unwrap_or_else(|| panic!("a union of {lhs:?} and {rhs:?}")),
            Combination::Minus | Combination::IncludedMinus => lhs.clone(),
        }
    }

    /// The type of relations of types `lhs` and `rhs` combined, as
    /// [`Combination::heading`] says.
    ///
    /// # Panics
    ///
    /// If either type isn't a relation type, which sema rules out.
    #[must_use]
    pub fn combined(self, lhs: &Type, rhs: &Type) -> Type {
        match (lhs, rhs) {
            (Type::Relation(lhs), Type::Relation(rhs)) => Type::Relation(self.heading(lhs, rhs)),
            _ => panic!("combining {lhs} and {rhs}"),
        }
    }
}

/// A tuple that breaks a checked [`Combination`]'s condition: one both
/// operands of a disjoint union, or of disjoint changes, have, or one of
/// the right operand of an included minus that the left lacks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Broken {
    pub combination: Combination,
    pub tuple: TupleValue,
}

/// Why tuples have no join: they disagree on `attribute`, whose value is
/// `lhs` in one and `rhs` in the other.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mismatch {
    pub attribute: String,
    pub lhs: Box<Value>,
    pub rhs: Box<Value>,
}

/// The type an arithmetic operator works on: both operands and the result
/// have it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Numeric {
    Integer,
    Approximate,
    /// Values of the type parameter with this index, of a kind that requires
    /// the operator, as values of the type a call gives it.
    Parameter(usize),
}

impl Numeric {
    /// The scalar type, or `None` for a type parameter's, which each call
    /// gives.
    #[must_use]
    pub fn scalar(self) -> Option<Scalar> {
        match self {
            Numeric::Integer => Some(Scalar::Integer),
            Numeric::Approximate => Some(Scalar::Approximate),
            Numeric::Parameter(_) => None,
        }
    }

    /// The numeric type, in an operator instantiated with `types`.
    ///
    /// # Panics
    ///
    /// If a type parameter's type isn't numeric, which only a kind's types
    /// can be, and the prelude checks that they are.
    #[must_use]
    pub fn instantiated(self, types: &[Type]) -> Numeric {
        match self {
            Numeric::Parameter(index) => match types[index] {
                Type::Scalar(scalar) => Numeric::of(scalar),
                _ => None,
            }
            .unwrap_or_else(|| panic!("arithmetic on {}", types[index])),
            _ => self,
        }
    }

    /// The numeric type `scalar` is, if it is one.
    #[must_use]
    pub fn of(scalar: Scalar) -> Option<Numeric> {
        match scalar {
            Scalar::Integer => Some(Numeric::Integer),
            Scalar::Approximate => Some(Numeric::Approximate),
            Scalar::Boolean | Scalar::Text => None,
        }
    }
}

/// A finite approximate result, or why there is none.
fn finite(r: f64) -> Result<Value, Failure> {
    if r.is_finite() {
        Ok(Value::Approximate(Approximate::new(r)))
    } else {
        Err(Failure::Overflow(Scalar::Approximate))
    }
}

/// A monadic operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnaryOp {
    /// `not`, of a boolean.
    Not,
    /// `-`. It never fails for an approximate number, and only for the
    /// smallest integer.
    Negate(Numeric),
}

impl UnaryOp {
    /// The scalar type of the operand, and of the result: `None` for a type
    /// parameter's values.
    #[must_use]
    pub fn operand(self) -> Option<Scalar> {
        match self {
            UnaryOp::Not => Some(Scalar::Boolean),
            UnaryOp::Negate(numeric) => numeric.scalar(),
        }
    }

    /// The operator, in an operator instantiated with `types`.
    #[must_use]
    pub fn instantiated(self, types: &[Type]) -> UnaryOp {
        match self {
            UnaryOp::Negate(numeric) => UnaryOp::Negate(numeric.instantiated(types)),
            UnaryOp::Not => self,
        }
    }

    /// The result for `operand`, which has the operand type.
    ///
    /// # Errors
    ///
    /// Returns why there is no result: negating the smallest integer
    /// overflows.
    ///
    /// # Panics
    ///
    /// If `operand` isn't of the operand type, which sema rules out.
    pub fn apply(self, operand: &Value) -> Result<Value, Failure> {
        match (self, operand) {
            (UnaryOp::Not, Value::Boolean(b)) => Ok(Value::Boolean(!b)),
            (UnaryOp::Negate(Numeric::Integer), Value::Integer(n)) => n
                .checked_neg()
                .map(Value::Integer)
                .ok_or(Failure::Overflow(Scalar::Integer)),
            (UnaryOp::Negate(Numeric::Approximate), Value::Approximate(r)) => {
                Ok(Value::Approximate(Approximate::new(-r.get())))
            }
            _ => panic!("`{self:?}` of {operand:?}"),
        }
    }
}

/// A boolean connective, of any number of operands (Tutorial D's n-adic
/// `AND`, `OR` and `XOR`), whose order doesn't matter. `a implies b` is
/// `not a or b`; and of booleans, `a <> b` is `a xor b`, and `a = b` is
/// `not (a xor b)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Connective {
    /// True when every operand is. Those that can't fail (see
    /// [`Expr::can_fail`]) are computed first, and if one is false, the
    /// result is false and the others aren't computed; otherwise every one
    /// that can fail is. So a condition that can't fail guards the others,
    /// wherever it is written: `x div y > 1 and y <> 0` never divides by
    /// zero. Which runs first is then for an optimizer to choose.
    And,
    /// True when an operand is, its operands computed as `And`'s are, but
    /// for true: one that can't fail and is true decides the result.
    Or,
    /// True when an odd number of operands are, each counted as often as
    /// it is written, so that it is a chain of the dyadic `xor` (TTM,
    /// chapter 5, exercise 5.6). No operand decides it, so every one is
    /// computed.
    Xor,
}

impl Connective {
    /// The result when an operand is `value`, if that decides it whatever
    /// the others are: false for `and`, and true for `or`.
    #[must_use]
    pub fn decided(self, value: bool) -> Option<bool> {
        match self {
            Connective::And => (!value).then_some(false),
            Connective::Or => value.then_some(true),
            Connective::Xor => None,
        }
    }

    /// The result of none: true for `and`, false for `or` and `xor`. An
    /// operand with this value leaves the result as the others make it.
    #[must_use]
    pub fn identity(self) -> bool {
        self == Connective::And
    }

    /// The connective of `operands` with those that can fail computed only
    /// when those that can't don't decide its result, as an `If`, if it is
    /// an `and` or an `or` with operands of both kinds (see
    /// [`Connective::And`]).
    #[must_use]
    pub fn guarded(self, operands: &[Expr]) -> Option<Expr> {
        let (failing, guards): (Vec<Expr>, Vec<Expr>) =
            operands.iter().cloned().partition(Expr::can_fail);
        // No operand decides an `xor`, so none guards another.
        if self == Connective::Xor || failing.is_empty() || guards.is_empty() {
            return None;
        }
        let (guards, failing) = (self.of(guards), self.of(failing));
        let decided = Box::new(Expr::Constant(Value::Boolean(!self.identity())));
        Some(if self == Connective::And {
            Expr::If(Box::new(guards), Box::new(failing), decided)
        } else {
            Expr::If(Box::new(guards), decided, Box::new(failing))
        })
    }

    /// `operands`, booleans, combined by the connective, in normal form
    /// (see [`Expr::Connective`]): the operands of one of the same
    /// connective are its own, as a chain of them is one however it is
    /// grouped. A constant that decides an `and` or an `or` is the result,
    /// as it can't fail, and so guards the others; one that doesn't is
    /// dropped. Of an `and` or an `or`, an operand written twice is kept
    /// once. Of an `xor`, two of one that can't fail cancel, and a true
    /// one, or a `not` on one, negates the rest, so that `a xor not b` is
    /// `not (a xor b)`, and `(a = b) xor c` is `not (a xor b xor c)`. Order
    /// doesn't matter, but the operands stay in it.
    /// The connective of one operand is that operand, and of none, its
    /// identity.
    #[must_use]
    pub fn of(self, operands: Vec<Expr>) -> Expr {
        let mut flat = Vec::new();
        // Whether the result is the negation of the operands', as of an
        // `xor` of an odd number of trues.
        let mut negated = false;
        for operand in operands {
            let operand = match operand {
                // A `not` comes out of an operand of an `xor`, to stop at it,
                Expr::Unary(UnaryOp::Not, inner) if self == Connective::Xor => {
                    negated = !negated;
                    *inner
                }
                // as it does out of `a = b`, which is `not (a xor b)`.
                Expr::Binary(
                    BinaryOp::Compare(Comparison::Equal, Compared::Scalar(Scalar::Boolean)),
                    lhs,
                    rhs,
                ) if self == Connective::Xor => {
                    negated = !negated;
                    Expr::Connective(Connective::Xor, vec![*lhs, *rhs])
                }
                operand => operand,
            };
            match operand {
                Expr::Connective(c, inner) if c == self => flat.extend(inner),
                operand => flat.push(operand),
            }
        }
        let mut kept: Vec<Expr> = Vec::new();
        for operand in flat {
            match operand {
                Expr::Constant(Value::Boolean(value)) => {
                    if let Some(result) = self.decided(value) {
                        return Expr::Constant(Value::Boolean(result));
                    }
                    negated ^= value != self.identity();
                }
                operand if self != Connective::Xor => {
                    if !kept.contains(&operand) {
                        kept.push(operand);
                    }
                }
                operand => match kept.iter().position(|k| *k == operand) {
                    Some(i) if !operand.can_fail() => {
                        kept.remove(i);
                    }
                    _ => kept.push(operand),
                },
            }
        }
        let result = if kept.len() > 1 {
            Expr::Connective(self, kept)
        } else {
            kept.pop()
                .unwrap_or(Expr::Constant(Value::Boolean(self.identity())))
        };
        if negated { result.negated() } else { result }
    }
}

/// A dyadic operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BinaryOp {
    Add(Numeric),
    Subtract(Numeric),
    Multiply(Numeric),
    /// `/`, of approximate numbers.
    Divide,
    /// Integer division, truncating toward zero.
    Div,
    /// The remainder of [`BinaryOp::Div`], with the sign of the dividend:
    /// `a = (a div b) * b + a mod b`.
    Mod,
    /// `||`, of texts.
    Concatenate,
    /// A comparison of two values of the same type, whose result is a
    /// boolean.
    Compare(Comparison, Compared),
}

/// What a comparison compares. Scalars are ordered; tuples only have `=`
/// and `<>`; and relations have those, and `<=`, `<`, `>=` and `>` as
/// subset, proper subset, superset and proper superset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Compared {
    Scalar(Scalar),
    /// Tuples of one tuple type, equal when every attribute is.
    Tuple,
    /// Values of one of the program's own types, equal when their
    /// components are, and ordered by the type's order, if it has one.
    Defined,
    /// Relations of one relation type, equal when they have the same tuples,
    /// and one less than or equal to another when it is a subset of it.
    Relation,
    /// Arrays of one array type, equal when they have the same elements in
    /// the same order: only `=` and `<>`.
    Array,
    /// Values of the type parameter with this index, compared as values of
    /// the type a call gives it: only `=` and `<>`, but for `any relation`,
    /// which is a relation, and a kind that requires an order.
    Parameter(usize),
}

impl Compared {
    /// What values of type `ty` are, for comparing them.
    #[must_use]
    pub fn of(ty: &Type) -> Compared {
        match ty {
            Type::Scalar(scalar) => Compared::Scalar(*scalar),
            Type::Defined(_) | Type::Dummy(_) => Compared::Defined,
            Type::Tuple(_) => Compared::Tuple,
            Type::Relation(_) => Compared::Relation,
            Type::Array(_) => Compared::Array,
            Type::Parameter(p) => Compared::Parameter(p.index),
        }
    }

    /// What it compares, in an operator instantiated with `types`.
    #[must_use]
    pub fn instantiated(self, types: &[Type]) -> Compared {
        match self {
            Compared::Parameter(index) => Compared::of(&types[index]),
            _ => self,
        }
    }
}

/// A comparison operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Comparison {
    Equal,
    NotEqual,
    Less,
    LessOrEqual,
    Greater,
    GreaterOrEqual,
}

impl Comparison {
    /// The comparison true exactly when this one is false, of values of a
    /// type whose values are all ordered, as scalars are. Of two
    /// relations, ordered by inclusion, neither may include the other, so
    /// only `=` and `<>` negate each other there.
    #[must_use]
    pub fn negated(self) -> Comparison {
        match self {
            Comparison::Equal => Comparison::NotEqual,
            Comparison::NotEqual => Comparison::Equal,
            Comparison::Less => Comparison::GreaterOrEqual,
            Comparison::LessOrEqual => Comparison::Greater,
            Comparison::Greater => Comparison::LessOrEqual,
            Comparison::GreaterOrEqual => Comparison::Less,
        }
    }

    /// Whether two values in the order `ordering` compare true.
    #[must_use]
    pub fn holds(self, ordering: std::cmp::Ordering) -> bool {
        use std::cmp::Ordering::{Equal, Greater, Less};
        match self {
            Comparison::Equal => ordering == Equal,
            Comparison::NotEqual => ordering != Equal,
            Comparison::Less => ordering == Less,
            Comparison::LessOrEqual => ordering != Greater,
            Comparison::Greater => ordering == Greater,
            Comparison::GreaterOrEqual => ordering != Less,
        }
    }
}

/// Values of types of the program's own whose order compares the
/// components of another possible representation than the stored one, each
/// with those components, which its conversion computes. Only sema can
/// call a conversion, so it converts the values a comparison it folds
/// compares, and [`compare`] looks them up here.
#[derive(Debug, Clone, Default)]
pub struct Converted(BTreeMap<DefinedValue, TupleValue>);

impl Converted {
    /// Whether `value` is converted already.
    #[must_use]
    pub fn contains(&self, value: &DefinedValue) -> bool {
        self.0.contains_key(value)
    }

    /// `value`, with its `components` in the representation its type's
    /// order compares.
    pub fn insert(&mut self, value: DefinedValue, components: TupleValue) {
        self.0.insert(value, components);
    }

    /// The components of `value` its type's order compares.
    fn compared<'v>(&'v self, value: &'v DefinedValue, order: &Order) -> &'v TupleValue {
        match order.from {
            None => &value.components,
            Some(_) => self
                .0
                .get(value)
                .expect("sema converts each value an order compares in another representation"),
        }
    }
}

/// The order of two values of the same ordered type: `false` before
/// `true`, integers and approximate numbers numerically, text by code
/// point, which is the order of its UTF-8 bytes, and values of a type of
/// the program's own by its order, the components of its representation,
/// in `converted` if it isn't the stored one, compared in turn until one
/// differs, a descending one the other way.
///
/// # Panics
///
/// If the values aren't of one ordered type, which sema rules out.
#[must_use]
pub fn compare(lhs: &Value, rhs: &Value, converted: &Converted) -> std::cmp::Ordering {
    match (lhs, rhs) {
        (Value::Boolean(a), Value::Boolean(b)) => a.cmp(b),
        (Value::Integer(a), Value::Integer(b)) => a.cmp(b),
        (Value::Approximate(a), Value::Approximate(b)) => a.cmp(b),
        (Value::Text(a), Value::Text(b)) => a.as_bytes().cmp(b.as_bytes()),
        (Value::Defined(a), Value::Defined(b)) => {
            let order =
                a.ty.order
                    .as_ref()
                    .expect("only ordered values are compared");
            let (a, b) = (converted.compared(a, order), converted.compared(b, order));
            order
                .items
                .iter()
                .map(|item| {
                    let component = |components: &TupleValue| {
                        components
                            .get(&item.attribute)
                            .expect("an order names its representation's components")
                            .clone()
                    };
                    let ordering = compare(&component(a), &component(b), converted);
                    if item.descending {
                        ordering.reverse()
                    } else {
                        ordering
                    }
                })
                .find(|ordering| ordering.is_ne())
                .unwrap_or(std::cmp::Ordering::Equal)
        }
        _ => panic!("comparing {lhs:?} with {rhs:?}"),
    }
}

impl BinaryOp {
    /// The scalar type of both operands: `None` for the program's own
    /// types, tuples, relations, arrays and a type parameter's values.
    #[must_use]
    pub fn operand(self) -> Option<Scalar> {
        match self {
            BinaryOp::Add(numeric) | BinaryOp::Subtract(numeric) | BinaryOp::Multiply(numeric) => {
                numeric.scalar()
            }
            BinaryOp::Divide => Some(Scalar::Approximate),
            BinaryOp::Div | BinaryOp::Mod => Some(Scalar::Integer),
            BinaryOp::Concatenate => Some(Scalar::Text),
            BinaryOp::Compare(_, Compared::Scalar(scalar)) => Some(scalar),
            BinaryOp::Compare(
                _,
                Compared::Defined
                | Compared::Tuple
                | Compared::Relation
                | Compared::Array
                | Compared::Parameter(_),
            ) => None,
        }
    }

    /// The scalar type of the result: `None` for arithmetic on a type
    /// parameter's values, which has their type.
    #[must_use]
    pub fn result(self) -> Option<Scalar> {
        match self {
            BinaryOp::Compare(..) => Some(Scalar::Boolean),
            _ => self.operand(),
        }
    }

    /// The operator, in an operator instantiated with `types`.
    #[must_use]
    pub fn instantiated(self, types: &[Type]) -> BinaryOp {
        match self {
            BinaryOp::Add(numeric) => BinaryOp::Add(numeric.instantiated(types)),
            BinaryOp::Subtract(numeric) => BinaryOp::Subtract(numeric.instantiated(types)),
            BinaryOp::Multiply(numeric) => BinaryOp::Multiply(numeric.instantiated(types)),
            BinaryOp::Compare(comparison, compared) => {
                BinaryOp::Compare(comparison, compared.instantiated(types))
            }
            _ => self,
        }
    }

    /// The result for `lhs` and `rhs`, which have the operand type, values
    /// of types of the program's own compared with the components in
    /// `converted` (see [`compare`]). Approximate results are rounded as
    /// IEEE 754 binary64 arithmetic rounds them, to nearest, ties to even.
    ///
    /// # Errors
    ///
    /// Returns why there is no result: a result out of range, or a division
    /// by zero.
    ///
    /// # Panics
    ///
    /// If an operand isn't of the operand type, which sema rules out.
    pub fn apply(self, lhs: &Value, rhs: &Value, converted: &Converted) -> Result<Value, Failure> {
        let ordered = || compare(lhs, rhs, converted);
        match self {
            BinaryOp::Compare(comparison, Compared::Scalar(_)) => {
                return Ok(Value::Boolean(comparison.holds(ordered())));
            }
            // Value equality is the language's: attribute-wise for tuples,
            // set equality for relations.
            BinaryOp::Compare(Comparison::Equal, _) => return Ok(Value::Boolean(lhs == rhs)),
            BinaryOp::Compare(Comparison::NotEqual, _) => return Ok(Value::Boolean(lhs != rhs)),
            BinaryOp::Compare(comparison, Compared::Relation) => {
                return Ok(Value::Boolean(contains(comparison, lhs, rhs)));
            }
            // By the type's order.
            BinaryOp::Compare(comparison, Compared::Defined) => {
                return Ok(Value::Boolean(comparison.holds(ordered())));
            }
            // Values of a type parameter's type, as the values are: of
            // `any relation`, ordered by inclusion, or of a kind that
            // requires an order.
            BinaryOp::Compare(comparison, Compared::Parameter(_)) => {
                let holds = match lhs {
                    Value::Relation(_) => contains(comparison, lhs, rhs),
                    _ => comparison.holds(ordered()),
                };
                return Ok(Value::Boolean(holds));
            }
            BinaryOp::Compare(..) => panic!("tuples and arrays aren't ordered"),
            _ => {}
        }
        match (lhs, rhs) {
            (&Value::Integer(a), &Value::Integer(b)) => {
                let result = match self {
                    BinaryOp::Add(_) => a.checked_add(b),
                    BinaryOp::Subtract(_) => a.checked_sub(b),
                    BinaryOp::Multiply(_) => a.checked_mul(b),
                    BinaryOp::Div | BinaryOp::Mod if b == 0 => {
                        return Err(Failure::DivisionByZero);
                    }
                    BinaryOp::Div => a.checked_div(b),
                    // The smallest integer `mod -1` is 0, though the matching
                    // `div` overflows.
                    BinaryOp::Mod => Some(a.wrapping_rem(b)),
                    _ => panic!("`{self:?}` of integers"),
                };
                result
                    .map(Value::Integer)
                    .ok_or(Failure::Overflow(Scalar::Integer))
            }
            (Value::Text(a), Value::Text(b)) if self == BinaryOp::Concatenate => {
                Ok(Value::Text(format!("{a}{b}")))
            }
            (Value::Approximate(a), Value::Approximate(b)) => {
                let (a, b) = (a.get(), b.get());
                match self {
                    BinaryOp::Add(_) => finite(a + b),
                    BinaryOp::Subtract(_) => finite(a - b),
                    BinaryOp::Multiply(_) => finite(a * b),
                    // Either zero.
                    BinaryOp::Divide if b == 0.0 => Err(Failure::DivisionByZero),
                    BinaryOp::Divide => finite(a / b),
                    _ => panic!("`{self:?}` of approximate numbers"),
                }
            }
            _ => panic!("`{self:?}` of {lhs:?} and {rhs:?}"),
        }
    }
}

/// A variable defined at the top level of the program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Variable {
    /// Unique among variables and operators.
    pub name: String,
    pub ty: Type,
    /// Always explicit: sema supplies the type's default when the source
    /// gives no value.
    pub init: Value,
    /// A relation variable's keys (TTM RM Prescription 15), each a set of
    /// attribute names, which `init` satisfies: no two tuples have the same
    /// values for a key's attributes, and no key contains another. There is
    /// always at least one: the whole heading when the source declares
    /// none. Variables of other types have none.
    pub keys: Vec<Key>,
}

/// A key: a set of a relation's attributes, by name.
pub type Key = BTreeSet<String>;

/// A type: a built-in scalar type, one of the program's own, a tuple type, a
/// relation type, an array type, or a type parameter of the operator it is
/// written in.
///
/// Types are structural: two tuple types, or two relation types, are the
/// same type exactly when their headings are equal, wherever they are
/// written. A program's own types are scalar, so their names tell them
/// apart. A type parameter is a type of its own inside its operator, the
/// same type only as itself, and outside it means nothing: a call gives it
/// a type, which [`Type::instantiated`] puts in its place.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Type {
    Scalar(Scalar),
    Defined(Box<Defined>),
    /// A union type of the program's own with no possible representation,
    /// a dummy type, whose values are its subtypes'.
    Dummy(Box<Dummy>),
    Tuple(Heading),
    Relation(Heading),
    /// Arrays of elements of this type (TTM's ARRAY type generator, chapter
    /// 3), of any length.
    Array(Box<Type>),
    Parameter(TypeParameter),
}

impl Type {
    /// The values a value of the type is made of, laid out in order: a
    /// tuple's attributes, or the stored components of a value of a type of
    /// the program's own.
    #[must_use]
    pub fn fields(&self) -> Option<&Heading> {
        match self {
            Type::Tuple(heading) => Some(heading),
            Type::Defined(defined) => Some(&defined.components),
            _ => None,
        }
    }

    /// The type at the top of the hierarchy the type is in, by name, if it
    /// is in one: a union type, or a regular type a type is defined as a
    /// subtype of by a constraint; itself, if it is a subtype of none.
    /// Every value of every type in it is laid out alike, as one of the
    /// root's, after its own type.
    #[must_use]
    pub fn root(&self) -> Option<&str> {
        match self {
            Type::Defined(defined) => match defined.supertypes.last() {
                Some(root) => Some(root),
                None => defined.specialized.then_some(defined.name.as_str()),
            },
            Type::Dummy(dummy) => Some(dummy.supertypes.last().unwrap_or(&dummy.name)),
            _ => None,
        }
    }

    /// The types a type of the program's own is a subtype of, by name, the
    /// immediate one first, and so on up: none for any other type.
    fn supertypes(&self) -> &[String] {
        match self {
            Type::Defined(defined) => &defined.supertypes,
            Type::Dummy(dummy) => &dummy.supertypes,
            _ => &[],
        }
    }

    /// The type a type of the program's own is a subtype of at `index`
    /// among its supertypes (see [`Type::supertypes`]): a regular type, of
    /// which it is defined as a subtype by a constraint, or a union type.
    fn supertype(&self, index: usize) -> Type {
        let dummy = |supertypes: &[String]| {
            Type::Dummy(Box::new(Dummy {
                name: supertypes[index].clone(),
                supertypes: supertypes[index + 1..].to_vec(),
            }))
        };
        match self {
            Type::Defined(defined) => match (&defined.specialization, index) {
                (Some(specialization), 0) => Type::Defined(specialization.supertype.clone()),
                (Some(specialization), _) => {
                    Type::Defined(specialization.supertype.clone()).supertype(index - 1)
                }
                (None, _) => dummy(&defined.supertypes),
            },
            Type::Dummy(own) => dummy(&own.supertypes),
            _ => unreachable!("only a type of the program's own has supertypes"),
        }
    }

    /// Whether every value of the type is a value of `other` (*Type
    /// Inheritance and Relational Theory*, IM Prescriptions 2, 3 and 22):
    /// it is `other`, or one of `other`'s subtypes, or, for tuples and
    /// relations with the same attributes, each attribute's type is a
    /// subtype of `other`'s, and for arrays, the elements' type is.
    #[must_use]
    pub fn is_subtype_of(&self, other: &Type) -> bool {
        match (self, other) {
            _ if self == other => true,
            (_, Type::Dummy(dummy)) => self.supertypes().contains(&dummy.name),
            (_, Type::Defined(defined)) if defined.specialized => {
                self.supertypes().contains(&defined.name)
            }
            (Type::Tuple(a), Type::Tuple(b)) | (Type::Relation(a), Type::Relation(b)) => {
                a.0.len() == b.0.len()
                    && a.0.iter().all(|(name, ty)| {
                        b.0.get(name).is_some_and(|wider| ty.is_subtype_of(wider))
                    })
            }
            (Type::Array(a), Type::Array(b)) => a.is_subtype_of(b),
            _ => false,
        }
    }

    /// The most specific type that both this type and `other` are subtypes
    /// of, if they have one (IM Prescriptions 8 and 24): of two types of
    /// the program's own, the nearest type above both, and of tuples
    /// and relations with the same attributes, and arrays, attribute by
    /// attribute, and element type.
    #[must_use]
    pub fn common_supertype(&self, other: &Type) -> Option<Type> {
        if other.is_subtype_of(self) {
            return Some(self.clone());
        }
        if self.is_subtype_of(other) {
            return Some(other.clone());
        }
        match (self, other) {
            (Type::Tuple(a), Type::Tuple(b)) => a.common_supertype(b).map(Type::Tuple),
            (Type::Relation(a), Type::Relation(b)) => a.common_supertype(b).map(Type::Relation),
            (Type::Array(a), Type::Array(b)) => a
                .common_supertype(b)
                .map(|element| Type::Array(Box::new(element))),
            _ => {
                let above = self.supertypes();
                let nearest = above.iter().position(|s| other.supertypes().contains(s))?;
                Some(self.supertype(nearest))
            }
        }
    }

    /// The least specific type that is a subtype of both this type and
    /// `other`, whose values are those of both, if some value is (IM
    /// Prescriptions 7 and 13): the more specific of the two, if one is a
    /// subtype of the other, since a union type's subtypes are disjoint,
    /// and of tuples and relations with the same attributes, and arrays,
    /// attribute by attribute, and element type.
    #[must_use]
    pub fn common_subtype(&self, other: &Type) -> Option<Type> {
        if self.is_subtype_of(other) {
            return Some(self.clone());
        }
        if other.is_subtype_of(self) {
            return Some(other.clone());
        }
        match (self, other) {
            (Type::Tuple(a), Type::Tuple(b)) => a.common_subtype(b).map(Type::Tuple),
            (Type::Relation(a), Type::Relation(b)) => a.common_subtype(b).map(Type::Relation),
            (Type::Array(a), Type::Array(b)) => a
                .common_subtype(b)
                .map(|element| Type::Array(Box::new(element))),
            _ => None,
        }
    }

    /// Whether some value is of both this type and `other` (IM
    /// Prescription 7): whether they have a common subtype.
    #[must_use]
    pub fn overlaps(&self, other: &Type) -> bool {
        self.common_subtype(other).is_some()
    }

    /// The type, in an operator instantiated with `types`: each type
    /// parameter in it replaced by its type, by index.
    #[must_use]
    pub fn instantiated(&self, types: &[Type]) -> Type {
        match self {
            Type::Scalar(_) | Type::Defined(_) | Type::Dummy(_) => self.clone(),
            Type::Tuple(heading) => Type::Tuple(heading.instantiated(types)),
            Type::Relation(heading) => Type::Relation(heading.instantiated(types)),
            Type::Array(element) => Type::Array(Box::new(element.instantiated(types))),
            Type::Parameter(p) => types[p.index].clone(),
        }
    }

    /// The type parameters in the type.
    pub fn parameters(&self) -> Box<dyn Iterator<Item = &TypeParameter> + '_> {
        match self {
            Type::Scalar(_) | Type::Defined(_) | Type::Dummy(_) => Box::new(std::iter::empty()),
            Type::Tuple(heading) | Type::Relation(heading) => {
                Box::new(heading.0.values().flat_map(Type::parameters))
            }
            Type::Array(element) => element.parameters(),
            Type::Parameter(p) => Box::new(std::iter::once(p)),
        }
    }

    /// The value a variable of this type starts with when none is given:
    /// for a tuple, each attribute's default. A type of the program's own
    /// has none, since no value of it is any more a start than another,
    /// nor has a union type, nor a type parameter, whose type each call
    /// gives, nor a tuple with one. A relation or an array starts empty, whatever is in it.
    #[must_use]
    pub fn default_value(&self) -> Option<Value> {
        Some(match self {
            Type::Scalar(scalar) => scalar.default_value(),
            Type::Defined(_) | Type::Dummy(_) | Type::Parameter(_) => return None,
            Type::Tuple(heading) => Value::Tuple(TupleValue(
                heading
                    .attributes()
                    .map(|(name, ty)| Some((name.to_string(), ty.default_value()?)))
                    .collect::<Option<_>>()?,
            )),
            // TTM's default for a relation variable: the empty relation.
            Type::Relation(heading) => Value::Relation(RelationValue {
                heading: heading.clone(),
                tuples: BTreeSet::new(),
            }),
            Type::Array(element) => Value::Array(ArrayValue {
                element: (**element).clone(),
                elements: Vec::new(),
            }),
        })
    }
}

/// Source text for the type, with attributes in name order, as in
/// `tuple { a: integer, b: text }`.
impl std::fmt::Display for Type {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Type::Scalar(scalar) => f.write_str(scalar.name()),
            Type::Defined(defined) => f.write_str(&defined.name),
            Type::Dummy(dummy) => f.write_str(&dummy.name),
            Type::Tuple(heading) => write!(f, "tuple {heading}"),
            Type::Relation(heading) => write!(f, "relation {heading}"),
            Type::Array(element) => write!(f, "array {element}"),
            Type::Parameter(p) => write!(f, "{p}"),
        }
    }
}

/// A constraint of a type of the program's own (TTM RM Prescription 23):
/// the read-only operator that computes it, a boolean, from a value's
/// components, its parameters, and the condition as it is written, which a
/// value that breaks it is reported with.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Constraint {
    pub operator: OperatorId,
    pub text: String,
}

/// A scalar type of the program's own: its name, the components of the
/// possible representation its values are stored as (TTM RM Prescription
/// 4), its other possible representations, its constraints, in order,
/// which every value's components satisfy (RM Prescription 23), and its
/// order, if it has one, which makes the type ordered.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Defined {
    pub name: String,
    pub components: Heading,
    pub representations: Vec<Representation>,
    pub constraints: Vec<Constraint>,
    /// The keys of its relation-valued components, of any of its possible
    /// representations, by the component's name: no two tuples of the
    /// component's relation have the same values for one of its keys (RM
    /// Prescription 15), which, as its constraints are, is true of every
    /// value of the type.
    pub keys: BTreeMap<String, Vec<Key>>,
    pub order: Option<Order>,
    /// The types it is a subtype of, by name, the one it names first, and
    /// so on up: none, if it names none.
    pub supertypes: Vec<String>,
    /// Whether a type is defined as a subtype of it by a constraint (see
    /// [`Defined::specialization`]), which puts it in a hierarchy.
    pub specialized: bool,
    /// Of a type defined as a subtype of a regular type by a constraint,
    /// what makes a value of that type one of it.
    pub specialization: Option<Specialization>,
}

/// What makes a value of a regular type, `supertype`, one of its subtype
/// defined by a constraint (*Type Inheritance and Relational Theory*, IM
/// Prescription 10): a value of it is one of the subtype exactly when the
/// subtype's specialization constraints are true of it, so a selector of
/// the supertype can give one of the subtype (specialization by
/// constraint), and an update can make one of the subtype one of just the
/// supertype again (generalization by constraint). The subtype has no
/// stored representation of its own: its components, keys and order are
/// the supertype's, its constraints the supertype's, then its own, its
/// specialization constraints, and its representations the supertype's,
/// then its own, each converted to and from the stored components.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Specialization {
    pub supertype: Box<Defined>,
}

/// A dummy type of the program's own (*Type Inheritance and Relational
/// Theory*, chapter 12, IM Prescription 20): a union type, whose every
/// value is of one of its immediate subtypes, each a type of the program's
/// own or a dummy type itself, with no possible representation of its own,
/// and so no selector and no components.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Dummy {
    pub name: String,
    /// The union types it is a subtype of, as [`Defined::supertypes`].
    pub supertypes: Vec<String>,
}

/// The order of a type of the program's own (*Type Inheritance and
/// Relational Theory*, chapter 2): its values compared by the components
/// of one of its possible representations, each of an ordered type, in
/// turn, every one of them, so that it is a total order consistent with
/// `=`. They are the stored components, or, if `from` is the conversion
/// from those to another representation, that one's.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Order {
    pub from: Option<OperatorId>,
    pub items: Vec<OrderItem>,
}

impl Defined {
    /// The constraints its declaration writes, not its supertype's: of a
    /// type defined as a subtype of a regular type by a constraint, its
    /// specialization constraints (see [`Specialization`]), and otherwise
    /// all of them.
    #[must_use]
    pub fn own_constraints(&self) -> &[Constraint] {
        let inherited = self
            .specialization
            .as_ref()
            .map_or(0, |s| s.supertype.constraints.len());
        &self.constraints[inherited..]
    }

    /// The possible representations other than the stored one that its
    /// declaration writes, not its supertype's (see [`Specialization`]).
    #[must_use]
    pub fn own_representations(&self) -> &[Representation] {
        let inherited = self
            .specialization
            .as_ref()
            .map_or(0, |s| s.supertype.representations.len());
        &self.representations[inherited..]
    }

    /// The type whose stored representation its values are stored as: of a
    /// type defined as a subtype of a regular type by a constraint, which
    /// has none of its own, its nearest supertype's that has one, and
    /// otherwise itself.
    #[must_use]
    pub fn stored_as(&self) -> &Defined {
        match &self.specialization {
            Some(specialization) => specialization.supertype.stored_as(),
            None => self,
        }
    }

    /// The name its values are written with, in the stored representation,
    /// the one the others convert to: of the type that declares it, whose
    /// selector in it selects the same value (see [`Defined::stored_as`]).
    #[must_use]
    pub fn written(&self) -> &str {
        &self.stored_as().name
    }

    /// Its order, if it has one, and the components of the possible
    /// representation it compares.
    ///
    /// # Panics
    ///
    /// If the order's conversion isn't one of its representations', which
    /// sema rules out.
    #[must_use]
    pub fn ordered_by(&self) -> Option<(&Order, &Heading)> {
        let order = self.order.as_ref()?;
        let components = match order.from {
            None => &self.components,
            Some(from) => {
                &self
                    .representations
                    .iter()
                    .find(|r| r.from == from)
                    .expect("an order's conversion is one of the type's")
                    .components
            }
        };
        Some((order, components))
    }

    /// Whether selecting a value of it checks anything: it has
    /// constraints, or keys of components.
    #[must_use]
    pub fn is_constrained(&self) -> bool {
        !self.constraints.is_empty() || !self.keys.is_empty()
    }

    /// The read-only operators selecting a value of it calls, to check its
    /// components: each constraint's, and each conversion from the stored
    /// representation that computes a keyed component of another.
    pub fn checks(&self) -> impl Iterator<Item = OperatorId> + '_ {
        let conversions = self
            .keys
            .keys()
            .filter_map(|component| self.representation(component)?.1)
            .map(|r| r.from);
        self.constraints
            .iter()
            .map(|c| c.operator)
            .chain(conversions)
    }

    /// The components of each of its possible representations, the stored
    /// one's first.
    pub fn possible_representations(&self) -> impl Iterator<Item = &Heading> {
        std::iter::once(&self.components).chain(self.representations.iter().map(|r| &r.components))
    }

    /// The components of the possible representation with a component
    /// named `name`, the stored one's or another's, and, if it isn't the
    /// stored one, the representation, with its conversions (see
    /// [`Defined::representation_with`]).
    #[must_use]
    pub fn representation(&self, name: &str) -> Option<(&Heading, Option<&Representation>)> {
        self.representation_with(&[name])
    }

    /// The components of the first possible representation, the stored one
    /// first, with a component of each of `names`, and, if it isn't the
    /// stored one, the representation. A subtype's representation can have
    /// its supertype's component, which it carries unchanged, so a name
    /// alone needn't say which.
    #[must_use]
    pub fn representation_with(
        &self,
        names: &[&str],
    ) -> Option<(&Heading, Option<&Representation>)> {
        let all = |heading: &Heading| names.iter().all(|n| heading.get(n).is_some());
        if all(&self.components) {
            return Some((&self.components, None));
        }
        self.representations
            .iter()
            .find(|r| all(&r.components))
            .map(|r| (&r.components, Some(r)))
    }
}

/// The type `load_csv` reads a record's fields as, for a value of type `ty`
/// whose columns `columns`, the type of its part of the call's `columns`,
/// gives: of a tuple, each attribute's; of a value of a type of the
/// program's own, read in the possible representation whose components the
/// columns name, the tuple of those, which is converted to the stored ones,
/// or the type itself if it is read as stored, and so is each value inside
/// it, but in a hierarchy, where it is laid out after its type, the tuple
/// of its components; and otherwise `ty` itself.
#[must_use]
pub fn read_as(ty: &Type, columns: &Type) -> Type {
    let Type::Tuple(given) = columns else {
        return ty.clone();
    };
    let read = |heading: &Heading| {
        Heading::new(
            heading
                .attributes()
                .map(|(name, ty)| {
                    let read = given
                        .get(name)
                        .map_or_else(|| ty.clone(), |c| read_as(ty, c));
                    (name.to_string(), read)
                })
                .collect(),
        )
    };
    match ty {
        Type::Tuple(heading) => Type::Tuple(read(heading)),
        Type::Defined(defined) => {
            let names: Vec<&str> = given.named().map(|(c, _)| c).collect();
            let representation = defined.representation_with(&names);
            if let Some((components, Some(_))) = representation {
                return Type::Tuple(read(components));
            }
            let stored = read(&defined.components);
            if stored == defined.components && ty.root().is_none() {
                ty.clone()
            } else {
                Type::Tuple(stored)
            }
        }
        _ => ty.clone(),
    }
}

/// A possible representation of a type of the program's own other than
/// the one its values are stored as: its components, and its conversions,
/// each a read-only operator: `to` the stored one, whose value is the tuple
/// of the stored components its `to` computes from its components, its
/// parameters, and `from` it, whose value is the tuple of its components
/// its `from` computes from the stored ones, its parameters.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Representation {
    pub components: Heading,
    pub to: OperatorId,
    pub from: OperatorId,
}

/// Source text for the heading: `{ a: integer, b: text }` or `{}`. A
/// wrapped attribute, which no source can name, is written `…`, last.
impl std::fmt::Display for Heading {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write_attributes(f, self.attributes().map(|(name, ty)| (name, ty as _)))
    }
}

/// `{ name: item, … }` or `{}`, for `attributes`, in their order, with the
/// wrapped ones, which no source can name, as one `…`, last, as the runtime
/// writes them.
fn write_attributes<'a>(
    f: &mut std::fmt::Formatter<'_>,
    attributes: impl Iterator<Item = (&'a str, &'a dyn std::fmt::Display)>,
) -> std::fmt::Result {
    let mut written = Vec::new();
    let mut wrapped = false;
    for (name, item) in attributes {
        if is_wrapped(name) {
            wrapped = true;
        } else {
            written.push(format!("{name}: {item}"));
        }
    }
    if wrapped {
        written.push("…".to_string());
    }
    if written.is_empty() {
        return f.write_str("{}");
    }
    write!(f, "{{ {} }}", written.join(", "))
}

/// The name of the attribute that the tuples of a relation, passed for an
/// operator's parameter `param` that `per` parameters refer to, keep their
/// own attributes in, wrapped, inside the operator. No source can write
/// it, so it never clashes with the `per` parameters' attributes beside
/// it, which the operator names.
#[must_use]
pub fn wrapped(param: &str) -> String {
    format!("%{param}")
}

/// Whether `name` is a wrapped attribute's (see [`wrapped`]).
#[must_use]
pub fn is_wrapped(name: &str) -> bool {
    name.starts_with('%')
}

/// A built-in scalar type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Scalar {
    /// `true` or `false`.
    Boolean,
    /// A 64-bit signed integer.
    Integer,
    /// A number with a fractional part, implemented as a 64-bit IEEE 754
    /// binary floating-point number.
    Approximate,
    /// A sequence of Unicode characters, stored as UTF-8.
    Text,
}

impl Scalar {
    /// Whether the type has what `requirement` names: every built-in type
    /// has an order, `false` before `true`, numbers numerically and text by
    /// code point, and the numbers have arithmetic.
    #[must_use]
    pub fn has(self, requirement: Requirement) -> bool {
        match requirement {
            Requirement::Order => true,
            Requirement::Addition | Requirement::Multiplication => Numeric::of(self).is_some(),
        }
    }

    /// In alphabetical order, as messages list them.
    pub const ALL: [Scalar; 4] = [
        Scalar::Boolean,
        Scalar::Integer,
        Scalar::Approximate,
        Scalar::Text,
    ];

    /// The type's name in source code.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Scalar::Boolean => "boolean",
            Scalar::Integer => "integer",
            Scalar::Approximate => "approximate",
            Scalar::Text => "text",
        }
    }

    /// The built-in type called `name`, if there is one.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Scalar> {
        Scalar::ALL.into_iter().find(|t| t.name() == name)
    }

    #[must_use]
    pub fn default_value(self) -> Value {
        match self {
            Scalar::Boolean => Value::Boolean(false),
            Scalar::Integer => Value::Integer(0),
            Scalar::Approximate => Value::Approximate(Approximate::new(0.0)),
            Scalar::Text => Value::Text(String::new()),
        }
    }
}

/// A tuple type's heading: its attributes' names and types. Attributes are
/// unordered (TTM RM Proscription 1); they are kept sorted by name, which is
/// also the order they are displayed and laid out in.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Heading(BTreeMap<String, Type>);

impl Heading {
    /// The heading whose attributes' types are the most specific common
    /// supertypes of this one's and `other`'s, if they have the same
    /// attributes and each pair has one (see [`Type::common_supertype`]).
    #[must_use]
    pub fn common_supertype(&self, other: &Heading) -> Option<Heading> {
        if self.0.len() != other.0.len() {
            return None;
        }
        self.0
            .iter()
            .map(|(name, ty)| Some((name.clone(), ty.common_supertype(other.0.get(name)?)?)))
            .collect::<Option<_>>()
            .map(Heading)
    }

    /// The heading whose attributes' types are the least specific common
    /// subtypes of this one's and `other`'s, if they have the same
    /// attributes and each pair has one (see [`Type::common_subtype`]).
    #[must_use]
    pub fn common_subtype(&self, other: &Heading) -> Option<Heading> {
        if self.0.len() != other.0.len() {
            return None;
        }
        self.0
            .iter()
            .map(|(name, ty)| Some((name.clone(), ty.common_subtype(other.0.get(name)?)?)))
            .collect::<Option<_>>()
            .map(Heading)
    }

    #[must_use]
    pub fn new(attributes: BTreeMap<String, Type>) -> Self {
        Heading(attributes)
    }

    /// The attributes, sorted by name.
    pub fn attributes(&self) -> impl Iterator<Item = (&str, &Type)> {
        self.0.iter().map(|(name, ty)| (name.as_str(), ty))
    }

    /// The attributes a source can name, sorted by name: all but a
    /// wrapped one (see [`wrapped`]).
    pub fn named(&self) -> impl Iterator<Item = (&str, &Type)> {
        self.attributes().filter(|(name, _)| !is_wrapped(name))
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Type> {
        self.0.get(name)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The heading, in an operator instantiated with `types`.
    #[must_use]
    pub fn instantiated(&self, types: &[Type]) -> Heading {
        Heading(
            self.0
                .iter()
                .map(|(name, ty)| (name.clone(), ty.instantiated(types)))
                .collect(),
        )
    }

    /// The heading with attributes renamed, all at once, from each key of
    /// `renamings` to its value.
    #[must_use]
    pub fn renamed(&self, renamings: &BTreeMap<String, String>) -> Heading {
        Heading(
            self.0
                .iter()
                .map(|(name, ty)| (renamed(name, renamings), ty.clone()))
                .collect(),
        )
    }

    /// The heading with the relation-valued attributes named replaced by
    /// their own attributes, as for [`Expr::Ungroup`].
    ///
    /// # Panics
    ///
    /// If a name isn't a relation-valued attribute, which sema rules out.
    #[must_use]
    pub fn ungrouped(&self, names: &BTreeSet<String>) -> Heading {
        let mut attributes = self.without(names).0;
        for name in names {
            let Some(Type::Relation(inner)) = self.get(name) else {
                panic!("ungrouping `{name}`, which isn't relation-valued");
            };
            attributes.extend(inner.0.iter().map(|(a, ty)| (a.clone(), ty.clone())));
        }
        Heading(attributes)
    }

    /// The attributes of both headings, a common one of the least specific
    /// common subtype of its types, since its value in a tuple of a join
    /// is of both (*Type Inheritance and Relational Theory*, IM
    /// Prescription 13).
    ///
    /// # Panics
    ///
    /// If a common attribute's types have no common subtype, which sema
    /// rules out.
    #[must_use]
    pub fn and(&self, other: &Heading) -> Heading {
        let mut attributes = self.0.clone();
        for (name, ty) in &other.0 {
            let ty = match attributes.get(name) {
                Some(own) => own
                    .common_subtype(ty)
                    .unwrap_or_else(|| panic!("joining `{name}` of {own} and {ty}")),
                None => ty.clone(),
            };
            attributes.insert(name.clone(), ty);
        }
        Heading(attributes)
    }

    /// The names of the attributes both headings have.
    #[must_use]
    pub fn common(&self, other: &Heading) -> BTreeSet<String> {
        self.0
            .keys()
            .filter(|name| other.0.contains_key(*name))
            .cloned()
            .collect()
    }

    /// The heading with the attributes of `computed` added, or replacing
    /// those of the same names, as an `extend` or a `replace` has.
    #[must_use]
    pub fn with(&self, computed: &Heading) -> Heading {
        let mut all = self.0.clone();
        all.extend(
            computed
                .0
                .iter()
                .map(|(name, ty)| (name.clone(), ty.clone())),
        );
        Heading(all)
    }

    /// The heading without the attributes named.
    #[must_use]
    pub fn without(&self, names: &BTreeSet<String>) -> Heading {
        Heading(
            self.0
                .iter()
                .filter(|(name, _)| !names.contains(*name))
                .map(|(name, ty)| (name.clone(), ty.clone()))
                .collect(),
        )
    }
}

/// A value. Values are totally ordered, so that a relation's tuples form a
/// sorted set. The order has no meaning in the language, but it is the
/// canonical order of a relation's tuples in memory, which the runtime
/// reproduces when it builds relations (`trendy_runtime::order`), so the
/// two must agree: values of one type compare as `hir::compare` does for
/// scalars, tuples attribute by attribute in name order, and relations by
/// their sorted tuples, lexicographically.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Value {
    Boolean(bool),
    Integer(i64),
    Approximate(Approximate),
    Text(String),
    Tuple(TupleValue),
    Relation(RelationValue),
    Array(ArrayValue),
    Defined(DefinedValue),
}

impl Value {
    /// Whether the value is of the type `ty`: its own type is `ty` or one
    /// of its subtypes (*Type Inheritance and Relational Theory*, IM
    /// Prescription 15).
    #[must_use]
    pub fn is_of(&self, ty: &Type) -> bool {
        self.ty().is_subtype_of(ty)
    }

    #[must_use]
    pub fn ty(&self) -> Type {
        let scalar = |s| Type::Scalar(s);
        match self {
            Value::Boolean(_) => scalar(Scalar::Boolean),
            Value::Integer(_) => scalar(Scalar::Integer),
            Value::Approximate(_) => scalar(Scalar::Approximate),
            Value::Text(_) => scalar(Scalar::Text),
            Value::Tuple(tuple) => Type::Tuple(Heading(
                tuple
                    .attributes()
                    .map(|(name, value)| (name.to_string(), value.ty()))
                    .collect(),
            )),
            Value::Relation(relation) => Type::Relation(relation.heading.clone()),
            Value::Array(array) => Type::Array(Box::new(array.element.clone())),
            Value::Defined(value) => Type::Defined(value.ty.clone()),
        }
    }

    /// The value, in an operator instantiated with `types`: the only
    /// values with a type parameter in their types are relations and
    /// arrays with one in them, and so empty, or values with them inside.
    #[must_use]
    pub fn instantiated(&self, types: &[Type]) -> Value {
        match self {
            Value::Tuple(tuple) => Value::Tuple(tuple.instantiated(types)),
            Value::Relation(relation) => Value::Relation(RelationValue {
                heading: relation.heading.instantiated(types),
                tuples: relation
                    .tuples
                    .iter()
                    .map(|t| t.instantiated(types))
                    .collect(),
            }),
            Value::Array(array) => Value::Array(ArrayValue {
                element: array.element.instantiated(types),
                elements: array
                    .elements
                    .iter()
                    .map(|e| e.instantiated(types))
                    .collect(),
            }),
            _ => self.clone(),
        }
    }
}

/// The value as trendy source, as the runtime writes it
/// (`trendy_runtime::write_value`), for `debug_value` and `to_text`: tuples
/// and relations with their attributes in name order, and relations with
/// their heading first, so it is a literal for the value. A wrapped
/// attribute, which no source can name, is written `…`, last, without its
/// value, as a [`Heading`]'s source text writes its type.
impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use std::fmt::Write;
        match self {
            Value::Boolean(b) => write!(f, "{b}"),
            Value::Integer(n) => write!(f, "{n}"),
            Value::Approximate(r) => write!(f, "{r}"),
            Value::Text(text) => {
                f.write_char('"')?;
                trendy_text::escape(text, f)?;
                f.write_char('"')
            }
            Value::Tuple(tuple) => write!(f, "tuple {tuple}"),
            // Its selector's invocation, as a call is written.
            Value::Defined(value) => {
                let components: Vec<String> = value
                    .components
                    .attributes()
                    .map(|(name, value)| format!("{name}: {value}"))
                    .collect();
                write!(f, "{}{{{}}}", value.ty.written(), components.join(", "))
            }
            Value::Relation(relation) => {
                write!(f, "relation {} ", relation.heading)?;
                if relation.tuples.is_empty() {
                    return f.write_str("{}");
                }
                let tuples: Vec<String> = relation.tuples.iter().map(ToString::to_string).collect();
                write!(f, "{{ {} }}", tuples.join(", "))
            }
            // An empty one with its elements' type, which there is no
            // element to give.
            Value::Array(array) if array.elements.is_empty() => {
                write!(f, "array {} []", array.element)
            }
            Value::Array(array) => {
                let elements: Vec<String> =
                    array.elements.iter().map(ToString::to_string).collect();
                write!(f, "array [{}]", elements.join(", "))
            }
        }
    }
}

/// A value of one of the program's own types, `ty`, which it carries, as
/// TTM has every scalar value carry its type (RM Prescription 2): its components,
/// in its possible representation. Two of a type are equal when their
/// components are (RM Prescription 8).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DefinedValue {
    pub ty: Box<Defined>,
    pub components: TupleValue,
}

/// A tuple value: a value for each attribute of its heading. Two tuples are
/// equal when every attribute's values are equal.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TupleValue(BTreeMap<String, Value>);

impl TupleValue {
    #[must_use]
    pub fn new(attributes: BTreeMap<String, Value>) -> Self {
        TupleValue(attributes)
    }

    /// The attributes, sorted by name.
    pub fn attributes(&self) -> impl Iterator<Item = (&str, &Value)> {
        self.0.iter().map(|(name, value)| (name.as_str(), value))
    }

    /// The tuple, in an operator instantiated with `types`.
    #[must_use]
    pub fn instantiated(&self, types: &[Type]) -> TupleValue {
        TupleValue(
            self.0
                .iter()
                .map(|(name, value)| (name.clone(), value.instantiated(types)))
                .collect(),
        )
    }
}

/// Source text for the tuple's attributes, `{ a: 1, b: "Lima" }` or `{}`,
/// with a wrapped attribute written `…`, last, as for a [`Heading`].
impl std::fmt::Display for TupleValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write_attributes(f, self.attributes().map(|(name, value)| (name, value as _)))
    }
}

/// An array value: elements of one type, in order. Two arrays are equal
/// when they have the same elements in the same order, and they compare
/// element by element, a shorter one first if it is the other's start.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ArrayValue {
    /// The elements' type, which an empty array has none to take it from.
    pub element: Type,
    /// In order, the first at position 1.
    pub elements: Vec<Value>,
}

impl ArrayValue {
    /// The relation a `load` makes of the array, as `unloading` says: each
    /// element a tuple, and those that turn out equal one.
    #[must_use]
    pub fn unloaded(&self, unloading: &Unloading) -> RelationValue {
        RelationValue {
            heading: unloading.heading(&self.element),
            tuples: self
                .elements
                .iter()
                .enumerate()
                .map(|(i, element)| unloading.tuple(element, i + 1))
                .collect(),
        }
    }
}

/// A relation value: a set of tuples, all with the relation's heading.
///
/// The heading is part of the value because an empty relation has no
/// tuples to take it from. Two relations are equal when they have the same
/// heading and the same tuples.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RelationValue {
    pub heading: Heading,
    /// Sorted and without duplicates; the order has no meaning.
    pub tuples: BTreeSet<TupleValue>,
}

impl RelationValue {
    /// The array a `load` makes of the relation, as `loading` says: its
    /// tuples, ordered by its items, which are attributes of scalar types,
    /// values of types of the program's own compared with the components in
    /// `converted` (see [`compare`]), each without the attributes it
    /// removes, or the value of the only one left.
    ///
    /// # Panics
    ///
    /// If an item isn't an attribute of a scalar type, or the values are
    /// loaded of tuples of more attributes than one, which sema rules out.
    #[must_use]
    pub fn loaded(&self, loading: &Loading, converted: &Converted) -> ArrayValue {
        fn key<'t>(tuple: &'t TupleValue, item: &OrderItem) -> &'t Value {
            tuple
                .get(&item.attribute)
                .expect("sema orders only by the relation's attributes")
        }
        let items = &loading.order;
        let mut tuples: Vec<&TupleValue> = self.tuples.iter().collect();
        // A stable sort of the tuples in canonical order, which so breaks
        // ties, as the runtime does.
        tuples.sort_by(|a, b| {
            items
                .iter()
                .map(|item| {
                    let ordering = compare(key(a, item), key(b, item), converted);
                    if item.descending {
                        ordering.reverse()
                    } else {
                        ordering
                    }
                })
                .find(|ordering| ordering.is_ne())
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let elements = tuples
            .into_iter()
            .map(|tuple| {
                let kept = tuple.without(&loading.removed);
                match loading.loaded {
                    Loaded::Tuples => Value::Tuple(kept),
                    Loaded::Values => kept
                        .attributes()
                        .next()
                        .map(|(_, value)| value.clone())
                        .expect("sema loads the values only of one attribute"),
                }
            })
            .collect();
        ArrayValue {
            element: loading.element(&self.heading),
            elements,
        }
    }
}

impl Value {
    /// The tuple or relation without the attributes named, which it has.
    /// A relation's tuples can become duplicates, which are dropped.
    ///
    /// # Panics
    ///
    /// If the value isn't a tuple or relation, which sema rules out.
    #[must_use]
    pub fn without(&self, names: &BTreeSet<String>) -> Value {
        match self {
            Value::Tuple(tuple) => Value::Tuple(tuple.without(names)),
            Value::Relation(relation) => Value::Relation(RelationValue {
                heading: relation.heading.without(names),
                tuples: relation.tuples.iter().map(|t| t.without(names)).collect(),
            }),
            _ => panic!("removing attributes from {self:?}"),
        }
    }

    /// This relation and `other`, of the same attributes, combined as
    /// `combination` says, as for [`Expr::Combine`].
    ///
    /// # Errors
    ///
    /// Returns a tuple that breaks a disjoint union's or an included
    /// minus's condition.
    ///
    /// # Panics
    ///
    /// If either value isn't a relation, which sema rules out.
    pub fn combined(&self, other: &Value, combination: Combination) -> Result<Value, Box<Broken>> {
        let (Value::Relation(lhs), Value::Relation(rhs)) = (self, other) else {
            panic!("combining {self:?} with {other:?}");
        };
        let broken = |tuple: &TupleValue| {
            Box::new(Broken {
                combination,
                tuple: tuple.clone(),
            })
        };
        match combination {
            Combination::DisjointUnion | Combination::DisjointChanges => {
                if let Some(shared) = lhs.tuples.intersection(&rhs.tuples).next() {
                    return Err(broken(shared));
                }
            }
            Combination::IncludedMinus => {
                if let Some(missing) = rhs.tuples.difference(&lhs.tuples).next() {
                    return Err(broken(missing));
                }
            }
            Combination::Union | Combination::ExclusiveUnion | Combination::Minus => {}
        }
        let tuples = match combination {
            Combination::Minus | Combination::IncludedMinus => {
                lhs.tuples.difference(&rhs.tuples).cloned().collect()
            }
            Combination::ExclusiveUnion => lhs
                .tuples
                .symmetric_difference(&rhs.tuples)
                .cloned()
                .collect(),
            Combination::Union | Combination::DisjointUnion | Combination::DisjointChanges => {
                lhs.tuples.union(&rhs.tuples).cloned().collect()
            }
        };
        Ok(Value::Relation(RelationValue {
            heading: combination.heading(&lhs.heading, &rhs.heading),
            tuples,
        }))
    }

    /// The tuples of this relation that have a match in `other` on their
    /// common attributes, or, if `negated`, those that have none, as for
    /// [`Expr::Semijoin`] and [`Expr::Antijoin`].
    ///
    /// # Panics
    ///
    /// If either value isn't a relation, which sema rules out.
    #[must_use]
    pub fn matching(&self, other: &Value, negated: bool) -> Value {
        let (Value::Relation(lhs), Value::Relation(rhs)) = (self, other) else {
            panic!("matching {self:?} with {other:?}");
        };
        let common = lhs.heading.common(&rhs.heading);
        let key = |heading: &Heading, tuple: &TupleValue| {
            let others: BTreeSet<String> = heading
                .attributes()
                .map(|(name, _)| name.to_string())
                .filter(|name| !common.contains(name))
                .collect();
            tuple.without(&others)
        };
        let matched: BTreeSet<TupleValue> =
            rhs.tuples.iter().map(|t| key(&rhs.heading, t)).collect();
        Value::Relation(RelationValue {
            heading: lhs.heading.clone(),
            tuples: lhs
                .tuples
                .iter()
                .filter(|t| matched.contains(&key(&lhs.heading, t)) != negated)
                .cloned()
                .collect(),
        })
    }

    /// The relation with the relation-valued attributes named replaced by
    /// their attributes, as for [`Expr::Ungroup`].
    ///
    /// # Panics
    ///
    /// If the value isn't a relation with those attributes, which sema rules
    /// out.
    #[must_use]
    pub fn ungrouped(&self, names: &BTreeSet<String>) -> Value {
        let Value::Relation(relation) = self else {
            panic!("ungrouping {self:?}");
        };
        let mut tuples = BTreeSet::new();
        for tuple in &relation.tuples {
            // Each combination of a tuple of each ungrouped relation, built
            // up one relation at a time.
            let mut combined = vec![tuple.without(names)];
            for name in names {
                let Some(Value::Relation(inner)) = tuple.get(name) else {
                    panic!("ungrouping `{name}`, which isn't relation-valued");
                };
                combined = combined
                    .iter()
                    .flat_map(|t| inner.tuples.iter().map(move |i| t.with(i.0.clone())))
                    .collect();
            }
            tuples.extend(combined);
        }
        Value::Relation(RelationValue {
            heading: relation.heading.ungrouped(names),
            tuples,
        })
    }

    /// The tuple or relation with attributes renamed, as for
    /// [`Heading::renamed`]. A relation's tuples are sorted again, since
    /// renaming can change their attributes' order.
    ///
    /// # Panics
    ///
    /// If the value isn't a tuple or relation, which sema rules out.
    #[must_use]
    pub fn renamed(&self, renamings: &BTreeMap<String, String>) -> Value {
        match self {
            Value::Tuple(tuple) => Value::Tuple(tuple.renamed(renamings)),
            Value::Relation(relation) => Value::Relation(RelationValue {
                heading: relation.heading.renamed(renamings),
                tuples: relation
                    .tuples
                    .iter()
                    .map(|t| t.renamed(renamings))
                    .collect(),
            }),
            _ => panic!("renaming attributes of {self:?}"),
        }
    }
}

impl Value {
    /// `relation {} { {} }`, the relation with no attributes and one tuple
    /// (Tutorial D's `TABLE_DEE`), which is the identity of AND.
    #[must_use]
    pub fn dee() -> Value {
        Value::Relation(RelationValue {
            heading: Heading::default(),
            tuples: BTreeSet::from([TupleValue::default()]),
        })
    }

    /// The AND of two relations, or the join of two tuples, as for
    /// [`Expr::And`].
    ///
    /// # Errors
    ///
    /// Returns the attribute two tuples disagree on, with both values.
    ///
    /// # Panics
    ///
    /// If the values aren't both relations or both tuples, which sema rules
    /// out.
    pub fn and(&self, other: &Value) -> Result<Value, Mismatch> {
        let (lhs, rhs) = match (self, other) {
            (Value::Relation(lhs), Value::Relation(rhs)) => (lhs, rhs),
            (Value::Tuple(lhs), Value::Tuple(rhs)) => return lhs.join(rhs).map(Value::Tuple),
            _ => panic!("the AND of {self:?} and {other:?}"),
        };
        let common = lhs.heading.common(&rhs.heading);
        let uncommon = |heading: &Heading| -> BTreeSet<String> {
            heading
                .attributes()
                .map(|(name, _)| name.to_string())
                .filter(|name| !common.contains(name))
                .collect()
        };
        // The right operand's tuples, by their values for the common
        // attributes.
        let (lhs_only, rhs_only) = (uncommon(&lhs.heading), uncommon(&rhs.heading));
        let mut matching: BTreeMap<TupleValue, Vec<&TupleValue>> = BTreeMap::new();
        for tuple in &rhs.tuples {
            matching
                .entry(tuple.without(&rhs_only))
                .or_default()
                .push(tuple);
        }
        let mut tuples = BTreeSet::new();
        for tuple in &lhs.tuples {
            for other in matching
                .get(&tuple.without(&lhs_only))
                .into_iter()
                .flatten()
            {
                let mut attributes = tuple.0.clone();
                attributes.extend(other.0.iter().map(|(name, v)| (name.clone(), v.clone())));
                tuples.insert(TupleValue(attributes));
            }
        }
        Ok(Value::Relation(RelationValue {
            heading: lhs.heading.and(&rhs.heading),
            tuples,
        }))
    }
}

/// `name`'s new name in `renamings`, or `name` if it isn't renamed.
fn renamed(name: &str, renamings: &BTreeMap<String, String>) -> String {
    renamings
        .get(name)
        .map_or_else(|| name.to_string(), Clone::clone)
}

impl TupleValue {
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Value> {
        self.0.get(name)
    }

    /// The tuple with attributes renamed, as for [`Heading::renamed`].
    #[must_use]
    pub fn renamed(&self, renamings: &BTreeMap<String, String>) -> TupleValue {
        TupleValue(
            self.0
                .iter()
                .map(|(name, value)| (renamed(name, renamings), value.clone()))
                .collect(),
        )
    }

    /// The tuple with `attributes` added, or replacing those it has.
    #[must_use]
    pub fn with(&self, attributes: impl IntoIterator<Item = (String, Value)>) -> TupleValue {
        let mut all = self.0.clone();
        all.extend(attributes);
        TupleValue(all)
    }

    /// The tuple of both tuples' attributes, as for [`Expr::And`], or the
    /// first attribute they disagree on.
    ///
    /// # Errors
    ///
    /// Returns the common attribute whose values differ, and both values.
    pub fn join(&self, other: &TupleValue) -> Result<TupleValue, Mismatch> {
        for (attribute, value) in &self.0 {
            if let Some(theirs) = other.0.get(attribute)
                && theirs != value
            {
                return Err(Mismatch {
                    attribute: attribute.clone(),
                    lhs: Box::new(value.clone()),
                    rhs: Box::new(theirs.clone()),
                });
            }
        }
        Ok(self.with(other.0.clone()))
    }

    /// The tuple without the attributes named.
    #[must_use]
    pub fn without(&self, names: &BTreeSet<String>) -> TupleValue {
        TupleValue(
            self.0
                .iter()
                .filter(|(name, _)| !names.contains(*name))
                .map(|(name, value)| (name.clone(), value.clone()))
                .collect(),
        )
    }
}

impl Type {
    /// The tuple or relation type without the attributes named.
    ///
    /// # Panics
    ///
    /// If the type isn't a tuple or relation type, which sema rules out.
    #[must_use]
    pub fn without(&self, names: &BTreeSet<String>) -> Type {
        match self {
            Type::Tuple(heading) => Type::Tuple(heading.without(names)),
            Type::Relation(heading) => Type::Relation(heading.without(names)),
            _ => panic!("removing attributes from {self}"),
        }
    }

    /// The relation type with the relation-valued attributes named replaced
    /// by their attributes, as for [`Expr::Ungroup`].
    ///
    /// # Panics
    ///
    /// If the type isn't a relation type, which sema rules out.
    #[must_use]
    pub fn ungrouped(&self, names: &BTreeSet<String>) -> Type {
        match self {
            Type::Relation(heading) => Type::Relation(heading.ungrouped(names)),
            _ => panic!("ungrouping {self}"),
        }
    }

    /// The tuple or relation type with the attributes of `computed` added,
    /// or replacing those it has, as for [`Expr::Extend`].
    ///
    /// # Panics
    ///
    /// If the type isn't a tuple or relation type, which sema rules out.
    #[must_use]
    pub fn with(&self, computed: &Heading) -> Type {
        match self {
            Type::Tuple(heading) => Type::Tuple(heading.with(computed)),
            Type::Relation(heading) => Type::Relation(heading.with(computed)),
            _ => panic!("adding attributes to {self}"),
        }
    }

    /// The type of the AND of relations of types `self` and `other`, as for
    /// [`Expr::And`].
    ///
    /// # Panics
    ///
    /// If either type isn't a relation type, which sema rules out.
    #[must_use]
    pub fn and(&self, other: &Type) -> Type {
        match (self, other) {
            (Type::Relation(lhs), Type::Relation(rhs)) => Type::Relation(lhs.and(rhs)),
            (Type::Tuple(lhs), Type::Tuple(rhs)) => Type::Tuple(lhs.and(rhs)),
            _ => panic!("the AND of {self} and {other}"),
        }
    }

    /// Whether the type has what `requirement` names: a scalar type as
    /// [`Scalar::has`] says, and a type parameter if its kind requires it.
    /// Tuples, relations and arrays have no total order, and no arithmetic.
    #[must_use]
    pub fn has(&self, requirement: Requirement) -> bool {
        match self {
            Type::Scalar(scalar) => scalar.has(requirement),
            Type::Parameter(parameter) => parameter.any.requires(requirement),
            Type::Defined(defined) => requirement == Requirement::Order && defined.order.is_some(),
            _ => false,
        }
    }

    /// The tuple or relation type with attributes renamed, as for
    /// [`Heading::renamed`].
    ///
    /// # Panics
    ///
    /// If the type isn't a tuple or relation type, which sema rules out.
    #[must_use]
    pub fn renamed(&self, renamings: &BTreeMap<String, String>) -> Type {
        match self {
            Type::Tuple(heading) => Type::Tuple(heading.renamed(renamings)),
            Type::Relation(heading) => Type::Relation(heading.renamed(renamings)),
            _ => panic!("renaming attributes of {self}"),
        }
    }
}

/// An `approximate` value: a finite 64-bit IEEE 754 binary floating-point
/// number, with one zero.
///
/// *The Third Manifesto* requires `=` to hold exactly when two values can't
/// be told apart by any operator (RM Prescription 8). IEEE 754 equality
/// breaks that twice: NaN is not equal to itself, and `-0.0` equals `0.0`
/// although `1/x` tells them apart. Neither is an approximate number. No
/// operator makes an infinity or a NaN, since a result out of range fails,
/// as an integer's does; and `-0.0` is always made `0.0`, as SQL databases
/// have one zero too. So two approximate numbers are equal exactly when
/// their bits are identical, and they sort by value.
///
/// Every comparison, ordering and hash of an approximate number must go
/// through this type, never through `f64` operators.
#[derive(Debug, Clone, Copy)]
pub struct Approximate(f64);

impl Approximate {
    /// The approximate number `value`, which must be finite; `-0.0` is
    /// `0.0`.
    #[must_use]
    pub fn new(value: f64) -> Self {
        debug_assert!(value.is_finite(), "the approximate number {value}");
        // Adding `0.0` turns `-0.0` into `0.0`, and leaves every other
        // number as it is (IEEE 754, 6.3).
        Approximate(value + 0.0)
    }

    #[must_use]
    pub fn get(self) -> f64 {
        self.0
    }

    /// The IEEE 754 bit pattern, which identifies the value exactly.
    #[must_use]
    pub fn to_bits(self) -> u64 {
        self.0.to_bits()
    }

    /// The approximate number nearest `n`, ties to even: `n` itself up to
    /// 2^53 in magnitude, beyond which not every integer is one.
    #[must_use]
    pub fn nearest(n: i64) -> Self {
        // Each half is exact, and so is the high half times 2^32, so the one
        // rounding is the sum's: to the number nearest `n`.
        let [b0, b1, b2, b3, b4, b5, b6, b7] = n.to_le_bytes();
        let high = i32::from_le_bytes([b4, b5, b6, b7]);
        let low = u32::from_le_bytes([b0, b1, b2, b3]);
        Approximate::new(f64::from(high) * 4_294_967_296.0 + f64::from(low))
    }
}

/// The value as a literal, which always has an exponent: the shortest one
/// that reads back as the same value, as in `0e0`, `1e0`, `2.5e0` or
/// `-5e-1`.
impl std::fmt::Display for Approximate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:e}", self.0)
    }
}

impl PartialEq for Approximate {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == std::cmp::Ordering::Equal
    }
}

impl Eq for Approximate {}

impl PartialOrd for Approximate {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Approximate {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // IEEE 754 totalOrder, which, without NaN or `-0.0`, is the order
        // of the values.
        self.0.total_cmp(&other.0)
    }
}

impl std::hash::Hash for Approximate {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        // Equal exactly when the bits are, so hashing the bits is consistent.
        self.to_bits().hash(state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tuple(attributes: &[(&str, Type)]) -> Type {
        Type::Tuple(Heading::new(
            attributes
                .iter()
                .map(|(name, ty)| ((*name).to_string(), ty.clone()))
                .collect(),
        ))
    }

    #[test]
    fn nested_tuple_defaults() {
        let ty = tuple(&[
            ("c", Type::Scalar(Scalar::Text)),
            ("a", tuple(&[("b", Type::Scalar(Scalar::Boolean))])),
        ]);
        let expected = Value::Tuple(TupleValue::new(
            [
                (
                    "a".to_string(),
                    Value::Tuple(TupleValue::new(
                        [("b".to_string(), Value::Boolean(false))].into(),
                    )),
                ),
                ("c".to_string(), Value::Text(String::new())),
            ]
            .into(),
        ));
        assert_eq!(ty.default_value(), Some(expected.clone()));
        assert_eq!(expected.ty(), ty);
    }

    #[test]
    fn a_programs_own_types_are_named_and_have_no_default() {
        let components =
            Heading::new([("degrees".to_string(), Type::Scalar(Scalar::Approximate))].into());
        let celsius = Defined {
            name: "celsius".to_string(),
            components,
            representations: Vec::new(),
            constraints: Vec::new(),
            keys: BTreeMap::new(),
            order: None,
            supertypes: Vec::new(),
            specialized: false,
            specialization: None,
        };
        let ty = Type::Defined(Box::new(celsius.clone()));
        assert_eq!(ty.to_string(), "celsius");
        assert_eq!(ty.default_value(), None);
        // Nor has a tuple with one.
        let tuple = Type::Tuple(Heading::new([("t".to_string(), ty.clone())].into()));
        assert_eq!(tuple.default_value(), None);
        // A value is written as its selector's invocation.
        let value = Value::Defined(DefinedValue {
            ty: Box::new(celsius.clone()),
            components: TupleValue::new(
                [(
                    "degrees".to_string(),
                    Value::Approximate(Approximate::new(21.5)),
                )]
                .into(),
            ),
        });
        assert_eq!(value.to_string(), "celsius{degrees: 2.15e1}");
        assert_eq!(value.ty(), ty);
        // Its components, and the value they select.
        let select = Expr::Select(
            Box::new(celsius),
            Box::new(Expr::Parameter("t".to_string())),
        );
        assert_eq!(select.type_with(&|_| unreachable!()), ty);
        assert_eq!(
            Expr::Components(Box::new(select)).type_with(&|_| unreachable!()),
            Type::Tuple(Heading::new(
                [("degrees".to_string(), Type::Scalar(Scalar::Approximate))].into()
            ))
        );
    }

    #[test]
    fn tuple_types_are_structural_and_unordered() {
        let ab = tuple(&[
            ("a", Type::Scalar(Scalar::Integer)),
            ("b", Type::Scalar(Scalar::Text)),
        ]);
        let ba = tuple(&[
            ("b", Type::Scalar(Scalar::Text)),
            ("a", Type::Scalar(Scalar::Integer)),
        ]);
        assert_eq!(ab, ba);
        assert_eq!(ba.to_string(), "tuple { a: integer, b: text }");
        assert_eq!(tuple(&[]).to_string(), "tuple {}");
    }

    #[test]
    fn subtypes_are_subsets_of_their_union_types() {
        let defined = |name: &str, supertypes: &[&str]| {
            Type::Defined(Box::new(Defined {
                name: name.to_string(),
                components: Heading::default(),
                representations: Vec::new(),
                constraints: Vec::new(),
                keys: BTreeMap::new(),
                order: None,
                supertypes: supertypes.iter().map(ToString::to_string).collect(),
                specialized: false,
                specialization: None,
            }))
        };
        let dummy = |name: &str, supertypes: &[&str]| {
            Type::Dummy(Box::new(Dummy {
                name: name.to_string(),
                supertypes: supertypes.iter().map(ToString::to_string).collect(),
            }))
        };
        // figure > { curve > { ellipse, parabola }, polygon }
        let figure = dummy("figure", &[]);
        let curve = dummy("curve", &["figure"]);
        let ellipse = defined("ellipse", &["curve", "figure"]);
        let parabola = defined("parabola", &["curve", "figure"]);
        let polygon = defined("polygon", &["figure"]);
        assert!(ellipse.is_subtype_of(&figure));
        assert!(ellipse.is_subtype_of(&curve));
        assert!(curve.is_subtype_of(&figure));
        assert!(!figure.is_subtype_of(&ellipse));
        assert!(!ellipse.is_subtype_of(&polygon));
        // The nearest union type above both.
        assert_eq!(ellipse.common_supertype(&parabola), Some(curve.clone()));
        assert_eq!(ellipse.common_supertype(&polygon), Some(figure.clone()));
        assert_eq!(ellipse.common_supertype(&curve), Some(curve.clone()));
        assert_eq!(
            ellipse.common_supertype(&Type::Scalar(Scalar::Integer)),
            None
        );
        // Subtypes of one type are disjoint.
        assert!(ellipse.overlaps(&figure) && figure.overlaps(&ellipse));
        assert!(!ellipse.overlaps(&polygon));
        // Tuples, relations and arrays, attribute by attribute and element
        // type.
        let tuple = |ty: &Type| Type::Tuple(Heading::new([("s".to_string(), ty.clone())].into()));
        let relation =
            |ty: &Type| Type::Relation(Heading::new([("s".to_string(), ty.clone())].into()));
        assert!(relation(&ellipse).is_subtype_of(&relation(&figure)));
        assert!(!relation(&figure).is_subtype_of(&relation(&ellipse)));
        assert_eq!(
            tuple(&ellipse).common_supertype(&tuple(&polygon)),
            Some(tuple(&figure))
        );
        let array = |ty: &Type| Type::Array(Box::new(ty.clone()));
        assert_eq!(
            array(&ellipse).common_supertype(&array(&parabola)),
            Some(array(&curve))
        );
        assert!(!relation(&ellipse).overlaps(&relation(&polygon)));
        // The least specific common subtype is the more specific type, of
        // two that overlap.
        assert_eq!(figure.common_subtype(&ellipse), Some(ellipse.clone()));
        assert_eq!(ellipse.common_subtype(&curve), Some(ellipse.clone()));
        assert_eq!(ellipse.common_subtype(&polygon), None);
        assert_eq!(
            tuple(&curve).common_subtype(&tuple(&parabola)),
            Some(tuple(&parabola))
        );
        // A join's common attribute is of it, and a union's attribute of
        // the common supertype, a minus's of the left operand's (IM
        // Prescription 13).
        let heading = |ty: &Type| {
            Heading::new(
                [
                    ("s".to_string(), ty.clone()),
                    ("n".to_string(), Type::Scalar(Scalar::Integer)),
                ]
                .into(),
            )
        };
        let joined = heading(&figure).and(&Heading::new([("s".to_string(), curve.clone())].into()));
        assert_eq!(joined, heading(&curve));
        let (ellipses, polygons) = (heading(&ellipse), heading(&polygon));
        assert_eq!(
            Combination::Union.heading(&ellipses, &polygons),
            heading(&figure)
        );
        assert_eq!(
            Combination::Minus.heading(&heading(&figure), &ellipses),
            heading(&figure)
        );
        assert_eq!(
            Combination::IncludedMinus.heading(&ellipses, &heading(&figure)),
            ellipses
        );
        // A known relation takes the wider heading, which its value
        // carries; another value is treated as one of the wider type.
        let empty = Expr::Constant(relation(&ellipse).default_value().unwrap());
        assert_eq!(
            empty.treated(&relation(&figure)),
            Expr::Constant(relation(&figure).default_value().unwrap())
        );
        let value = Expr::Parameter("e".to_string());
        assert_eq!(
            value.clone().treated(&curve).treated(&figure),
            Expr::Treat(Box::new(figure.clone()), Box::new(value))
        );
    }

    #[test]
    fn a_subtype_defined_by_constraints_is_its_supertypes() {
        let constraint = |n: usize| Constraint {
            operator: OperatorId(n),
            text: String::new(),
        };
        // figure > ellipse > circle, ellipse stored, circle by constraint.
        let ellipse = Defined {
            name: "ellipse".to_string(),
            components: Heading::new([("a".to_string(), Type::Scalar(Scalar::Integer))].into()),
            representations: Vec::new(),
            constraints: vec![constraint(0)],
            keys: BTreeMap::new(),
            order: None,
            supertypes: vec!["figure".to_string()],
            specialized: true,
            specialization: None,
        };
        let subtype = |name: &str, n: usize| Defined {
            name: name.to_string(),
            constraints: vec![constraint(0), constraint(n)],
            supertypes: vec!["ellipse".to_string(), "figure".to_string()],
            specialized: false,
            specialization: Some(Specialization {
                supertype: Box::new(ellipse.clone()),
            }),
            ..ellipse.clone()
        };
        let (circle, small) = (subtype("circle", 1), subtype("small", 2));
        assert_eq!(circle.own_constraints(), [constraint(1)]);
        assert_eq!(ellipse.own_constraints(), [constraint(0)]);
        assert_eq!(circle.stored_as().name, "ellipse");
        assert_eq!(circle.written(), "ellipse");
        let ty = |d: &Defined| Type::Defined(Box::new(d.clone()));
        assert!(ty(&circle).is_subtype_of(&ty(&ellipse)));
        assert!(!ty(&ellipse).is_subtype_of(&ty(&circle)));
        assert_eq!(ty(&circle).root(), Some("figure"));
        // Two subtypes of a regular type meet at it, and higher up at the
        // union type.
        assert_eq!(
            ty(&circle).common_supertype(&ty(&small)),
            Some(ty(&ellipse))
        );
        let figure = Type::Dummy(Box::new(Dummy {
            name: "figure".to_string(),
            supertypes: Vec::new(),
        }));
        let polygon = Type::Defined(Box::new(Defined {
            name: "polygon".to_string(),
            supertypes: vec!["figure".to_string()],
            specialized: false,
            ..ellipse.clone()
        }));
        assert_eq!(ty(&circle).common_supertype(&polygon), Some(figure));
        // A regular type with such subtypes and no supertype is a root.
        let alone = Defined {
            supertypes: Vec::new(),
            ..ellipse.clone()
        };
        assert_eq!(ty(&alone).root(), Some("ellipse"));
    }

    #[test]
    fn a_type_parameter_has_no_default_but_a_relation_or_an_array_of_one_has() {
        let t = Type::Parameter(TypeParameter {
            index: 0,
            name: Some("T".to_string()),
            any: Any::Type,
        });
        assert_eq!(t.default_value(), None);
        let tuple = Type::Tuple(Heading::new([("v".to_string(), t.clone())].into()));
        assert_eq!(tuple.default_value(), None);
        let relation = Type::Relation(Heading::new([("v".to_string(), t.clone())].into()));
        let array = Type::Array(Box::new(relation.clone()));
        for ty in [relation, array] {
            let empty = ty.default_value().expect("it starts empty");
            assert_eq!(empty.ty(), ty);
            assert_eq!(
                empty.instantiated(&[Type::Scalar(Scalar::Integer)]).ty(),
                ty.instantiated(&[Type::Scalar(Scalar::Integer)])
            );
        }
    }

    #[test]
    fn instantiating_replaces_type_parameters() {
        let t = TypeParameter {
            index: 0,
            name: Some("T".to_string()),
            any: Any::Type,
        };
        let generic = Type::Relation(Heading::new(
            [("v".to_string(), Type::Parameter(t.clone()))].into(),
        ));
        assert_eq!(generic.to_string(), "relation { v: T }");
        let text = Type::Scalar(Scalar::Text);
        let types = [text.clone()];
        assert_eq!(
            generic.instantiated(&types).to_string(),
            "relation { v: text }"
        );
        // An empty relation of it, and how its values are compared.
        let compared = Expr::Binary(
            BinaryOp::Compare(Comparison::Equal, Compared::of(&Type::Parameter(t))),
            Box::new(Expr::Constant(
                generic.default_value().expect("a relation has a default"),
            )),
            Box::new(Expr::Parameter("a".to_string())),
        );
        let Expr::Binary(op, lhs, _) = compared.instantiated(&types) else {
            panic!("a comparison");
        };
        assert_eq!(
            op,
            BinaryOp::Compare(Comparison::Equal, Compared::Scalar(Scalar::Text))
        );
        assert_eq!(
            lhs.type_with(&|_| text.clone()).to_string(),
            "relation { v: text }"
        );
    }

    #[test]
    fn relations() {
        let heading = Heading::new([("a".to_string(), Type::Scalar(Scalar::Integer))].into());
        let ty = Type::Relation(heading.clone());
        assert_eq!(ty.to_string(), "relation { a: integer }");
        let Some(Value::Relation(empty)) = ty.default_value() else {
            panic!("a relation's default is a relation");
        };
        assert!(empty.tuples.is_empty());
        assert_eq!(Value::Relation(empty).ty(), ty);

        let tuple = |n| TupleValue::new([("a".to_string(), Value::Integer(n))].into());
        let r = RelationValue {
            heading,
            tuples: [tuple(2), tuple(1), tuple(2)].into(),
        };
        assert_eq!(r.tuples.len(), 2, "a set has no duplicates");
    }

    #[test]
    fn boolean_operators() {
        let decided = |c: Connective| [false, true].map(|value| c.decided(value));
        assert_eq!(decided(Connective::And), [Some(false), None]);
        assert_eq!(decided(Connective::Or), [None, Some(true)]);
        assert_eq!(decided(Connective::Xor), [None, None]);
        let identities =
            [Connective::And, Connective::Or, Connective::Xor].map(Connective::identity);
        assert_eq!(identities, [true, false, false]);
        assert_eq!(
            UnaryOp::Not.apply(&Value::Boolean(true)),
            Ok(Value::Boolean(false))
        );
    }

    #[test]
    fn instantiating_keeps_connectives_in_normal_form() {
        let t = Type::Parameter(TypeParameter {
            index: 0,
            name: Some("T".to_string()),
            any: Any::Type,
        });
        let param = |name: &str| Box::new(Expr::Parameter(name.to_string()));
        let compare = |comparison| {
            Expr::Binary(
                BinaryOp::Compare(comparison, Compared::of(&t)),
                param("a"),
                param("b"),
            )
        };
        let boolean = [Type::Scalar(Scalar::Boolean)];
        let integer = [Type::Scalar(Scalar::Integer)];
        // `a <> b` of booleans is `a xor b`,
        assert_eq!(
            compare(Comparison::NotEqual).instantiated(&boolean),
            Expr::Connective(Connective::Xor, vec![*param("a"), *param("b")])
        );
        // `a = b` is `not (a xor b)`, which an `xor` takes in,
        let c = *param("c");
        let Expr::Unary(UnaryOp::Not, xor) =
            Expr::Connective(Connective::Xor, vec![compare(Comparison::Equal), c.clone()])
                .instantiated(&boolean)
        else {
            panic!("a `not`");
        };
        assert_eq!(
            *xor,
            Expr::Connective(Connective::Xor, vec![*param("a"), *param("b"), c])
        );
        // and a `not` goes into a comparison of scalars,
        let not = |e| Expr::Unary(UnaryOp::Not, Box::new(e));
        let Expr::Binary(op, ..) = not(compare(Comparison::Less)).instantiated(&integer) else {
            panic!("a comparison");
        };
        assert_eq!(
            op,
            BinaryOp::Compare(
                Comparison::GreaterOrEqual,
                Compared::Scalar(Scalar::Integer)
            )
        );
        // in the operands of a connective too.
        let either = Expr::Connective(
            Connective::Or,
            vec![
                not(compare(Comparison::Less)),
                not(compare(Comparison::Greater)),
            ],
        );
        let Expr::Connective(Connective::Or, operands) = either.instantiated(&integer) else {
            panic!("an `or`");
        };
        assert!(
            operands.iter().all(|o| matches!(o, Expr::Binary(..))),
            "{operands:?}"
        );
    }

    #[test]
    fn approximate_operators() {
        let r = |x| Value::Approximate(Approximate::new(x));
        let apply = |op: BinaryOp, a, b| op.apply(&r(a), &r(b), &Converted::default());
        assert_eq!(
            apply(BinaryOp::Add(Numeric::Approximate), 0.5, 0.25),
            Ok(r(0.75))
        );
        assert_eq!(apply(BinaryOp::Divide, 1.0, 4.0), Ok(r(0.25)));
        assert_eq!(
            apply(BinaryOp::Divide, 1.0, 0.0),
            Err(Failure::DivisionByZero)
        );
        assert_eq!(
            apply(BinaryOp::Multiply(Numeric::Approximate), 1e300, 1e10),
            Err(Failure::Overflow(Scalar::Approximate))
        );
        assert_eq!(
            apply(BinaryOp::Divide, 1e300, 1e-10),
            Err(Failure::Overflow(Scalar::Approximate))
        );
        // There is one zero: where IEEE 754 makes -0.0, the result is 0.0,
        // whose bits are all clear.
        let bits = |v: Result<Value, Failure>| match v {
            Ok(Value::Approximate(x)) => x.to_bits(),
            other => panic!("{other:?}"),
        };
        let multiply = BinaryOp::Multiply(Numeric::Approximate);
        assert_eq!(bits(apply(multiply, -5.0, 0.0)), 0);
        assert_eq!(bits(apply(BinaryOp::Divide, -5e-324, 4.0)), 0);
        let negate = UnaryOp::Negate(Numeric::Approximate);
        assert_eq!(bits(negate.apply(&r(0.0))), 0);
    }

    #[test]
    fn comparisons() {
        let holds = |c: Comparison, a: Value, b: Value| {
            let Type::Scalar(scalar) = a.ty() else {
                panic!("not a scalar");
            };
            BinaryOp::Compare(c, Compared::Scalar(scalar)).apply(&a, &b, &Converted::default())
                == Ok(Value::Boolean(true))
        };
        let r = |x| Value::Approximate(Approximate::new(x));
        let t = |s: &str| Value::Text(s.to_string());
        assert!(holds(
            Comparison::Less,
            Value::Boolean(false),
            Value::Boolean(true)
        ));
        assert!(holds(
            Comparison::Less,
            Value::Integer(-2),
            Value::Integer(1)
        ));
        assert!(holds(Comparison::Less, r(-5e-324), r(0.0)));
        assert!(holds(Comparison::Equal, r(-0.0), r(0.0)));
        assert!(holds(Comparison::Equal, r(0.5), r(0.5)));
        // By code point: `Z` before `a`, and `é` (U+E9) before `😀`.
        assert!(holds(Comparison::Less, t("Z"), t("a")));
        assert!(holds(Comparison::Less, t("é"), t("😀")));
        assert!(holds(Comparison::Less, t("ab"), t("abc")));
        assert!(holds(Comparison::LessOrEqual, t(""), t("")));
        assert!(holds(
            Comparison::GreaterOrEqual,
            Value::Integer(3),
            Value::Integer(3)
        ));
        assert!(!holds(
            Comparison::Greater,
            Value::Integer(3),
            Value::Integer(3)
        ));
    }

    #[test]
    fn tuple_and_relation_equality() {
        let tuple = |a, b: &str| {
            Value::Tuple(TupleValue::new(
                [
                    ("a".to_string(), Value::Integer(a)),
                    ("b".to_string(), Value::Text(b.to_string())),
                ]
                .into(),
            ))
        };
        let equal = BinaryOp::Compare(Comparison::Equal, Compared::Tuple);
        assert_eq!(
            equal.apply(&tuple(1, "x"), &tuple(1, "x"), &Converted::default()),
            Ok(Value::Boolean(true))
        );
        assert_eq!(
            equal.apply(&tuple(1, "x"), &tuple(1, "y"), &Converted::default()),
            Ok(Value::Boolean(false))
        );
        let relation = |tuples: &[Value]| {
            let heading = match tuples.first().map(Value::ty) {
                Some(Type::Tuple(heading)) => heading,
                _ => Heading::new(BTreeMap::new()),
            };
            Value::Relation(RelationValue {
                heading,
                tuples: tuples
                    .iter()
                    .map(|t| match t {
                        Value::Tuple(t) => t.clone(),
                        _ => unreachable!(),
                    })
                    .collect(),
            })
        };
        let not_equal = BinaryOp::Compare(Comparison::NotEqual, Compared::Relation);
        let ab = relation(&[tuple(1, "x"), tuple(2, "y")]);
        let ba = relation(&[tuple(2, "y"), tuple(1, "x")]);
        assert_eq!(
            not_equal.apply(&ab, &ba, &Converted::default()),
            Ok(Value::Boolean(false)),
            "sets"
        );
        assert_eq!(
            not_equal.apply(&ab, &relation(&[tuple(1, "x")]), &Converted::default()),
            Ok(Value::Boolean(true))
        );
    }

    #[test]
    fn remove() {
        let names = |n: &[&str]| n.iter().map(|s| (*s).to_string()).collect::<BTreeSet<_>>();
        let tuple = |a, b| {
            TupleValue::new(
                [
                    ("a".to_string(), Value::Integer(a)),
                    ("b".to_string(), Value::Integer(b)),
                ]
                .into(),
            )
        };
        let relation = Value::Relation(RelationValue {
            heading: Heading::new(
                [
                    ("a".to_string(), Type::Scalar(Scalar::Integer)),
                    ("b".to_string(), Type::Scalar(Scalar::Integer)),
                ]
                .into(),
            ),
            tuples: [tuple(1, 1), tuple(1, 2), tuple(2, 1)].into(),
        });
        let Value::Relation(without_b) = relation.without(&names(&["b"])) else {
            panic!("a relation");
        };
        assert_eq!(without_b.tuples.len(), 2, "duplicates are dropped");
        assert_eq!(without_b.heading.attributes().count(), 1);
        // Degree zero: table_dee, since there were tuples.
        let Value::Relation(dee) = relation.without(&names(&["a", "b"])) else {
            panic!("a relation");
        };
        assert_eq!(dee.tuples.len(), 1);
        assert_eq!(
            Value::Tuple(tuple(1, 2))
                .without(&names(&["a"]))
                .ty()
                .to_string(),
            "tuple { b: integer }"
        );
    }

    #[test]
    fn rename() {
        let renamings = |pairs: &[(&str, &str)]| {
            pairs
                .iter()
                .map(|(a, b)| ((*a).to_string(), (*b).to_string()))
                .collect::<BTreeMap<_, _>>()
        };
        let tuple = |pairs: &[(&str, i64)]| {
            TupleValue::new(
                pairs
                    .iter()
                    .map(|(n, v)| ((*n).to_string(), Value::Integer(*v)))
                    .collect(),
            )
        };
        let t = Value::Tuple(tuple(&[("a", 1), ("b", 2)]));
        // All at once, so a swap is a swap.
        assert_eq!(
            t.renamed(&renamings(&[("a", "b"), ("b", "a")])),
            Value::Tuple(tuple(&[("a", 2), ("b", 1)]))
        );
        assert_eq!(
            t.renamed(&renamings(&[("a", "z")])).ty().to_string(),
            "tuple { b: integer, z: integer }"
        );
    }

    #[test]
    fn concatenation() {
        let t = |s: &str| Value::Text(s.to_string());
        assert_eq!(
            BinaryOp::Concatenate.apply(&t("ab"), &t("é"), &Converted::default()),
            Ok(t("abé"))
        );
        assert_eq!(
            BinaryOp::Concatenate.apply(&t(""), &t(""), &Converted::default()),
            Ok(t(""))
        );
    }

    #[test]
    fn integer_operators() {
        let apply = |op: BinaryOp, a, b| {
            op.apply(
                &Value::Integer(a),
                &Value::Integer(b),
                &Converted::default(),
            )
        };
        let ok = |n| Ok(Value::Integer(n));
        assert_eq!(apply(BinaryOp::Add(Numeric::Integer), 2, 3), ok(5));
        assert_eq!(apply(BinaryOp::Subtract(Numeric::Integer), 2, 3), ok(-1));
        assert_eq!(apply(BinaryOp::Multiply(Numeric::Integer), -2, 3), ok(-6));
        // Truncating: the quotient rounds toward zero, and the remainder has
        // the dividend's sign.
        assert_eq!(apply(BinaryOp::Div, -7, 2), ok(-3));
        assert_eq!(apply(BinaryOp::Mod, -7, 2), ok(-1));
        assert_eq!(apply(BinaryOp::Div, 7, -2), ok(-3));
        assert_eq!(apply(BinaryOp::Mod, 7, -2), ok(1));
        assert_eq!(
            apply(BinaryOp::Add(Numeric::Integer), i64::MAX, 1),
            Err(Failure::Overflow(Scalar::Integer))
        );
        assert_eq!(
            apply(BinaryOp::Subtract(Numeric::Integer), i64::MIN, 1),
            Err(Failure::Overflow(Scalar::Integer))
        );
        assert_eq!(
            apply(BinaryOp::Multiply(Numeric::Integer), i64::MAX, 2),
            Err(Failure::Overflow(Scalar::Integer))
        );
        assert_eq!(
            apply(BinaryOp::Div, i64::MIN, -1),
            Err(Failure::Overflow(Scalar::Integer))
        );
        assert_eq!(apply(BinaryOp::Mod, i64::MIN, -1), ok(0));
        assert_eq!(apply(BinaryOp::Div, 1, 0), Err(Failure::DivisionByZero));
        assert_eq!(apply(BinaryOp::Mod, 1, 0), Err(Failure::DivisionByZero));
        assert_eq!(
            UnaryOp::Negate(Numeric::Integer).apply(&Value::Integer(i64::MIN)),
            Err(Failure::Overflow(Scalar::Integer))
        );
        assert_eq!(
            UnaryOp::Negate(Numeric::Integer).apply(&Value::Integer(5)),
            ok(-5)
        );
    }

    #[test]
    fn loads_order_tuples_by_their_items() {
        let tuple = |a: i64, b: &str| {
            TupleValue::new(
                [
                    ("a".to_string(), Value::Integer(a)),
                    ("b".to_string(), Value::Text(b.to_string())),
                ]
                .into(),
            )
        };
        let relation = RelationValue {
            heading: Heading::new(
                [
                    ("a".to_string(), Type::Scalar(Scalar::Integer)),
                    ("b".to_string(), Type::Scalar(Scalar::Text)),
                ]
                .into(),
            ),
            tuples: [tuple(1, "x"), tuple(2, "y"), tuple(1, "z")].into(),
        };
        let item = |attribute: &str, descending| OrderItem {
            attribute: attribute.to_string(),
            descending,
        };
        let loaded = |items: &[OrderItem]| {
            let loading = Loading {
                order: items.to_vec(),
                removed: BTreeSet::new(),
                loaded: Loaded::Tuples,
            };
            Value::Array(relation.loaded(&loading, &Converted::default())).to_string()
        };
        assert_eq!(
            loaded(&[item("a", true), item("b", true)]),
            r#"array [tuple { a: 2, b: "y" }, tuple { a: 1, b: "z" }, tuple { a: 1, b: "x" }]"#
        );
        // Tuples equal on every item come in canonical order, as in the
        // runtime, which the language leaves undefined.
        assert_eq!(
            loaded(&[item("a", false)]),
            r#"array [tuple { a: 1, b: "x" }, tuple { a: 1, b: "z" }, tuple { a: 2, b: "y" }]"#
        );
        let names = RelationValue {
            heading: Heading::new([("b".to_string(), Type::Scalar(Scalar::Text))].into()),
            tuples: ["x", "z", "y"]
                .map(|b| TupleValue::new([("b".to_string(), Value::Text(b.to_string()))].into()))
                .into(),
        };
        let values = Loading {
            order: vec![item("b", true)],
            removed: BTreeSet::new(),
            loaded: Loaded::Values,
        };
        assert_eq!(
            Value::Array(names.loaded(&values, &Converted::default())).to_string(),
            r#"array ["z", "y", "x"]"#
        );
        // Ordered by an attribute that the elements then leave out, which
        // keeps those that turn out equal.
        let without_b = Loading {
            order: vec![item("b", false)],
            removed: ["b".to_string()].into(),
            loaded: Loaded::Values,
        };
        assert_eq!(
            Value::Array(relation.loaded(&without_b, &Converted::default())).to_string(),
            "array [1, 2, 1]"
        );
        let tuples = Loading {
            loaded: Loaded::Tuples,
            ..without_b
        };
        assert_eq!(
            Value::Array(relation.loaded(&tuples, &Converted::default())).to_string(),
            "array [tuple { a: 1 }, tuple { a: 2 }, tuple { a: 1 }]"
        );
    }

    #[test]
    fn approximate_equality_is_identity() {
        assert_eq!(Approximate::new(-0.0).to_bits(), 0);
        assert_eq!(Approximate::new(-0.0), Approximate::new(0.0));
        assert_eq!(Approximate::new(1.5), Approximate::new(1.5));
        assert_ne!(Approximate::new(1.5), Approximate::new(-1.5));
    }

    #[test]
    fn approximates_sort_by_value() {
        let mut values = [1.0, f64::MAX, 0.0, -1.0, 5e-324, -f64::MAX].map(Approximate::new);
        values.sort();
        let bits = values.map(Approximate::to_bits);
        let expected = [-f64::MAX, -1.0, 0.0, 5e-324, 1.0, f64::MAX].map(f64::to_bits);
        assert_eq!(bits, expected);
    }
}
