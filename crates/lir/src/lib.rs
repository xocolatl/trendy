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
//! [`RuntimeFn::Begin`]). Every function returns a boolean, whether it
//! failed, which the caller checks after the call.
//!
//! An operator's function in a cycle of calls ([`Function::recursive`]) has
//! the tail calling convention, and copies its arguments as it starts, as they
//! may be where another tail call puts them. It owns what they point to, so
//! its caller takes a reference for it to each, and it gives them up as it ends
//! or makes a tail call ([`Terminator::TailCall`]), which gives up all it holds
//! first, and leaves its result pointer and its boolean to the function it
//! calls. It checks there is room on the stack as it starts ([`Inst::CheckStack`]).
//!
//! # Failures
//!
//! A failure makes the function it happens in fail, and so each caller in
//! turn, up to the entry, whose failing makes the program exit with
//! status 1. Each block says where a failure in it goes
//! ([`Block::failure`]): the function's failure exit, which gives up what
//! the function holds, as the end of its statements and blocks would,
//! records that the failure passed through the call of it
//! ([`RuntimeFn::Trace`]), and returns to its caller, failing
//! ([`Terminator::Failed`]). The entry writes those calls after the error
//! ([`RuntimeFn::WriteTrace`]). Four things
//! fail: a call whose callee fails; a runtime function that fails on its
//! own ([`RuntimeFn::fails`]), such as for memory it can't allocate, which
//! writes its error and returns that it failed, checked as a call's is; an
//! arithmetic instruction whose result doesn't exist, which first calls
//! [`RuntimeFn::IntegerFailed`] or [`RuntimeFn::ApproximateFailed`] with
//! the operation; and a check that fails, which ends its block with
//! [`Terminator::Fail`], whose runtime function takes the values involved.
//! Each of those writes the error, with its values, to stderr, with the
//! site the program is at, which [`Inst::Site`] sets. The runtime never
//! stops the program itself. A `fail` writes nothing: compiled code keeps
//! its error, and where it was, and ends its block with
//! [`Terminator::Raise`], for whatever takes the error, or else the entry,
//! which writes it.
//!
//! # Type descriptors
//!
//! Runtime functions that accept any type, such as
//! [`RuntimeFn::DebugValue`], also receive a type descriptor: read-only data
//! laid out as `trendy_runtime::TypeDesc`, built by lowering from ordinary
//! fields. Its sizes and offsets are [`Field::Size`] and [`Field::Offset`],
//! which codegen resolves with the same layout rules it uses for values.

use std::collections::{BTreeMap, BTreeSet};

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

impl Program {
    /// Whether it puts a relation it left in no particular order into canonical order
    /// ([`RuntimeFn::PutInOrder`]). That gives the right answer, so [`Function::loose_read`]
    /// allows it, but it is a mistake: lowering leaves a relation so only if nothing goes on to
    /// want it in order, as it costs a sort that keeping it in order would have saved, each time
    /// it is read.
    #[must_use]
    pub fn sorts_what_it_left_unsorted(&self) -> bool {
        self.functions
            .iter()
            .flat_map(|function| &function.blocks)
            .flat_map(|block| &block.insts)
            .any(|inst| {
                matches!(
                    inst,
                    Inst::CallRuntime {
                        function: RuntimeFn::PutInOrder,
                        ..
                    }
                )
            })
    }
}

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
    /// Whether it is an operator's in a cycle of calls, so that it can be
    /// tail called ([`Terminator::TailCall`]) and call so: the tail calling
    /// convention, and it owns what its arguments point to.
    pub recursive: bool,
    /// Stack slots, each with the layout of what it holds.
    pub slots: Vec<Layout>,
    /// The first block is the entry block.
    pub blocks: Vec<Block>,
}

/// Index into [`Function::slots`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SlotId(pub usize);

impl Function {
    /// Whether it can return failing: whether a [`Terminator::Failed`] can be
    /// reached from its entry, through jumps, branches, and the failure
    /// targets of blocks that can fail, or a [`Terminator::TailCall`], whose
    /// callee, in the same cycle of calls, can be.
    #[must_use]
    pub fn can_fail(&self) -> bool {
        let mut seen = vec![false; self.blocks.len()];
        let mut waiting = vec![BlockId(0)];
        while let Some(at) = waiting.pop() {
            if std::mem::replace(&mut seen[at.0], true) {
                continue;
            }
            let block = &self.blocks[at.0];
            if matches!(
                block.terminator,
                Terminator::Failed | Terminator::TailCall { .. }
            ) {
                return true;
            }
            waiting.extend(block.terminator.successors());
            if block.can_fail() {
                waiting.extend(block.failure);
            }
        }
        false
    }

    /// The function without the sites it sets ([`Inst::Site`],
    /// [`Inst::SaveSite`] and [`Inst::RestoreSite`]), which only a failure
    /// reads, so that, of one that can't fail, a call leaves the program at
    /// its caller's site.
    #[must_use]
    pub fn without_sites(mut self) -> Function {
        for block in &mut self.blocks {
            block.insts.retain(|inst| {
                !matches!(
                    inst,
                    Inst::Site(_) | Inst::SaveSite { .. } | Inst::RestoreSite { .. }
                )
            });
        }
        self
    }

    /// The data its instructions and terminators read or write: of the
    /// program's variables, those it uses.
    #[must_use]
    pub fn data_used(&self) -> BTreeSet<DataId> {
        let mut used = BTreeSet::new();
        let mut note = |place: &Place| {
            if let Base::Data(id) = place.base {
                used.insert(id);
            }
        };
        for block in &self.blocks {
            for inst in &block.insts {
                let (reads, write) = inst.reads_and_write();
                reads.into_iter().chain(write).for_each(&mut note);
            }
            block.terminator.each_read(&mut note);
        }
        used
    }

    /// A slot that something reads after [`RuntimeFn::Release`] has made it zero, or
    /// [`RuntimeFn::DropOperand`] may have freed what it points to, with nothing having
    /// written all of it since, on some way through the function, and the block that reads
    /// it. An address taken of a slot or into its value is given up with it. A value read
    /// from there has lost all it held, so lowering must have copied it out first. What
    /// frees a value twice is found too, as the second may free whatever has since been
    /// given the same memory, which only sometimes shows.
    #[must_use]
    pub fn read_after_release(&self) -> Option<(BlockId, SlotId)> {
        let mut derived: Vec<(SlotId, SlotId)> = Vec::new();
        for inst in self.blocks.iter().flat_map(|block| &block.insts) {
            if let Inst::Element {
                dst, array: from, ..
            }
            | Inst::Address { dst, src: from } = inst
                && let (Base::Slot(address), Base::Slot(of)) = (&dst.base, &from.base)
            {
                derived.push((*address, *of));
            }
        }
        self.first_found(
            |inst, released, found| inst.released(released, &derived, found),
            |place, released, found| read_of(place, released, found),
        )
    }

    /// A slot holding a relation not in canonical order given to something that could
    /// depend on its being in it, on some way through the function, and the block that does
    /// so. A relation is loose, its tuples in no particular order, or sorted as its
    /// descriptor's sort order says, otherwise than by name ([`Arrangement`]). Of equal
    /// relations, only arrays in canonical order are equal, and merges, binary searches and
    /// what depends on them are wrong of a relation in any other order, unless its
    /// descriptor says which, with nothing to say so.
    ///
    /// So such a relation can only be read by what says it can read one: compiled code
    /// going through its tuples or reading its count, what just holds it, what
    /// `Inst::loosely` lists, and, of one sorted otherwise, what is given the descriptor that
    /// says how. Anything else fails this, so a new reader of relations can't be given one by
    /// mistake. Tuples compiled code fills are loose unless it says otherwise, so one that
    /// forgets to say fails this, rather than being trusted. A slot in one order on one way
    /// here and in another on another is loose. `data` is the program's, for the flags a call
    /// has been given, and its descriptors' sort orders.
    #[must_use]
    pub fn loose_read(&self, data: &[Data]) -> Option<(BlockId, SlotId)> {
        self.first_found(
            |inst, arranged, found| inst.loosely(arranged, data, found),
            |place, arranged, found| read_loosely(place, arranged, found),
        )
    }

    /// The first slot `step` or `end` finds, going through each way through the function
    /// with what is known of slots, which `step` keeps as each instruction goes by, and `end`
    /// reads for each place the block's terminator reads, and the block that is in.
    fn first_found<S: Known>(
        &self,
        step: impl Fn(&Inst, &mut S, &mut dyn FnMut(SlotId)),
        end: impl Fn(&Place, &S, &mut dyn FnMut(SlotId)),
    ) -> Option<(BlockId, SlotId)> {
        let mut entering: Vec<Option<S>> = vec![None; self.blocks.len()];
        entering[0] = Some(S::default());
        let mut waiting = vec![BlockId(0)];
        while let Some(at) = waiting.pop() {
            let block = &self.blocks[at.0];
            let mut slots = entering[at.0].clone().unwrap_or_default();
            // What fails can do so anywhere in the block.
            let mut ever = slots.clone();
            let mut found = None;
            for inst in &block.insts {
                step(inst, &mut slots, &mut |slot| found = Some(slot));
                ever.join(&slots);
            }
            block
                .terminator
                .each_read(&mut |place| end(place, &slots, &mut |slot| found = Some(slot)));
            if let Some(slot) = found {
                return Some((at, slot));
            }
            for (next, state) in block
                .terminator
                .successors()
                .into_iter()
                .map(|next| (next, &slots))
                .chain(block.failure.map(|next| (next, &ever)))
            {
                let known = &mut entering[next.0];
                let grows = if let Some(known) = known {
                    known.join(state)
                } else {
                    *known = Some(state.clone());
                    true
                };
                if grows {
                    waiting.push(next);
                }
            }
        }
        None
    }
}

/// What [`Function::first_found`] knows of slots as it goes through a function, which it
/// joins where ways through it meet.
trait Known: Clone + Default {
    /// Add what `other` knows to this, as of either way: whether this then knows more.
    fn join(&mut self, other: &Self) -> bool;
}

/// Slots that something is true of, on some way.
impl Known for BTreeSet<SlotId> {
    fn join(&mut self, other: &Self) -> bool {
        let before = self.len();
        self.extend(other.iter().copied());
        self.len() > before
    }
}

/// How a slot's relation is arranged, as [`Function::loose_read`] follows it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Arrangement {
    /// In canonical order, as every relation's descriptor says unless it says otherwise.
    Canonical,
    /// In no particular order.
    Loose,
    /// Sorted otherwise than by name, by the attributes at the positions in this data
    /// object, a descriptor's sort order: a relation sorted so is described by any
    /// descriptor of its layout with that sort order, whatever its attributes are named,
    /// as through a rename that leaves each where it is.
    Sorted(DataId),
    /// No value at all, which nothing reads until something gives it one
    /// ([`Inst::NoValueYet`]): so in whatever order it is in on another way.
    NoValue,
}

/// The arrangement of each slot's relation that isn't in canonical order, on every way
/// to here: one that isn't there is in canonical order, so one in one order on one way and
/// in another on another, canonical included, is loose, but where it has no value on one,
/// it is in the order it is in on the other.
impl Known for BTreeMap<SlotId, Arrangement> {
    fn join(&mut self, other: &Self) -> bool {
        let mut grows = false;
        let slots: BTreeSet<SlotId> = self.keys().chain(other.keys()).copied().collect();
        for slot in slots {
            let (known, given) = (self.get(&slot).copied(), other.get(&slot).copied());
            let joined = match (known, given) {
                (Some(Arrangement::NoValue), given) => given,
                (known, Some(Arrangement::NoValue)) => known,
                (known, given) if known == given => known,
                _ => Some(Arrangement::Loose),
            };
            if joined != known {
                match joined {
                    Some(arrangement) => self.insert(slot, arrangement),
                    None => self.remove(&slot),
                };
                grows = true;
            }
        }
        grows
    }
}

/// The arrangement of a relation that the descriptor at `place` describes, as it
/// says: sorted otherwise than by name if its sort order isn't null, and otherwise in
/// canonical order.
fn arranged_as(place: Option<&Place>, data: &[Data]) -> Arrangement {
    if let Some(Place {
        base: Base::Data(id),
        ..
    }) = place
        && let Some(&Field::Address(positions)) = data
            .get(id.0)
            .and_then(|data| data.fields.get(SORTED_BY_FIELD))
    {
        return Arrangement::Sorted(positions);
    }
    Arrangement::Canonical
}

/// `found` with the slot `place` reads, if its relation isn't in canonical order, and
/// this is anything but its count, which is true of it in any order.
fn read_loosely(
    place: &Place,
    arranged: &BTreeMap<SlotId, Arrangement>,
    found: &mut dyn FnMut(SlotId),
) {
    read_as(place, Arrangement::Canonical, arranged, found);
}

/// `found` with the slot `place` reads, if its relation isn't arranged as `wanted`, and
/// this is anything but its count, which is true of it in any order.
fn read_as(
    place: &Place,
    wanted: Arrangement,
    arranged: &BTreeMap<SlotId, Arrangement>,
    found: &mut dyn FnMut(SlotId),
) {
    // What has no value is never read where it hasn't.
    if let Base::Slot(slot) = &place.base
        && let arrangement = arranged
            .get(slot)
            .copied()
            .unwrap_or(Arrangement::Canonical)
        && arrangement != wanted
        && arrangement != Arrangement::NoValue
        && place.path != [1]
    {
        found(*slot);
    }
}

/// The flags in the [`Field::Usize`] constant at `place`, or none if it isn't one.
fn flags_at(place: Option<&Place>, data: &[Data]) -> u64 {
    if let Some(Place {
        base: Base::Data(id),
        ..
    }) = place
        && let Some(Field::Usize(flags)) = data.get(id.0).and_then(|data| data.fields.first())
    {
        return *flags;
    }
    0
}

