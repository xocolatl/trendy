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
//! Value representations, each a single field with the matching [`Layout`]:
//! - `boolean`: [`Field::U8`], `0` for false and `1` for true.
//! - `integer`: [`Field::I64`].
//! - `rational`: [`Field::F64Bits`], a 64-bit IEEE 754 binary float.
//! - `text`: a [`Field::Struct`] of the [`Field::Address`] of a read-only
//!   constant holding the UTF-8 bytes, and a [`Field::Usize`] with their
//!   length.
//! - a relation: a [`Field::Struct`] of the [`Field::Address`] of a
//!   read-only constant holding its tuples, as consecutive structs in their
//!   canonical (sorted) order, and a [`Field::Usize`] with their number.
//!   Since each struct is padded to its alignment, the array's stride is the
//!   tuple's size.
//! - a tuple: a [`Field::Struct`] of its attributes' values, in attribute
//!   name order. Because a struct's layout doesn't depend on where it is, an
//!   attribute that is itself a tuple can be used, or passed, on its own.
//!
//! # Calling convention
//!
//! A function with parameters takes one pointer, to a struct of its
//! parameters in name order: a call's arguments are a tuple. The caller
//! builds that struct in a stack slot. A function without parameters takes
//! nothing.
//!
//! # Failures
//!
//! An arithmetic instruction whose result doesn't exist stops the program by
//! calling [`RuntimeFn::Fail`], which writes the error to stderr and exits
//! with status 1.
//!
//! # Type descriptors
//!
//! Runtime functions that accept any type, such as
//! [`RuntimeFn::DebugValue`], also receive a type descriptor: read-only data
//! laid out as `trendy_runtime::TypeDesc`, built by lowering from ordinary
//! fields. Its sizes and offsets are [`Field::Size`] and [`Field::Offset`],
//! which codegen resolves with the same layout rules it uses for values.

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
    /// The layout of the parameter struct, a [`Layout::Struct`], if the
    /// function takes parameters.
    pub params: Option<Layout>,
    /// Stack slots, each with the layout of what it holds.
    pub slots: Vec<Layout>,
    /// The first block is the entry block.
    pub blocks: Vec<Block>,
}

/// Index into [`Function::slots`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SlotId(pub usize);

/// The shape of a value in memory. Codegen derives sizes, alignments and
/// offsets from it for the target: every field at its natural alignment, and
/// every struct padded to its alignment.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Layout {
    U8,
    I64,
    F64Bits,
    /// An unsigned integer the size of a pointer.
    Usize,
    Address,
    /// Raw bytes, with no alignment.
    Bytes(usize),
    Struct(Vec<Layout>),
}

/// A location in memory: a base, then a path of field indexes through
/// nested structs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Place {
    pub base: Base,
    pub path: Vec<usize>,
}

