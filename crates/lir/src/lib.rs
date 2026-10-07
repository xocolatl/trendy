//! Low-level IR: what codegen consumes.
//!
//! Functions are control-flow graphs of basic blocks. There is no nesting and
//! nothing implicit: every jump and return is spelled out. Relational
//! operators will arrive here already fused into explicit loops.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Program {
    pub functions: Vec<Function>,
    /// The function the executable starts in.
    pub entry: FunctionId,
}

/// Index into [`Program::functions`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FunctionId(pub usize);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Function {
    /// Unique within the program; codegen derives the symbol from it.
    pub name: String,
    /// The first block is the entry block.
    pub blocks: Vec<Block>,
}

/// Index into [`Function::blocks`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BlockId(pub usize);

/// Instructions will precede the terminator once the language has expressions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub terminator: Terminator,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Terminator {
    Return,
}