/// The slot that is all of `place`, if it is one.
fn whole_slot(place: &Place) -> Option<SlotId> {
    match (&place.base, place.path.is_empty()) {
        (Base::Slot(slot), true) => Some(*slot),
        _ => None,
    }
}

/// How the relation in the slot that is all of `place` is arranged, if it is one, and
/// otherwise canonical, as what isn't in a slot is.
fn arrangement_of(arranged: &BTreeMap<SlotId, Arrangement>, place: &Place) -> Arrangement {
    whole_slot(place)
        .and_then(|slot| arranged.get(&slot).copied())
        .unwrap_or(Arrangement::Canonical)
}

/// Note that the relation in the slot that is all of `place`, if it is one, is arranged
/// as `arrangement` says.
fn arrange(
    arranged: &mut BTreeMap<SlotId, Arrangement>,
    place: Option<&Place>,
    arrangement: Arrangement,
) {
    if let Some(slot) = place.and_then(whole_slot) {
        if arrangement == Arrangement::Canonical {
            arranged.remove(&slot);
        } else {
            arranged.insert(slot, arrangement);
        }
    }
}

/// What a call of the runtime's `function` with `args` does to `arranged`, as
/// [`Inst::loosely`] says of an instruction.
fn runtime_loosely(
    function: RuntimeFn,
    args: &[Place],
    arranged: &mut BTreeMap<SlotId, Arrangement>,
    data: &[Data],
    found: &mut dyn FnMut(SlotId),
) {
    // Loose if `flag` is among `flags`, and otherwise as the descriptor at `desc` says.
    let flagged_or = |flags: u64, flag: u64, desc: Option<&Place>| {
        if flags & flag == 0 {
            arranged_as(desc, data)
        } else {
            Arrangement::Loose
        }
    };
    match function {
        // They hold it and give it up, or are exact, and so the same in any order
        // (`trendy_numeric`).
        RuntimeFn::Retain
        | RuntimeFn::Keep
        | RuntimeFn::DropOperand
        | RuntimeFn::Adopt
        | RuntimeFn::Sum
        | RuntimeFn::Product => {}
        RuntimeFn::Release => arrange(arranged, args.first(), Arrangement::Canonical),
        // They sort it as the descriptor says.
        RuntimeFn::Sort | RuntimeFn::PutInOrder | RuntimeFn::Reorder => {
            arrange(arranged, args.first(), arranged_as(args.get(1), data));
        }
        RuntimeFn::And | RuntimeFn::Matching => {
            let orders = flags_at(args.last(), data);
            // A join's operands' descriptors come after its result's.
            let first = if function == RuntimeFn::And { 4 } else { 3 };
            // Each operand is as its descriptor says, unless the call is told it is in no
            // particular order.
            for (arg, desc, flag) in [
                (args.get(1), args.get(first), LOOSE_LHS),
                (args.get(2), args.get(first + 1), LOOSE_RHS),
            ] {
                if orders & flag == 0
                    && let Some(arg) = arg
                {
                    read_as(arg, arranged_as(desc, data), arranged, found);
                }
            }
            // A semijoin's tuples are in the order they are in its left operand.
            let result = if function == RuntimeFn::And {
                flagged_or(orders, LOOSE_RESULT, args.get(3))
            } else {
                flagged_or(orders, LOOSE_LHS, args.get(first))
            };
            arrange(arranged, args.first(), result);
        }
        // It searches the relation as the descriptor says it is sorted, and its run is a
        // part of it, sorted alike.
        RuntimeFn::EqualRun => {
            let described = arranged_as(args.get(3), data);
            if let Some(src) = args.get(1) {
                read_as(src, described, arranged, found);
            }
            arrange(arranged, args.first(), described);
        }
        // Both operands are sorted as the descriptor says, and so is the result.
        RuntimeFn::Combine => {
            let described = arranged_as(args.get(3), data);
            for arg in [args.get(1), args.get(2)].into_iter().flatten() {
                read_as(arg, described, arranged, found);
            }
            arrange(arranged, args.first(), described);
        }
        // It takes a reference to the tuples, in the order they are in: only a local
        // variable's slot can say that of them, not a variable of the program.
        RuntimeFn::Assign => {
            let (dst, src) = (args.first(), args.get(1));
            let arrangement =
                src.map_or(Arrangement::Canonical, |src| arrangement_of(arranged, src));
            if let Some(src) = src.filter(|_| dst.and_then(whole_slot).is_none()) {
                read_loosely(src, arranged, found);
            }
            arrange(arranged, dst, arrangement);
        }
        // It gives its first argument new memory, whatever was there before: the last
        // time round a loop.
        RuntimeFn::Allocate => {
            for arg in args.iter().skip(1) {
                read_loosely(arg, arranged, found);
            }
            arrange(arranged, args.first(), Arrangement::Canonical);
        }
        // It makes its tuples anew: sorting them as its result's descriptor says, or
        // leaving them as they come.
        RuntimeFn::Reshape => {
            let flags = flags_at(args.last(), data);
            arrange(
                arranged,
                args.first(),
                flagged_or(flags, LOOSE_RESULT, args.get(2)),
            );
        }
        // A join of several is told what each operand is as it is given it.
        RuntimeFn::Join => {
            let flags = flags_at(args.last(), data);
            arrange(
                arranged,
                args.first(),
                flagged_or(flags, LOOSE_RESULT, None),
            );
        }
        _ => {
            for arg in args {
                read_loosely(arg, arranged, found);
            }
        }
    }
}

/// As for `trendy_runtime::JOIN_LOOSE`.
pub const JOIN_LOOSE: u64 = 1;
/// As for `trendy_runtime::JOIN_DROPS`.
pub const JOIN_DROPS: u64 = 2;
/// As for `trendy_runtime::JOIN_RELEASES`.
pub const JOIN_RELEASES: u64 = 4;

/// As for `trendy_runtime::LOOSE_LHS`.
pub const LOOSE_LHS: u64 = 1;
/// As for `trendy_runtime::LOOSE_RHS`.
pub const LOOSE_RHS: u64 = 2;
/// As for `trendy_runtime::LOOSE_RESULT`.
pub const LOOSE_RESULT: u64 = 4;
/// As for `trendy_runtime::ORDERED_RESULT`.
pub const ORDERED_RESULT: u64 = 8;

/// Where a descriptor, laid out as `trendy_runtime::TypeDesc`, has its sort order among
/// its fields: a [`Field::Address`] of the positions of the attributes a relation's tuples
/// are sorted by, or a null [`Field::Usize`] if by name. Lowering makes one data object for
/// each list of positions, so that two descriptors of one sort order have the same one.
pub const SORTED_BY_FIELD: usize = 6;

/// `found` with the slot `place` reads, if it is among `released`.
fn read_of(place: &Place, released: &BTreeSet<SlotId>, found: &mut dyn FnMut(SlotId)) {
    let slot = match &place.base {
        Base::Slot(slot) | Base::Indirect { slot, .. } => *slot,
        Base::Data(_) | Base::Params | Base::Result => return,
    };
    if released.contains(&slot) {
        found(slot);
    }
}

/// The shape of a value in memory. Codegen derives sizes, alignments and
/// offsets from it for the target: every field at its natural alignment, and
/// every struct padded to its alignment, so that a struct of one field is
/// laid out as that field is.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Layout {
    U8,
    U16,
    U32,
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

