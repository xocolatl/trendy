//! High-level IR: the checked program handed from sema onward.
//!
//! Tree-shaped like the source, but every name is resolved and every
//! invariant sema promises holds. This is where relational algebra will live
//! and where the optimizer will rewrite it. Nothing here refers to syntax.
//!
//! Operators, variables and types each have a namespace of their own.

use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Program {
    /// The prelude's built-in operators, then the program's own.
    pub operators: Vec<Operator>,
    pub variables: Vec<Variable>,
    /// The operator the executable starts in: a user operator with no
    /// parameters.
    pub entry: OperatorId,
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
    pub fn type_of(&self, expr: &Expr, params: &BTreeMap<String, Type>, locals: &[Local]) -> Type {
        expr.type_with(&|leaf| match leaf {
            Expr::Variable(id) => self.variable(*id).ty.clone(),
            Expr::Parameter(name) => params[name].clone(),
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
            OperatorKind::Builtin(builtin) => builtin.result(),
            OperatorKind::Update { .. } => None,
        };
        result
            .expect("only read-only operators are called in expressions")
            .instantiated(&call.types)
    }
}

/// Index into [`Program::operators`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct OperatorId(pub usize);

/// Index into [`Program::variables`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VariableId(pub usize);

/// Index into an update operator's local variables
/// ([`OperatorKind::Update`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LocalId(pub usize);

/// A local variable of an update operator, purely local to it (TTM RM
/// Prescription 3): in scope in its whole body, and assigned its initial
/// value where it is declared, which is before any use.
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
    /// Unique among operators.
    pub name: String,
    /// The type parameters, the `i`th with index `i`. Each is in some
    /// parameter's type, so a call's arguments determine it.
    pub types: Vec<TypeParameter>,
    /// The parameters, by name. A call's arguments are a tuple of them.
    pub params: BTreeMap<String, Type>,
    pub kind: OperatorKind,
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
                body: body.iter().map(|s| s.instantiated(types)).collect(),
            },
            OperatorKind::ReadOnly { result, value } => OperatorKind::ReadOnly {
                result: result.instantiated(types),
                value: value.instantiated(types),
            },
            OperatorKind::Builtin(builtin) => OperatorKind::Builtin(*builtin),
        };
        Operator {
            name: self.name.clone(),
            types: Vec::new(),
            params: self
                .params
                .iter()
                .map(|(name, ty)| (name.clone(), ty.instantiated(types)))
                .collect(),
            kind,
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

/// What a type parameter can be: `any type`, every type; `any tuple` or
/// `any relation`, every type that type generator makes; or any type of a
/// kind, such as `any ordered`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Any {
    Type,
    Tuple,
    Relation,
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
    /// An update operator defined in the program, with its local variables
    /// and its statements.
    Update {
        locals: Vec<Local>,
        body: Vec<Statement>,
    },
    /// A read-only operator defined in the program: its value, of type
    /// `result`, computed from its parameters. It changes nothing, and
    /// calls no operator but read-only ones, none of which calls itself.
    ReadOnly { result: Type, value: Expr },
    /// Declared in the prelude and implemented by the compiler: an update
    /// operator, or a read-only one, with a [`Builtin::result`].
    Builtin(Builtin),
}

/// The operators the compiler implements. Each is declared in the prelude,
/// and sema checks the declaration against [`Builtin::params`] and
/// [`Builtin::result`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Builtin {
    /// Print a value to stderr as one line of trendy source.
    DebugValue,
    /// The number of tuples of a relation.
    Count,
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
}

