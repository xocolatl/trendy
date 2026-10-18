//! Low-level IR: what codegen consumes.
//!
//! Functions are control-flow graphs of basic blocks. There is no nesting and
//! nothing implicit: every jump and return is spelled out. A relational
//! operator that computes something for each tuple, such as `extend`,
//! arrives here as an explicit loop over the tuples; the others are runtime
//! calls.
//!
//! Static data is described field by field rather than as bytes, so the
//! description doesn't depend on the target: codegen lays fields out with the
//! target's byte order and pointer size.
//!
//! Value representations, each a single field with the matching [`Layout`]:
//! - `boolean`: [`Field::U8`], `0` for false and `1` for true.
//! - `integer`: [`Field::I64`].
//! - `approximate`: [`Field::F64Bits`], a 64-bit IEEE 754 binary float.
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
//! nothing. A function with a result, a read-only operator's, then takes
//! a pointer to where the result goes, a stack slot of the caller's. What
//! it makes, such as the result's texts, is the calling statement's, as if
//! the caller had computed the value itself, so it begins no scope (see
//! [`RuntimeFn::Begin`]).
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
    /// The layout of its result, if it has one: a read-only operator's
    /// function stores its value at [`Base::Result`].
    pub result: Option<Layout>,
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

impl Layout {
    /// A statement's scope (see [`RuntimeFn::Begin`]), which only the
    /// runtime reads: `trendy_runtime::Scope`, of two addresses and a
    /// 64-bit time.
    #[must_use]
    pub fn scope() -> Layout {
        Layout::Struct(vec![Layout::Address, Layout::Address, Layout::I64])
    }
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Base {
    /// A data object, whose layout is the struct of its fields.
    Data(DataId),
    Slot(SlotId),
    /// The current function's parameter struct.
    Params,
    /// Where the current function's result goes, as its caller says.
    Result,
    /// The value with layout `pointee` at the address in `slot`, a
    /// [`Layout::Address`]: such as a relation's tuple, as a loop goes
    /// through them.
    Indirect {
        slot: SlotId,
        pointee: Layout,
    },
}

/// A function in the runtime library.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeFn {
    /// `trendy_rt_debug_value(value, descriptor)`.
    DebugValue,
    /// `trendy_rt_to_text(dst, value, descriptor)`: store at `dst` the
    /// text [`RuntimeFn::DebugValue`] writes for the value, of the type the
    /// descriptor describes, in memory the statement owns (see
    /// [`RuntimeFn::Begin`]). It stops the program if the memory can't be
    /// allocated.
    ToText,
    /// `trendy_rt_sum(dst, src, descriptor, index)`: store at `dst` the
    /// sum of the attribute `index` (a [`Field::Usize`], its position by
    /// name) of the tuples of the relation `src`, of the type the
    /// descriptor describes, whose type is integer or approximate: the
    /// exact sum, rounded once. It stops the program if the sum is beyond
    /// its type's range.
    Sum,
    /// `trendy_rt_fail(failure)`, which doesn't return. It takes a
    /// [`Failure::code`] by value, not a place, so only codegen calls it.
    Fail,
    /// `trendy_rt_concat(dst, lhs, rhs)`: store the concatenation of two
    /// texts at `dst`, in memory the statement owns (see
    /// [`RuntimeFn::Begin`]). It stops the program if the memory can't be
    /// allocated.
    Concat,
    /// `trendy_rt_begin(scope)`: begin a statement's scope, in a slot laid
    /// out as [`Layout::scope`]. Until [`RuntimeFn::End`], the statement
    /// owns the memory of every text and relation that runtime functions
    /// make. A variable's old value, once [`RuntimeFn::Assign`] replaces
    /// it, is the outermost statement's that began while it was the value,
    /// since those, and only those, can point into it. So a statement needs
    /// a scope if it makes a text or relation, or passes one to an update
    /// operator, whose assignments must not release it.
    Begin,
    /// `trendy_rt_end(scope)`: end the scope at `scope`, the innermost,
    /// releasing the memory its statement owns.
    End,
    /// `trendy_rt_reshape(dst, src, result, source, offsets)`: store at
    /// `dst` the relation `src` with its tuples reshaped into the
    /// attributes the descriptor `result` describes, sorted canonically and
    /// without duplicates, in memory the statement owns. `source` is the descriptor of `src`, and
    /// `offsets` an array of [`Field::Offset`]s: for each attribute of
    /// `result`, the offset in a tuple of `src` of the attribute it comes
    /// from. Removing and renaming attributes are both reshaping.
    Reshape,
    /// `trendy_rt_relation(dst, tuples, count, descriptor)`: store at `dst`
    /// the relation, of the type the descriptor describes, whose tuples are
    /// the `count` (a [`Field::Usize`]) at `tuples`, one after another,
    /// sorted canonically and without duplicates, in memory the statement
    /// owns.
    Relation,
    /// `trendy_rt_and(dst, lhs, rhs, result, lhs_desc, rhs_desc, sources,
    /// common, common_count)`: store at `dst` the relation, of the type the
    /// descriptor `result` describes, of every tuple made of a tuple of
    /// `lhs` and one of `rhs` that agree on their common attributes
    /// (Algebra A's AND), sorted canonically, in memory the statement owns. `lhs_desc` and `rhs_desc`
    /// describe the operands. `sources` is an array with, for each attribute
    /// of `result`, a struct of a [`Field::Usize`], 1 if it comes from `rhs`
    /// and 0 if from `lhs`, and its [`Field::Offset`] there. `common` is an
    /// array of `common_count` (a [`Field::Usize`]) structs, one per common
    /// attribute: its offset in a tuple of `lhs`, its offset in one of
    /// `rhs`, and the [`Field::Address`] of its type's descriptor.
    And,
    /// `trendy_rt_ungroup(dst, src, result, source, sources, ungrouped,
    /// ungrouped_count)`: store at `dst` the relation, of the type the
    /// descriptor `result` describes, of every tuple made of a tuple of
    /// `src` and one tuple of each of its relation-valued attributes in
    /// `ungrouped`, sorted canonically, in memory the statement owns.
    /// `source` describes `src`. `sources` is an array with, for each
    /// attribute of `result`, a struct of a [`Field::Usize`], 0 if it comes
    /// from the tuple of `src` and `k` if from the tuple of the `k`th
    /// ungrouped relation, and its [`Field::Offset`] there. `ungrouped` is
    /// an array of `ungrouped_count` (a [`Field::Usize`]) structs, one per
    /// ungrouped attribute: its offset in a tuple of `src`, and the
    /// [`Field::Size`] of its relation's tuples.
    Ungroup,
    /// `trendy_rt_matching(dst, lhs, rhs, lhs_desc, rhs_desc, common,
    /// common_count, negated)`: store at `dst` the relation of the tuples
    /// of `lhs` that have a match in `rhs` on their common attributes, or,
    /// if `negated` (a [`Field::Usize`]) is 1, those that have none, sorted
    /// canonically, in memory the statement owns. The other arguments are
    /// as for [`RuntimeFn::And`]'s.
    Matching,
    /// `trendy_rt_subset(dst, lhs, rhs, descriptor, proper)`: store at
    /// `dst`, a [`Layout::U8`], whether every tuple of the relation `lhs` is
    /// in the relation `rhs`, of the type the descriptor describes, and, if
    /// `proper` (a [`Field::Usize`]) is 1, `rhs` has more.
    Subset,
    /// `trendy_rt_combine(dst, lhs, rhs, descriptor, combination)`: store
    /// at `dst` the relations `lhs` and `rhs`, of the type the descriptor
    /// describes, combined as `combination` (a [`Field::Usize`], a
    /// [`Combination::code`]) says, sorted canonically, in
    /// memory the statement owns. It stops the program if a disjoint
    /// union's operands share a tuple, or an included minus's right operand
    /// has one the left lacks.
    Combine,
    /// `trendy_rt_allocate(dst, count, descriptor)`: store at `dst` a
    /// relation, of the type the descriptor describes, of `count` (a
    /// [`Layout::Usize`]) tuples, zeroed, in memory the statement owns, for
    /// compiled code to fill in. [`RuntimeFn::Sort`] then puts them in the
    /// canonical order.
    Allocate,
    /// `trendy_rt_sort(relation, descriptor)`: sort the tuples of the
    /// relation at `relation`, of the type the descriptor describes, into the
    /// canonical order, in place, and drop duplicates, shortening it.
    Sort,
    /// `trendy_rt_compare_text(dst, lhs, rhs)`: store -1, 0 or 1 at `dst`,
    /// an [`Layout::I64`], as `lhs` is before, equal to or after `rhs` in
    /// code point order.
    CompareText,
    /// `trendy_rt_equal(dst, lhs, rhs, descriptor)`: store at `dst`, a
    /// [`Layout::U8`], whether two values of the type the descriptor
    /// describes are equal. For tuples and relations.
    Equal,
    /// `trendy_rt_assign(dst, owner, src, descriptor)`: store at `dst`, a
    /// variable's value, a copy of the value at `src`, of the type the
    /// descriptor describes, with what it points to in memory of its own,
    /// which `owner`, a [`Layout::Address`] next to the value, then holds.
    /// The memory `owner` held before, if any, a statement owns (see
    /// [`RuntimeFn::Begin`]). For values that point to texts or
    /// relations; others are copied.
    Assign,
}

