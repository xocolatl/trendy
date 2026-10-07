//! High-level IR: the checked program handed from sema onward.
//!
//! Tree-shaped like the source, but every name is resolved and every
//! invariant sema promises holds. This is where relational algebra will live
//! and where the optimizer will rewrite it. Nothing here refers to syntax.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Program {
    pub operators: Vec<Operator>,
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