impl Builtin {
    pub const ALL: [Builtin; 8] = [
        Builtin::DebugValue,
        Builtin::Count,
        Builtin::Min,
        Builtin::Max,
        Builtin::Sum,
        Builtin::Product,
        Builtin::ToText,
        Builtin::ToApproximate,
    ];

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Builtin::DebugValue => "debug_value",
            Builtin::Count => "count",
            Builtin::Min => "min",
            Builtin::Max => "max",
            Builtin::Sum => "sum",
            Builtin::Product => "product",
            Builtin::ToText => "to_text",
            Builtin::ToApproximate => "to_approximate",
        }
    }

    #[must_use]
    pub fn from_name(name: &str) -> Option<Builtin> {
        Builtin::ALL.into_iter().find(|b| b.name() == name)
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
            Builtin::DebugValue | Builtin::ToText => vec![unnamed(0, Any::Type)],
            Builtin::Count => vec![unnamed(0, Any::Relation)],
            Builtin::ToApproximate => Vec::new(),
            Builtin::Min | Builtin::Max => of_kind("ordered", &[Requirement::Order]),
            Builtin::Sum | Builtin::Product => of_kind(
                "numeric",
                &[Requirement::Addition, Requirement::Multiplication],
            ),
        }
    }

    /// The parameters the compiler's implementation takes, as it sees them:
    /// a `per` parameter is an attribute of its relation's tuples.
    #[must_use]
    pub fn params(self) -> BTreeMap<String, Type> {
        let types = self.types();
        let parameter = |index: usize| Type::Parameter(types[index].clone());
        match self {
            Builtin::DebugValue => [("v".to_string(), parameter(0))].into(),
            Builtin::Count => [("r".to_string(), parameter(0))].into(),
            Builtin::ToText => [("from".to_string(), parameter(0))].into(),
            Builtin::ToApproximate => [("from".to_string(), Type::Scalar(Scalar::Integer))].into(),
            Builtin::Min | Builtin::Max | Builtin::Sum | Builtin::Product => {
                let tuples = Heading::new(
                    [
                        ("of".to_string(), parameter(0)),
                        (wrapped("r"), parameter(1)),
                    ]
                    .into(),
                );
                [("r".to_string(), Type::Relation(tuples))].into()
            }
        }
    }

    /// The result type of the compiler's implementation, for a read-only
    /// built-in, whose call is a value.
    #[must_use]
    pub fn result(self) -> Option<Type> {
        match self {
            Builtin::DebugValue => None,
            Builtin::Count => Some(Type::Scalar(Scalar::Integer)),
            Builtin::Min | Builtin::Max | Builtin::Sum | Builtin::Product => {
                Some(Type::Parameter(self.types()[0].clone()))
            }
            Builtin::ToText => Some(Type::Scalar(Scalar::Text)),
            Builtin::ToApproximate => Some(Type::Scalar(Scalar::Approximate)),
        }
    }

    /// The value of a call of the read-only built-in with the arguments
    /// `args`, known when compiling, by parameter name.
    ///
    /// # Errors
    ///
    /// Returns why there is no value: `min` or `max` of a relation without
    /// tuples, which has neither, or a `sum` or `product` out of its type's
    /// range.
    ///
    /// # Panics
    ///
    /// If the built-in is an update operator, which has no value, or the
    /// arguments aren't those of [`Builtin::params`], which sema rules out.
    pub fn evaluate(self, args: &BTreeMap<String, Value>) -> Result<Value, BuiltinFailure> {
        let relation = || match &args["r"] {
            Value::Relation(r) => r,
            other => panic!("{} of {other:?}", self.name()),
        };
        let tuples = || &relation().tuples;
        let values = || {
            tuples()
                .iter()
                .map(|t| t.get("of").expect("`of` is an attribute"))
        };
        match self {
            Builtin::DebugValue => panic!("`debug_value` is an update operator"),
            Builtin::Count => {
                let count = i64::try_from(tuples().len()).expect("a relation's tuples fit");
                Ok(Value::Integer(count))
            }
            Builtin::Min => values()
                .min_by(|a, b| compare(a, b))
                .cloned()
                .ok_or(BuiltinFailure::NoTuples),
            Builtin::Max => values()
                .max_by(|a, b| compare(a, b))
                .cloned()
                .ok_or(BuiltinFailure::NoTuples),
            Builtin::Sum | Builtin::Product => {
                let of = relation().heading.get("of").expect("`of` is an attribute");
                let folded = if self == Builtin::Sum {
                    sum(of, values())
                } else {
                    product(of, values())
                };
                folded.map_err(BuiltinFailure::Failure)
            }
            Builtin::ToText => Ok(Value::Text(args["from"].to_string())),
            Builtin::ToApproximate => match args["from"] {
                Value::Integer(n) => Ok(Value::Approximate(Approximate::nearest(n))),
                ref other => panic!("`to_approximate` of {other:?}"),
            },
        }
    }
}

/// Why a call of a read-only built-in has no value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinFailure {
    /// `min` or `max` of a relation without tuples, which has neither.
    NoTuples,
    /// An operator's failure, such as a `sum` out of its type's range.
    Failure(Failure),
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
    /// The arguments, for the operator's parameters, by name.
    pub args: BTreeMap<String, Expr>,
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
}

