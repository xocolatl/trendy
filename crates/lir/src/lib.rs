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
//! - `text`: a [`Field::Struct`] of the address of its buffer, holding the
//!   UTF-8 bytes, and a [`Field::Usize`] with their length.
//! - a relation: a [`Field::Struct`] of the address of its buffer, holding
//!   its tuples, as consecutive structs in their canonical (sorted) order,
//!   and a [`Field::Usize`] with their number. Since each struct is padded
//!   to its alignment, the array's stride is the tuple's size.
//! - an array: as a relation, of its elements, in its order.
//! - a tuple: a [`Field::Struct`] of its attributes' values, in attribute
//!   name order. Because a struct's layout doesn't depend on where it is, an
//!   attribute that is itself a tuple can be used, or passed, on its own.
//!
//! # Memory
//!
//! Values share the buffers they point to, each after a header that
//! counts its references: a copy of a value is a copy of its pointers, so
//! assigning a variable, or passing an argument, never copies a buffer. A
//! constant's buffer is a read-only constant of two fields,
//! [`Field::immortal_header`], then the buffer, which the value points
//! to, with a [`Field::FieldAddress`] of field 1. The runtime
//! counts the references of the buffers its functions make: from the
//! statement that makes one (see [`RuntimeFn::Begin`]), from each variable
//! assigned it ([`RuntimeFn::Assign`]), and from each buffer whose tuples
//! or elements point to it.
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
//! calling [`RuntimeFn::IntegerFailed`] or [`RuntimeFn::ApproximateFailed`]
//! with the operation, and a check that fails ends its block with
//! [`Terminator::Fail`], whose runtime function takes the values involved.
//! Each writes the error, with those values, to stderr and exits with
//! status 1. Every failure, a runtime function's too, also writes the site
//! the program is at, which [`Inst::Site`] sets.
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
/// every struct padded to its alignment, so that a struct of one field is
/// laid out as that field is.
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
    /// Room for a value of any one of these layouts, its variants: as big
    /// and as aligned as the biggest and the most aligned of them. A
    /// path's index into it is a variant's, which is at its start.
    Union(Vec<Layout>),
}

impl Layout {
    /// A statement's scope (see [`RuntimeFn::Begin`]), which only the
    /// runtime reads: `trendy_runtime::Scope`, of the address of its list
    /// of buffers, the list's length and capacity, and the address of the
    /// scope it is in.
    #[must_use]
    pub fn scope() -> Layout {
        Layout::Struct(vec![
            Layout::Address,
            Layout::Usize,
            Layout::Usize,
            Layout::Address,
        ])
    }

