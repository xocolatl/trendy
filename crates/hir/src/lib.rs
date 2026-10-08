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
            Expr::Unary(op, _) => Type::Scalar(op.result()),
            Expr::Binary(op, ..) => Type::Scalar(op.result()),
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
    /// A monadic operator: `not x` or `-x`.
    Unary(UnaryOp, Box<Expr>),
    /// A dyadic operator, such as `x and y` or `x + y`.
    Binary(BinaryOp, Box<Expr>, Box<Expr>),
}

/// Why an operator has no result: it is out of its type's range (for a
/// rational, beyond the largest finite magnitude), or it divides by zero.
/// Rationals are therefore always finite: no operator produces an infinity
/// or a NaN.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Failure {
    Overflow,
    DivisionByZero,
}

/// The type an arithmetic operator works on: both operands and the result
/// have it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Numeric {
    Integer,
    Rational,
}

impl Numeric {
    #[must_use]
    pub fn scalar(self) -> Scalar {
        match self {
            Numeric::Integer => Scalar::Integer,
            Numeric::Rational => Scalar::Rational,
        }
    }

    /// The numeric type `scalar` is, if it is one.
    #[must_use]
    pub fn of(scalar: Scalar) -> Option<Numeric> {
        match scalar {
            Scalar::Integer => Some(Numeric::Integer),
            Scalar::Rational => Some(Numeric::Rational),
            Scalar::Boolean | Scalar::Text => None,
        }
    }
}

/// A finite rational result, or why there is none.
fn finite(r: f64) -> Result<Value, Failure> {
    if r.is_finite() {
        Ok(Value::Rational(Rational::new(r)))
    } else {
        Err(Failure::Overflow)
    }
}

/// A monadic operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnaryOp {
    /// `not`, of a boolean.
    Not,
    /// `-`. It never fails for a rational, and only for the smallest
    /// integer.
    Negate(Numeric),
}

impl UnaryOp {
    /// The type of the operand.
    #[must_use]
    pub fn operand(self) -> Scalar {
        match self {
            UnaryOp::Not => Scalar::Boolean,
            UnaryOp::Negate(numeric) => numeric.scalar(),
        }
    }