impl Statement {
    /// The statement, in an operator instantiated with `types`.
    #[must_use]
    pub fn instantiated(&self, types: &[Type]) -> Statement {
        match self {
            Statement::Call(call) => Statement::Call(call.instantiated(types)),
            Statement::Assign(assignments) => Statement::Assign(
                assignments
                    .iter()
                    .map(|a| Assignment {
                        target: a.target,
                        value: a.value.instantiated(types),
                    })
                    .collect(),
            ),
        }
    }
}

/// `target := value`, in an assignment statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assignment {
    pub target: Target,
    pub value: Expr,
}

/// What an assignment assigns: a variable of the program, or a local
/// variable of the operator it is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Target {
    Variable(VariableId),
    Local(LocalId),
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
    /// A local variable of the update operator the expression is in.
    Local(LocalId),
    /// A monadic operator: `not x` or `-x`.
    Unary(UnaryOp, Box<Expr>),
    /// A dyadic operator, such as `x and y` or `x + y`.
    Binary(BinaryOp, Box<Expr>, Box<Expr>),
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
    /// have the same types.
    And(Box<Expr>, Box<Expr>),
    /// Two relations of the same heading combined, as the `Combination`
    /// says: the tuples of either, or of the left that aren't in the right,
    /// where the checked ones fail if a tuple breaks their condition.
    /// `intersect` is an `And`, of relations with all their attributes in
    /// common.
    Combine(Combination, Box<Expr>, Box<Expr>),
    /// The tuples of the left relation that have a match in the right, on
    /// their common attributes (`matching`, the semijoin): the result has
    /// the left's heading. The operands' common attributes have the same
    /// types.
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
    /// The only tuple of a relation (`tuple from`), which fails if it has
    /// none or several. `value from` is the extraction of its only
    /// attribute.
    Only(Box<Expr>),
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
            // Arithmetic on a type parameter's values has their type.
            Expr::Unary(op, operand) => op
                .operand()
                .map_or_else(|| operand.type_with(leaf), Type::Scalar),
            Expr::Binary(op, lhs, _) => op
                .result()
                .map_or_else(|| lhs.type_with(leaf), Type::Scalar),
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
            Expr::Extend(operand, computed, _) => operand.type_with(leaf).with(
                computed
                    .attributes()
                    .map(|(name, ty)| (name.to_string(), ty.clone())),
            ),
            Expr::Current(_, heading) => Type::Tuple(heading.clone()),
            Expr::Ungroup(operand, names) => operand.type_with(leaf).ungrouped(names),
            Expr::Restrict(operand, _)
            | Expr::Combine(_, operand, _)
            | Expr::Semijoin(operand, _)
            | Expr::Antijoin(operand, _) => operand.type_with(leaf),
            Expr::Only(operand) => match operand.type_with(leaf) {
                Type::Relation(heading) => Type::Tuple(heading),
                other => panic!("the only tuple of {other}"),
            },
        }
    }
}

impl Expr {
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
            Expr::Unary(op, operand) => Expr::Unary(op.instantiated(types), inst(operand)),
            Expr::Binary(op, lhs, rhs) => {
                Expr::Binary(op.instantiated(types), inst(lhs), inst(rhs))
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
            Expr::Current(depth, heading) => Expr::Current(*depth, heading.instantiated(types)),
            Expr::Call(call) => Expr::Call(call.instantiated(types)),
        }
    }
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

/// How relations of one heading are combined, by [`Expr::Combine`].
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

/// A dyadic operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BinaryOp {
    And,
    Or,
    /// Exactly one is true.
    Xor,
    /// False only when the left operand is true and the right is false.
    Implies,
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
    /// Relations of one relation type, equal when they have the same tuples,
    /// and one less than or equal to another when it is a subset of it.
    Relation,
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
            Type::Tuple(_) => Compared::Tuple,
            Type::Relation(_) => Compared::Relation,
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