    /// Where `load_csv` read a value, for a failure to write:
    /// `trendy_runtime::Record`, of the text naming the attribute or
    /// component it was read for, the file's path, a text, and the
    /// record's line.
    #[must_use]
    pub fn record() -> Layout {
        let text = || Layout::Struct(vec![Layout::Address, Layout::Usize]);
        Layout::Struct(vec![text(), text(), Layout::Usize])
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
    /// `trendy_rt_debug_table(value, descriptor)`, of a relation. It stops
    /// the program if the memory for its columns' widths can't be
    /// allocated.
    DebugTable,
    /// `trendy_rt_to_text(dst, value, descriptor)`: store at `dst` the
    /// text [`RuntimeFn::DebugValue`] writes for the value, of the type the
    /// descriptor describes, in memory the statement owns (see
    /// [`RuntimeFn::Begin`]). It stops the program if the memory can't be
    /// allocated.
    ToText,
    /// `trendy_rt_parse_integer(dst, text)`: store at `dst` the integer the
    /// text writes, as `parse_integer` reads one. It stops the program if
    /// the text writes none.
    ParseInteger,
    /// `trendy_rt_parse_approximate(dst, text)`: store at `dst` the
    /// approximate number nearest the decimal number the text writes, as
    /// `parse_approximate` reads one. It stops the program if the text
    /// writes none.
    ParseApproximate,
    /// `trendy_rt_parse_boolean(dst, text)`: store at `dst` the boolean the
    /// text writes, as `parse_boolean` reads one. It stops the program if
    /// the text writes none.
    ParseBoolean,
    /// `trendy_rt_sqrt(dst, x)`: store at `dst` the square root of the
    /// approximate number `x`, as `sqrt` computes it. It stops the program
    /// if `x` is negative, which has none.
    Sqrt,
    /// `trendy_rt_sin(dst, x)`: store at `dst` the sine of the angle `x`,
    /// in radians, as `sin` computes it.
    Sin,
    /// `trendy_rt_cos(dst, x)`: store at `dst` the cosine of the angle
    /// `x`, in radians, as `cos` computes it.
    Cos,
    /// `trendy_rt_atan2(dst, y, x)`: store at `dst` the angle, in radians,
    /// from the positive x axis to the point (`x`, `y`), as `atan2`
    /// computes it.
    Atan2,
    /// `trendy_rt_load_text_file(dst, path, descriptor)`: store at `dst` the
    /// array of the lines of the text file at `path`, a text, in order, of
    /// the type the descriptor describes, `array text`, in memory the
    /// statement owns. It stops the program if the file can't be read, or
    /// isn't UTF-8 text, or if the memory can't be allocated.
    LoadTextFile,
    /// `trendy_rt_load_text_file_with_errors(dst, path, errors,
    /// descriptor)`: as [`RuntimeFn::LoadTextFile`], but a file that can't
    /// be read, or isn't UTF-8 text, gives no lines, and its error at
    /// `errors`, rather than stopping the program: an `unreadable_file` or
    /// a `not_utf8`. `errors` is a struct of `relation { error: error }`,
    /// which it stores the errors in, none if the file is read, and the
    /// address of an array of descriptors' addresses, the relation's, then
    /// each error's type's, in `hir::UpdateBuiltin::errors`'s order, as
    /// its selector writes its components.
    LoadTextFileWithErrors,
    /// `trendy_rt_load_csv(dst, path, header, columns, lines, descriptor)`:
    /// store at `dst` the array of the records of the CSV file at `path`, a
    /// text, in order, after its first if `header`, a boolean, is true, of
    /// the type the descriptor describes, an array of tuples whose
    /// attributes are the fields in the columns `columns` gives them, a
    /// struct of the address of a tuple of integers and its type's
    /// descriptor's, and at `lines` the line each starts on, both in memory
    /// the statement owns.
    /// It stops the program if a column isn't one of the file's, or two
    /// attributes have one, the file can't be read, isn't UTF-8 text or
    /// CSV, a field isn't a value of its attribute's type, or the memory
    /// can't be allocated.
    LoadCsv,
    /// `trendy_rt_load_csv_with_errors(dst, path, header, columns, lines,
    /// errors, descriptor)`: as [`RuntimeFn::LoadCsv`], with `errors` as
    /// [`RuntimeFn::LoadTextFileWithErrors`]'s, but rather than stopping the program for a
    /// file it can't read, it gives no records, and its error at `errors`,
    /// and for a record it can't, skips it, giving its error there, as
    /// [`RuntimeFn::LoadTextFileWithErrors`] gives its: an
    /// `unreadable_file`, a `not_utf8`, a `malformed_csv`, a
    /// `missing_column` or an `unreadable_field`. Two attributes of one
    /// column, or one of a column below 1, still stop it.
    LoadCsvWithErrors,
    /// `trendy_rt_broken_constraint_error(errors, value, descriptor, ty,
    /// constraint, record)`: add to the errors at `errors`, as
    /// [`RuntimeFn::LoadCsvWithErrors`] gives them, that `value`, whose
    /// components the descriptor describes, read from the record `record`
    /// describes, laid out as [`Layout::record`], breaks the constraint of
    /// the type named `ty` written `constraint`, both texts: a
    /// `broken_constraint_in_file`.
    BrokenConstraintError,
    /// `trendy_rt_sum(dst, src, descriptor, index)`: store at `dst` the
    /// sum of the attribute `index` (a [`Field::Usize`], its position by
    /// name) of the tuples of the relation `src`, of the type the
    /// descriptor describes, whose type is integer or approximate: the
    /// exact sum, rounded once. It stops the program if the sum is beyond
    /// its type's range.
    Sum,
    /// `trendy_rt_product(dst, src, descriptor, index)`: as
    /// [`RuntimeFn::Sum`], of the product.
    Product,
    /// `trendy_rt_integer_failed(op, lhs, rhs)`, which doesn't return:
    /// stop the program because `lhs op rhs`, of integers, has no result,
    /// writing the operation: a division by zero, or an overflow. `op` is
    /// an [`Arithmetic::code`]; a negation's `lhs` is 0. It takes them by
    /// value, not by place, so only codegen calls it.
    IntegerFailed,
    /// `trendy_rt_approximate_failed(op, lhs, rhs)`: as
    /// [`RuntimeFn::IntegerFailed`], of approximate numbers.
    ApproximateFailed,
    /// `trendy_rt_tuple_mismatch(attribute, lhs, rhs, descriptor)`, which
    /// doesn't return: stop the program because tuples being joined
    /// disagree on the common attribute named `attribute`, a text, which is
    /// `lhs` in one and `rhs` in the other, of the type the descriptor
    /// describes, writing them.
    TupleMismatch,
    /// `trendy_rt_not_one_tuple(count)`, which doesn't return: stop the
    /// program because a relation has `count` (a [`Layout::Usize`]) tuples,
    /// not one, where its only one is taken, writing the count.
    NotOneTuple,
    /// `trendy_rt_no_min()`, which doesn't return: stop the program
    /// because a relation has no tuples, where the least value of an
    /// attribute is taken.
    NoMin,
    /// `trendy_rt_no_max()`: as [`RuntimeFn::NoMin`], of the greatest.
    NoMax,
    /// `trendy_rt_index_out_of_range(position, length)`, which doesn't
    /// return: stop the program because an array of `length` elements has
    /// none at `position`, both [`Layout::I64`], writing them.
    IndexOutOfRange,
    /// `trendy_rt_element_conflict(positions, count, nested)`, which
    /// doesn't return: stop the program because two changes to an array
    /// in an `assign` block set the element at `positions`, a struct of
    /// `count` (a [`Layout::Usize`]) [`Layout::I64`]s, from the outermost
    /// array in, or, if `nested` (a [`Layout::U8`]) is 1, one sets it and
    /// the other an element in it, writing the positions.
    ElementConflict,
    /// `trendy_rt_broken_constraint(value, descriptor, ty, constraint)`,
    /// which doesn't return: stop the program because `value`, components
    /// the descriptor describes, selected of the type named `ty`, a text,
    /// break its constraint written `constraint`, a text, writing them.
    BrokenConstraint,
    /// `trendy_rt_not_of_type(value, descriptor, ty)`, which doesn't
    /// return: stop the program because `value`, of the type in a
    /// hierarchy the descriptor describes, isn't a value of the type named
    /// `ty`, a text, which it was treated as, writing them.
    NotOfType,
    /// `trendy_rt_failed(message)`, which doesn't return: stop the program
    /// because it fails with an error whose message is `message`, a text,
    /// writing it.
    Failed,
    /// `trendy_rt_broken_constraint_in_file(value, descriptor, ty,
    /// constraint, record)`, which doesn't return: as
    /// [`RuntimeFn::BrokenConstraint`],
    /// of a value `load_csv` read from the record `record` describes,
    /// laid out as [`Layout::record`], writing where too.
    BrokenConstraintInFile,
    /// `trendy_rt_overlapping_subtypes(value, descriptor, first, second)`,
    /// which doesn't return: stop the program because `value`, of the type
    /// of the program's own the descriptor describes, is of both of its
    /// subtypes defined by constraints named `first` and `second`, texts,
    /// writing them.
    OverlappingSubtypes,
    /// `trendy_rt_key_shared(shared, pair, src, descriptor, key)`: store at
    /// `shared`, a [`Layout::U8`], 1 if the relation `src`, of the type the
    /// descriptor describes, has two tuples with the same values for the
    /// attributes of `key`, the descriptor of its projection on one of its
    /// keys, and their positions at `pair`, two [`Layout::Usize`]s; else 0.
    KeyShared,
    /// `trendy_rt_key_violated(src, descriptor, key, pair, variable)`,
    /// which doesn't return: stop the program because the tuples at `pair`
    /// of the relation `src`, about to be assigned to the variable named
    /// `variable`, a text, share their values for `key`, as
    /// [`RuntimeFn::KeyShared`] found, writing the variable, the key and
    /// the tuples.
    KeyViolated,
    /// `trendy_rt_component_key_violated(src, descriptor, key, pair, ty,
    /// component)`, which doesn't return: as [`RuntimeFn::KeyViolated`],
    /// of the relation that is the component named `component`, a text, of
    /// a value selected of the type named `ty`, a text.
    ComponentKeyViolated,
    /// `trendy_rt_component_key_violated_in_file(src, descriptor, key,
    /// pair, ty, component, record)`, which doesn't return: as
    /// [`RuntimeFn::ComponentKeyViolated`], of a value `load_csv` read
    /// from the record `record` describes, laid out as
    /// [`Layout::record`], writing where too.
    ComponentKeyViolatedInFile,
    /// `trendy_rt_concat(dst, lhs, rhs)`: store the concatenation of two
    /// texts at `dst`, in memory the statement owns (see
    /// [`RuntimeFn::Begin`]). It stops the program if the memory can't be
    /// allocated.
    Concat,
    /// `trendy_rt_release(value, descriptor)`: give up the references
    /// that a local variable's value, of the type the descriptor
    /// describes, holds, at the end of its block, and make its pointers
    /// null, as they are before its declaration.
    Release,
    /// `trendy_rt_begin(scope)`: begin a statement's scope, in a slot laid
    /// out as [`Layout::scope`]. Until [`RuntimeFn::End`], it holds a
    /// reference to each buffer that runtime functions make, so a value
    /// that no variable holds lasts until its statement ends, and no
    /// longer. A statement needs a scope if it makes a text, relation or
    /// array, assigns a value that points to one, or passes one to an
    /// update operator.
    Begin,
    /// `trendy_rt_end(scope)`: end the scope at `scope`, the innermost,
    /// giving up its references.
    End,
    /// `trendy_rt_keep(value, descriptor)`: have the statement's scope hold
    /// a reference to each buffer a value, of the type the descriptor
    /// describes, points to: an update operator's argument, which a
    /// variable the operator assigns may hold the only other reference to.
    Keep,
    /// `trendy_rt_adopt(value, descriptor)`: have the buffer of a relation
    /// or array, of the type the descriptor describes, that
    /// [`RuntimeFn::Allocate`] made and compiled code filled, take a
    /// reference to what each of its tuples or elements points to, which
    /// it then holds.
    Adopt,
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
    /// relation or an array, of the type the descriptor describes, of
    /// `count` (a [`Layout::Usize`]) tuples or elements, zeroed, in memory
    /// the statement owns, for compiled code to fill in. Then either
    /// [`RuntimeFn::Sort`] copies a relation's tuples, or
    /// [`RuntimeFn::Adopt`] has the buffer hold what they point to.
    Allocate,
    /// `trendy_rt_unique(array, descriptor)`: make the array at `array`,
    /// of the type the descriptor describes, the only holder of its
    /// buffer, copying it if anything else holds it, so that its elements
    /// can be changed in place. The array is a variable's value, or an
    /// element of a buffer that only its holder holds.
    Unique,
    /// `trendy_rt_duplicate(dst, src, descriptor)`: store at `dst` a copy
    /// of the array `src`, of the type the descriptor describes, in a new
    /// buffer that only the statement holds, whose elements can be changed
    /// in place.
    Duplicate,
    /// `trendy_rt_sort(relation, descriptor)`: replace the relation at
    /// `relation`, of the type the descriptor describes, with one of its
    /// tuples in the canonical order, without duplicates, in memory the
    /// statement owns.
    Sort,
    /// `trendy_rt_load(dst, src, source, result, items, count, parts,
    /// part_count)`: store at `dst` the array, of the type the descriptor
    /// `result` describes, of the tuples of the relation `src`, of the type
    /// the descriptor `source` describes, in the order of `items`, in
    /// memory the statement owns. `items` is an array of `count` (a
    /// [`Field::Usize`]) structs, one per order item: its attribute's
    /// [`Field::Offset`] in a tuple, the [`Field::Address`] of the
    /// attribute's type's descriptor, and a [`Field::Usize`], 1 if
    /// descending and 0 if ascending. Each element is made of `part_count`
    /// (a [`Field::Usize`]) parts of its tuple, each a struct of the offset
    /// in the tuple of an attribute it keeps, its offset in the element,
    /// and its size, each a [`Field::Usize`]-sized field: the tuple without
    /// the attributes removed, or the value of the only one left. Tuples
    /// equal on every item are in canonical order.
    Load,
    /// `trendy_rt_unload(dst, src, source, result, parts, part_count,
    /// numbers, number_count)`: store at `dst` the relation, of the type
    /// the descriptor `result` describes, of the elements of the array
    /// `src`, of the type the descriptor `source` describes, in memory the
    /// statement owns. Each element makes a tuple of `part_count` (a
    /// [`Field::Usize`]) parts, laid out as [`RuntimeFn::Load`]'s, each a
    /// struct of an offset in the element, the offset in the tuple, and
    /// the size: the element's attributes, or the element, as the value of
    /// an attribute. At each of the `number_count` (a [`Field::Usize`])
    /// offsets in a tuple at `numbers`, [`Field::Usize`]s, none or one, is
    /// the element's position, counting from 1, an integer. Tuples that
    /// turn out equal are one.
    Unload,
    /// `trendy_rt_compare_text(dst, lhs, rhs)`: store -1, 0 or 1 at `dst`,
    /// an [`Layout::I64`], as `lhs` is before, equal to or after `rhs` in
    /// code point order.
    CompareText,
    /// `trendy_rt_equal(dst, lhs, rhs, descriptor)`: store at `dst`, a
    /// [`Layout::U8`], whether two values of the type the descriptor
    /// describes are equal. For tuples, relations and arrays.
    Equal,
    /// `trendy_rt_assign(dst, src, descriptor)`: store at `dst`, a
    /// variable's value, the value at `src`, of the type the descriptor
    /// describes, taking a reference to each buffer it points to. The
    /// statement's scope takes over the variable's references to the
    /// buffers of its old value, which the statement may still point into.
    /// For values that point to texts, relations or arrays; others are
    /// copied.
    Assign,
}

impl RuntimeFn {
    /// The symbol the runtime library exports.
    #[must_use]
    pub fn symbol(self) -> &'static str {
        match self {
            RuntimeFn::DebugValue => "trendy_rt_debug_value",
            RuntimeFn::DebugTable => "trendy_rt_debug_table",
            RuntimeFn::ToText => "trendy_rt_to_text",
            RuntimeFn::ParseInteger => "trendy_rt_parse_integer",
            RuntimeFn::ParseApproximate => "trendy_rt_parse_approximate",
            RuntimeFn::ParseBoolean => "trendy_rt_parse_boolean",
            RuntimeFn::Sqrt => "trendy_rt_sqrt",
            RuntimeFn::Sin => "trendy_rt_sin",
            RuntimeFn::Cos => "trendy_rt_cos",
            RuntimeFn::Atan2 => "trendy_rt_atan2",
            RuntimeFn::LoadTextFile => "trendy_rt_load_text_file",
            RuntimeFn::LoadTextFileWithErrors => "trendy_rt_load_text_file_with_errors",
            RuntimeFn::LoadCsvWithErrors => "trendy_rt_load_csv_with_errors",
            RuntimeFn::BrokenConstraintError => "trendy_rt_broken_constraint_error",
            RuntimeFn::LoadCsv => "trendy_rt_load_csv",
            RuntimeFn::Sum => "trendy_rt_sum",
            RuntimeFn::Product => "trendy_rt_product",
            RuntimeFn::IntegerFailed => "trendy_rt_integer_failed",
            RuntimeFn::ApproximateFailed => "trendy_rt_approximate_failed",
            RuntimeFn::TupleMismatch => "trendy_rt_tuple_mismatch",
            RuntimeFn::NotOneTuple => "trendy_rt_not_one_tuple",
            RuntimeFn::NoMin => "trendy_rt_no_min",
            RuntimeFn::NoMax => "trendy_rt_no_max",
            RuntimeFn::IndexOutOfRange => "trendy_rt_index_out_of_range",
            RuntimeFn::ElementConflict => "trendy_rt_element_conflict",
            RuntimeFn::BrokenConstraint => "trendy_rt_broken_constraint",
            RuntimeFn::NotOfType => "trendy_rt_not_of_type",
            RuntimeFn::Failed => "trendy_rt_failed",
            RuntimeFn::BrokenConstraintInFile => "trendy_rt_broken_constraint_in_file",
            RuntimeFn::OverlappingSubtypes => "trendy_rt_overlapping_subtypes",
            RuntimeFn::KeyShared => "trendy_rt_key_shared",
            RuntimeFn::KeyViolated => "trendy_rt_key_violated",
            RuntimeFn::ComponentKeyViolated => "trendy_rt_component_key_violated",
            RuntimeFn::ComponentKeyViolatedInFile => "trendy_rt_component_key_violated_in_file",
            RuntimeFn::Concat => "trendy_rt_concat",
            RuntimeFn::Release => "trendy_rt_release",
            RuntimeFn::Begin => "trendy_rt_begin",
            RuntimeFn::End => "trendy_rt_end",
            RuntimeFn::Keep => "trendy_rt_keep",
            RuntimeFn::Adopt => "trendy_rt_adopt",
            RuntimeFn::Reshape => "trendy_rt_reshape",
            RuntimeFn::Relation => "trendy_rt_relation",
            RuntimeFn::And => "trendy_rt_and",
            RuntimeFn::Ungroup => "trendy_rt_ungroup",
            RuntimeFn::Matching => "trendy_rt_matching",
            RuntimeFn::Combine => "trendy_rt_combine",
            RuntimeFn::Subset => "trendy_rt_subset",
            RuntimeFn::Allocate => "trendy_rt_allocate",
            RuntimeFn::Unique => "trendy_rt_unique",
            RuntimeFn::Duplicate => "trendy_rt_duplicate",
            RuntimeFn::Sort => "trendy_rt_sort",
            RuntimeFn::Load => "trendy_rt_load",
            RuntimeFn::Unload => "trendy_rt_unload",
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
    /// The program is at the site the data object describes, laid out as
    /// `trendy_runtime::Site`, until the next: a failure from here on
    /// writes it. Codegen stores its address in the runtime's
    /// `trendy_rt_site`.
    Site(DataId),
    /// Store at `dst`, an [`Layout::Address`], the site the program is at,
    /// as [`Inst::Site`] sets it: a function with no site of its own saves
    /// the one it is entered at, which a call it makes then changes.
    SaveSite { dst: Place },
    /// The program is at the site whose address `src` holds, which
    /// [`Inst::SaveSite`] saved, again.
    RestoreSite { src: Place },
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
    /// Store at `dst`, a [`Layout::Address`], the address of element number
    /// `index` (a [`Layout::Usize`], from 0) of `array`, a relation's tuples
    /// or an array's elements, each laid out as `element`.
    Element {
        dst: Place,
        array: Place,
        index: Place,
        element: Layout,
    },
    /// Add 1 to the [`Layout::Usize`] at `place`, which counts a loop's
    /// iterations, and so never overflows.
    Increment { place: Place },
    /// Store at `dst`, a [`Layout::Address`], the address of `src`: a
    /// variable passed to a `var` or `out` parameter.
    Address { dst: Place, src: Place },
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
    /// comparisons and [`UnaryOp::Integer`] take it, and only
    /// [`UnaryOp::Usize`] makes it.
    Usize,
}

/// A monadic operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    /// Of a boolean.
    Not,
    /// Overflows for the smallest integer; never fails for an approximate
    /// number.
    Negate(Numeric),
    /// The integer a [`Layout::Usize`] is, such as a relation's count,
    /// which is never more than the largest integer.
    Integer,
    /// The [`Layout::Usize`] an integer is, which is never negative, nor
    /// more than an array's count: an array's position, less 1.
    Usize,
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
            UnaryOp::Usize | UnaryOp::Approximate => Operand::Numeric(Numeric::Integer),
        }
    }
}

