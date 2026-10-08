//! Low-level IR: what codegen consumes.
//!
//! Functions are control-flow graphs of basic blocks. There is no nesting and
//! nothing implicit: every jump and return is spelled out. Relational
//! operators will arrive here already fused into explicit loops.
//!
//! Static data is described field by field rather than as bytes, so the
//! description doesn't depend on the target: codegen lays fields out with the
//! target's byte order and pointer size.
//!
//! Value representations:
//! - `boolean`: one [`Field::U8`], `0` for false and `1` for true.
//! - `integer`: one [`Field::I64`].
//! - `rational`: one [`Field::F64Bits`], a 64-bit IEEE 754 binary float.
//! - `text`: an [`Field::Address`] of a read-only constant holding the UTF-8
//!   bytes, then a [`Field::Usize`] with the length in bytes.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Program {
    pub functions: Vec<Function>,
    pub data: Vec<Data>,
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

/// Index into [`Program::data`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DataId(pub usize);

/// A statically allocated object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Data {
    pub kind: DataKind,
    pub writable: bool,
    /// Laid out in order, each at its natural alignment.
    pub fields: Vec<Field>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DataKind {
    /// A program variable. `name` is unique among variables; codegen derives
    /// the symbol from it.
    Variable { name: String },
    /// Data with no name in the program, such as a text literal's bytes.
    Constant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Field {
    U8(u8),
    I64(i64),
    /// A 64-bit IEEE 754 float, as its exact bit pattern, so that no value
    /// is ever compared or changed by float arithmetic on the way.
    F64Bits(u64),
    /// An unsigned integer the size of a pointer.
    Usize(u64),
    /// The address of another data object.
    Address(DataId),
    /// Raw bytes, with no alignment.
    Bytes(Vec<u8>),
}