/// The order of two values of the same scalar type: `false` before `true`,
/// integers numerically, approximate numbers by IEEE 754 totalOrder (so
/// `-0.0` before `0.0`, and two approximate numbers are equal exactly when
/// they are the same value), and text by code point, which is the order of
/// its UTF-8 bytes.
///
/// # Panics
///
/// If the values aren't of one scalar type, which sema rules out.
#[must_use]
pub fn compare(lhs: &Value, rhs: &Value) -> std::cmp::Ordering {
    match (lhs, rhs) {
        (Value::Boolean(a), Value::Boolean(b)) => a.cmp(b),
        (Value::Integer(a), Value::Integer(b)) => a.cmp(b),
        (Value::Approximate(a), Value::Approximate(b)) => a.cmp(b),
        (Value::Text(a), Value::Text(b)) => a.as_bytes().cmp(b.as_bytes()),
        _ => panic!("comparing {lhs:?} with {rhs:?}"),
    }
}

impl BinaryOp {
    /// The scalar type of both operands: `None` for tuples, relations and a
    /// type parameter's values.
    #[must_use]
    pub fn operand(self) -> Option<Scalar> {
        match self {
            BinaryOp::And | BinaryOp::Or | BinaryOp::Xor | BinaryOp::Implies => {
                Some(Scalar::Boolean)
            }
            BinaryOp::Add(numeric) | BinaryOp::Subtract(numeric) | BinaryOp::Multiply(numeric) => {
                numeric.scalar()
            }
            BinaryOp::Divide => Some(Scalar::Approximate),
            BinaryOp::Div | BinaryOp::Mod => Some(Scalar::Integer),
            BinaryOp::Concatenate => Some(Scalar::Text),
            BinaryOp::Compare(_, Compared::Scalar(scalar)) => Some(scalar),
            BinaryOp::Compare(_, Compared::Tuple | Compared::Relation | Compared::Parameter(_)) => {
                None
            }
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

    /// The result for `lhs` and `rhs`, which have the operand type.
    /// Approximate results are rounded as IEEE 754 binary64 arithmetic rounds
    /// them, to nearest, ties to even.
    ///
    /// # Errors
    ///
    /// Returns why there is no result: a result out of range, or a division
    /// by zero.
    ///
    /// # Panics
    ///
    /// If an operand isn't of the operand type, which sema rules out.
    pub fn apply(self, lhs: &Value, rhs: &Value) -> Result<Value, Failure> {
        match self {
            BinaryOp::Compare(comparison, Compared::Scalar(_)) => {
                return Ok(Value::Boolean(comparison.holds(compare(lhs, rhs))));
            }
            // Value equality is the language's: attribute-wise for tuples,
            // set equality for relations.
            BinaryOp::Compare(Comparison::Equal, _) => return Ok(Value::Boolean(lhs == rhs)),
            BinaryOp::Compare(Comparison::NotEqual, _) => return Ok(Value::Boolean(lhs != rhs)),
            BinaryOp::Compare(comparison, Compared::Relation) => {
                return Ok(Value::Boolean(contains(comparison, lhs, rhs)));
            }
            // Values of a type parameter's type, as the values are: of
            // `any relation`, ordered by inclusion, or of a kind that
            // requires an order.
            BinaryOp::Compare(comparison, Compared::Parameter(_)) => {
                let holds = match lhs {
                    Value::Relation(_) => contains(comparison, lhs, rhs),
                    _ => comparison.holds(compare(lhs, rhs)),
                };
                return Ok(Value::Boolean(holds));
            }
            BinaryOp::Compare(..) => panic!("tuples aren't ordered"),
            _ => {}
        }
        match (lhs, rhs) {
            (Value::Boolean(a), Value::Boolean(b)) => Ok(Value::Boolean(match self {
                BinaryOp::And => *a && *b,
                BinaryOp::Or => *a || *b,
                BinaryOp::Xor => a != b,
                BinaryOp::Implies => !a || *b,
                _ => panic!("`{self:?}` of booleans"),
            })),
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

/// A type: a built-in scalar type, a tuple type, a relation type, or a
/// type parameter of the operator it is written in.
///
/// Types are structural: two tuple types, or two relation types, are the
/// same type exactly when their headings are equal, wherever they are
/// written. A type parameter is a type of its own inside its operator, the
/// same type only as itself, and outside it means nothing: a call gives it
/// a type, which [`Type::instantiated`] puts in its place.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Type {
    Scalar(Scalar),
    Tuple(Heading),
    Relation(Heading),
    Parameter(TypeParameter),
}

impl Type {
    /// The type, in an operator instantiated with `types`: each type
    /// parameter in it replaced by its type, by index.
    #[must_use]
    pub fn instantiated(&self, types: &[Type]) -> Type {
        match self {
            Type::Scalar(_) => self.clone(),
            Type::Tuple(heading) => Type::Tuple(heading.instantiated(types)),
            Type::Relation(heading) => Type::Relation(heading.instantiated(types)),
            Type::Parameter(p) => types[p.index].clone(),
        }
    }

    /// The type parameters in the type.
    pub fn parameters(&self) -> Box<dyn Iterator<Item = &TypeParameter> + '_> {
        match self {
            Type::Scalar(_) => Box::new(std::iter::empty()),
            Type::Tuple(heading) | Type::Relation(heading) => {
                Box::new(heading.0.values().flat_map(Type::parameters))
            }
            Type::Parameter(p) => Box::new(std::iter::once(p)),
        }
    }

    /// The value a variable of this type starts with when none is given:
    /// for a tuple, each attribute's default.
    ///
    /// # Panics
    ///
    /// If the type has a type parameter in it, which only operators'
    /// parameters, and not variables, have.
    #[must_use]
    pub fn default_value(&self) -> Value {
        match self {
            Type::Scalar(scalar) => scalar.default_value(),
            Type::Tuple(heading) => Value::Tuple(TupleValue(
                heading
                    .attributes()
                    .map(|(name, ty)| (name.to_string(), ty.default_value()))
                    .collect(),
            )),
            // TTM's default for a relation variable: the empty relation.
            Type::Relation(heading) => Value::Relation(RelationValue {
                heading: heading.clone(),
                tuples: BTreeSet::new(),
            }),
            Type::Parameter(p) => panic!("no variable has the type parameter {p}"),
        }
    }
}

/// Source text for the type, with attributes in name order, as in
/// `tuple { a: integer, b: text }`.
impl std::fmt::Display for Type {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Type::Scalar(scalar) => f.write_str(scalar.name()),
            Type::Tuple(heading) => write!(f, "tuple {heading}"),
            Type::Relation(heading) => write!(f, "relation {heading}"),
            Type::Parameter(p) => write!(f, "{p}"),
        }
    }
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

    /// The attributes of both headings, whose common attributes have the
    /// same types.
    #[must_use]
    pub fn and(&self, other: &Heading) -> Heading {
        let mut attributes = self.0.clone();
        attributes.extend(other.0.iter().map(|(name, ty)| (name.clone(), ty.clone())));
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
}

impl Value {
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
        }
    }