/// A dyadic operator. Arithmetic is checked: a result that doesn't exist
/// stops the program, as if the instruction were followed by a branch to a
/// call to [`RuntimeFn::IntegerFailed`] or [`RuntimeFn::ApproximateFailed`].
/// Codegen emits that branch. Approximate results are rounded as IEEE 754
/// binary64 rounds them, and must be finite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    And,
    Or,
    Xor,
    Add(Numeric),
    Subtract(Numeric),
    Multiply(Numeric),
    /// Of approximate numbers. Fails for either zero.
    Divide,
    /// Of integers, truncating toward zero.
    Div,
    /// Of integers, with the sign of the dividend. `i64::MIN mod -1` is 0.
    Mod,
    /// A comparison, whose result is a boolean: `false` before `true`, and
    /// integers and approximate numbers numerically.
    /// Texts are compared by [`RuntimeFn::CompareText`], and tuples,
    /// relations and arrays by [`RuntimeFn::Equal`].
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
            BinaryOp::And | BinaryOp::Or | BinaryOp::Xor => Operand::Boolean,
            BinaryOp::Add(numeric) | BinaryOp::Subtract(numeric) | BinaryOp::Multiply(numeric) => {
                Operand::Numeric(numeric)
            }
            BinaryOp::Divide => Operand::Numeric(Numeric::Approximate),
            BinaryOp::Div | BinaryOp::Mod => Operand::Numeric(Numeric::Integer),
            BinaryOp::Compare(_, operand) => operand,
        }
    }
}

