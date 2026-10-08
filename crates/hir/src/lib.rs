//! High-level IR: the checked program handed from sema onward.
//!
//! Tree-shaped like the source, but every name is resolved and every
//! invariant sema promises holds. This is where relational algebra will live
//! and where the optimizer will rewrite it. Nothing here refers to syntax.
//!
//! Variables and operators share one namespace; types have their own.

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

    /// The type of `expr`, inside the operator `within`.
    ///
    /// # Panics
    ///
    /// If `expr` names a parameter `within` doesn't have, or one of type
    /// `any`, which sema never produces.
    #[must_use]
    pub fn type_of(&self, expr: &Expr, within: OperatorId) -> Type {
        match expr {
            Expr::Constant(value) => value.ty(),
            Expr::Variable(id) => self.variable(*id).ty.clone(),
            Expr::Parameter(name) => match &self.operator(within).params[name] {
                ParamType::Type(ty) => ty.clone(),
                ParamType::Any => unreachable!("only built-ins have `any` parameters"),
            },
            Expr::Not(_) | Expr::Binary(..) => Type::Scalar(Scalar::Boolean),
        }
    }
}

/// Index into [`Program::operators`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct OperatorId(pub usize);

/// Index into [`Program::variables`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VariableId(pub usize);

/// An update operator: it returns nothing and is invoked by `call`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Operator {
    /// Unique among variables and operators.
    pub name: String,
    /// The parameters, by name. A call's arguments are a tuple of them.
    pub params: BTreeMap<String, ParamType>,
    pub kind: OperatorKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperatorKind {
    /// Defined in the program, with a body.
    User { body: Vec<Statement> },
    /// Declared in the prelude and implemented by the compiler.
    Builtin(Builtin),
}

/// The type of a parameter: a type, or `any`, which only built-in operators
/// use. `any` is not a type: no value or variable has it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParamType {
    Type(Type),
    Any,
}

impl std::fmt::Display for ParamType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParamType::Type(ty) => write!(f, "{ty}"),
            ParamType::Any => f.write_str("any"),
        }
    }
}

/// The operators the compiler implements. Each is declared in the prelude,
/// and sema checks the declaration against [`Builtin::params`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Builtin {
    /// Print a value to stderr as one line of trendy source.
    DebugValue,
}

impl Builtin {
    pub const ALL: [Builtin; 1] = [Builtin::DebugValue];

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Builtin::DebugValue => "debug_value",
        }
    }

    #[must_use]
    pub fn from_name(name: &str) -> Option<Builtin> {
        Builtin::ALL.into_iter().find(|b| b.name() == name)
    }

    /// The parameters the compiler's implementation takes.
    #[must_use]
    pub fn params(self) -> BTreeMap<String, ParamType> {
        match self {
            Builtin::DebugValue => [("v".to_string(), ParamType::Any)].into(),
        }
    }
}

/// A statement in an operator's body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Statement {
    /// `call name{arguments};`: the arguments are the callee's parameters,
    /// by name.
    Call {
        operator: OperatorId,
        args: BTreeMap<String, Expr>,
    },
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
    /// `not x`, of a boolean.
    Not(Box<Expr>),
    /// A dyadic boolean operator.
    Binary(BoolOp, Box<Expr>, Box<Expr>),
}

/// A dyadic boolean operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BoolOp {
    And,
    Or,
    /// Exactly one is true.
    Xor,
    /// False only when the left operand is true and the right is false.
    Implies,
}

impl BoolOp {
    #[must_use]
    pub fn apply(self, lhs: bool, rhs: bool) -> bool {
        match self {
            BoolOp::And => lhs && rhs,
            BoolOp::Or => lhs || rhs,
            BoolOp::Xor => lhs != rhs,
            BoolOp::Implies => !lhs || rhs,
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
}

/// A type: a built-in scalar type, a tuple type, or a relation type.
///
/// Types are structural: two tuple types, or two relation types, are the
/// same type exactly when their headings are equal, wherever they are
/// written.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Type {
    Scalar(Scalar),
    Tuple(Heading),
    Relation(Heading),
}

impl Type {
    /// The value a variable of this type starts with when none is given:
    /// for a tuple, each attribute's default.
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
        }
    }
}