    /// The value, in an operator instantiated with `types`: the only
    /// values with a type parameter in their types are relations whose
    /// headings have one, and so no tuples, or values with them inside.
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
            Value::Relation(relation) => {
                write!(f, "relation {} ", relation.heading)?;
                if relation.tuples.is_empty() {
                    return f.write_str("{}");
                }
                let tuples: Vec<String> = relation.tuples.iter().map(ToString::to_string).collect();
                write!(f, "{{ {} }}", tuples.join(", "))
            }
        }
    }
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

    /// This relation and `other`, of the same heading, combined as
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
            heading: lhs.heading.clone(),
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

    /// The tuple or relation type with `attributes` added, or replacing
    /// those it has.
    ///
    /// # Panics
    ///
    /// If the type isn't a tuple or relation type, which sema rules out.
    #[must_use]
    pub fn with(&self, attributes: impl IntoIterator<Item = (String, Type)>) -> Type {
        let added = |heading: &Heading| {
            let mut all = heading.0.clone();
            all.extend(attributes);
            Heading(all)
        };
        match self {
            Type::Tuple(heading) => Type::Tuple(added(heading)),
            Type::Relation(heading) => Type::Relation(added(heading)),
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

/// An `approximate` value: a 64-bit IEEE 754 binary floating-point number.
///
/// *The Third Manifesto* requires `=` to hold exactly when two values can't
/// be told apart by any operator (RM Prescription 8). Float equality breaks
/// that: NaN is not equal to itself, and `-0.0` equals `0.0` although `1/x`
/// tells them apart. So approximate numbers are compared by IEEE 754
/// totalOrder instead. Two approximate numbers are equal exactly when their
/// bits are identical, and they sort as
/// `-NaN < -∞ < … < -0.0 < 0.0 < … < ∞ < NaN`.
///
/// Every comparison, ordering and hash of an approximate number must go
/// through this type, never through `f64` operators.
#[derive(Debug, Clone, Copy)]
pub struct Approximate(f64);

impl Approximate {
    #[must_use]
    pub fn new(value: f64) -> Self {
        Approximate(value)
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
        Approximate(f64::from(high) * 4_294_967_296.0 + f64::from(low))
    }
}

/// The value as a literal, which always has an exponent: the shortest one
/// that reads back as the same value, as in `1e0`, `2.5e0`, `5e-1` or
/// `-0e0`. No operator makes an infinity or a NaN, which have none.
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
        assert_eq!(ty.default_value(), expected);
        assert_eq!(expected.ty(), ty);
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
            Box::new(Expr::Constant(generic.default_value())),
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
        let Value::Relation(empty) = ty.default_value() else {
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
        let table = |op: BinaryOp| {
            [(false, false), (false, true), (true, false), (true, true)]
                .map(|(a, b)| op.apply(&Value::Boolean(a), &Value::Boolean(b)).unwrap())
                .map(|v| v == Value::Boolean(true))
        };
        assert_eq!(table(BinaryOp::And), [false, false, false, true]);
        assert_eq!(table(BinaryOp::Or), [false, true, true, true]);
        assert_eq!(table(BinaryOp::Xor), [false, true, true, false]);
        assert_eq!(table(BinaryOp::Implies), [true, true, false, true]);
        assert_eq!(
            UnaryOp::Not.apply(&Value::Boolean(true)),
            Ok(Value::Boolean(false))
        );
    }

    #[test]
    fn approximate_operators() {
        let r = |x| Value::Approximate(Approximate::new(x));
        let apply = |op: BinaryOp, a, b| op.apply(&r(a), &r(b));
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
            apply(BinaryOp::Divide, 0.0, -0.0),
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
        // IEEE 754 sign rules: these are different values.
        assert_eq!(
            apply(BinaryOp::Add(Numeric::Approximate), -0.0, 0.0),
            Ok(r(0.0))
        );
        assert_eq!(
            apply(BinaryOp::Multiply(Numeric::Approximate), -5.0, 0.0),
            Ok(r(-0.0))
        );
        assert_eq!(
            UnaryOp::Negate(Numeric::Approximate).apply(&r(0.0)),
            Ok(r(-0.0))
        );
    }

    #[test]
    fn comparisons() {
        let holds = |c: Comparison, a: Value, b: Value| {
            let Type::Scalar(scalar) = a.ty() else {
                panic!("not a scalar");
            };
            BinaryOp::Compare(c, Compared::Scalar(scalar)).apply(&a, &b) == Ok(Value::Boolean(true))
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
        assert!(holds(Comparison::Less, r(-0.0), r(0.0)));
        assert!(holds(Comparison::NotEqual, r(-0.0), r(0.0)));
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
            equal.apply(&tuple(1, "x"), &tuple(1, "x")),
            Ok(Value::Boolean(true))
        );
        assert_eq!(
            equal.apply(&tuple(1, "x"), &tuple(1, "y")),
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
        assert_eq!(not_equal.apply(&ab, &ba), Ok(Value::Boolean(false)), "sets");
        assert_eq!(
            not_equal.apply(&ab, &relation(&[tuple(1, "x")])),
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
        assert_eq!(BinaryOp::Concatenate.apply(&t("ab"), &t("é")), Ok(t("abé")));
        assert_eq!(BinaryOp::Concatenate.apply(&t(""), &t("")), Ok(t("")));
    }

    #[test]
    fn integer_operators() {
        let apply = |op: BinaryOp, a, b| op.apply(&Value::Integer(a), &Value::Integer(b));
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
    fn approximate_equality_is_identity() {
        assert_ne!(Approximate::new(-0.0), Approximate::new(0.0));
        assert_eq!(Approximate::new(f64::NAN), Approximate::new(f64::NAN));
        assert_eq!(Approximate::new(1.5), Approximate::new(1.5));
    }

    #[test]
    fn approximates_sort_by_total_order() {
        let mut values = [
            f64::NAN,
            1.0,
            f64::INFINITY,
            0.0,
            -0.0,
            f64::NEG_INFINITY,
            -f64::NAN,
        ]
        .map(Approximate::new);
        values.sort();
        let bits = values.map(Approximate::to_bits);
        let expected = [
            -f64::NAN,
            f64::NEG_INFINITY,
            -0.0,
            0.0,
            1.0,
            f64::INFINITY,
            f64::NAN,
        ]
        .map(f64::to_bits);
        assert_eq!(bits, expected);
    }
}