    /// The type of the result.
    #[must_use]
    pub fn result(self) -> Scalar {
        self.operand()
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
            (UnaryOp::Negate(Numeric::Integer), Value::Integer(n)) => {
                n.checked_neg().map(Value::Integer).ok_or(Failure::Overflow)
            }
            (UnaryOp::Negate(Numeric::Rational), Value::Rational(r)) => {
                Ok(Value::Rational(Rational::new(-r.get())))
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
    /// `/`, of rationals.
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

/// What a comparison compares. Scalars are ordered; tuples and relations
/// only have `=` and `<>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Compared {
    Scalar(Scalar),
    /// Tuples of one tuple type, equal when every attribute is.
    Tuple,
    /// Relations of one relation type, equal when they have the same tuples.
    Relation,
}

impl Compared {
    /// What values of type `ty` are, for comparing them.
    #[must_use]
    pub fn of(ty: &Type) -> Compared {
        match ty {
            Type::Scalar(scalar) => Compared::Scalar(*scalar),
            Type::Tuple(_) => Compared::Tuple,
            Type::Relation(_) => Compared::Relation,
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
/// integers numerically, rationals by IEEE 754 totalOrder (so `-0.0` before
/// `0.0`, and two rationals are equal exactly when they are the same value),
/// and text by code point, which is the order of its UTF-8 bytes.
///
/// # Panics
///
/// If the values aren't of one scalar type, which sema rules out.
#[must_use]
pub fn compare(lhs: &Value, rhs: &Value) -> std::cmp::Ordering {
    match (lhs, rhs) {
        (Value::Boolean(a), Value::Boolean(b)) => a.cmp(b),
        (Value::Integer(a), Value::Integer(b)) => a.cmp(b),
        (Value::Rational(a), Value::Rational(b)) => a.cmp(b),
        (Value::Text(a), Value::Text(b)) => a.as_bytes().cmp(b.as_bytes()),
        _ => panic!("comparing {lhs:?} with {rhs:?}"),
    }
}

impl BinaryOp {
    /// The type of both operands.
    ///
    /// # Panics
    ///
    /// For a comparison of tuples or relations, whose operand types aren't
    /// scalars: see [`Compared`].
    #[must_use]
    pub fn operand(self) -> Scalar {
        match self {
            BinaryOp::And | BinaryOp::Or | BinaryOp::Xor | BinaryOp::Implies => Scalar::Boolean,
            BinaryOp::Add(numeric) | BinaryOp::Subtract(numeric) | BinaryOp::Multiply(numeric) => {
                numeric.scalar()
            }
            BinaryOp::Divide => Scalar::Rational,
            BinaryOp::Div | BinaryOp::Mod => Scalar::Integer,
            BinaryOp::Concatenate => Scalar::Text,
            BinaryOp::Compare(_, Compared::Scalar(scalar)) => scalar,
            BinaryOp::Compare(_, Compared::Tuple | Compared::Relation) => {
                panic!("tuple and relation comparisons have no scalar operand type")
            }
        }
    }

    /// The type of the result.
    #[must_use]
    pub fn result(self) -> Scalar {
        match self {
            BinaryOp::Compare(..) => Scalar::Boolean,
            _ => self.operand(),
        }
    }

    /// The result for `lhs` and `rhs`, which have the operand type. Rational
    /// results are rounded as IEEE 754 binary64 arithmetic rounds them, to
    /// nearest, ties to even.
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
            BinaryOp::Compare(..) => panic!("tuples and relations aren't ordered"),
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
                result.map(Value::Integer).ok_or(Failure::Overflow)
            }
            (Value::Text(a), Value::Text(b)) if self == BinaryOp::Concatenate => {
                Ok(Value::Text(format!("{a}{b}")))
            }
            (Value::Rational(a), Value::Rational(b)) => {
                let (a, b) = (a.get(), b.get());
                match self {
                    BinaryOp::Add(_) => finite(a + b),
                    BinaryOp::Subtract(_) => finite(a - b),
                    BinaryOp::Multiply(_) => finite(a * b),
                    // Either zero.
                    BinaryOp::Divide if b == 0.0 => Err(Failure::DivisionByZero),
                    BinaryOp::Divide => finite(a / b),
                    _ => panic!("`{self:?}` of rationals"),
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
    fn rational_operators() {
        let r = |x| Value::Rational(Rational::new(x));
        let apply = |op: BinaryOp, a, b| op.apply(&r(a), &r(b));
        assert_eq!(
            apply(BinaryOp::Add(Numeric::Rational), 0.5, 0.25),
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
            apply(BinaryOp::Multiply(Numeric::Rational), 1e300, 1e10),
            Err(Failure::Overflow)
        );
        assert_eq!(
            apply(BinaryOp::Divide, 1e300, 1e-10),
            Err(Failure::Overflow)
        );
        // IEEE 754 sign rules: these are different values.
        assert_eq!(
            apply(BinaryOp::Add(Numeric::Rational), -0.0, 0.0),
            Ok(r(0.0))
        );
        assert_eq!(
            apply(BinaryOp::Multiply(Numeric::Rational), -5.0, 0.0),
            Ok(r(-0.0))
        );
        assert_eq!(
            UnaryOp::Negate(Numeric::Rational).apply(&r(0.0)),
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
        let r = |x| Value::Rational(Rational::new(x));
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
            Err(Failure::Overflow)
        );
        assert_eq!(
            apply(BinaryOp::Subtract(Numeric::Integer), i64::MIN, 1),
            Err(Failure::Overflow)
        );
        assert_eq!(
            apply(BinaryOp::Multiply(Numeric::Integer), i64::MAX, 2),
            Err(Failure::Overflow)
        );
        assert_eq!(apply(BinaryOp::Div, i64::MIN, -1), Err(Failure::Overflow));
        assert_eq!(apply(BinaryOp::Mod, i64::MIN, -1), ok(0));
        assert_eq!(apply(BinaryOp::Div, 1, 0), Err(Failure::DivisionByZero));
        assert_eq!(apply(BinaryOp::Mod, 1, 0), Err(Failure::DivisionByZero));
        assert_eq!(
            UnaryOp::Negate(Numeric::Integer).apply(&Value::Integer(i64::MIN)),
            Err(Failure::Overflow)
        );
        assert_eq!(
            UnaryOp::Negate(Numeric::Integer).apply(&Value::Integer(5)),
            ok(-5)
        );
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
