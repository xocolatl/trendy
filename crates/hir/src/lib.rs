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
        expr.type_with(&|leaf| match leaf {
            Expr::Variable(id) => self.variable(*id).ty.clone(),
            Expr::Parameter(name) => match &self.operator(within).params[name] {
                ParamType::Type(ty) => ty.clone(),
                ParamType::Any => unreachable!("only built-ins have `any` parameters"),
            },
            _ => unreachable!("only variables and parameters are leaves"),
        })
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
    /// The tuple at hand, with this heading: the one an `Extend` is computing
    /// attributes for, `depth` `Extend`s out from here, counting the
    /// innermost as 0. Its attributes are in scope by name in the source,
    /// and are extracted from it here.
    Current(usize, Heading),
}

impl Expr {
    /// The expression's type, where `leaf` gives the type of each variable
    /// and parameter in it.
    ///
    /// # Panics
    ///
    /// If the expression isn't well typed, which sema rules out.
    #[must_use]
    pub fn type_with(&self, leaf: &dyn Fn(&Expr) -> Type) -> Type {
        match self {
            Expr::Constant(value) => value.ty(),
            Expr::Variable(_) | Expr::Parameter(_) => leaf(self),
            Expr::Unary(op, _) => Type::Scalar(op.result()),
            Expr::Binary(op, ..) => Type::Scalar(op.result()),
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
    Overflow,
    DivisionByZero,
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
}

impl Numeric {
    #[must_use]
    pub fn scalar(self) -> Scalar {
        match self {
            Numeric::Integer => Scalar::Integer,
            Numeric::Approximate => Scalar::Approximate,
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
        Err(Failure::Overflow)
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
            BinaryOp::Divide => Scalar::Approximate,
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
    Approximate,
    /// A sequence of Unicode characters, stored as UTF-8.
    Text,
}

impl Scalar {
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

    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Type> {
        self.0.get(name)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
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
            Type::Scalar(_) => panic!("removing attributes from {self}"),
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
            Type::Scalar(_) => panic!("adding attributes to {self}"),
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
            Type::Scalar(_) => panic!("renaming attributes of {self}"),
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

    /// The value as a literal, which always has an exponent: the shortest
    /// one that reads back as the same value, as in `1e0`, `2.5e0`, `5e-1`
    /// or `-0e0`. The runtime writes values the same way. `None` for an
    /// infinity or a NaN, which no operator produces.
    #[must_use]
    pub fn literal(self) -> Option<String> {
        self.0.is_finite().then(|| format!("{:e}", self.0))
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
            Err(Failure::Overflow)
        );
        assert_eq!(
            apply(BinaryOp::Divide, 1e300, 1e-10),
            Err(Failure::Overflow)
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