impl RuntimeFn {
    /// The symbol the runtime library exports.
    #[must_use]
    pub fn symbol(self) -> &'static str {
        match self {
            RuntimeFn::DebugValue => "trendy_rt_debug_value",
            RuntimeFn::ToText => "trendy_rt_to_text",
            RuntimeFn::Sum => "trendy_rt_sum",
            RuntimeFn::Fail => "trendy_rt_fail",
            RuntimeFn::Concat => "trendy_rt_concat",
            RuntimeFn::Begin => "trendy_rt_begin",
            RuntimeFn::End => "trendy_rt_end",
            RuntimeFn::Reshape => "trendy_rt_reshape",
            RuntimeFn::Relation => "trendy_rt_relation",
            RuntimeFn::And => "trendy_rt_and",
            RuntimeFn::Ungroup => "trendy_rt_ungroup",
            RuntimeFn::Matching => "trendy_rt_matching",
            RuntimeFn::Combine => "trendy_rt_combine",
            RuntimeFn::Subset => "trendy_rt_subset",
            RuntimeFn::Allocate => "trendy_rt_allocate",
            RuntimeFn::Sort => "trendy_rt_sort",
            RuntimeFn::CompareText => "trendy_rt_compare_text",
            RuntimeFn::Equal => "trendy_rt_equal",
            RuntimeFn::Assign => "trendy_rt_assign",
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
    /// Call a function, passing the address of its parameter struct, and
    /// of where its result goes.
    Call {
        function: FunctionId,
        args: Option<Place>,
        result: Option<Place>,
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
    /// Store at `dst`, a [`Layout::Address`], the address of tuple number
    /// `index` (a [`Layout::Usize`]) of the relation at `relation`, whose
    /// tuples have layout `tuple`.
    Element {
        dst: Place,
        relation: Place,
        index: Place,
        tuple: Layout,
    },
    /// Add 1 to the [`Layout::Usize`] at `place`, which counts a loop's
    /// iterations, and so never overflows.
    Increment { place: Place },
}

/// The type an arithmetic operator works on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Numeric {
    /// [`Layout::I64`].
    Integer,
    /// [`Layout::F64Bits`].
    Approximate,
}

/// What an instruction's operands are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operand {
    /// [`Layout::U8`], `0` or `1`.
    Boolean,
    Numeric(Numeric),
    /// [`Layout::Usize`], such as a loop's index, compared unsigned. Only
    /// comparisons and [`UnaryOp::Integer`] take it.
    Usize,
}

