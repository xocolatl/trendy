//! High-level IR: the checked program handed from sema onward.
//!
//! Tree-shaped like the source, but every name is resolved and every
//! invariant sema promises holds. This is where relational algebra will live
//! and where the optimizer will rewrite it. Nothing here refers to syntax.
//!
//! Variables and operators share one namespace; types have their own.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Program {
    pub operators: Vec<Operator>,
    pub variables: Vec<Variable>,
    /// The operator the executable starts in.
    pub entry: OperatorId,
}

impl Program {
    #[must_use]
    pub fn operator(&self, id: OperatorId) -> &Operator {
        &self.operators[id.0]
    }
}

/// Index into [`Program::operators`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct OperatorId(pub usize);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Operator {
    /// Unique within the program.
    pub name: String,
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

/// A built-in scalar type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Type {
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

impl Type {
    /// In alphabetical order, as messages list them.
    pub const ALL: [Type; 4] = [Type::Boolean, Type::Integer, Type::Rational, Type::Text];

    /// The type's name in source code.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Type::Boolean => "boolean",
            Type::Integer => "integer",
            Type::Rational => "rational",
            Type::Text => "text",
        }
    }

    /// The built-in type called `name`, if there is one.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Type> {
        Type::ALL.into_iter().find(|t| t.name() == name)
    }

    /// The value a variable of this type starts with when none is given.
    #[must_use]
    pub fn default_value(self) -> Value {
        match self {
            Type::Boolean => Value::Boolean(false),
            Type::Integer => Value::Integer(0),
            Type::Rational => Value::Rational(Rational::new(0.0)),
            Type::Text => Value::Text(String::new()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Value {
    Boolean(bool),
    Integer(i64),
    Rational(Rational),
    Text(String),
}

impl Value {
    #[must_use]
    pub fn ty(&self) -> Type {
        match self {
            Value::Boolean(_) => Type::Boolean,
            Value::Integer(_) => Type::Integer,
            Value::Rational(_) => Type::Rational,
            Value::Text(_) => Type::Text,
        }
    }
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