impl Place {
    /// The whole of `base`.
    #[must_use]
    pub fn whole(base: Base) -> Self {
        Place {
            base,
            path: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Base {
    /// A data object, whose layout is the struct of its fields.
    Data(DataId),
    Slot(SlotId),
    /// The current function's parameter struct.
    Params,
}

/// A function in the runtime library.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeFn {
    /// `trendy_rt_debug_value(value, descriptor)`.
    DebugValue,
    /// `trendy_rt_fail(failure)`, which doesn't return. It takes a
    /// [`Failure::code`] by value, not a place, so only codegen calls it.
    Fail,
    /// `trendy_rt_concat(dst, lhs, rhs)`: store the concatenation of two
    /// texts at `dst`, in allocated memory that must be released with
    /// [`RuntimeFn::FreeText`]. It stops the program if the memory can't be
    /// allocated.
    Concat,
    /// `trendy_rt_free_text(text)`: release a text made by
    /// [`RuntimeFn::Concat`].
    FreeText,
    /// `trendy_rt_compare_text(dst, lhs, rhs)`: store -1, 0 or 1 at `dst`,
    /// an [`Layout::I64`], as `lhs` is before, equal to or after `rhs` in
    /// code point order.
    CompareText,
    /// `trendy_rt_equal(dst, lhs, rhs, descriptor)`: store at `dst`, a
    /// [`Layout::U8`], whether two values of the type the descriptor
    /// describes are equal. For tuples and relations.
    Equal,
}

impl RuntimeFn {
    /// The symbol the runtime library exports.
    #[must_use]
    pub fn symbol(self) -> &'static str {
        match self {
            RuntimeFn::DebugValue => "trendy_rt_debug_value",
            RuntimeFn::Fail => "trendy_rt_fail",
            RuntimeFn::Concat => "trendy_rt_concat",
            RuntimeFn::FreeText => "trendy_rt_free_text",
            RuntimeFn::CompareText => "trendy_rt_compare_text",
            RuntimeFn::Equal => "trendy_rt_equal",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Inst {
    /// Copy a value with `layout` from `src` to `dst`.
    Copy {
        dst: Place,
        src: Place,
        layout: Layout,
    },
    /// Call a function, passing the address of its parameter struct.
    Call {
        function: FunctionId,
        args: Option<Place>,
    },
    /// Call a runtime function, passing the address of each place.
    CallRuntime {
        function: RuntimeFn,
        args: Vec<Place>,
    },
    /// Store `op src` at `dst`.
    Unary { dst: Place, op: UnaryOp, src: Place },
    /// Store `lhs op rhs` at `dst`.
    Binary {
        dst: Place,
        op: BinaryOp,
        lhs: Place,
        rhs: Place,
    },
}

/// The type an arithmetic operator works on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Numeric {
    /// [`Layout::I64`].
    Integer,
    /// [`Layout::F64Bits`].
    Rational,
}

/// What an instruction's operands are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operand {
    /// [`Layout::U8`], `0` or `1`.
    Boolean,
    Numeric(Numeric),
}

/// A monadic operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    /// Of a boolean.
    Not,
    /// Fails with [`Failure::IntegerOverflow`] for the smallest integer;
    /// never fails for a rational.
    Negate(Numeric),
}

impl UnaryOp {
    #[must_use]
    pub fn operand(self) -> Operand {
        match self {
            UnaryOp::Not => Operand::Boolean,
            UnaryOp::Negate(numeric) => Operand::Numeric(numeric),
        }
    }
}

/// A dyadic operator. Arithmetic is checked: a result that doesn't exist
/// stops the program with a [`Failure`], as if the instruction were
/// followed by a branch to a call to [`RuntimeFn::Fail`]. Codegen emits
/// that branch. Rational results are rounded as IEEE 754 binary64 rounds
/// them, and must be finite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    And,
    Or,
    Xor,
    /// `!lhs || rhs`.
    Implies,
    Add(Numeric),
    Subtract(Numeric),
    Multiply(Numeric),
    /// Of rationals. Fails with [`Failure::DivisionByZero`] for either zero.
    Divide,
    /// Of integers, truncating toward zero.
    Div,
    /// Of integers, with the sign of the dividend. `i64::MIN mod -1` is 0.
    Mod,
    /// A comparison, whose result is a boolean: `false` before `true`,
    /// integers numerically, and rationals by IEEE 754 totalOrder. Texts
    /// are compared by [`RuntimeFn::CompareText`], and tuples and relations
    /// by [`RuntimeFn::Equal`].
    Compare(Comparison, Operand),
}

/// A comparison operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Comparison {
    Equal,
    NotEqual,
    Less,
    LessOrEqual,
    Greater,
    GreaterOrEqual,
}

impl BinaryOp {
    #[must_use]
    pub fn operand(self) -> Operand {
        match self {
            BinaryOp::And | BinaryOp::Or | BinaryOp::Xor | BinaryOp::Implies => Operand::Boolean,
            BinaryOp::Add(numeric) | BinaryOp::Subtract(numeric) | BinaryOp::Multiply(numeric) => {
                Operand::Numeric(numeric)
            }
            BinaryOp::Divide => Operand::Numeric(Numeric::Rational),
            BinaryOp::Div | BinaryOp::Mod => Operand::Numeric(Numeric::Integer),
            BinaryOp::Compare(_, operand) => operand,
        }
    }
}

/// Why a program stops: the argument of [`RuntimeFn::Fail`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    IntegerOverflow,
    DivisionByZero,
    RationalOverflow,
}

impl Failure {
    /// The `trendy_runtime::FAIL_` constant for this failure.
    #[must_use]
    pub fn code(self) -> u32 {
        match self {
            Failure::IntegerOverflow => 0,
            Failure::DivisionByZero => 1,
            Failure::RationalOverflow => 2,
        }
    }
}

/// Index into [`Function::blocks`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BlockId(pub usize);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub insts: Vec<Inst>,
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
    /// Fields laid out together. A struct is aligned to its widest field
    /// and padded to a multiple of that alignment, so it has the same layout
    /// wherever it is embedded.
    Struct(Vec<Field>),
    /// The size of a value with this layout, as a pointer-sized integer.
    Size(Layout),
    /// The offset of field `index` in a [`Layout::Struct`], as a
    /// pointer-sized integer.
    Offset(Layout, usize),
}