/// A monadic operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    /// Of a boolean.
    Not,
    /// Fails with [`Failure::IntegerOverflow`] for the smallest integer;
    /// never fails for an approximate number.
    Negate(Numeric),
    /// The integer a [`Layout::Usize`] is, such as a relation's count,
    /// which is never more than the largest integer.
    Integer,
    /// The approximate number nearest an integer, ties to even: the
    /// integer itself up to 2^53 in magnitude.
    Approximate,
}

impl UnaryOp {
    #[must_use]
    pub fn operand(self) -> Operand {
        match self {
            UnaryOp::Not => Operand::Boolean,
            UnaryOp::Negate(numeric) => Operand::Numeric(numeric),
            UnaryOp::Integer => Operand::Usize,
            UnaryOp::Approximate => Operand::Numeric(Numeric::Integer),
        }
    }
}

/// A dyadic operator. Arithmetic is checked: a result that doesn't exist
/// stops the program with a [`Failure`], as if the instruction were
/// followed by a branch to a call to [`RuntimeFn::Fail`]. Codegen emits
/// that branch. Approximate results are rounded as IEEE 754 binary64 rounds
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
    /// Of approximate numbers. Fails with [`Failure::DivisionByZero`] for
    /// either zero.
    Divide,
    /// Of integers, truncating toward zero.
    Div,
    /// Of integers, with the sign of the dividend. `i64::MIN mod -1` is 0.
    Mod,
    /// A comparison, whose result is a boolean: `false` before `true`,
    /// integers numerically, and approximate numbers by IEEE 754 totalOrder.
    /// Texts are compared by [`RuntimeFn::CompareText`], and tuples and
    /// relations by [`RuntimeFn::Equal`].
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
            BinaryOp::Divide => Operand::Numeric(Numeric::Approximate),
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
    ApproximateOverflow,
    /// Tuples being joined disagree on a common attribute.
    TupleMismatch,
    /// A relation has no tuples, where its only one is taken.
    NoTuple,
    /// A relation has several tuples, where its only one is taken.
    SeveralTuples,
    /// Two tuples of a relation assigned to a variable have the same
    /// values for one of its keys.
    KeyViolated,
    /// A relation has no tuples, where the least value of an attribute is
    /// taken.
    NoMin,
    /// A relation has no tuples, where the greatest value of an attribute
    /// is taken.
    NoMax,
}