/// Source text for the heading: `{ a: integer, b: text }` or `{}`.
impl std::fmt::Display for Heading {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.is_empty() {
            return f.write_str("{}");
        }
        f.write_str("{ ")?;
        for (i, (name, ty)) in self.attributes().enumerate() {
            if i > 0 {
                f.write_str(", ")?;
            }
            write!(f, "{name}: {ty}")?;
        }
        f.write_str(" }")
    }
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
    Rational,
    /// A sequence of Unicode characters, stored as UTF-8.
    Text,
}

impl Scalar {
    /// In alphabetical order, as messages list them.
    pub const ALL: [Scalar; 4] = [
        Scalar::Boolean,
        Scalar::Integer,
        Scalar::Rational,
        Scalar::Text,
    ];

    /// The type's name in source code.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Scalar::Boolean => "boolean",
            Scalar::Integer => "integer",
            Scalar::Rational => "rational",
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
            Scalar::Rational => Value::Rational(Rational::new(0.0)),
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

    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Type> {
        self.0.get(name)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// A value. Values are totally ordered, so that a relation's tuples form a
/// sorted set; the order has no meaning in the language.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Value {
    Boolean(bool),
    Integer(i64),
    Rational(Rational),
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
            Value::Rational(_) => scalar(Scalar::Rational),
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

/// A `rational` value: a 64-bit IEEE 754 binary floating-point number.
///
/// *The Third Manifesto* requires `=` to hold exactly when two values can't
/// be told apart by any operator (RM Prescription 8). Float equality breaks
/// that: NaN is not equal to itself, and `-0.0` equals `0.0` although `1/x`
/// tells them apart. So rationals are compared by IEEE 754 totalOrder
/// instead. Two rationals are equal exactly when their bits are identical,
/// and they sort as
/// `-NaN < -∞ < … < -0.0 < 0.0 < … < ∞ < NaN`.
///
/// Every comparison, ordering and hash of a rational must go through this
/// type, never through `f64` operators.
#[derive(Debug, Clone, Copy)]
pub struct Rational(f64);

impl Rational {
    #[must_use]
    pub fn new(value: f64) -> Self {
        Rational(value)
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
}

impl PartialEq for Rational {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == std::cmp::Ordering::Equal
    }
}

impl Eq for Rational {}

impl PartialOrd for Rational {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Rational {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.total_cmp(&other.0)
    }
}

impl std::hash::Hash for Rational {
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
        let table = |op: BoolOp| {
            [(false, false), (false, true), (true, false), (true, true)]
                .map(|(a, b)| op.apply(a, b))
        };
        assert_eq!(table(BoolOp::And), [false, false, false, true]);
        assert_eq!(table(BoolOp::Or), [false, true, true, true]);
        assert_eq!(table(BoolOp::Xor), [false, true, true, false]);
        assert_eq!(table(BoolOp::Implies), [true, true, false, true]);
    }

    #[test]
    fn rational_equality_is_identity() {
        assert_ne!(Rational::new(-0.0), Rational::new(0.0));
        assert_eq!(Rational::new(f64::NAN), Rational::new(f64::NAN));
        assert_eq!(Rational::new(1.5), Rational::new(1.5));
    }

    #[test]
    fn rationals_sort_by_total_order() {
        let mut values = [
            f64::NAN,
            1.0,
            f64::INFINITY,
            0.0,
            -0.0,
            f64::NEG_INFINITY,
            -f64::NAN,
        ]
        .map(Rational::new);
        values.sort();
        let bits = values.map(Rational::to_bits);
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