/// Declare [`RuntimeFn`], from each function's documentation, its name, the symbol the runtime
/// library exports, and whether it can fail, so that what is known of each is in one place.
macro_rules! runtime_functions {
    ($($(#[$doc:meta])* $name:ident => $symbol:literal, $fails:literal;)*) => {
        /// A function in the runtime library. One that can fail
        /// ([`RuntimeFn::fails`]) returns whether it did, a boolean.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum RuntimeFn {
            $($(#[$doc])* $name,)*
        }

        impl RuntimeFn {
            /// Every runtime function, for checking the runtime against them.
            pub const ALL: &'static [RuntimeFn] = &[$(RuntimeFn::$name),*];

        /// Whether it can fail: for memory it can't allocate, as any that
        /// makes a text, relation or array, or holds a reference in a
        /// statement's scope, can, or for a value it can't make, as a sum
        /// beyond its type's range, a text that writes no number, or a file it
        /// can't read is. It returns whether it failed, a boolean, which
        /// compiled code checks as it does a call's. The runtime functions a
        /// failure calls only write its error, and don't fail themselves.
            #[must_use]
            pub fn fails(self) -> bool {
                match self {
                    $(RuntimeFn::$name => $fails,)*
                }
            }

            /// The symbol the runtime library exports.
            #[must_use]
            pub fn symbol(self) -> &'static str {
                match self {
                    $(RuntimeFn::$name => $symbol,)*
                }
            }
        }
    };
}

runtime_functions! {
    /// `trendy_rt_debug_value(value, descriptor)`.
    DebugValue => "trendy_rt_debug_value", false;
    /// `trendy_rt_debug_table(value, descriptor)`, of a relation. It fails
    /// if the memory for its columns' widths can't be allocated.
    DebugTable => "trendy_rt_debug_table", true;
    /// `trendy_rt_to_text(dst, value, descriptor)`: store at `dst` the
    /// text [`RuntimeFn::DebugValue`] writes for the value, of the type the
    /// descriptor describes, in memory the statement owns (see
    /// [`RuntimeFn::Begin`]). It fails if the memory can't be allocated.
    ToText => "trendy_rt_to_text", true;
    /// `trendy_rt_parse_integer(dst, text)`: store at `dst` the integer the
    /// text writes, as `parse_integer` reads one. It fails if the text
    /// writes none.
    ParseInteger => "trendy_rt_parse_integer", true;
    /// `trendy_rt_parse_approximate(dst, text)`: store at `dst` the
    /// approximate number nearest the decimal number the text writes, as
    /// `parse_approximate` reads one. It fails if the text writes none.
    ParseApproximate => "trendy_rt_parse_approximate", true;
    /// `trendy_rt_parse_boolean(dst, text)`: store at `dst` the boolean the
    /// text writes, as `parse_boolean` reads one. It fails if the text
    /// writes none.
    ParseBoolean => "trendy_rt_parse_boolean", true;
    /// `trendy_rt_to_code_points(dst, text, descriptor)`: store at `dst`
    /// the array of the code points of the text, in order, of the type the
    /// descriptor describes, whose elements are a code point's stored
    /// components, `{ value: integer }`, in memory the statement owns. It
    /// fails if the memory can't be allocated.
    ToCodePoints => "trendy_rt_to_code_points", true;
    /// `trendy_rt_from_code_points(dst, array, descriptor)`: store at `dst`
    /// the text of the code points of the array, in order, of the type the
    /// descriptor describes, whose elements are a code point's stored
    /// components, in memory the statement owns. It fails if the memory
    /// can't be allocated.
    FromCodePoints => "trendy_rt_from_code_points", true;
    /// `trendy_rt_to_characters(dst, text, descriptor)`: store at `dst` the array of
    /// the characters of the text, in order, of the type the descriptor describes, in
    /// memory the statement owns. It fails if the memory can't be allocated.
    ToCharacters => "trendy_rt_to_characters", true;
    /// `trendy_rt_from_characters(dst, array)`: store at `dst` the text of the
    /// characters of the array, in order, in memory the statement owns. It fails if
    /// the memory can't be allocated.
    FromCharacters => "trendy_rt_from_characters", true;
    /// `trendy_rt_sqrt(dst, x)`: store at `dst` the square root of the
    /// approximate number `x`, as `sqrt` computes it. It fails if `x` is
    /// negative, which has none.
    Sqrt => "trendy_rt_sqrt", true;
    /// `trendy_rt_sin(dst, x)`: store at `dst` the sine of the angle `x`,
    /// in radians, as `sin` computes it.
    Sin => "trendy_rt_sin", false;
    /// `trendy_rt_cos(dst, x)`: store at `dst` the cosine of the angle
    /// `x`, in radians, as `cos` computes it.
    Cos => "trendy_rt_cos", false;
    /// `trendy_rt_atan2(dst, y, x)`: store at `dst` the angle, in radians,
    /// from the positive x axis to the point (`x`, `y`), as `atan2`
    /// computes it.
    Atan2 => "trendy_rt_atan2", false;
    /// `trendy_rt_arguments(dst, descriptor)`: store at `dst` the array of the program's
    /// arguments, of the type the descriptor describes, `array text`, but its path, in
    /// memory the statement owns. It fails, with a `not_utf8_argument`, if one isn't
    /// UTF-8 text, or if the memory can't be allocated.
    Arguments => "trendy_rt_arguments", true;
    /// `trendy_rt_environment(dst, descriptor)`: store at `dst` the array of the
    /// variables of the environment the program was started with, each a tuple of its
    /// name and value, the first of each name, but one without a name, of the type the
    /// descriptor describes, in memory the statement owns. It fails, with a
    /// `not_utf8_environment`, if one isn't UTF-8 text, or if the memory can't be
    /// allocated.
    Environment => "trendy_rt_environment", true;
    /// `trendy_rt_read_clock(seconds, nanos)`: store at `seconds` the seconds since
    /// 1970-01-01 in UTC the system's clock shows, rounded down, and at `nanos` the
    /// nanoseconds past them.
    ReadClock => "trendy_rt_read_clock", false;
    /// `trendy_rt_read_transaction_start(seconds, nanos)`: store what the clock showed
    /// as the outermost transaction running began, as [`RuntimeFn::ReadClock`] does.
    ReadTransactionStart => "trendy_rt_read_transaction_start", false;
    /// `trendy_rt_transaction_begins()`: a transaction that can read when it began
    /// ([`RuntimeFn::ReadTransactionStart`]) begins. The outermost reads the clock.
    TransactionBegins => "trendy_rt_transaction_begins", false;
    /// `trendy_rt_transaction_ends()`: one that called
    /// [`RuntimeFn::TransactionBegins`] ends, however it does.
    TransactionEnds => "trendy_rt_transaction_ends", false;
    /// `trendy_rt_random_bytes(dst, count, descriptor)`: store at `dst` an array of
    /// the next `count` random bytes, of the type the descriptor describes, `array byte`
    /// or, for bytes in a hierarchy, the array of their components,
    /// `{ value: integer }`, in memory the statement owns. It fails if the memory
    /// can't be allocated.
    RandomBytes => "trendy_rt_random_bytes", true;
    /// `trendy_rt_put_text(text, error)`: write the text to standard output, or, if
    /// the boolean `error` is true, to standard error, after what was written to either
    /// before it. Standard output is held until it is written all at once, as when it
    /// is full: it fails if writing what it holds fails.
    PutText => "trendy_rt_put_text", true;
    /// `trendy_rt_put_bytes(bytes, error, descriptor)`: as [`RuntimeFn::PutText`], of
    /// the bytes of an array the descriptor describes, an `array byte`.
    PutBytes => "trendy_rt_put_bytes", true;
    /// `trendy_rt_append_text(path, text)`: write the text at the end of the file at
    /// `path`, a text, made if there is none. It fails if the file can't be written.
    AppendText => "trendy_rt_append_text", true;
    /// `trendy_rt_append_bytes(path, bytes, descriptor)`: as [`RuntimeFn::AppendText`],
    /// of the bytes of an array the descriptor describes, an `array byte`.
    AppendBytes => "trendy_rt_append_bytes", true;
    /// `trendy_rt_append_lines(path, lines)`: as [`RuntimeFn::AppendText`], of each
    /// text of an array, followed by a line feed, all at once.
    AppendLines => "trendy_rt_append_lines", true;
    /// `trendy_rt_read_line(dst)`: store at `dst` the next line of standard input, a
    /// text, in memory the statement owns, after writing what standard output holds.
    /// It fails at the end of standard input, for a line that isn't UTF-8 text, for
    /// standard input that can't be read, or if the memory can't be allocated.
    ReadLine => "trendy_rt_read_line", true;
    /// `trendy_rt_read_line_with_errors(dst, errors)`: as [`RuntimeFn::ReadLine`],
    /// but where there is no line, store no text at `dst`, and its error at
    /// `errors`, as for [`RuntimeFn::LoadTextFileWithErrors`].
    ReadLineWithErrors => "trendy_rt_read_line_with_errors", true;
    /// `trendy_rt_read_lines(dst, descriptor)`: store at `dst` the rest of standard
    /// input's lines, an array of the type the descriptor describes, `array text`.
    ReadLines => "trendy_rt_read_lines", true;
    /// `trendy_rt_read_lines_with_errors(dst, errors, descriptor)`: as
    /// [`RuntimeFn::ReadLines`], giving its error at `errors`.
    ReadLinesWithErrors => "trendy_rt_read_lines_with_errors", true;
    /// `trendy_rt_read_bytes(dst, descriptor)`: store at `dst` the rest of standard
    /// input's bytes, an array of the type the descriptor describes, as for
    /// [`RuntimeFn::RandomBytes`].
    ReadBytes => "trendy_rt_read_bytes", true;
    /// `trendy_rt_read_bytes_with_errors(dst, errors, descriptor)`: as
    /// [`RuntimeFn::ReadBytes`], giving its error at `errors`.
    ReadBytesWithErrors => "trendy_rt_read_bytes_with_errors", true;
    /// `trendy_rt_output_ends()`: write what standard output holds, as the program
    /// ends. It fails if that, or a write before it whose failure was found only as
    /// it was written, fails.
    OutputEnds => "trendy_rt_output_ends", true;
    /// `trendy_rt_join(dst, relations, operands, count, result, joined, orders)`: if the
    /// largest of the `count` relations whose addresses are at `relations` is large enough
    /// for it to matter, store at `dst` their join, of the type `result` describes, in an
    /// order of joining chosen from their sizes and samples of them, and store 1 at
    /// `joined`, and otherwise store 0 there, and do nothing else. `operands` says of each
    /// what the runtime's `JoinOperand` does; `orders` has [`LOOSE_RESULT`] if the result
    /// can be left as it comes. It fails if the memory can't be allocated.
    Join => "trendy_rt_join", true;
    /// `trendy_rt_load_text_file(dst, path, descriptor)`: store at `dst` the
    /// array of the lines of the text file at `path`, a text, in order, of
    /// the type the descriptor describes, `array text`, in memory the
    /// statement owns. It fails if the file can't be read, or isn't UTF-8
    /// text, or if the memory can't be allocated.
    LoadTextFile => "trendy_rt_load_text_file", true;
    /// `trendy_rt_load_text_file_with_errors(dst, path, errors,
    /// descriptor)`: as [`RuntimeFn::LoadTextFile`], but a file that can't
    /// be read, or isn't UTF-8 text, gives no lines, and its error at
    /// `errors`, rather than failing: an `unreadable_file` or a
    /// `not_utf8`. `errors` is a struct of `relation { error: error }`,
    /// which it stores the errors in, none if the file is read, and its
    /// descriptor's address; their types are in the table of errors
    /// ([`RuntimeFn::Failures`]).
    LoadTextFileWithErrors => "trendy_rt_load_text_file_with_errors", true;
    /// `trendy_rt_load_binary_file(dst, path, descriptor)`: store at `dst` the
    /// array of the bytes of the file at `path`, a text, in order, of the type
    /// the descriptor describes, as for [`RuntimeFn::RandomBytes`], in memory
    /// the statement owns. It fails if the file can't be read, or if the
    /// memory can't be allocated.
    LoadBinaryFile => "trendy_rt_load_binary_file", true;
    /// `trendy_rt_load_binary_file_with_errors(dst, path, errors, descriptor)`:
    /// as [`RuntimeFn::LoadBinaryFile`], but a file that can't be read gives no
    /// bytes, and its error at `errors`, an `unreadable_file`, rather than
    /// failing, as for [`RuntimeFn::LoadTextFileWithErrors`].
    LoadBinaryFileWithErrors => "trendy_rt_load_binary_file_with_errors", true;
    /// `trendy_rt_save_text_file(path, lines)`: write the array of texts
    /// `lines` to the text file at `path`, a text, each ending in a line feed.
    /// It fails if the file can't be written, or if a line wouldn't read back
    /// as one, before anything is written, or if the memory can't be allocated.
    SaveTextFile => "trendy_rt_save_text_file", true;
    /// `trendy_rt_save_text_file_with_errors(path, lines, errors)`: as
    /// [`RuntimeFn::SaveTextFile`], but a file that can't be written gives its
    /// error at `errors`, an `unwritable_file`, rather than failing: `errors`
    /// as for [`RuntimeFn::LoadTextFileWithErrors`].
    SaveTextFileWithErrors => "trendy_rt_save_text_file_with_errors", true;
    /// `trendy_rt_load_csv(dst, path, header, columns, lines, descriptor)`:
    /// store at `dst` the array of the records of the CSV file at `path`, a
    /// text, in order, after its first if `header`, a boolean, is true, of
    /// the type the descriptor describes, an array of tuples whose
    /// attributes are the fields in the columns `columns` gives them, a
    /// struct of the address of a tuple of integers and its type's
    /// descriptor's, and at `lines` the line each starts on, both in memory
    /// the statement owns.
    /// It fails if a column isn't one of the file's, or two attributes have
    /// one, the file can't be read, isn't UTF-8 text or
    /// CSV, a field isn't a value of its attribute's type, or the memory
    /// can't be allocated.
    LoadCsv => "trendy_rt_load_csv", true;
    /// `trendy_rt_load_csv_with_errors(dst, path, header, columns, lines,
    /// errors, descriptor)`: as [`RuntimeFn::LoadCsv`], with `errors` as
    /// [`RuntimeFn::LoadTextFileWithErrors`]'s, but rather than failing for
    /// a file it can't read, it gives no records, and its error at `errors`,
    /// and for a record it can't, skips it, giving its error there, as
    /// [`RuntimeFn::LoadTextFileWithErrors`] gives its: an
    /// `unreadable_file`, a `not_utf8`, a `malformed_csv`, a
    /// `missing_column` or an `unreadable_field`. Two attributes of one
    /// column, or one of a column below 1, still fail it.
    LoadCsvWithErrors => "trendy_rt_load_csv_with_errors", true;
    /// `trendy_rt_broken_constraint_error(errors, value, descriptor, ty,
    /// constraint, record)`: add to the errors at `errors`, as
    /// [`RuntimeFn::LoadCsvWithErrors`] gives them, that `value`, whose
    /// components the descriptor describes, read from the record `record`
    /// describes, laid out as [`Layout::record`], breaks the constraint of
    /// the type named `ty` written `constraint`, both texts: a
    /// `broken_constraint_in_file`.
    BrokenConstraintError => "trendy_rt_broken_constraint_error", true;
    /// `trendy_rt_component_key_error(errors, value, descriptor, key,
    /// pair, of, record)`: as [`RuntimeFn::BrokenConstraintError`] adds its
    /// error, that the tuples at `pair` of the relation `value`, the
    /// component of a value of a type `of` names, a struct of the type's
    /// name and the component's, texts, share their values for `key`, as
    /// [`RuntimeFn::KeyShared`] found: a `component_key_violated_in_file`.
    ComponentKeyError => "trendy_rt_component_key_error", true;
    /// `trendy_rt_overlapping_subtypes_error(errors, value, descriptor,
    /// first, second, record)`: as [`RuntimeFn::BrokenConstraintError`]
    /// adds its error, that `value` is of both of the subtypes of its type
    /// named `first` and `second`, texts: an
    /// `overlapping_subtypes_in_file`.
    OverlappingSubtypesError => "trendy_rt_overlapping_subtypes_error", true;
    /// `trendy_rt_sum(dst, src, descriptor, index)`: store at `dst` the
    /// sum of the attribute `index` (a [`Field::Usize`], its position by
    /// name) of the tuples of the relation `src`, of the type the
    /// descriptor describes, whose type is integer or approximate: the
    /// exact sum, rounded once. It fails if the sum is beyond its type's
    /// range.
    Sum => "trendy_rt_sum", true;
    /// `trendy_rt_product(dst, src, descriptor, index)`: as
    /// [`RuntimeFn::Sum`], of the product.
    Product => "trendy_rt_product", true;
    /// `trendy_rt_integer_failed(op, lhs, rhs)`: write the error of
    /// `lhs op rhs`, of integers, which has no result, with the operation:
    /// a division by zero, or an overflow. `op` is an [`Arithmetic::code`];
    /// a negation's `lhs` is 0. It takes them by value, not by place, so
    /// only codegen calls it.
    IntegerFailed => "trendy_rt_integer_failed", false;
    /// `trendy_rt_approximate_failed(op, lhs, rhs)`: as
    /// [`RuntimeFn::IntegerFailed`], of approximate numbers.
    ApproximateFailed => "trendy_rt_approximate_failed", false;
    /// `trendy_rt_tuple_mismatch(attribute, lhs, rhs, descriptor)`: write
    /// the error of tuples being joined that disagree on the common
    /// attribute named `attribute`, a text, which is `lhs` in one and `rhs`
    /// in the other, of the type the descriptor describes, with them.
    TupleMismatch => "trendy_rt_tuple_mismatch", false;
    /// `trendy_rt_not_one_tuple(count)`: write the error of a relation
    /// that has `count` (a [`Layout::Usize`]) tuples, not one, where its
    /// only one is taken, with the count.
    NotOneTuple => "trendy_rt_not_one_tuple", false;
    /// `trendy_rt_no_min()`: write the error of a relation that has no
    /// tuples, where the least value of an attribute is taken.
    NoMin => "trendy_rt_no_min", false;
    /// `trendy_rt_no_max()`: as [`RuntimeFn::NoMin`], of the greatest.
    NoMax => "trendy_rt_no_max", false;
    /// `trendy_rt_index_out_of_range(position, length)`: write the error
    /// of an array of `length` elements that has none at `position`, both
    /// [`Layout::I64`], with them.
    IndexOutOfRange => "trendy_rt_index_out_of_range", false;
    /// `trendy_rt_element_conflict(positions, count, nested)`: write the
    /// error of two changes to an array in an `assign` block that set the
    /// element at `positions`, a struct of `count` (a [`Layout::Usize`])
    /// [`Layout::I64`]s, from the outermost array in, or, if `nested` (a
    /// [`Layout::U8`]) is 1, of one that sets it and another an element in
    /// it, with the positions.
    ElementConflict => "trendy_rt_element_conflict", false;
    /// `trendy_rt_broken_constraint(value, descriptor, ty, constraint)`:
    /// write the error of `value`, components the descriptor describes,
    /// selected of the type named `ty`, a text, which break its constraint
    /// written `constraint`, a text, with them.
    BrokenConstraint => "trendy_rt_broken_constraint", false;
    /// `trendy_rt_not_of_type(value, descriptor, ty)`: write the error of
    /// `value`, of the type in a hierarchy the descriptor describes, which
    /// isn't a value of the type named `ty`, a text, that it was treated
    /// as, with them.
    NotOfType => "trendy_rt_not_of_type", false;
    /// `trendy_rt_constraint_violated(constraint, condition)`: write the
    /// error of a statement that left the variables so that the constraint
    /// named `constraint`, a text, is false: its part written `condition`,
    /// a text.
    ConstraintViolated => "trendy_rt_constraint_violated", false;
    /// `trendy_rt_constraint_violated_by(constraint, condition, tuples)`:
    /// as [`RuntimeFn::ConstraintViolated`], with the relation of the
    /// tuples that make it false, written as `debug_value` writes it,
    /// `tuples`, a text.
    ConstraintViolatedBy => "trendy_rt_constraint_violated_by", false;
    /// `trendy_rt_failed(message, error)`: write the error `error`, a
    /// value of the prelude's type `error`, that nothing took, whose
    /// message is `message`, a text, with its kind's code.
    Failed => "trendy_rt_failed", false;
    /// `trendy_rt_warned(message)`: write the warning of a `warn` outside
    /// any transaction, whose message is `message`, a text, and carry on.
    Warned => "trendy_rt_warned", false;
    /// `trendy_rt_report(message, error, failed)`: write an error `main`
    /// gives, a value of the prelude's type `error`, whose message is
    /// `message`, a text, as a failure, if `failed`, a boolean, or else as
    /// a warning, with its kind's code, and no site.
    Report => "trendy_rt_report", false;
    /// `trendy_rt_broken_constraint_in_file(value, descriptor, ty,
    /// constraint, record)`: as [`RuntimeFn::BrokenConstraint`], of a
    /// value `load_csv` read from the record `record` describes, laid out
    /// as [`Layout::record`], with where too.
    BrokenConstraintInFile => "trendy_rt_broken_constraint_in_file", false;
    /// `trendy_rt_overlapping_subtypes(value, descriptor, first, second)`:
    /// write the error of `value`, of the type of the program's own the
    /// descriptor describes, which is of both of its subtypes defined by
    /// constraints named `first` and `second`, texts, with them.
    OverlappingSubtypes => "trendy_rt_overlapping_subtypes", false;
    /// `trendy_rt_broken_call_constraint(value, descriptor, operator,
    /// constraint)`: write the error of a call of the operator named
    /// `operator`, a text, with the arguments at `value`, a tuple the
    /// descriptor describes, which break the constraint written
    /// `constraint`, a text, with them.
    BrokenCallConstraint => "trendy_rt_broken_call_constraint", false;
    /// `trendy_rt_insert(relation, tuples, descriptor, combination)`: add
    /// the tuples of the relation `tuples` to the relation variable at
    /// `relation`, both of the type the descriptor describes, in its own
    /// buffer if nothing else holds it, as `combination`, a
    /// [`Combination::code`] of a union or a disjoint union, says.
    Insert => "trendy_rt_insert", true;
    /// `trendy_rt_tclose_of(dst, relation, value, descriptor, attribute)`: the
    /// tuples of the relation's transitive closure whose attribute, 0 or 1 in
    /// its sort order, is `value`, searching only by paths from or to it.
    TcloseOf => "trendy_rt_tclose_of", true;
    /// `trendy_rt_tclose_of_each(dst, relation, values, descriptor,
    /// values_descriptor, common)`: the tuples of the relation's transitive closure
    /// whose attribute `common` says, of those it describes in each, is a value in
    /// the relation `values`, searching only by paths from or to those.
    TcloseOfEach => "trendy_rt_tclose_of_each", true;
    /// `trendy_rt_drop_operand(value, descriptor)`: give up the current scope's
    /// reference to what the operand at `value` points to, if it is the only
    /// one, as the operation that read it has its result.
    DropOperand => "trendy_rt_drop_operand", false;
    /// `trendy_rt_too_deep()`: write the error of calls that have used up
    /// the stack, each with work left when it returns.
    TooDeep => "trendy_rt_too_deep", true;
    /// `trendy_rt_run(entry, count, strings, environment)`: run the program, whose entry
    /// function is `entry`, on the stack it needs, with the arguments and the environment
    /// C's `main` is given, giving the status to exit with, which only `main` calls.
    Run => "trendy_rt_run", false;
    /// `trendy_rt_remove(relation, tuples, descriptor, combination)`: take
    /// the tuples of the relation `tuples` from the relation variable at
    /// `relation`, both of the type the descriptor describes, in its own
    /// buffer if nothing else holds it, as `combination`, a
    /// [`Combination::code`] of a minus or an included minus, says.
    Remove => "trendy_rt_remove", true;
    /// `trendy_rt_remove_run(relation, run, descriptor)`: take from the
    /// relation variable at `relation`, of the type the descriptor describes,
    /// the tuples of `run`, a run of its own, as [`RuntimeFn::EqualRun`]
    /// makes one, in its own buffer if nothing else holds it.
    RemoveRun => "trendy_rt_remove_run", true;
    /// `trendy_rt_remove_marked(relation, run, marks, descriptor)`: as
    /// [`RuntimeFn::RemoveRun`], of those of the run with a non-zero byte at
    /// their position in `marks`, an array of bytes as long as it.
    RemoveMarked => "trendy_rt_remove_marked", true;
    /// `trendy_rt_key_added(shared, pair, src, descriptor, key, added)`: as
    /// [`RuntimeFn::KeyShared`], of a relation that was without two such
    /// tuples before the tuples of the relation `added` were added to it.
    KeyAdded => "trendy_rt_key_added", true;
    /// `trendy_rt_key_shared(shared, pair, src, descriptor, key)`: store at
    /// `shared`, a [`Layout::U8`], 1 if the relation `src`, of the type the
    /// descriptor describes, has two tuples with the same values for the
    /// attributes of `key`, the descriptor of its projection on one of its
    /// keys, and their positions at `pair`, two [`Layout::Usize`]s; else 0.
    KeyShared => "trendy_rt_key_shared", true;
    /// `trendy_rt_key_violated(src, descriptor, key, pair, variable)`:
    /// write the error of the tuples at `pair` of the relation `src`, about
    /// to be assigned to the variable named `variable`, a text, which share
    /// their values for `key`, as [`RuntimeFn::KeyShared`] found, with the
    /// variable, the key and the tuples.
    KeyViolated => "trendy_rt_key_violated", false;
    /// `trendy_rt_component_key_violated(src, descriptor, key, pair, ty,
    /// component)`: as [`RuntimeFn::KeyViolated`],
    /// of the relation that is the component named `component`, a text, of
    /// a value selected of the type named `ty`, a text.
    ComponentKeyViolated => "trendy_rt_component_key_violated", false;
    /// `trendy_rt_component_key_violated_in_file(src, descriptor, key,
    /// pair, ty, component, record)`: as
    /// [`RuntimeFn::ComponentKeyViolated`], of a value `load_csv` read
    /// from the record `record` describes, laid out as
    /// [`Layout::record`], with where too.
    ComponentKeyViolatedInFile => "trendy_rt_component_key_violated_in_file", false;
    /// `trendy_rt_concat(dst, lhs, rhs)`: store the concatenation of two
    /// texts at `dst`, in memory the statement owns (see
    /// [`RuntimeFn::Begin`]). It fails if the memory can't be allocated.
    Concat => "trendy_rt_concat", true;
    /// `trendy_rt_release(value, descriptor)`: give up the references
    /// that a local variable's value, of the type the descriptor
    /// describes, holds, at the end of its block, and make all of it zero, as
    /// its pointers are before its declaration: so nothing is to read it, or part
    /// of it, from then on ([`Function::read_after_release`]).
    Release => "trendy_rt_release", false;
    /// `trendy_rt_begin(scope)`: begin a statement's scope, in a slot laid
    /// out as [`Layout::scope`]. Until [`RuntimeFn::End`], it holds a
    /// reference to each buffer that runtime functions make, so a value
    /// that no variable holds lasts until its statement ends, and no
    /// longer. A statement needs a scope if it makes a text, relation or
    /// array, assigns a value that points to one, or passes one to an
    /// update operator.
    Begin => "trendy_rt_begin", false;
    /// `trendy_rt_end(scope)`: end the scope at `scope`, the innermost,
    /// giving up its references.
    End => "trendy_rt_end", false;
    /// `trendy_rt_innermost(dst)`: store at `dst`, a [`Layout::Address`],
    /// the address of the innermost scope, or null if there is none: as a
    /// function that begins scopes starts, for its failure exit to end
    /// those it began ([`RuntimeFn::EndSince`]).
    Innermost => "trendy_rt_innermost", false;
    /// `trendy_rt_end_since(saved)`: end, innermost first, as
    /// [`RuntimeFn::End`] does, every scope begun since the one whose
    /// address `saved` holds, which [`RuntimeFn::Innermost`] stored, was
    /// the innermost: those a function that fails began.
    EndSince => "trendy_rt_end_since", false;
    /// `trendy_rt_trace(site, function)`: record that the failure under
    /// way passes through a call of the function named `function`, a text,
    /// made at the site whose address `site` holds, which the function
    /// saved as it started: a function's failure exit does. A null site,
    /// the entry's, records nothing.
    Trace => "trendy_rt_trace", false;
    /// `trendy_rt_traced(dst)`: store at `dst`, a [`Layout::Usize`], how
    /// many calls the trace has recorded.
    Traced => "trendy_rt_traced", false;
    /// `trendy_rt_trace_since(since)`: forget the first `since` (a
    /// [`Layout::Usize`]) calls the trace recorded, which
    /// [`RuntimeFn::Traced`] counted: those of a failure another replaces.
    TraceSince => "trendy_rt_trace_since", false;
    /// `trendy_rt_untrace(since)`: forget the calls the trace recorded
    /// after the first `since` (a [`Layout::Usize`]), which
    /// [`RuntimeFn::Traced`] counted: those of a failure a transaction
    /// takes.
    Untrace => "trendy_rt_untrace", false;
    /// `trendy_rt_unfailed()`: stop the program with an internal error, as a tuple
    /// whose work failed, computed again to fail as it did, didn't: it can't, as what is
    /// computed for a tuple is a function of it.
    Unfailed => "trendy_rt_unfailed", false;
    /// `trendy_rt_rank(state, count)`: order the `count` (a [`Layout::Usize`]) operands of a
    /// condition that can fail, whose state is at `state`, `4 * count` usizes: which is
    /// computed in each place, in turn, then how many times each was computed, how many
    /// times it decided the condition, and what it costs. Those likeliest to decide it for
    /// what they cost come first, and the counts are halved.
    Rank => "trendy_rt_rank", false;
    /// `trendy_rt_write_trace()`: write the calls the trace recorded, as
    /// notes after the failure's error, and forget them.
    WriteTrace => "trendy_rt_write_trace", false;
    /// `trendy_rt_failures(failures)`: give the runtime, once, as the
    /// program starts, the table at `failures` of the errors the language
    /// makes, as its `Failures` lays it out: where a failure's error
    /// waits, and each kind's type and code.
    Failures => "trendy_rt_failures", false;
    /// `trendy_rt_keep(value, descriptor)`: have the statement's scope hold
    /// a reference to each buffer a value, of the type the descriptor
    /// describes, points to: an update operator's argument, which a
    /// variable the operator assigns may hold the only other reference to.
    Keep => "trendy_rt_keep", true;
    /// `trendy_rt_hand_out(value, descriptor)`: have the scope the
    /// innermost one is in hold a reference to each buffer a value, of the
    /// type the descriptor describes, points to: what the work of a
    /// tuple, or of a read-only operator's call, made in a scope of its own
    /// that outlives it, its result, so that the scope's end frees the
    /// rest.
    HandOut => "trendy_rt_hand_out", true;
    /// `trendy_rt_retain(value, descriptor)`: take a reference to each
    /// buffer that a value, of the type the descriptor describes, points
    /// to, for a copy of it that is to hold them: a `fail`'s error, which
    /// waits for what takes it.
    Retain => "trendy_rt_retain", false;
    /// `trendy_rt_adopt(value, descriptor)`: have the buffer of a relation
    /// or array, of the type the descriptor describes, that
    /// [`RuntimeFn::Allocate`] made and compiled code filled, take a
    /// reference to what each of its tuples or elements points to, which
    /// it then holds.
    Adopt => "trendy_rt_adopt", false;
    /// `trendy_rt_reshape(dst, src, result, source, offsets, orders)`: store at
    /// `dst` the relation `src` with its tuples reshaped into the
    /// attributes the descriptor `result` describes, sorted canonically and
    /// without duplicates, in memory the statement owns. `source` is the descriptor of `src`, and
    /// `offsets` an array of [`Field::Offset`]s: for each attribute of
    /// `result`, the offset in a tuple of `src` of the attribute it comes
    /// from. Removing and renaming attributes are both reshaping.
    Reshape => "trendy_rt_reshape", true;
    /// `trendy_rt_relation(dst, tuples, count, descriptor)`: store at `dst`
    /// the relation, of the type the descriptor describes, whose tuples are
    /// the `count` (a [`Field::Usize`]) at `tuples`, one after another,
    /// sorted canonically and without duplicates, in memory the statement
    /// owns.
    Relation => "trendy_rt_relation", true;
    /// `trendy_rt_and(dst, lhs, rhs, result, lhs_desc, rhs_desc, sources,
    /// common, common_count, orders)`: store at `dst` the relation, of the type the
    /// descriptor `result` describes, of every tuple made of a tuple of
    /// `lhs` and one of `rhs` that agree on their common attributes
    /// (Algebra A's AND), sorted canonically, in memory the statement owns. `lhs_desc` and `rhs_desc`
    /// describe the operands. `sources` is an array with, for each attribute
    /// of `result`, a struct of a [`Field::Usize`], 1 if it comes from `rhs`
    /// and 0 if from `lhs`, and its [`Field::Offset`] there. `common` is an
    /// array of `common_count` (a [`Field::Usize`]) structs, one per common
    /// attribute: its offset in a tuple of `lhs`, its offset in one of
    /// `rhs`, and the [`Field::Address`] of its type's descriptor.
    And => "trendy_rt_and", true;
    /// `trendy_rt_index(dst, rel, rel_desc, index_desc, common,
    /// common_count)`: store at `dst` an index of the relation `rel`, which
    /// the descriptor `rel_desc` describes, on its attributes in `common`,
    /// for [`RuntimeFn::Lookup`]: an array of integers, which `index_desc`
    /// describes, of the positions of its tuples, sorted by those
    /// attributes, in memory the statement owns. `common` and
    /// `common_count` are as for [`RuntimeFn::And`]'s, `rel` its left
    /// operand.
    Index => "trendy_rt_index", true;
    /// `trendy_rt_lookup(dst, rel, index, key, result, rel_desc, sources,
    /// common, common_count)`: store at `dst` the relation, of the type the
    /// descriptor `result` describes, of every tuple made of a tuple of
    /// `rel` and the tuple `key` that agree on their common attributes, as
    /// [`RuntimeFn::And`] makes of `rel` and a relation of `key` alone,
    /// finding them in `index`, `rel`'s index on those attributes (see
    /// [`RuntimeFn::Index`]), in memory the statement owns. The other
    /// arguments are as for [`RuntimeFn::And`]'s, `key` its right operand's
    /// one tuple.
    Lookup => "trendy_rt_lookup", true;
    /// `trendy_rt_equal_run(dst, src, value, descriptor, attribute)`: store
    /// at `dst` the run of tuples of the relation at `src`, of the type the
    /// descriptor describes, whose attribute at `attribute` (a
    /// [`Field::Usize`]) in its sort order, from the first, is equal to `value`,
    /// which they are sorted by, as those before it are the same in all of
    /// them: the address of the first in `src`'s buffer, and their number,
    /// so only to be read while `src` holds them.
    EqualRun => "trendy_rt_equal_run", false;
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
    Ungroup => "trendy_rt_ungroup", true;
    /// `trendy_rt_matching(dst, lhs, rhs, lhs_desc, rhs_desc, common,
    /// common_count, negated, orders)`: store at `dst` the relation of the tuples
    /// of `lhs` that have a match in `rhs` on their common attributes, or,
    /// if `negated` (a [`Field::Usize`]) is 1, those that have none, in the order they
    /// are in `lhs`, in memory the statement owns. The other arguments are
    /// as for [`RuntimeFn::And`]'s.
    Matching => "trendy_rt_matching", true;
    /// `trendy_rt_tclose(dst, src, descriptor)`: store at `dst` the
    /// transitive closure of the relation at `src`, of two attributes of
    /// one type, of the type the descriptor describes, in memory the
    /// statement owns: a tuple for each path through its tuples. Fails if
    /// the memory can't be allocated.
    Tclose => "trendy_rt_tclose", true;
    /// `trendy_rt_subset(dst, lhs, rhs, descriptor, proper)`: store at
    /// `dst`, a [`Layout::U8`], whether every tuple of the relation `lhs` is
    /// in the relation `rhs`, of the type the descriptor describes, and, if
    /// `proper` (a [`Field::Usize`]) is 1, `rhs` has more.
    Subset => "trendy_rt_subset", false;
    /// `trendy_rt_combine(dst, lhs, rhs, descriptor, combination)`: store
    /// at `dst` the relations `lhs` and `rhs`, of the type the descriptor
    /// describes, combined as `combination` (a [`Field::Usize`], a
    /// [`Combination::code`]) says, sorted canonically, in
    /// memory the statement owns. It fails if a disjoint union's operands
    /// share a tuple, or an included minus's right operand has one the left
    /// lacks.
    Combine => "trendy_rt_combine", true;
    /// `trendy_rt_allocate(dst, count, descriptor)`: store at `dst` a
    /// relation or an array, of the type the descriptor describes, of
    /// `count` (a [`Layout::Usize`]) tuples or elements, zeroed, in memory
    /// the statement owns, for compiled code to fill in. Then either
    /// [`RuntimeFn::Sort`] copies a relation's tuples, or
    /// [`RuntimeFn::Adopt`] has the buffer hold what they point to.
    Allocate => "trendy_rt_allocate", true;
    /// `trendy_rt_unique(array, descriptor)`: make the array at `array`,
    /// of the type the descriptor describes, the only holder of its
    /// buffer, copying it if anything else holds it, so that its elements
    /// can be changed in place. The array is a variable's value, or an
    /// element of a buffer that only its holder holds.
    Unique => "trendy_rt_unique", true;
    /// `trendy_rt_duplicate(dst, src, descriptor)`: store at `dst` a copy
    /// of the array `src`, of the type the descriptor describes, in a new
    /// buffer that only the statement holds, whose elements can be changed
    /// in place.
    Duplicate => "trendy_rt_duplicate", true;
    /// `trendy_rt_put_in_order(relation, descriptor)`: put the loose relation at
    /// `relation`, whose tuples hold references to what they point to, in canonical
    /// order, in its own buffer if only the statement's scope holds that, and otherwise
    /// in a copy (see [`Function::loose_read`]).
    PutInOrder => "trendy_rt_put_in_order", true;
    /// `trendy_rt_reorder(relation, descriptor)`: put the relation at `relation`, sorted
    /// otherwise than the descriptor says, whose tuples hold references to what they point
    /// to, in the descriptor's order, as [`RuntimeFn::PutInOrder`] does a loose one. That is
    /// choosing an order, for what wants another, not putting in order what was left in none.
    Reorder => "trendy_rt_reorder", true;
    /// `trendy_rt_sort(relation, descriptor)`: replace the relation at
    /// `relation`, of the type the descriptor describes, with one of its
    /// tuples in the canonical order, without duplicates, in memory the
    /// statement owns, freeing the buffer it had if only the statement's
    /// scope holds it.
    Sort => "trendy_rt_sort", true;
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
    Load => "trendy_rt_load", true;
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
    Unload => "trendy_rt_unload", true;
    /// `trendy_rt_compare_text(dst, lhs, rhs)`: store -1, 0 or 1 at `dst`,
    /// an [`Layout::I64`], as `lhs` is before, equal to or after `rhs` in
    /// code point order.
    CompareText => "trendy_rt_compare_text", false;
    /// `trendy_rt_compare(dst, lhs, rhs, descriptor)`: store -1, 0 or 1 at `dst`, an
    /// [`Layout::I64`], as `lhs` is before, equal to or after `rhs`, values of the type
    /// the descriptor describes, in the order the runtime sorts them by.
    Compare => "trendy_rt_compare", false;
    /// `trendy_rt_concat_arrays(dst, lhs, rhs, descriptor)`: store at `dst` the array
    /// of the elements of `lhs` then those of `rhs`, of the type the descriptor
    /// describes, in memory the statement owns. It fails if the memory can't be
    /// allocated.
    ConcatArrays => "trendy_rt_concat_arrays", true;
    /// `trendy_rt_equal(dst, lhs, rhs, descriptor)`: store at `dst`, a
    /// [`Layout::U8`], whether two values of the type the descriptor
    /// describes are equal. For tuples, relations and arrays.
    Equal => "trendy_rt_equal", false;
    /// `trendy_rt_assign(dst, src, descriptor)`: store at `dst`, a
    /// variable's value, the value at `src`, of the type the descriptor
    /// describes, taking a reference to each buffer it points to. The
    /// statement's scope takes over the variable's references to the
    /// buffers of its old value, which the statement may still point into.
    /// For values that point to texts, relations or arrays; others are
    /// copied.
    Assign => "trendy_rt_assign", true;
}

impl RuntimeFn {
    /// Whether its first argument is only where it stores what it makes, which it doesn't
    /// read, and which can hold anything before.
    #[must_use]
    pub fn stores_at_first(self) -> bool {
        matches!(
            self,
            RuntimeFn::ToText
                | RuntimeFn::ParseInteger
                | RuntimeFn::ParseApproximate
                | RuntimeFn::ParseBoolean
                | RuntimeFn::ToCodePoints
                | RuntimeFn::FromCodePoints
                | RuntimeFn::ToCharacters
                | RuntimeFn::FromCharacters
                | RuntimeFn::Sqrt
                | RuntimeFn::Sin
                | RuntimeFn::Cos
                | RuntimeFn::Atan2
                | RuntimeFn::Join
                | RuntimeFn::LoadTextFile
                | RuntimeFn::LoadTextFileWithErrors
                | RuntimeFn::LoadBinaryFile
                | RuntimeFn::LoadBinaryFileWithErrors
                | RuntimeFn::LoadCsv
                | RuntimeFn::LoadCsvWithErrors
                | RuntimeFn::ReadLine
                | RuntimeFn::ReadLineWithErrors
                | RuntimeFn::ReadLines
                | RuntimeFn::ReadLinesWithErrors
                | RuntimeFn::ReadBytes
                | RuntimeFn::ReadBytesWithErrors
                | RuntimeFn::Sum
                | RuntimeFn::Product
                | RuntimeFn::TcloseOf
                | RuntimeFn::TcloseOfEach
                | RuntimeFn::Concat
                | RuntimeFn::Innermost
                | RuntimeFn::Traced
                | RuntimeFn::Reshape
                | RuntimeFn::Relation
                | RuntimeFn::And
                | RuntimeFn::Index
                | RuntimeFn::EqualRun
                | RuntimeFn::Ungroup
                | RuntimeFn::Matching
                | RuntimeFn::Tclose
                | RuntimeFn::Subset
                | RuntimeFn::Combine
                | RuntimeFn::Allocate
                | RuntimeFn::Duplicate
                | RuntimeFn::Load
                | RuntimeFn::Unload
                | RuntimeFn::CompareText
                | RuntimeFn::Compare
                | RuntimeFn::ConcatArrays
                | RuntimeFn::Equal
                | RuntimeFn::Assign
                | RuntimeFn::ReadClock
                | RuntimeFn::ReadTransactionStart
                | RuntimeFn::RandomBytes
                | RuntimeFn::Arguments
                | RuntimeFn::Environment
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Inst {
    /// Fail, writing the error of [`RuntimeFn::TooDeep`], unless the stack has
    /// room: as a [`Function::recursive`] function starts, so that calls that
    /// go too deep fail, as anything else does.
    CheckStack,
    /// Copy a value with `layout` from `src` to `dst`.
    Copy {
        dst: Place,
        src: Place,
        layout: Layout,
    },
    /// Call a function, passing the address of its parameter struct, and
    /// of where its result goes. If the callee fails, so does this, as if
    /// the call were followed by a branch to the block's failure target,
    /// which codegen emits only if it `can_fail`: otherwise the callee never
    /// does, and nothing follows the call for a failure.
    Call {
        function: FunctionId,
        args: Option<Place>,
        result: Option<Place>,
        can_fail: bool,
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
    /// The relation in the slot `relation`, just given room for tuples compiled code
    /// fills, is in no particular order. No code: this says so for
    /// [`Function::loose_read`], for which only [`RuntimeFn::Sort`] or
    /// [`Inst::InOrderOf`] can then say otherwise.
    InAnyOrder(Place),
    /// The tuples of the relation `dst` are in the order they are in the relation `src`,
    /// which compiled code has read them from. No code, as for [`Inst::InAnyOrder`].
    InOrderOf { dst: Place, src: Place },
    /// The tuples of the relation `relation`, which compiled code has filled, are sorted
    /// as the descriptor at `descriptor` says, by name or otherwise, as it filled them in
    /// that order. No code, as for [`Inst::InAnyOrder`].
    SortedAs { relation: Place, descriptor: Place },
    /// Whether the address at `src` is null, as a slot's is once
    /// [`RuntimeFn::Release`] has made it zero, or before anything has been made for
    /// it: the one read of such a slot that means what it says.
    IsNull { dst: Place, src: Place },
    /// Add 1 to the [`Layout::Usize`] at `place`, which counts a loop's
    /// iterations, and so never overflows.
    Increment { place: Place },
    /// Store at `dst`, a [`Layout::I64`], the integer at `src`, stored in `width`.
    Widen {
        dst: Place,
        src: Place,
        width: Width,
    },
    /// Store at `dst`, in `width`, the integer at `src`, a [`Layout::I64`], which is in
    /// its range.
    Narrow {
        dst: Place,
        src: Place,
        width: Width,
    },
    /// Store at `dst`, a [`Layout::Address`], the address of `src`: a
    /// variable passed to a `var` or `out` parameter.
    Address { dst: Place, src: Place },
    /// A local variable at `0` has no value from here until something gives it one, and
    /// what it held is gone: round a loop, what the variable held the last time. No code,
    /// as for [`Inst::InAnyOrder`], and it counts as writing all of it, as
    /// [`Function::read_after_release`] goes: nothing reads it until it has been given one,
    /// which sema checks, and passing its address to an `out` parameter is how it gets one.
    NoValueYet(Place),
    /// As [`Inst::Address`], of a relation given to [`RuntimeFn::Join`], which says if it
    /// is `loose`, in no particular order, as anything that goes on to read one has to
    /// ([`Function::loose_read`]). Only the join reads it, before it gives anything up, so
    /// it isn't among what [`Function::read_after_release`] says goes with `src`.
    RelationAddress { dst: Place, src: Place, loose: bool },
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
    /// Of a boolean, its negation; of an integer, its complement, each bit
    /// flipped.
    Not(Bits),
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
    /// Whether its result may not exist: an integer negation's, which
    /// overflows for the smallest integer.
    #[must_use]
    pub fn can_fail(self) -> bool {
        self == UnaryOp::Negate(Numeric::Integer)
    }

    #[must_use]
    pub fn operand(self) -> Operand {
        match self {
            UnaryOp::Not(bits) => bits.operand(),
            UnaryOp::Negate(numeric) => Operand::Numeric(numeric),
            UnaryOp::Integer => Operand::Usize,
            UnaryOp::Usize | UnaryOp::Approximate => Operand::Numeric(Numeric::Integer),
        }
    }
}

/// What `not`, `and`, `or` and `xor` take the bits of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bits {
    /// Booleans, whose one bit, of a byte, is their value.
    Boolean,
    /// Integers, in two's complement.
    Integer,
}

impl Bits {
    #[must_use]
    pub fn operand(self) -> Operand {
        match self {
            Bits::Boolean => Operand::Boolean,
            Bits::Integer => Operand::Numeric(Numeric::Integer),
        }
    }
}

/// A dyadic operator. Arithmetic is checked: a result that doesn't exist
/// fails, as if the instruction were followed by a branch to a call to
/// [`RuntimeFn::IntegerFailed`] or [`RuntimeFn::ApproximateFailed`], then
/// to the block's failure target. Codegen emits those branches. Approximate results are rounded as IEEE 754
/// binary64 rounds them, and must be finite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    And(Bits),
    Or(Bits),
    Xor(Bits),
    Add(Numeric),
    Subtract(Numeric),
    Multiply(Numeric),
    /// Of approximate numbers. Fails for either zero.
    Divide,
    /// Of integers, truncating toward zero.
    Div,
    /// Of integers, with the sign of the dividend. `i64::MIN mod -1` is 0.
    Mod,
    /// Of integers: the left one times 2 to the power of the right one,
    /// failing if that is out of range, or, of a negative right one, shifted
    /// right by its magnitude (see `trendy_numeric::shifted_left`).
    ShiftLeft,
    /// Of integers: the left one divided by 2 to the power of the right one,
    /// rounded down, or, of a negative right one, shifted left by its
    /// magnitude, failing if that is out of range (see
    /// `trendy_numeric::shifted_right`).
    ShiftRight,
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
    /// Whether its result may not exist: arithmetic's, which overflows or
    /// divides by zero.
    #[must_use]
    pub fn can_fail(self) -> bool {
        match self {
            BinaryOp::And(_) | BinaryOp::Or(_) | BinaryOp::Xor(_) | BinaryOp::Compare(..) => false,
            BinaryOp::Add(_)
            | BinaryOp::Subtract(_)
            | BinaryOp::Multiply(_)
            | BinaryOp::Divide
            | BinaryOp::Div
            | BinaryOp::Mod
            | BinaryOp::ShiftLeft
            | BinaryOp::ShiftRight => true,
        }
    }

    #[must_use]
    pub fn operand(self) -> Operand {
        match self {
            BinaryOp::And(bits) | BinaryOp::Or(bits) | BinaryOp::Xor(bits) => bits.operand(),
            BinaryOp::Add(numeric) | BinaryOp::Subtract(numeric) | BinaryOp::Multiply(numeric) => {
                Operand::Numeric(numeric)
            }
            BinaryOp::Divide => Operand::Numeric(Numeric::Approximate),
            BinaryOp::Div | BinaryOp::Mod | BinaryOp::ShiftLeft | BinaryOp::ShiftRight => {
                Operand::Numeric(Numeric::Integer)
            }
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
    ShiftLeft,
    ShiftRight,
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
            Arithmetic::ShiftLeft => 7,
            Arithmetic::ShiftRight => 8,
        }
    }
}

/// Why a program fails, at a [`Terminator::Fail`], with the values its
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
    /// The program fails with the error at `error`, a value of the
    /// prelude's type `error` that nothing took, whose message is
    /// `message`, a text.
    Own { message: Place, error: Place },
    /// A statement left the variables so that the constraint named
    /// `constraint`, a text, is false: its part written `condition`, a
    /// text, with, where its form says which, the relation of the tuples
    /// that make it false, written as `debug_value` writes it, `tuples`, a
    /// text.
    ConstraintViolated {
        constraint: Place,
        condition: Place,
        tuples: Option<Place>,
    },
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
    /// The arguments at `value`, a tuple `descriptor` describes, of a call
    /// of the operator named `operator`, a text, break the constraint
    /// written `constraint`, a text.
    BrokenCallConstraint {
        value: Place,
        descriptor: Place,
        operator: Place,
        constraint: Place,
    },
    /// Two tuples of a relation share their values for a key.
    Key(KeyFailure),
}

/// Two tuples at `pair` of the relation `value`, of the type `descriptor`
/// describes, that share their values for the key `key`, the descriptor of
/// its projection on the key, as [`RuntimeFn::KeyShared`] found: each of
/// these says what the relation is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyFailure {
    /// About to be assigned to the variable named `variable`, a text.
    Variable {
        value: Place,
        descriptor: Place,
        key: Place,
        pair: Place,
        variable: Place,
    },
    /// The component named `component`, a text, of a value selected of the
    /// type named `ty`, a text.
    Component {
        value: Place,
        descriptor: Place,
        key: Place,
        pair: Place,
        ty: Place,
        component: Place,
    },
    /// As [`KeyFailure::Component`], of a value `load_csv` read from the
    /// record `record` describes, laid out as [`Layout::record`].
    ComponentInFile {
        value: Place,
        descriptor: Place,
        key: Place,
        pair: Place,
        ty: Place,
        component: Place,
        record: Place,
    },
}

impl KeyFailure {
    /// The runtime function that writes this failure's error, and its
    /// arguments, each passed by address.
    #[must_use]
    pub fn call(&self) -> (RuntimeFn, Vec<&Place>) {
        match self {
            KeyFailure::Variable {
                value,
                descriptor,
                key,
                pair,
                variable,
            } => (
                RuntimeFn::KeyViolated,
                vec![value, descriptor, key, pair, variable],
            ),
            KeyFailure::Component {
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
            KeyFailure::ComponentInFile {
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

impl Failure {
    /// The runtime function that writes this failure's error, and its
    /// arguments, each passed by address.
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
            Failure::Own { message, error } => (RuntimeFn::Failed, vec![message, error]),
            Failure::ConstraintViolated {
                constraint,
                condition,
                tuples,
            } => match tuples {
                None => (RuntimeFn::ConstraintViolated, vec![constraint, condition]),
                Some(tuples) => (
                    RuntimeFn::ConstraintViolatedBy,
                    vec![constraint, condition, tuples],
                ),
            },
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
            Failure::BrokenCallConstraint {
                value,
                descriptor,
                operator,
                constraint,
            } => (
                RuntimeFn::BrokenCallConstraint,
                vec![value, descriptor, operator, constraint],
            ),
            Failure::Key(key) => key.call(),
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
    /// Where the program goes when something in the block fails, or
    /// `None` if nothing in it can ([`Block::can_fail`]).
    pub failure: Option<BlockId>,
}

impl Inst {
    /// What the instruction does to `released`, the slots [`RuntimeFn::Release`] has made
    /// zero or [`RuntimeFn::DropOperand`] has given up, with `derived` the addresses taken of
    /// each, as [`Function::read_after_release`] says: `found` with each of them it reads.
    fn released(
        &self,
        released: &mut BTreeSet<SlotId>,
        derived: &[(SlotId, SlotId)],
        found: &mut dyn FnMut(SlotId),
    ) {
        if let Inst::CallRuntime {
            function: function @ (RuntimeFn::Release | RuntimeFn::DropOperand),
            args,
        } = self
        {
            // Releasing what is zero does nothing, and one attribute of a tuple can be given
            // up with the others still to be read.
            let goes = if *function == RuntimeFn::Release {
                args.first()
            } else {
                args.first().filter(|place| place.path.is_empty())
            };
            if *function == RuntimeFn::DropOperand
                && let Some(place) = args.first()
            {
                read_of(place, released, found);
            }
            if let Some(Place {
                base: Base::Slot(slot),
                ..
            }) = goes
            {
                released.insert(*slot);
                let mut more = vec![*slot];
                while let Some(of) = more.pop() {
                    for (address, origin) in derived {
                        if *origin == of && released.insert(*address) {
                            more.push(*address);
                        }
                    }
                }
            }
            return;
        }
        let (reads, written) = self.reads_and_write();
        for place in reads {
            read_of(place, released, found);
        }
        // An address just taken is of what has a value, as reading it just said, however
        // much of what holds the address it is: one field of what a call is given.
        let fresh = matches!(self, Inst::Address { .. } | Inst::Element { .. });
        if let Some(Place {
            base: Base::Slot(slot),
            path,
        }) = written
            && (path.is_empty() || fresh)
        {
            released.remove(slot);
        }
    }

    /// What the instruction does to `arranged`, how each slot's relation is arranged, as
    /// [`Function::loose_read`] says: `found` with each it reads in a way that could depend
    /// on its order. `data` is the program's.
    fn loosely(
        &self,
        arranged: &mut BTreeMap<SlotId, Arrangement>,
        data: &[Data],
        found: &mut dyn FnMut(SlotId),
    ) {
        match self {
            Inst::InAnyOrder(relation) => arrange(arranged, Some(relation), Arrangement::Loose),
            Inst::NoValueYet(variable) => arrange(arranged, Some(variable), Arrangement::NoValue),
            Inst::SortedAs {
                relation,
                descriptor,
            } => arrange(
                arranged,
                Some(relation),
                arranged_as(Some(descriptor), data),
            ),
            Inst::InOrderOf { dst, src } => {
                let arrangement = arrangement_of(arranged, src);
                arrange(arranged, Some(dst), arrangement);
            }
            Inst::Copy { dst, src, .. } => {
                let arrangement = arrangement_of(arranged, src);
                // Into a tuple, an argument or a result, where nothing says what it is.
                if arrangement == Arrangement::Canonical || whole_slot(dst).is_none() {
                    read_loosely(src, arranged, found);
                }
                arrange(arranged, Some(dst), arrangement);
            }
            // Given to a join that says it takes it so.
            Inst::RelationAddress {
                dst, loose: true, ..
            } => arrange(arranged, Some(dst), Arrangement::Canonical),
            // Going through its tuples, in the order they are in.
            Inst::Element { dst, index, .. } => {
                read_loosely(index, arranged, found);
                arrange(arranged, Some(dst), Arrangement::Canonical);
            }
            Inst::CallRuntime { function, args } => {
                runtime_loosely(*function, args, arranged, data, found);
            }
            _ => {
                let (reads, written) = self.reads_and_write();
                for place in reads {
                    read_loosely(place, arranged, found);
                }
                arrange(arranged, written, Arrangement::Canonical);
            }
        }
    }

    /// The places the instruction reads, and the one it writes, if it does.
    fn reads_and_write(&self) -> (Vec<&Place>, Option<&Place>) {
        match self {
            Inst::NoValueYet(place) => (Vec::new(), Some(place)),
            Inst::CheckStack
            | Inst::Site(_)
            | Inst::InAnyOrder(_)
            | Inst::InOrderOf { .. }
            | Inst::SortedAs { .. } => (Vec::new(), None),
            Inst::Copy { dst, src, .. }
            | Inst::Unary { dst, src, .. }
            | Inst::Widen { dst, src, .. }
            | Inst::Narrow { dst, src, .. }
            | Inst::Address { dst, src }
            | Inst::RelationAddress { dst, src, .. } => (vec![src], Some(dst)),
            Inst::Call { args, result, .. } => (args.iter().collect(), result.as_ref()),
            Inst::SaveSite { dst } | Inst::IsNull { dst, .. } => (Vec::new(), Some(dst)),
            Inst::RestoreSite { src } => (vec![src], None),
            // They store a value there, whatever was: a variable's old one, or the
            // index of a loop that has ended.
            // What it looks up in is made first, if `IsNull` says it hasn't been.
            Inst::CallRuntime {
                function: RuntimeFn::Lookup,
                args,
            } => (
                args.iter()
                    .enumerate()
                    .filter(|(i, _)| !matches!(i, 0..=2))
                    .map(|(_, arg)| arg)
                    .collect(),
                args.first(),
            ),
            Inst::CallRuntime { function, args } if function.stores_at_first() => {
                (args.iter().skip(1).collect(), args.first())
            }
            Inst::CallRuntime { args, .. } => (args.iter().collect(), None),
            Inst::Binary { dst, lhs, rhs, .. } => (vec![lhs, rhs], Some(dst)),
            Inst::Element {
                dst, array, index, ..
            } => (vec![array, index], Some(dst)),
            Inst::Increment { place } => (vec![place], None),
        }
    }
}

impl Terminator {
    /// What it goes to next, without failing.
    fn successors(&self) -> Vec<BlockId> {
        match self {
            Terminator::Jump(next) => vec![*next],
            Terminator::Branch {
                then, otherwise, ..
            } => vec![*then, *otherwise],
            Terminator::Return
            | Terminator::Fail(_)
            | Terminator::Raise
            | Terminator::TailCall { .. }
            | Terminator::Failed => Vec::new(),
        }
    }

    /// `read` with each place it reads.
    fn each_read(&self, read: &mut impl FnMut(&Place)) {
        match self {
            Terminator::Branch { condition, .. } => read(condition),
            Terminator::TailCall { args, .. } => {
                if let Some(args) = args {
                    read(args);
                }
            }
            Terminator::Fail(failure) => {
                for place in failure.call().1 {
                    read(place);
                }
            }
            Terminator::Return | Terminator::Jump(_) | Terminator::Raise | Terminator::Failed => {}
        }
    }
}

impl Block {
    /// Whether something in the block can fail: a call, a runtime
    /// function's that can, an arithmetic instruction, a
    /// [`Terminator::Fail`] or a [`Terminator::Raise`].
    #[must_use]
    pub fn can_fail(&self) -> bool {
        self.insts.iter().any(Inst::can_fail)
            || matches!(self.terminator, Terminator::Fail(_) | Terminator::Raise)
    }
}

impl Inst {
    /// Whether the instruction can fail: a call whose callee can, a
    /// runtime function's that can ([`RuntimeFn::fails`]), or an
    /// arithmetic instruction, whose result may not exist.
    #[must_use]
    pub fn can_fail(&self) -> bool {
        match self {
            Inst::Call { can_fail, .. } => *can_fail,
            Inst::CheckStack => true,
            Inst::CallRuntime { function, .. } => function.fails(),
            Inst::Unary { op, .. } => op.can_fail(),
            Inst::Binary { op, .. } => op.can_fail(),
            _ => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Terminator {
    /// Return to the caller, which carries on.
    Return,
    Jump(BlockId),
    /// To `then` if the [`Layout::U8`] at `condition` is 1, and to
    /// `otherwise` if it is 0.
    Branch {
        condition: Place,
        then: BlockId,
        otherwise: BlockId,
    },
    /// Fail because of this failure: call its runtime function, which
    /// writes its error, with its values, then go to the block's failure
    /// target.
    Fail(Box<Failure>),
    /// Fail with an error compiled code has kept, a `fail`'s, writing
    /// nothing: go to the block's failure target, which may take it.
    Raise,
    /// Become a call of the function `function`, of the same result, in
    /// place of this one, which is gone, so that its caller is the caller of
    /// this one, which it returns to, and whose result pointer it is given:
    /// a tail call. So `args` is no slot, but what lasts: the function's
    /// [`Function::recursive`] copies them as it starts, and owns what they
    /// point to.
    TailCall {
        function: FunctionId,
        args: Option<Place>,
    },
    /// Return to the caller, failing: the end of a function's failure exit.
    Failed,
}

/// Index into [`Program::data`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
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

/// How an integer is stored in less than a [`Layout::I64`]: unsigned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Width {
    U8,
    U16,
    U32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Field {
    U8(u8),
    U16(u16),
    U32(u32),
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
        Field::Struct(vec![
            Field::I64(-1),
            Field::Usize(0),
            Field::Usize(0),
            Field::Usize(0),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot(n: usize) -> Place {
        Place::whole(Base::Slot(SlotId(n)))
    }

    fn part(n: usize) -> Place {
        Place {
            base: Base::Slot(SlotId(n)),
            path: vec![1],
        }
    }

    fn descriptor() -> Place {
        Place::whole(Base::Data(DataId(0)))
    }

    fn release(n: usize) -> Inst {
        Inst::CallRuntime {
            function: RuntimeFn::Release,
            args: vec![slot(n), descriptor()],
        }
    }

    fn copy(dst: Place, src: Place) -> Inst {
        Inst::Copy {
            dst,
            src,
            layout: Layout::I64,
        }
    }

    fn block(insts: Vec<Inst>, terminator: Terminator) -> Block {
        Block {
            insts,
            terminator,
            failure: None,
        }
    }

    fn function(blocks: Vec<Block>) -> Function {
        Function {
            name: "f".to_string(),
            params: None,
            result: None,
            recursive: false,
            slots: vec![Layout::I64; 8],
            blocks,
        }
    }

    fn found(blocks: Vec<Block>) -> Option<(usize, usize)> {
        function(blocks)
            .read_after_release()
            .map(|(block, slot)| (block.0, slot.0))
    }

    fn one(insts: Vec<Inst>) -> Option<(usize, usize)> {
        found(vec![block(insts, Terminator::Return)])
    }

    /// What a call is given holds the address of a variable, one of its fields. Given up since
    /// the last call, and given a value again, the address taken now is of that.
    #[test]
    fn an_address_taken_again_is_of_what_has_a_value() {
        let address = || Inst::Address {
            dst: part(1),
            src: slot(0),
        };
        let call = || copy(slot(2), slot(1));
        // Taken of what is given up, it reads what is gone.
        assert_eq!(one(vec![release(0), address()]), Some((0, 0)));
        // And taken before, what it holds is gone.
        assert_eq!(one(vec![address(), release(0), call()]), Some((0, 1)));
        // Given a value again, what is taken again is good.
        assert_eq!(
            one(vec![
                address(),
                release(0),
                copy(slot(0), slot(3)),
                address(),
                call()
            ]),
            None
        );
    }

    /// Going through what has been given up reads freed memory, and giving it up again can
    /// free what has been given that memory since.
    #[test]
    fn what_has_been_dropped_is_read_no_more() {
        let drop_at = |place: Place| Inst::CallRuntime {
            function: RuntimeFn::DropOperand,
            args: vec![place, descriptor()],
        };
        assert_eq!(one(vec![drop_at(slot(0)), drop_at(slot(0))]), Some((0, 0)));
        assert_eq!(
            one(vec![drop_at(slot(0)), copy(slot(1), part(0))]),
            Some((0, 0))
        );
        assert_eq!(one(vec![release(0), drop_at(slot(0))]), Some((0, 0)));
        // Not once it holds something else.
        assert_eq!(
            one(vec![
                drop_at(slot(0)),
                copy(slot(0), slot(1)),
                drop_at(slot(0))
            ]),
            None
        );
        // One attribute of a tuple goes, and the others are read.
        assert_eq!(one(vec![drop_at(part(0)), copy(slot(1), slot(0))]), None);
        // On one way of two.
        let branch = Terminator::Branch {
            condition: slot(7),
            then: BlockId(1),
            otherwise: BlockId(2),
        };
        assert_eq!(
            found(vec![
                block(vec![], branch),
                block(vec![drop_at(slot(0))], Terminator::Jump(BlockId(2))),
                block(vec![drop_at(slot(0))], Terminator::Return),
            ]),
            Some((2, 0))
        );
    }

    #[test]
    fn a_read_after_a_release_is_found() {
        // Part of it, as all of it is made zero.
        assert_eq!(one(vec![release(0), copy(slot(1), part(0))]), Some((0, 0)));
        assert_eq!(one(vec![release(0), copy(slot(1), slot(0))]), Some((0, 0)));
        // By any instruction that reads.
        let binary = Inst::Binary {
            dst: slot(1),
            op: BinaryOp::Compare(Comparison::Less, Operand::Usize),
            lhs: slot(2),
            rhs: part(0),
        };
        assert_eq!(one(vec![release(0), binary]), Some((0, 0)));
        let call = Inst::CallRuntime {
            function: RuntimeFn::Retain,
            args: vec![slot(0), descriptor()],
        };
        assert_eq!(one(vec![release(0), call]), Some((0, 0)));
        // By what the block ends with.
        let branch = Terminator::Branch {
            condition: part(0),
            then: BlockId(0),
            otherwise: BlockId(0),
        };
        assert_eq!(found(vec![block(vec![release(0)], branch)]), Some((0, 0)));
    }

    #[test]
    fn what_comes_before_a_release_or_after_a_write_is_read_as_it_was() {
        assert_eq!(one(vec![copy(slot(1), part(0)), release(0)]), None);
        assert_eq!(
            one(vec![
                release(0),
                copy(slot(0), slot(1)),
                copy(slot(2), part(0))
            ]),
            None
        );
        // Writing part of it leaves the rest as it is.
        assert_eq!(
            one(vec![
                release(0),
                copy(part(0), slot(1)),
                copy(slot(2), slot(0))
            ]),
            Some((0, 0))
        );
        // What stores a value in a slot doesn't read it.
        for function in [RuntimeFn::Assign, RuntimeFn::Index] {
            let stores = Inst::CallRuntime {
                function,
                args: vec![slot(0), slot(1), descriptor()],
            };
            assert_eq!(one(vec![release(0), stores, copy(slot(2), slot(0))]), None);
        }
        // Another slot, and releasing it again, which reads nothing.
        assert_eq!(
            one(vec![release(0), release(0), copy(slot(1), slot(2))]),
            None
        );
    }

    #[test]
    fn a_null_test_and_what_it_guards_read_what_they_mean_to() {
        let tested = Inst::IsNull {
            dst: slot(1),
            src: part(0),
        };
        assert_eq!(one(vec![release(0), tested]), None);
        let lookup = Inst::CallRuntime {
            function: RuntimeFn::Lookup,
            args: vec![slot(1), slot(0), slot(2), slot(3), descriptor()],
        };
        assert_eq!(one(vec![release(0), release(2), lookup.clone()]), None);
        // Its key is read like anything.
        assert_eq!(one(vec![release(3), lookup]), Some((0, 3)));
    }

    #[test]
    fn a_release_is_followed_along_every_way_through_the_function() {
        let jump = |to| Terminator::Jump(BlockId(to));
        let branch = |then, otherwise| Terminator::Branch {
            condition: slot(7),
            then: BlockId(then),
            otherwise: BlockId(otherwise),
        };
        // On one branch of two.
        assert_eq!(
            found(vec![
                block(vec![], branch(1, 2)),
                block(vec![release(0)], jump(3)),
                block(vec![], jump(3)),
                block(vec![copy(slot(1), slot(0))], Terminator::Return),
            ]),
            Some((3, 0))
        );
        assert_eq!(
            found(vec![
                block(vec![], branch(2, 1)),
                block(vec![release(0)], jump(3)),
                block(vec![], jump(3)),
                block(vec![copy(slot(1), slot(0))], Terminator::Return),
            ]),
            Some((3, 0))
        );
        // Round a loop, where it is read before it is released.
        assert_eq!(
            found(vec![
                block(vec![], jump(1)),
                block(vec![copy(slot(1), slot(0)), release(0)], branch(1, 2)),
                block(vec![], Terminator::Return),
            ]),
            Some((1, 0))
        );
        // But not if each time round writes it first.
        assert_eq!(
            found(vec![
                block(vec![], jump(1)),
                block(
                    vec![copy(slot(0), slot(2)), copy(slot(1), slot(0)), release(0)],
                    branch(1, 2)
                ),
                block(vec![], Terminator::Return),
            ]),
            None
        );
        // Where the block fails to, from anywhere in it.
        let mut failing = block(vec![release(0), copy(slot(0), slot(1))], Terminator::Return);
        failing.failure = Some(BlockId(1));
        assert_eq!(
            found(vec![
                failing,
                block(vec![copy(slot(2), slot(0))], Terminator::Failed),
            ]),
            Some((1, 0))
        );
    }

    /// The constant of `flags` at data 0, for what gives a loose relation or takes one.
    fn flagged(flags: u64) -> Vec<Data> {
        vec![Data {
            kind: DataKind::Constant,
            writable: false,
            fields: vec![Field::Usize(flags)],
        }]
    }

    /// A call of `function` with `dst` and `operands`, ending in the flags at data 0.
    fn with_flags(function: RuntimeFn, dst: usize, lhs: usize, rhs: usize) -> Inst {
        Inst::CallRuntime {
            function,
            args: vec![
                slot(dst),
                slot(lhs),
                slot(rhs),
                Place {
                    base: Base::Data(DataId(0)),
                    path: vec![0],
                },
            ],
        }
    }

    fn runtime(function: RuntimeFn, args: Vec<Place>) -> Inst {
        Inst::CallRuntime { function, args }
    }

    fn loose_in(insts: Vec<Inst>, flags: u64) -> Option<(usize, usize)> {
        function(vec![block(insts, Terminator::Return)])
            .loose_read(&flagged(flags))
            .map(|(block, slot)| (block.0, slot.0))
    }

    /// The flags at data 0, as for [`flagged`], then a descriptor of a relation sorted
    /// otherwise than by name at data 1, and one of a relation sorted by name at data 2.
    fn described(flags: u64) -> Vec<Data> {
        let descriptor = |sorted_by: Field| {
            let mut fields = vec![Field::Usize(0); SORTED_BY_FIELD + 1];
            fields[SORTED_BY_FIELD] = sorted_by;
            Data {
                kind: DataKind::Constant,
                writable: false,
                fields,
            }
        };
        let mut data = flagged(flags);
        data.push(descriptor(Field::Address(DataId(0))));
        data.push(descriptor(Field::Usize(0)));
        data
    }

    /// The descriptor at data `id`.
    fn desc(id: usize) -> Place {
        Place::whole(Base::Data(DataId(id)))
    }

    #[test]
    fn a_relation_sorted_otherwise_is_read_only_as_its_descriptor_says() {
        let checked = |blocks: Vec<Block>, flags: u64| {
            function(blocks)
                .loose_read(&described(flags))
                .map(|(block, slot)| (block.0, slot.0))
        };
        let in_one = |insts: Vec<Inst>| checked(vec![block(insts, Terminator::Return)], 0);
        let sorted = || runtime(RuntimeFn::Sort, vec![slot(0), desc(1)]);
        let combined = |described: usize| {
            runtime(
                RuntimeFn::Combine,
                vec![slot(4), slot(0), slot(0), desc(described)],
            )
        };
        // Merged with what describes it so, or with what describes it by name.
        assert_eq!(in_one(vec![sorted(), combined(1)]), None);
        assert_eq!(in_one(vec![sorted(), combined(2)]), Some((0, 0)));
        // A relation sorted by name isn't sorted otherwise.
        let by_name = || runtime(RuntimeFn::Sort, vec![slot(0), desc(2)]);
        assert_eq!(in_one(vec![by_name(), combined(1)]), Some((0, 0)));
        assert_eq!(in_one(vec![by_name(), combined(2)]), None);
        // What merges them is sorted as it says, for what reads it next.
        let next = runtime(RuntimeFn::Combine, vec![slot(6), slot(4), slot(4), desc(2)]);
        assert_eq!(in_one(vec![sorted(), combined(1), next]), Some((0, 4)));
        // Read by what knows nothing of sort orders.
        let printed = runtime(RuntimeFn::DebugValue, vec![slot(0)]);
        assert_eq!(in_one(vec![sorted(), printed]), Some((0, 0)));
        // Given to a join with a descriptor by name, unless it is told it is loose.
        let joined = |flags: u64| {
            checked(
                vec![block(
                    vec![
                        sorted(),
                        runtime(
                            RuntimeFn::And,
                            vec![
                                slot(4),
                                slot(0),
                                slot(5),
                                desc(2),
                                desc(2),
                                desc(2),
                                desc(0),
                            ],
                        ),
                    ],
                    Terminator::Return,
                )],
                flags,
            )
        };
        assert_eq!(joined(0), Some((0, 0)));
        assert_eq!(joined(LOOSE_LHS), None);
        // Sorted otherwise on one way here, and by name on the other: loose where they meet.
        let branches = |second: usize| {
            vec![
                block(
                    Vec::new(),
                    Terminator::Branch {
                        condition: slot(7),
                        then: BlockId(1),
                        otherwise: BlockId(2),
                    },
                ),
                block(vec![sorted()], Terminator::Jump(BlockId(3))),
                block(
                    vec![runtime(RuntimeFn::Sort, vec![slot(0), desc(second)])],
                    Terminator::Jump(BlockId(3)),
                ),
                block(vec![combined(1)], Terminator::Return),
            ]
        };
        assert_eq!(checked(branches(1), 0), None);
        assert_eq!(checked(branches(2), 0), Some((3, 0)));
        // With no value on one way, which nothing reads until it has one, it is as it is on
        // the other: a local constant read before what computes it, as in a loop, is read
        // only once it is computed. In canonical order on one, it is loose.
        let unvalued = |first: Inst| {
            vec![
                block(
                    vec![first],
                    Terminator::Branch {
                        condition: slot(7),
                        then: BlockId(1),
                        otherwise: BlockId(2),
                    },
                ),
                block(vec![sorted()], Terminator::Jump(BlockId(2))),
                block(vec![combined(1)], Terminator::Return),
            ]
        };
        assert_eq!(checked(unvalued(Inst::NoValueYet(slot(0))), 0), None);
        assert_eq!(checked(unvalued(copy(slot(0), slot(5))), 0), Some((2, 0)));
    }

    #[test]
    fn what_depends_on_order_is_not_given_a_loose_relation() {
        let made = || with_flags(RuntimeFn::And, 0, 1, 2);
        let found = Some((0, 0));
        // By anything not known to take one, whatever it is.
        for function in [
            RuntimeFn::Combine,
            RuntimeFn::EqualRun,
            RuntimeFn::Index,
            RuntimeFn::Tclose,
            RuntimeFn::DebugValue,
        ] {
            let read = runtime(function, vec![slot(3), slot(0)]);
            assert_eq!(
                loose_in(vec![made(), read], LOOSE_RESULT),
                found,
                "{function:?}"
            );
        }
        // Assigned to what isn't a local variable's slot, as a variable of the program is.
        let assigned = runtime(RuntimeFn::Assign, vec![part(3), slot(0)]);
        assert_eq!(loose_in(vec![made(), assigned], LOOSE_RESULT), found);
        // Read in order once it is assigned to a local variable, which goes on being loose.
        let assigned = runtime(RuntimeFn::Assign, vec![slot(3), slot(0)]);
        let read = runtime(RuntimeFn::Combine, vec![slot(4), slot(3)]);
        assert_eq!(
            loose_in(vec![made(), assigned, read], LOOSE_RESULT),
            Some((0, 3))
        );
        // As an operand it doesn't say it takes in any order.
        let joined = with_flags(RuntimeFn::And, 3, 0, 4);
        assert_eq!(loose_in(vec![made(), joined], LOOSE_RESULT), found);
        let semi = with_flags(RuntimeFn::Matching, 3, 4, 0);
        assert_eq!(loose_in(vec![made(), semi], LOOSE_RESULT), found);
        // Into what nothing says the order of.
        let into_field = copy(part(3), slot(0));
        assert_eq!(loose_in(vec![made(), into_field], LOOSE_RESULT), found);
        let into_result = copy(Place::whole(Base::Result), slot(0));
        assert_eq!(loose_in(vec![made(), into_result], LOOSE_RESULT), found);
        // Its pointer, and its address, as what could go on to read it in order.
        let pointer = Place {
            base: Base::Slot(SlotId(0)),
            path: vec![0],
        };
        assert_eq!(
            loose_in(vec![made(), copy(slot(3), pointer)], LOOSE_RESULT),
            found
        );
        let address = Inst::Address {
            dst: slot(3),
            src: slot(0),
        };
        assert_eq!(loose_in(vec![made(), address], LOOSE_RESULT), found);
        // Through another slot, as it is still what it was.
        let read = runtime(RuntimeFn::Combine, vec![slot(4), slot(3)]);
        assert_eq!(
            loose_in(vec![made(), copy(slot(3), slot(0)), read], LOOSE_RESULT),
            Some((0, 3))
        );
        // By a call, or what the function ends with.
        let call = Inst::Call {
            function: FunctionId(0),
            args: Some(slot(0)),
            result: None,
            can_fail: true,
        };
        assert_eq!(loose_in(vec![made(), call], LOOSE_RESULT), found);
        let tail = Terminator::TailCall {
            function: FunctionId(0),
            args: Some(slot(0)),
        };
        assert_eq!(
            function(vec![block(vec![made()], tail)])
                .loose_read(&flagged(LOOSE_RESULT))
                .map(|(block, slot)| (block.0, slot.0)),
            found
        );
    }

    #[test]
    fn what_takes_a_loose_relation_says_so() {
        let made = || with_flags(RuntimeFn::And, 0, 1, 2);
        let flags = LOOSE_RESULT | LOOSE_LHS | LOOSE_RHS;
        // Joins and semijoins, of what they are told is loose, either way round.
        for reader in [
            with_flags(RuntimeFn::And, 3, 0, 4),
            with_flags(RuntimeFn::And, 3, 4, 0),
            with_flags(RuntimeFn::Matching, 3, 0, 4),
            with_flags(RuntimeFn::Matching, 3, 4, 0),
            // What makes its tuples anew, sorted or not, and what is true in any order.
            runtime(
                RuntimeFn::Reshape,
                vec![
                    slot(3),
                    slot(0),
                    Place {
                        base: Base::Data(DataId(0)),
                        path: vec![0],
                    },
                ],
            ),
            runtime(RuntimeFn::Sum, vec![slot(3), slot(0)]),
            runtime(RuntimeFn::Product, vec![slot(3), slot(0)]),
            // What only holds it.
            runtime(RuntimeFn::Retain, vec![slot(0)]),
            runtime(RuntimeFn::Keep, vec![slot(0)]),
            runtime(RuntimeFn::DropOperand, vec![slot(0)]),
            runtime(RuntimeFn::Adopt, vec![slot(0)]),
            // Going through its tuples, and its count.
            Inst::Element {
                dst: slot(3),
                array: slot(0),
                index: slot(4),
                element: Layout::I64,
            },
            copy(slot(3), part(0)),
            copy(slot(3), slot(0)),
        ] {
            assert_eq!(
                loose_in(vec![made(), reader.clone()], flags),
                None,
                "{reader:?}"
            );
        }
    }

    #[test]
    fn only_what_puts_a_relation_in_order_says_it_is() {
        let made = || with_flags(RuntimeFn::And, 0, 1, 2);
        let read = || runtime(RuntimeFn::Combine, vec![slot(4), slot(0)]);
        // A join told to sort, or not.
        assert_eq!(loose_in(vec![made(), read()], 0), None);
        assert_eq!(loose_in(vec![made(), read()], LOOSE_RESULT), Some((0, 0)));
        // A semijoin's result is as its left operand is.
        let semi = || with_flags(RuntimeFn::Matching, 0, 1, 2);
        assert_eq!(loose_in(vec![semi(), read()], LOOSE_RHS), None);
        assert_eq!(loose_in(vec![semi(), read()], LOOSE_LHS), Some((0, 0)));
        // A reshape told not to sort.
        let reshape = |flags_at_end: bool| {
            let mut args = vec![slot(0), slot(1)];
            if flags_at_end {
                args.push(Place {
                    base: Base::Data(DataId(0)),
                    path: vec![0],
                });
            }
            runtime(RuntimeFn::Reshape, args)
        };
        assert_eq!(loose_in(vec![reshape(true), read()], 0), None);
        assert_eq!(
            loose_in(vec![reshape(true), read()], LOOSE_RESULT),
            Some((0, 0))
        );
        // Sorted, or sorted again, or released, or written with what isn't.
        for fixed in [
            runtime(RuntimeFn::Sort, vec![slot(0)]),
            runtime(RuntimeFn::PutInOrder, vec![slot(0)]),
            release(0),
            copy(slot(0), slot(7)),
        ] {
            assert_eq!(
                loose_in(vec![made(), fixed.clone(), read()], LOOSE_RESULT),
                None,
                "{fixed:?}"
            );
        }
        // What compiled code fills is loose until it says otherwise, or it is sorted.
        let filled = || Inst::InAnyOrder(slot(0));
        assert_eq!(loose_in(vec![filled(), read()], 0), Some((0, 0)));
        let kept = |src| Inst::InOrderOf { dst: slot(0), src };
        assert_eq!(loose_in(vec![filled(), kept(slot(5)), read()], 0), None);
        assert_eq!(
            loose_in(
                vec![
                    with_flags(RuntimeFn::And, 5, 1, 2),
                    filled(),
                    kept(slot(5)),
                    read()
                ],
                LOOSE_RESULT
            ),
            Some((0, 0))
        );
    }

    /// A join of several: what it says of its result is in the flags at data 0, and those of
    /// its operands are told by how their addresses were taken.
    #[test]
    fn a_join_of_several_says_what_it_reads_and_what_it_makes() {
        let join = runtime(
            RuntimeFn::Join,
            vec![
                slot(3),
                slot(4),
                slot(5),
                slot(6),
                descriptor(),
                slot(7),
                Place {
                    base: Base::Data(DataId(0)),
                    path: vec![0],
                },
            ],
        );
        let made = || with_flags(RuntimeFn::And, 0, 1, 2);
        let address = |loose| Inst::RelationAddress {
            dst: slot(4),
            src: slot(0),
            loose,
        };
        // A loose relation it isn't told is loose.
        assert_eq!(
            loose_in(vec![made(), address(false)], LOOSE_RESULT),
            Some((0, 0))
        );
        // Or one it is.
        assert_eq!(loose_in(vec![made(), address(true)], LOOSE_RESULT), None);
        // Its result, only if it is told it can leave it so.
        let read = runtime(RuntimeFn::Combine, vec![slot(8), slot(3)]);
        assert_eq!(
            loose_in(vec![join.clone(), read.clone()], LOOSE_RESULT),
            Some((0, 3))
        );
        assert_eq!(loose_in(vec![join, read], 0), None);
    }

    #[test]
    fn a_loose_relation_is_followed_along_every_way_through_the_function() {
        let made = || with_flags(RuntimeFn::And, 0, 1, 2);
        let read = || runtime(RuntimeFn::Combine, vec![slot(4), slot(0)]);
        let jump = |to| Terminator::Jump(BlockId(to));
        let branch = |then, otherwise| Terminator::Branch {
            condition: slot(7),
            then: BlockId(then),
            otherwise: BlockId(otherwise),
        };
        let found_in = |blocks: Vec<Block>| {
            function(blocks)
                .loose_read(&flagged(LOOSE_RESULT))
                .map(|(block, slot)| (block.0, slot.0))
        };
        // On one way of two.
        assert_eq!(
            found_in(vec![
                block(vec![], branch(2, 1)),
                block(vec![made()], jump(3)),
                block(vec![], jump(3)),
                block(vec![read()], Terminator::Return),
            ]),
            Some((3, 0))
        );
        // Round a loop.
        assert_eq!(
            found_in(vec![
                block(vec![], jump(1)),
                block(vec![read(), made()], branch(1, 2)),
                block(vec![], Terminator::Return),
            ]),
            Some((1, 0))
        );
        // Given new memory each time round, it is read as it is then.
        let allocated = runtime(RuntimeFn::Allocate, vec![slot(0), part(5), slot(6)]);
        assert_eq!(
            found_in(vec![
                block(vec![], jump(1)),
                block(
                    vec![
                        allocated,
                        Inst::InAnyOrder(slot(0)),
                        Inst::InOrderOf {
                            dst: slot(0),
                            src: slot(8)
                        },
                        read(),
                        Inst::InAnyOrder(slot(0)),
                    ],
                    branch(1, 2)
                ),
                block(vec![], Terminator::Return),
            ]),
            None
        );
    }

    #[test]
    fn an_address_of_what_is_released_goes_with_it() {
        let at_element = |address| Place {
            base: Base::Indirect {
                slot: SlotId(address),
                pointee: Layout::I64,
            },
            path: Vec::new(),
        };
        let element = Inst::Element {
            dst: slot(1),
            array: slot(0),
            index: slot(2),
            element: Layout::I64,
        };
        assert_eq!(
            one(vec![
                element.clone(),
                release(0),
                copy(slot(3), at_element(1))
            ]),
            Some((0, 1))
        );
        assert_eq!(
            one(vec![element, copy(slot(3), at_element(1)), release(0)]),
            None
        );
        let address = Inst::Address {
            dst: slot(1),
            src: slot(0),
        };
        assert_eq!(
            one(vec![address, release(0), copy(slot(3), at_element(1))]),
            Some((0, 1))
        );
    }
}