impl Failure {
    /// The `trendy_runtime::FAIL_` constant for this failure.
    #[must_use]
    pub fn code(self) -> u32 {
        match self {
            Failure::IntegerOverflow => 0,
            Failure::DivisionByZero => 1,
            Failure::ApproximateOverflow => 2,
            Failure::TupleMismatch => 4,
            Failure::NoTuple => 5,
            Failure::SeveralTuples => 6,
            Failure::KeyViolated => 9,
            Failure::NoMin => 11,
            Failure::NoMax => 12,
        }
    }
}

/// How [`RuntimeFn::Combine`] combines two relations: the argument it
/// takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Combination {
    Union,
    /// A union that fails if the operands share a tuple.
    DisjointUnion,
    /// The tuples of exactly one operand.
    ExclusiveUnion,
    Minus,
    /// A minus that fails if the right operand has a tuple the left lacks.
    IncludedMinus,
    /// A union that fails, as a conflict between changes, if the operands
    /// share a tuple.
    DisjointChanges,
}

impl Combination {
    /// The `trendy_runtime::COMBINE_` constant for this combination.
    #[must_use]
    pub fn code(self) -> u64 {
        match self {
            Combination::Union => 0,
            Combination::DisjointUnion => 1,
            Combination::Minus => 2,
            Combination::IncludedMinus => 3,
            Combination::ExclusiveUnion => 4,
            Combination::DisjointChanges => 5,
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
    Jump(BlockId),
    /// To `then` if the [`Layout::U8`] at `condition` is 1, and to
    /// `otherwise` if it is 0.
    Branch {
        condition: Place,
        then: BlockId,
        otherwise: BlockId,
    },
    /// Stop the program because of this failure, by calling
    /// [`RuntimeFn::Fail`].
    Fail(Failure),
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
    /// the symbol from it. Its first field is its value; a value that
    /// points to texts or relations has a second, a [`Field::Usize`] of 0,
    /// where [`RuntimeFn::Assign`] keeps the address of the memory it
    /// copies them to.
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