/// An arithmetic operator whose result may not exist, as
/// [`RuntimeFn::IntegerFailed`] and [`RuntimeFn::ApproximateFailed`] take
/// it, to write the operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arithmetic {
    Add,
    Subtract,
    Multiply,
    Divide,
    Div,
    Mod,
    Negate,
}

impl Arithmetic {
    /// The `trendy_runtime::ARITHMETIC_` constant for this operator.
    #[must_use]
    pub fn code(self) -> u32 {
        match self {
            Arithmetic::Add => 0,
            Arithmetic::Subtract => 1,
            Arithmetic::Multiply => 2,
            Arithmetic::Divide => 3,
            Arithmetic::Div => 4,
            Arithmetic::Mod => 5,
            Arithmetic::Negate => 6,
        }
    }
}

/// Why a program stops, at a [`Terminator::Fail`], with the values its
/// message writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// Tuples being joined disagree on the common attribute named
    /// `attribute`, a text, which is `lhs` in one and `rhs` in the other,
    /// of the type `descriptor` describes.
    TupleMismatch {
        attribute: Place,
        lhs: Place,
        rhs: Place,
        descriptor: Place,
    },
    /// A relation has `count` (a [`Layout::Usize`]) tuples, not one, where
    /// its only one is taken.
    NotOneTuple { count: Place },
    /// A relation has no tuples, where the least value of an attribute is
    /// taken.
    NoMin,
    /// A relation has no tuples, where the greatest value of an attribute
    /// is taken.
    NoMax,
    /// An array of `length` elements has none at `position`, both
    /// [`Layout::I64`].
    IndexOutOfRange { position: Place, length: Place },
    /// Two changes to an array in an `assign` block set the element at
    /// `positions`, as [`RuntimeFn::ElementConflict`] takes them, or one
    /// sets it and the other an element in it.
    ElementConflict {
        positions: Place,
        count: Place,
        nested: Place,
    },
    /// `value`, components `descriptor` describes, selected of the type of
    /// the program's own named `ty`, a text, break its constraint written
    /// `constraint`, a text.
    BrokenConstraint {
        value: Place,
        descriptor: Place,
        ty: Place,
        constraint: Place,
    },
    /// `value`, of the type in a hierarchy `descriptor` describes, isn't a
    /// value of the type named `ty`, a text, which it was treated as.
    NotOfType {
        value: Place,
        descriptor: Place,
        ty: Place,
    },
    /// The program fails with an error whose message is `message`, a text:
    /// a `fail`.
    Own { message: Place },
    /// As [`Failure::BrokenConstraint`], of a value `load_csv` read from the
    /// record `record` describes, laid out as [`Layout::record`].
    BrokenConstraintInFile {
        value: Place,
        descriptor: Place,
        ty: Place,
        constraint: Place,
        record: Place,
    },
    /// `value`, of the type of the program's own `descriptor` describes, is
    /// of both of its subtypes defined by constraints named `first` and
    /// `second`, texts.
    OverlappingSubtypes {
        value: Place,
        descriptor: Place,
        first: Place,
        second: Place,
    },
    /// The tuples at `pair` of the relation `value`, of the type
    /// `descriptor` describes, about to be assigned to the variable named
    /// `variable`, a text, share their values for the key `key`, the
    /// descriptor of its projection on the key, as
    /// [`RuntimeFn::KeyShared`] found.
    KeyViolated {
        value: Place,
        descriptor: Place,
        key: Place,
        pair: Place,
        variable: Place,
    },
    /// As [`Failure::KeyViolated`], of the relation that is the component
    /// named `component`, a text, of a value selected of the type named
    /// `ty`, a text.
    ComponentKeyViolated {
        value: Place,
        descriptor: Place,
        key: Place,
        pair: Place,
        ty: Place,
        component: Place,
    },
    /// As [`Failure::ComponentKeyViolated`], of a value `load_csv` read
    /// from the record `record` describes, laid out as [`Layout::record`].
    ComponentKeyViolatedInFile {
        value: Place,
        descriptor: Place,
        key: Place,
        pair: Place,
        ty: Place,
        component: Place,
        record: Place,
    },
}

impl Failure {
    /// The runtime function that stops the program because of this
    /// failure, and its arguments, each passed by address.
    #[must_use]
    pub fn call(&self) -> (RuntimeFn, Vec<&Place>) {
        match self {
            Failure::TupleMismatch {
                attribute,
                lhs,
                rhs,
                descriptor,
            } => (
                RuntimeFn::TupleMismatch,
                vec![attribute, lhs, rhs, descriptor],
            ),
            Failure::NotOneTuple { count } => (RuntimeFn::NotOneTuple, vec![count]),
            Failure::NoMin => (RuntimeFn::NoMin, Vec::new()),
            Failure::NoMax => (RuntimeFn::NoMax, Vec::new()),
            Failure::IndexOutOfRange { position, length } => {
                (RuntimeFn::IndexOutOfRange, vec![position, length])
            }
            Failure::ElementConflict {
                positions,
                count,
                nested,
            } => (RuntimeFn::ElementConflict, vec![positions, count, nested]),
            Failure::BrokenConstraint {
                value,
                descriptor,
                ty,
                constraint,
            } => (
                RuntimeFn::BrokenConstraint,
                vec![value, descriptor, ty, constraint],
            ),
            Failure::NotOfType {
                value,
                descriptor,
                ty,
            } => (RuntimeFn::NotOfType, vec![value, descriptor, ty]),
            Failure::Own { message } => (RuntimeFn::Failed, vec![message]),
            Failure::BrokenConstraintInFile {
                value,
                descriptor,
                ty,
                constraint,
                record,
            } => (
                RuntimeFn::BrokenConstraintInFile,
                vec![value, descriptor, ty, constraint, record],
            ),
            Failure::OverlappingSubtypes {
                value,
                descriptor,
                first,
                second,
            } => (
                RuntimeFn::OverlappingSubtypes,
                vec![value, descriptor, first, second],
            ),
            Failure::KeyViolated {
                value,
                descriptor,
                key,
                pair,
                variable,
            } => (
                RuntimeFn::KeyViolated,
                vec![value, descriptor, key, pair, variable],
            ),
            Failure::ComponentKeyViolated {
                value,
                descriptor,
                key,
                pair,
                ty,
                component,
            } => (
                RuntimeFn::ComponentKeyViolated,
                vec![value, descriptor, key, pair, ty, component],
            ),
            Failure::ComponentKeyViolatedInFile {
                value,
                descriptor,
                key,
                pair,
                ty,
                component,
                record,
            } => (
                RuntimeFn::ComponentKeyViolatedInFile,
                vec![value, descriptor, key, pair, ty, component, record],
            ),
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
    /// Stop the program because of this failure, by calling its runtime
    /// function, which writes it, with its values.
    Fail(Box<Failure>),
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
    /// A program variable, whose only field is its value. `name` is unique
    /// among variables; codegen derives the symbol from it.
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
    /// The address of field `index` of another data object: a constant
    /// buffer's, after its [`Field::immortal_header`].
    FieldAddress(DataId, usize),
    /// Raw bytes, with no alignment.
    Bytes(Vec<u8>),
    /// Fields laid out together. A struct is aligned to its widest field
    /// and padded to a multiple of that alignment, so it has the same layout
    /// wherever it is embedded.
    Struct(Vec<Field>),
    /// The size of a value with this layout, as a pointer-sized integer.
    Size(Layout),
    /// The offset of the field at a path in a [`Layout::Struct`], a field's
    /// index in each struct from the outermost in, as a pointer-sized
    /// integer.
    Offset(Layout, Vec<usize>),
    /// A value of one of a [`Layout::Union`]'s variants, at its start, and
    /// padded to the union's size.
    Variant(Layout, Box<Field>),
}

impl Field {
    /// The header of a buffer compiled into the program, laid out as
    /// `trendy_runtime::Header`: `trendy_runtime::IMMORTAL` references,
    /// `u64::MAX`, so that it is never released, through none of its
    /// elements, and no type, which only a buffer that is released needs.
    /// Its size is a multiple of every value's alignment, so the buffer
    /// right after it is aligned.
    #[must_use]
    pub fn immortal_header() -> Field {
        Field::Struct(vec![Field::I64(-1), Field::Usize(0), Field::Usize(0)])
    }
}
