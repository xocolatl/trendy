//! The runtime library linked into every trendy executable.
//!
//! It is `no_std` and depends only on the platform's C library, so it stays
//! small. In the workspace it builds as an `rlib`, so that its tests run with
//! `std`; `trendyc`'s build script builds it again as a `staticlib`, with
//! `panic = "abort"`, and embeds that archive for linking.
//!
//! # Type descriptors
//!
//! Operators with an `any` parameter receive the value's address and a
//! [`TypeDesc`] for its type. The compiler emits descriptors as read-only
//! data, laid out exactly like these `#[repr(C)]` structs. Every size and
//! offset in a descriptor comes from the compiler, so the runtime never
//! repeats its layout rules.

#![cfg_attr(not(test), no_std)]

use core::fmt::{self, Write};

pub const KIND_BOOLEAN: u8 = 0;
pub const KIND_INTEGER: u8 = 1;
pub const KIND_APPROXIMATE: u8 = 2;
pub const KIND_TEXT: u8 = 3;
pub const KIND_TUPLE: u8 = 4;
pub const KIND_RELATION: u8 = 5;

/// An integer result out of range.
pub const FAIL_INTEGER_OVERFLOW: u32 = 0;
/// `/`, `div` or `mod` by zero.
pub const FAIL_DIVISION_BY_ZERO: u32 = 1;
/// An approximate result beyond the largest finite magnitude.
pub const FAIL_APPROXIMATE_OVERFLOW: u32 = 2;
/// Memory for a result couldn't be allocated.
pub const FAIL_OUT_OF_MEMORY: u32 = 3;
/// Tuples being joined disagree on a common attribute.
pub const FAIL_TUPLE_MISMATCH: u32 = 4;
/// The operands of a disjoint union have a tuple in common.
pub const FAIL_SHARED_TUPLE: u32 = 7;
/// The right operand of an included minus has a tuple the left lacks.
pub const FAIL_MISSING_TUPLE: u32 = 8;
/// A relation has no tuples, where its only one is taken.
pub const FAIL_NO_TUPLE: u32 = 5;
/// A relation has several tuples, where its only one is taken.
pub const FAIL_SEVERAL_TUPLES: u32 = 6;
/// Two tuples of a relation assigned to a variable have the same values
/// for one of its keys.
pub const FAIL_KEY_VIOLATED: u32 = 9;
/// Two changes to a variable in an `assign` block remove the same tuple.
pub const FAIL_CONFLICT: u32 = 10;
/// A relation has no tuples, where the least value of an attribute is
/// taken.
pub const FAIL_NO_MIN: u32 = 11;
/// A relation has no tuples, where the greatest value of an attribute is
/// taken.
pub const FAIL_NO_MAX: u32 = 12;

/// The line written to stderr for each `FAIL_` constant, with the
/// compiler's error code, which `trendyc --explain` describes.
#[must_use]
pub fn failure_message(failure: u32) -> &'static str {
    match failure {
        FAIL_INTEGER_OVERFLOW => "error[R0001]: integer overflow\n",
        FAIL_DIVISION_BY_ZERO => "error[R0002]: division by zero\n",
        FAIL_APPROXIMATE_OVERFLOW => "error[R0001]: approximate overflow\n",
        FAIL_OUT_OF_MEMORY => "error[R0003]: out of memory\n",
        FAIL_TUPLE_MISMATCH => "error[R0004]: tuples disagree on a common attribute\n",
        FAIL_SHARED_TUPLE => {
            "error[R0006]: d_union or d_insert adds a tuple that is already there\n"
        }
        FAIL_MISSING_TUPLE => {
            "error[R0007]: i_minus or i_delete removes a tuple that isn't there\n"
        }
        FAIL_KEY_VIOLATED => {
            "error[R0008]: two tuples of a variable have the same values for one of its keys\n"
        }
        FAIL_CONFLICT => "error[R0009]: two changes in an assign block remove the same tuple\n",
        FAIL_NO_TUPLE => "error[R0005]: the relation has no tuples, but exactly one was taken\n",
        FAIL_SEVERAL_TUPLES => {
            "error[R0005]: the relation has several tuples, but exactly one was taken\n"
        }
        FAIL_NO_MIN => "error[R0010]: the relation has no tuples, so it has no min\n",
        FAIL_NO_MAX => "error[R0010]: the relation has no tuples, so it has no max\n",
        // The compiler and the runtime disagree: a bug.
        _ => "error[X0000]: internal error: unknown run-time failure\n",
    }
}

/// A `text` value: the address of its UTF-8 bytes, and their number. The
/// compiler lays text out as this pair.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Text {
    pub bytes: *const u8,
    pub len: usize,
}

/// Store `lhs || rhs` at `dst`, in memory the statement owns. Stops the
/// program if the memory can't be allocated.
///
/// # Safety
///
/// `lhs` and `rhs` must point to valid text values, and `dst` to memory for
/// one.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_concat(dst: *mut Text, lhs: *const Text, rhs: *const Text) {
    // SAFETY: guaranteed by the caller.
    unsafe {
        let (lhs, rhs) = (lhs.read_unaligned(), rhs.read_unaligned());
        let Some(len) = lhs.len.checked_add(rhs.len) else {
            trendy_rt_fail(FAIL_OUT_OF_MEMORY)
        };
        let bytes = allocate_owned(len);
        core::ptr::copy_nonoverlapping(lhs.bytes, bytes, lhs.len);
        core::ptr::copy_nonoverlapping(rhs.bytes, bytes.add(lhs.len), rhs.len);
        dst.write_unaligned(Text { bytes, len });
    }
}

/// Store at `dst` 1 if `lhs` and `rhs`, of the type `desc` describes, are
/// equal, and 0 if not. Two values are equal exactly when they are the same
/// value: approximate numbers when they have the same bits (IEEE 754
/// totalOrder), tuples attribute by attribute, and relations when they have
/// the same tuples. The compiler keeps every relation's tuples in one
/// canonical order, so equal relations have equal arrays.
///
/// # Safety
///
/// `lhs` and `rhs` must point to valid values of the type `desc` describes,
/// `desc` to a valid descriptor, and `dst` to a byte.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_equal(
    dst: *mut u8,
    lhs: *const u8,
    rhs: *const u8,
    desc: *const TypeDesc,
) {
    // SAFETY: guaranteed by the caller.
    unsafe { dst.write(u8::from(equal(lhs, rhs, &*desc))) }
}

/// Whether `lhs` and `rhs`, of the type `desc` describes, are equal.
///
/// # Safety
///
/// As for [`trendy_rt_equal`].
#[must_use]
pub unsafe fn equal(lhs: *const u8, rhs: *const u8, desc: &TypeDesc) -> bool {
    // SAFETY: guaranteed by the caller; each size and offset in a
    // descriptor is the layout of the value it describes.
    unsafe {
        match desc.kind {
            KIND_BOOLEAN => *lhs == *rhs,
            KIND_INTEGER | KIND_APPROXIMATE => {
                lhs.cast::<u64>().read_unaligned() == rhs.cast::<u64>().read_unaligned()
            }
            KIND_TEXT => {
                let (a, b) = (read_slice(lhs), read_slice(rhs));
                a.1 == b.1
                    && core::slice::from_raw_parts(a.0, a.1)
                        == core::slice::from_raw_parts(b.0, b.1)
            }
            KIND_TUPLE => tuples_equal(lhs, rhs, desc),
            KIND_RELATION => {
                let ((a, count), (b, other)) = (read_slice(lhs), read_slice(rhs));
                count == other
                    && (0..count).all(|i| {
                        let offset = i * desc.element_size;
                        tuples_equal(a.add(offset), b.add(offset), desc)
                    })
            }
            _ => false,
        }
    }
}

/// Whether two tuples with the attributes of `desc` are equal.
///
/// # Safety
///
/// As for [`equal`], with `desc` describing a tuple or relation.
unsafe fn tuples_equal(lhs: *const u8, rhs: *const u8, desc: &TypeDesc) -> bool {
    // SAFETY: guaranteed by the caller.
    unsafe {
        attributes(desc)
            .iter()
            .all(|attr| equal(lhs.add(attr.offset), rhs.add(attr.offset), &*attr.desc))
    }
}

/// Store at `dst` -1, 0 or 1, as `lhs` is before, equal to or after `rhs`
/// in code point order, which is the order of their UTF-8 bytes.
///
/// # Safety
///
/// `lhs` and `rhs` must point to valid text values, and `dst` to an `i64`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_compare_text(dst: *mut i64, lhs: *const Text, rhs: *const Text) {
    // SAFETY: guaranteed by the caller.
    unsafe {
        let ordering = text_bytes(lhs.read_unaligned()).cmp(text_bytes(rhs.read_unaligned()));
        dst.write_unaligned(ordering as i64);
    }
}

/// The bytes of `text`.
///
/// # Safety
///
/// `text` must be a valid text value.
unsafe fn text_bytes<'a>(text: Text) -> &'a [u8] {
    // SAFETY: guaranteed by the caller.
    unsafe { core::slice::from_raw_parts(text.bytes, text.len) }
}

/// The header of a block of memory holding what values point to: each
/// text's bytes and each relation's tuples. A block is a statement's, in
/// its [`Scope`], or a variable's, holding what its value points to. Its
/// alignment keeps the memory after it aligned for any value.
#[cfg(not(test))]
#[repr(C, align(16))]
struct Owned {
    /// The scope's next block: a scope keeps its blocks in a list, newest
    /// first.
    next: *mut Owned,
    /// For a variable's block, when it was made, on [`CLOCK`].
    made: u64,
}

/// The memory a statement owns, from [`trendy_rt_begin`] to
/// [`trendy_rt_end`]: every text and relation that runtime functions make
/// while it runs, since no value outlives the statement that computes it,
/// and variables' old values that it can still point into. So a value never
/// needs freeing on its own, however many times a loop made one.
///
/// Scopes nest as calls do: a statement that calls an update operator holds
/// its arguments while the operator's statements run. Compiled code keeps
/// each scope in a stack slot laid out as this struct, which only the
/// runtime reads.
#[cfg(not(test))]
#[repr(C)]
pub struct Scope {
    /// The blocks it owns, newest first.
    owned: *mut Owned,
    /// The scope of the statement this one's operator was called from, or
    /// null.
    outer: *mut Scope,
    /// When the statement began, on [`CLOCK`].
    begun: u64,
}

/// The scope of the statement running now.
#[cfg(not(test))]
static CURRENT: core::sync::atomic::AtomicPtr<Scope> =
    core::sync::atomic::AtomicPtr::new(core::ptr::null_mut());

/// The time, one more for each statement that begins and each value
/// assigned to a variable: whether a statement began while a variable had
/// a value.
#[cfg(not(test))]
static CLOCK: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// The next time on [`CLOCK`].
#[cfg(not(test))]
fn tick() -> u64 {
    CLOCK.fetch_add(1, core::sync::atomic::Ordering::Relaxed) + 1
}

/// Begin the scope at `scope` of a statement, which owns what runtime
/// functions make until [`trendy_rt_end`] ends it.
///
/// # Safety
///
/// `scope` must point to memory for a [`Scope`] that stays valid until
/// [`trendy_rt_end`] ends it, and each scope must end before the one it is
/// in.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_begin(scope: *mut Scope) {
    use core::sync::atomic::Ordering::Relaxed;
    // SAFETY: guaranteed by the caller.
    unsafe {
        scope.write(Scope {
            owned: core::ptr::null_mut(),
            outer: CURRENT.load(Relaxed),
            begun: tick(),
        });
    }
    CURRENT.store(scope, Relaxed);
}

/// End the scope at `scope`, releasing the memory its statement owns: the
/// end of the lifetime of every text and relation runtime functions made
/// while it ran.
///
/// # Safety
///
/// `scope` must point to the innermost scope, which [`trendy_rt_begin`]
/// began.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_end(scope: *mut Scope) {
    use core::sync::atomic::Ordering::Relaxed;
    // SAFETY: guaranteed by the caller. Every block in the list was
    // allocated by `allocate_owned` or `trendy_rt_assign`, and is in no
    // other list, so is released only here, once.
    unsafe {
        let mut block = (*scope).owned;
        while !block.is_null() {
            let next = (*block).next;
            free(block.cast());
            block = next;
        }
        CURRENT.store((*scope).outer, Relaxed);
    }
}

/// Memory for `bytes` bytes, owned by the current statement and released
/// when it ends. Stops the program if there is none.
#[cfg(not(test))]
fn allocate_owned(bytes: usize) -> *mut u8 {
    use core::sync::atomic::Ordering::Relaxed;
    // In units of the header: one for it, then enough for the bytes.
    let units = bytes.div_ceil(size_of::<Owned>()) + 1;
    let block = allocate::<Owned>(units);
    // SAFETY: `block` has room for the header and `bytes` more. Compiled
    // code begins a scope for every statement that makes a text or
    // relation, so there is a current one.
    unsafe {
        let scope = CURRENT.load(Relaxed);
        block.write(Owned {
            next: (*scope).owned,
            made: 0,
        });
        (*scope).owned = block;
        block.add(1).cast::<u8>()
    }
}

/// Store at `dst` a copy of the value at `src`, of the type `desc`
/// describes, in memory of its own, which outlives the statement: a
/// variable's new value, which `owner` then holds. `owner` holds the memory
/// of the variable's current value, or null while that is the value
/// compiled into the program. That memory becomes the outermost scope's
/// whose statement began while it was the variable's value, since those
/// statements, and only those, can point into it, such as with an
/// argument; it is released when that statement ends. Stops the program if
/// the memory can't be allocated.
///
/// # Safety
///
/// `src` must point to a valid value of the type `desc` describes, `desc`
/// to a valid descriptor, `dst` to memory for one value that doesn't
/// overlap `src`, and `owner` to null or memory that an earlier call
/// stored there. The statement must have begun a scope.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_assign(
    dst: *mut u8,
    owner: *mut *mut u8,
    src: *const u8,
    desc: *const TypeDesc,
) {
    use core::sync::atomic::Ordering::Relaxed;
    // SAFETY: guaranteed by the caller; the block has room for everything
    // the value points to, as measured.
    unsafe {
        let desc = &*desc;
        let mut measured = Arena::measuring();
        persist(core::ptr::null_mut(), src, desc, &mut measured);
        let units = measured.used.div_ceil(size_of::<Owned>()) + 1;
        let block = allocate::<Owned>(units);
        block.write(Owned {
            next: core::ptr::null_mut(),
            made: tick(),
        });
        let mut arena = Arena {
            base: block.add(1).cast(),
            used: 0,
        };
        core::ptr::copy_nonoverlapping(src, dst, desc.size);
        persist(dst, src, desc, &mut arena);
        let old = owner.cast::<*mut Owned>().read_unaligned();
        if !old.is_null() {
            // The current statement began after the old value was made,
            // and the scopes it is in began before it.
            let mut scope = CURRENT.load(Relaxed);
            while !(*scope).outer.is_null() && (*(*scope).outer).begun > (*old).made {
                scope = (*scope).outer;
            }
            (*old).next = (*scope).owned;
            (*scope).owned = old;
        }
        owner.cast::<*mut Owned>().write_unaligned(block);
    }
}

/// Where [`persist`] puts what a value points to: one block, filled in
/// order, or none while measuring how much it needs.
struct Arena {
    /// The block, or null while measuring.
    base: *mut u8,
    /// The bytes taken so far.
    used: usize,
}

impl Arena {
    fn measuring() -> Self {
        Arena {
            base: core::ptr::null_mut(),
            used: 0,
        }
    }

    /// Room for `size` bytes, aligned to `align` from the block's start:
    /// its address, or null while measuring.
    fn take(&mut self, size: usize, align: usize) -> *mut u8 {
        let start = self.used.next_multiple_of(align);
        self.used = start + size;
        if self.base.is_null() {
            core::ptr::null_mut()
        } else {
            self.base.wrapping_add(start)
        }
    }
}

/// The alignment of each relation's tuples in an [`Arena`]: that of any
/// value, as the block's start is.
const ARRAY_ALIGN: usize = 16;

/// Copy what the value at `src`, of the type `desc` describes, points to
/// into `arena`, and point the copy of the value at `dst` to it: each text's
/// bytes and each relation's tuples, and what those point to in turn. While
/// measuring, `dst` is null, and nothing is written.
///
/// # Safety
///
/// `src` must point to a valid value of the type `desc` describes, and,
/// unless measuring, `dst` to a copy of it, and the arena have room for
/// what it points to.
unsafe fn persist(dst: *mut u8, src: *const u8, desc: &TypeDesc, arena: &mut Arena) {
    // SAFETY: guaranteed by the caller.
    unsafe {
        match desc.kind {
            KIND_TEXT => {
                let (bytes, len) = read_slice(src);
                let to = arena.take(len, 1);
                if !to.is_null() {
                    core::ptr::copy_nonoverlapping(bytes, to, len);
                    dst.cast::<Text>().write_unaligned(Text { bytes: to, len });
                }
            }
            KIND_TUPLE => persist_tuple(dst, src, desc, arena),
            KIND_RELATION => {
                let (tuples, count) = read_slice(src);
                let size = count * desc.element_size;
                let to = arena.take(size, ARRAY_ALIGN);
                if !to.is_null() {
                    core::ptr::copy_nonoverlapping(tuples, to, size);
                    dst.cast::<Text>().write_unaligned(Text {
                        bytes: to,
                        len: count,
                    });
                }
                for i in 0..count {
                    let offset = i * desc.element_size;
                    persist_tuple(to.wrapping_add(offset), tuples.add(offset), desc, arena);
                }
            }
            _ => {}
        }
    }
}

/// [`persist`] for a tuple with the attributes of `desc`, a tuple or
/// relation descriptor.
///
/// # Safety
///
/// As for [`persist`].
unsafe fn persist_tuple(dst: *mut u8, src: *const u8, desc: &TypeDesc, arena: &mut Arena) {
    // SAFETY: guaranteed by the caller.
    unsafe {
        for attr in attributes(desc) {
            persist(
                dst.wrapping_add(attr.offset),
                src.add(attr.offset),
                &*attr.desc,
                arena,
            );
        }
    }
}

/// Memory for `count` values of type `T`, at least one byte, so that even
/// an empty text or relation has an address. `malloc`'s memory is aligned
/// for any type. Stops the program if there is none.
#[cfg(not(test))]
fn allocate<T>(count: usize) -> *mut T {
    let Some(size) = count.checked_mul(size_of::<T>()) else {
        trendy_rt_fail(FAIL_OUT_OF_MEMORY)
    };
    // SAFETY: `malloc` has no preconditions.
    let memory = unsafe { malloc(size.max(1)) };
    if memory.is_null() {
        trendy_rt_fail(FAIL_OUT_OF_MEMORY);
    }
    memory.cast()
}

/// Store at `dst` the relation at `src`, with each tuple reshaped into the
/// attributes `result` describes, in canonical order and without
/// duplicates, in memory the statement owns. `source` describes `src`,
/// and `offsets` gives, for each attribute of `result`, the offset in a
/// tuple of `src` of the attribute it comes from. Removing attributes is
/// reshaping, and can make tuples equal; renaming is too, and can change
/// their order.
///
/// # Safety
///
/// `src` must point to a valid relation of the type `source` describes,
/// `result` and `source` to valid relation descriptors, `offsets` to one
/// offset per attribute of `result`, and `dst` to memory for a relation.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_reshape(
    dst: *mut Text,
    src: *const u8,
    result: *const TypeDesc,
    source: *const TypeDesc,
    offsets: *const usize,
) {
    // SAFETY: guaranteed by the caller; each size and offset is the layout
    // of the value it describes.
    unsafe {
        let (result, source) = (&*result, &*source);
        let (tuples, count) = read_slice(src);
        let size = result.element_size;
        let Some(bytes) = count.checked_mul(size) else {
            trendy_rt_fail(FAIL_OUT_OF_MEMORY)
        };
        let attributes = attributes(result);
        let offsets = core::slice::from_raw_parts(offsets, attributes.len());
        // Each tuple with only the kept attributes, in the source's order.
        let projected = allocate::<u8>(bytes);
        core::ptr::write_bytes(projected, 0, bytes);
        for i in 0..count {
            let from = tuples.add(i * source.element_size);
            let to = projected.add(i * size);
            for (attr, &offset) in attributes.iter().zip(offsets) {
                core::ptr::copy_nonoverlapping(
                    from.add(offset),
                    to.add(attr.offset),
                    (*attr.desc).size,
                );
            }
        }
        let canonical = canonical(projected, count, result);
        free(projected.cast());
        dst.write_unaligned(Text {
            bytes: canonical.0,
            len: canonical.1,
        });
    }
}

/// Store at `dst` the relation `desc` describes whose tuples are the
/// `*count` at `tuples`, one after another, in canonical order and without
/// duplicates, in memory the statement owns.
///
/// # Safety
///
/// `tuples` must point to `*count` valid tuples of the relation's heading,
/// `desc` to a valid relation descriptor, and `dst` to memory for a
/// relation.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_relation(
    dst: *mut Text,
    tuples: *const u8,
    count: *const usize,
    desc: *const TypeDesc,
) {
    // SAFETY: guaranteed by the caller.
    unsafe {
        let (bytes, len) = canonical(tuples, count.read_unaligned(), &*desc);
        dst.write_unaligned(Text { bytes, len });
    }
}

/// An attribute the operands of [`trendy_rt_and`] have in common: where it
/// is in a tuple of each, and its type. The compiler emits an array of
/// these, laid out like this struct.
#[repr(C)]
pub struct Common {
    pub lhs: usize,
    pub rhs: usize,
    pub desc: *const TypeDesc,
}

/// Where an attribute of a result comes from: which tuple, `from`, and
/// where it is in that tuple. For [`trendy_rt_and`], `from` is 0 for the
/// left operand and 1 for the right; for [`trendy_rt_ungroup`], 0 for the
/// tuple being ungrouped and `k` for the tuple of its `k`th ungrouped
/// relation. The compiler emits an array of these, laid out like this
/// struct.
#[repr(C)]
pub struct Source {
    pub from: usize,
    pub offset: usize,
}

/// Tuples of [`trendy_rt_and`]'s operands that agree on the common
/// attributes: those in `lhs` from `lhs_start` to `lhs_end`, and those in
/// `rhs` from `rhs_start` to `rhs_end`, of the sorted tuples.
#[cfg(not(test))]
struct Group {
    lhs_start: usize,
    lhs_end: usize,
    rhs_start: usize,
    rhs_end: usize,
}

/// Store at `dst` the relation `result` describes whose tuples are each
/// made of a tuple of the relation `lhs` and one of `rhs` that agree on
/// their common attributes (Algebra A's AND), in canonical order, in memory
/// the statement owns. `lhs_desc` and `rhs_desc` describe the operands. `sources` says where each attribute of `result` comes
/// from, and `common` lists the `*common_count` common attributes.
///
/// Both operands' tuples are sorted on the common attributes, then merged:
/// each run of tuples of one that agree on them pairs with the matching run
/// of the other.
///
/// # Safety
///
/// `lhs` and `rhs` must point to valid relations of the types `lhs_desc`
/// and `rhs_desc` describe, `result` to a valid descriptor of the relation
/// type with the attributes of both, `sources` to one source per attribute
/// of `result`, `common` to `*common_count` common attributes, and `dst`
/// to memory for a relation.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_and(
    dst: *mut Text,
    lhs: *const u8,
    rhs: *const u8,
    result: *const TypeDesc,
    lhs_desc: *const TypeDesc,
    rhs_desc: *const TypeDesc,
    sources: *const Source,
    common: *const Common,
    common_count: *const usize,
) {
    // SAFETY: guaranteed by the caller; each size and offset is the layout
    // of the value it describes.
    unsafe {
        let (result, lhs_desc, rhs_desc) = (&*result, &*lhs_desc, &*rhs_desc);
        let ((lhs, lhs_count), (rhs, rhs_count)) = (read_slice(lhs), read_slice(rhs));
        let result_attrs = attributes(result);
        let sources = array(sources, result_attrs.len());
        let common = array(common, common_count.read_unaligned());
        let lhs_sorted = sorted_on(common, true, lhs, lhs_count, lhs_desc.element_size);
        let rhs_sorted = sorted_on(common, false, rhs, rhs_count, rhs_desc.element_size);
        let (groups, total) = matching_runs(common, lhs_sorted, rhs_sorted);

        let size = result.element_size;
        let Some(bytes) = total.checked_mul(size) else {
            trendy_rt_fail(FAIL_OUT_OF_MEMORY)
        };
        let joined = allocate::<u8>(bytes);
        core::ptr::write_bytes(joined, 0, bytes);
        let mut k = 0;
        for group in &*groups {
            for &l in &lhs_sorted[group.lhs_start..group.lhs_end] {
                for &r in &rhs_sorted[group.rhs_start..group.rhs_end] {
                    let to = joined.add(k * size);
                    for (attr, source) in result_attrs.iter().zip(sources) {
                        let from = if source.from == 1 { r } else { l };
                        core::ptr::copy_nonoverlapping(
                            from.add(source.offset),
                            to.add(attr.offset),
                            (*attr.desc).size,
                        );
                    }
                    k += 1;
                }
            }
        }
        let canonical = canonical(joined, total, result);
        for memory in [
            joined.cast(),
            groups.as_mut_ptr().cast(),
            lhs_sorted.as_mut_ptr().cast(),
            rhs_sorted.as_mut_ptr().cast(),
        ] {
            free(memory);
        }
        dst.write_unaligned(Text {
            bytes: canonical.0,
            len: canonical.1,
        });
    }
}

/// A relation-valued attribute that [`trendy_rt_ungroup`] ungroups: where it
/// is in a tuple of the relation being ungrouped, and the size of the
/// tuples of its own relation. The compiler emits an array of these, laid
/// out like this struct.
#[repr(C)]
pub struct Ungrouped {
    pub offset: usize,
    pub element_size: usize,
}

/// Store at `dst` the relation `result` describes whose tuples are, for
/// each tuple of the relation `src`, which `source` describes, one for each
/// combination of a tuple of each of its `*ungrouped_count` relation-valued
/// attributes in `ungrouped`, in canonical order, in memory the statement
/// owns. A tuple with an empty one has none.
/// `sources` says where each attribute of `result` comes from.
///
/// # Safety
///
/// `src` must point to a valid relation of the type `source` describes,
/// `result` and `source` to valid descriptors, `sources` to one [`Source`]
/// for each attribute of `result`, `ungrouped` to `*ungrouped_count`
/// [`Ungrouped`]s, each naming a relation-valued attribute of `src`'s
/// tuples, and `dst` to memory for a relation.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_ungroup(
    dst: *mut Text,
    src: *const u8,
    result: *const TypeDesc,
    source: *const TypeDesc,
    sources: *const Source,
    ungrouped: *const Ungrouped,
    ungrouped_count: *const usize,
) {
    // SAFETY: guaranteed by the caller; each size and offset is the layout
    // of the value it describes.
    unsafe {
        let (result, source) = (&*result, &*source);
        let (tuples, count) = read_slice(src);
        let result_attrs = attributes(result);
        let sources = array(sources, result_attrs.len());
        let ungrouped = array(ungrouped, ungrouped_count.read_unaligned());
        let tuple = |i: usize| tuples.add(i * source.element_size);
        let total = (0..count)
            .map(|i| {
                ungrouped.iter().try_fold(1usize, |product, u| {
                    product.checked_mul(read_slice(tuple(i).add(u.offset)).1)
                })
            })
            .try_fold(0usize, |sum, n| sum.checked_add(n?));
        let size = result.element_size;
        let Some((total, bytes)) = total.and_then(|total| Some((total, total.checked_mul(size)?)))
        else {
            trendy_rt_fail(FAIL_OUT_OF_MEMORY)
        };
        let made = allocate::<u8>(bytes);
        core::ptr::write_bytes(made, 0, bytes);
        // For the tuple at hand, the index in each ungrouped relation of the
        // tuple being combined, counted up like an odometer.
        let at =
            core::slice::from_raw_parts_mut(allocate::<usize>(ungrouped.len()), ungrouped.len());
        let mut k = 0;
        for i in 0..count {
            let outer = tuple(i);
            if ungrouped
                .iter()
                .any(|u| read_slice(outer.add(u.offset)).1 == 0)
            {
                continue;
            }
            at.fill(0);
            loop {
                let to = made.add(k * size);
                for (attr, source) in result_attrs.iter().zip(sources) {
                    let from = match source.from {
                        0 => outer,
                        n => {
                            let u = &ungrouped[n - 1];
                            read_slice(outer.add(u.offset))
                                .0
                                .add(at[n - 1] * u.element_size)
                        }
                    };
                    core::ptr::copy_nonoverlapping(
                        from.add(source.offset),
                        to.add(attr.offset),
                        (*attr.desc).size,
                    );
                }
                k += 1;
                // The next combination, or the end of this tuple's.
                let next = ungrouped
                    .iter()
                    .enumerate()
                    .rev()
                    .find(|&(j, u)| at[j] + 1 < read_slice(outer.add(u.offset)).1);
                let Some((j, _)) = next else {
                    break;
                };
                at[j] += 1;
                at[j + 1..].fill(0);
            }
        }
        let canonical = canonical(made, total, result);
        free(made.cast());
        free(at.as_mut_ptr().cast());
        dst.write_unaligned(Text {
            bytes: canonical.0,
            len: canonical.1,
        });
    }
}

/// Store at `dst`, a byte, 1 if every tuple of the relation `lhs` is in the
/// relation `rhs`, both of the type `desc` describes, and, if `*proper` is
/// 1, `rhs` has more tuples too; else 0. Both are in canonical order, so one
/// pass compares them.
///
/// # Safety
///
/// `lhs` and `rhs` must point to valid relations of the type `desc`
/// describes, `proper` to 0 or 1, and `dst` to a byte.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_subset(
    dst: *mut u8,
    lhs: *const u8,
    rhs: *const u8,
    desc: *const TypeDesc,
    proper: *const usize,
) {
    use core::cmp::Ordering;
    // SAFETY: guaranteed by the caller.
    unsafe {
        let desc = &*desc;
        let ((lhs, lhs_count), (rhs, rhs_count)) = (read_slice(lhs), read_slice(rhs));
        let size = desc.element_size;
        let mut j = 0;
        let mut subset = true;
        for i in 0..lhs_count {
            let tuple = lhs.add(i * size);
            // Past the tuples of `rhs` before it, to it if `rhs` has it.
            while j < rhs_count && compare_tuples(rhs.add(j * size), tuple, desc) == Ordering::Less
            {
                j += 1;
            }
            if j == rhs_count || compare_tuples(rhs.add(j * size), tuple, desc) != Ordering::Equal {
                subset = false;
                break;
            }
            j += 1;
        }
        let proper = proper.read_unaligned() == 1;
        dst.write(u8::from(subset && (!proper || lhs_count < rhs_count)));
    }
}

/// [`trendy_rt_combine`]'s union.
pub const COMBINE_UNION: usize = 0;
/// [`trendy_rt_combine`]'s union of operands with no tuple in common.
pub const COMBINE_DISJOINT_UNION: usize = 1;
/// [`trendy_rt_combine`]'s difference.
pub const COMBINE_MINUS: usize = 2;
/// [`trendy_rt_combine`]'s difference whose right operand is included in
/// the left.
pub const COMBINE_INCLUDED_MINUS: usize = 3;
/// [`trendy_rt_combine`]'s symmetric difference: the tuples of exactly one
/// operand.
pub const COMBINE_EXCLUSIVE_UNION: usize = 4;
/// [`trendy_rt_combine`]'s union of the tuples changes remove, which
/// conflict if they share one.
pub const COMBINE_DISJOINT_CHANGES: usize = 5;

/// Store at `dst` the relations `lhs` and `rhs`, of the one type `desc`
/// describes, combined as `*combination`, a `COMBINE_` constant, says: the
/// tuples of either, of exactly one, or of `lhs` but not `rhs`, in
/// canonical order, in memory the statement owns. Both operands are in
/// canonical order, so one pass merges them, and the result needs no
/// sorting. Stops the program if a disjoint union's operands, or disjoint
/// changes', share a tuple, or an included minus's right operand has one
/// the left lacks.
///
/// # Safety
///
/// `lhs` and `rhs` must point to valid relations of the type `desc`
/// describes, `combination` to a `COMBINE_` constant, and `dst` to memory
/// for a relation.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_combine(
    dst: *mut Text,
    lhs: *const u8,
    rhs: *const u8,
    desc: *const TypeDesc,
    combination: *const usize,
) {
    use core::cmp::Ordering;
    // SAFETY: guaranteed by the caller.
    unsafe {
        let desc = &*desc;
        let ((lhs, lhs_count), (rhs, rhs_count)) = (read_slice(lhs), read_slice(rhs));
        let combination = combination.read_unaligned();
        let minus = matches!(combination, COMBINE_MINUS | COMBINE_INCLUDED_MINUS);
        let size = desc.element_size;
        let Some(room) = lhs_count.checked_add(rhs_count) else {
            trendy_rt_fail(FAIL_OUT_OF_MEMORY)
        };
        let Some(bytes) = room.checked_mul(size) else {
            trendy_rt_fail(FAIL_OUT_OF_MEMORY)
        };
        let out = allocate_owned(bytes);
        let mut count = 0;
        let mut keep = |tuple: *const u8| {
            core::ptr::copy_nonoverlapping(tuple, out.add(count * size), size);
            count += 1;
        };
        let (mut i, mut j) = (0, 0);
        while i < lhs_count {
            let l = lhs.add(i * size);
            let order = if j < rhs_count {
                compare_tuples(l, rhs.add(j * size), desc)
            } else {
                Ordering::Less
            };
            match order {
                Ordering::Less => {
                    keep(l);
                    i += 1;
                }
                Ordering::Equal => {
                    match combination {
                        COMBINE_DISJOINT_UNION => trendy_rt_fail(FAIL_SHARED_TUPLE),
                        COMBINE_DISJOINT_CHANGES => trendy_rt_fail(FAIL_CONFLICT),
                        _ => {}
                    }
                    // A tuple of both is in an exclusive union's result
                    // no more than in a minus's.
                    if !minus && combination != COMBINE_EXCLUSIVE_UNION {
                        keep(l);
                    }
                    i += 1;
                    j += 1;
                }
                Ordering::Greater => {
                    if combination == COMBINE_INCLUDED_MINUS {
                        trendy_rt_fail(FAIL_MISSING_TUPLE);
                    }
                    if !minus {
                        keep(rhs.add(j * size));
                    }
                    j += 1;
                }
            }
        }
        // What is left of `rhs` is in no tuple of `lhs`.
        if combination == COMBINE_INCLUDED_MINUS && j < rhs_count {
            trendy_rt_fail(FAIL_MISSING_TUPLE);
        }
        if !minus {
            for k in j..rhs_count {
                keep(rhs.add(k * size));
            }
        }
        dst.write_unaligned(Text {
            bytes: out,
            len: count,
        });
    }
}

/// Store at `dst` the tuples of the relation `lhs`, which `lhs_desc`
/// describes, that have a match in the relation `rhs`, which `rhs_desc`
/// describes, on the `*common_count` attributes in `common`, or, if
/// `*negated` is 1, those that have none (the semijoin and the antijoin), in
/// canonical order, in memory the statement owns.
///
/// # Safety
///
/// As for [`trendy_rt_and`], with `negated` pointing to 0 or 1.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_matching(
    dst: *mut Text,
    lhs: *const u8,
    rhs: *const u8,
    lhs_desc: *const TypeDesc,
    rhs_desc: *const TypeDesc,
    common: *const Common,
    common_count: *const usize,
    negated: *const usize,
) {
    // SAFETY: guaranteed by the caller; each size and offset is the layout
    // of the value it describes.
    unsafe {
        let (lhs_desc, rhs_desc) = (&*lhs_desc, &*rhs_desc);
        let ((lhs, lhs_count), (rhs, rhs_count)) = (read_slice(lhs), read_slice(rhs));
        let common = array(common, common_count.read_unaligned());
        let negated = negated.read_unaligned() == 1;
        let lhs_sorted = sorted_on(common, true, lhs, lhs_count, lhs_desc.element_size);
        let rhs_sorted = sorted_on(common, false, rhs, rhs_count, rhs_desc.element_size);
        let size = lhs_desc.element_size;
        let kept = allocate::<u8>(lhs_count * size);
        let (mut count, mut j) = (0, 0);
        // Both sorted on the common attributes, so each tuple of `lhs` is
        // matched by moving forward through `rhs`.
        for &tuple in &*lhs_sorted {
            while j < rhs_sorted.len()
                && on_common(common, (rhs_sorted[j], false), (tuple, true)).is_lt()
            {
                j += 1;
            }
            let matched = j < rhs_sorted.len()
                && on_common(common, (rhs_sorted[j], false), (tuple, true)).is_eq();
            if matched != negated {
                core::ptr::copy_nonoverlapping(tuple, kept.add(count * size), size);
                count += 1;
            }
        }
        let canonical = canonical(kept, count, lhs_desc);
        for memory in [
            kept.cast(),
            lhs_sorted.as_mut_ptr().cast(),
            rhs_sorted.as_mut_ptr().cast(),
        ] {
            free(memory);
        }
        dst.write_unaligned(Text {
            bytes: canonical.0,
            len: canonical.1,
        });
    }
}

/// The order of two tuples by their values for the `common` attributes,
/// each tuple from the left operand of [`trendy_rt_and`] if its flag says
/// so, and else from the right.
///
/// # Safety
///
/// `x` and `y` must point to valid tuples of the operands their flags say.
#[cfg(not(test))]
unsafe fn on_common(
    common: &[Common],
    (x, x_lhs): (*const u8, bool),
    (y, y_lhs): (*const u8, bool),
) -> core::cmp::Ordering {
    for c in common {
        let at = |lhs: bool| if lhs { c.lhs } else { c.rhs };
        // SAFETY: guaranteed by the caller.
        let ordering = unsafe { order(x.add(at(x_lhs)), y.add(at(y_lhs)), &*c.desc) };
        if ordering.is_ne() {
            return ordering;
        }
    }
    core::cmp::Ordering::Equal
}

/// The `count` tuples at `tuples`, each `size` bytes, of the left operand
/// of [`trendy_rt_and`] if `is_lhs`, and else the right, sorted by their
/// values for the `common` attributes: an array of pointers to them, in
/// new memory.
///
/// # Safety
///
/// `tuples` must point to `count` valid tuples of that operand.
#[cfg(not(test))]
unsafe fn sorted_on<'a>(
    common: &[Common],
    is_lhs: bool,
    tuples: *const u8,
    count: usize,
    size: usize,
) -> &'a mut [*const u8] {
    // SAFETY: guaranteed by the caller; every pointer is written before
    // the array is read.
    unsafe {
        let sorted = core::slice::from_raw_parts_mut(allocate::<*const u8>(count), count);
        for (i, tuple) in sorted.iter_mut().enumerate() {
            *tuple = tuples.add(i * size);
        }
        sorted.sort_unstable_by(|x, y| on_common(common, (*x, is_lhs), (*y, is_lhs)));
        sorted
    }
}

/// The runs of tuples of `lhs` and of `rhs`, both sorted on the `common`
/// attributes, that agree on them, in new memory, and how many tuples they
/// make: the size of each run of `lhs` times the size of its run of `rhs`.
/// Stops the program if that is more than memory can hold.
///
/// # Safety
///
/// `lhs` and `rhs` must point to valid tuples of [`trendy_rt_and`]'s left
/// and right operands.
#[cfg(not(test))]
unsafe fn matching_runs<'a>(
    common: &[Common],
    lhs: &[*const u8],
    rhs: &[*const u8],
) -> (&'a mut [Group], usize) {
    use core::cmp::Ordering;
    let groups = allocate::<Group>(lhs.len().min(rhs.len()));
    let (mut count, mut total) = (0, 0usize);
    let (mut i, mut j) = (0, 0);
    // SAFETY: guaranteed by the caller; the array has room for a run per
    // tuple of the shorter operand, and each run holds at least one.
    unsafe {
        while i < lhs.len() && j < rhs.len() {
            match on_common(common, (lhs[i], true), (rhs[j], false)) {
                Ordering::Less => i += 1,
                Ordering::Greater => j += 1,
                Ordering::Equal => {
                    let run = |tuples: &[*const u8], start: usize, is_lhs: bool| {
                        let same = |t: &*const u8| {
                            on_common(common, (tuples[start], is_lhs), (*t, is_lhs)).is_eq()
                        };
                        start + 1 + tuples[start + 1..].iter().take_while(|t| same(t)).count()
                    };
                    let (lhs_end, rhs_end) = (run(lhs, i, true), run(rhs, j, false));
                    let Some(sum) = (lhs_end - i)
                        .checked_mul(rhs_end - j)
                        .and_then(|n| n.checked_add(total))
                    else {
                        trendy_rt_fail(FAIL_OUT_OF_MEMORY)
                    };
                    total = sum;
                    groups.add(count).write(Group {
                        lhs_start: i,
                        lhs_end,
                        rhs_start: j,
                        rhs_end,
                    });
                    count += 1;
                    (i, j) = (lhs_end, rhs_end);
                }
            }
        }
        (core::slice::from_raw_parts_mut(groups, count), total)
    }
}

/// Store at `dst` a relation of the type `desc` describes with `*count`
/// tuples, all zero bytes, in memory the statement owns, for compiled code to
/// fill in: a loop that computes each tuple of its result. Zeroing leaves
/// the padding in tuples zero, as everywhere else.
///
/// # Safety
///
/// `count` must point to a `usize`, `desc` to a valid relation descriptor,
/// and `dst` to memory for a relation.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_allocate(
    dst: *mut Text,
    count: *const usize,
    desc: *const TypeDesc,
) {
    // SAFETY: guaranteed by the caller.
    unsafe {
        let count = count.read_unaligned();
        let Some(bytes) = count.checked_mul((*desc).element_size) else {
            trendy_rt_fail(FAIL_OUT_OF_MEMORY)
        };
        let tuples = allocate_owned(bytes);
        core::ptr::write_bytes(tuples, 0, bytes);
        dst.write_unaligned(Text {
            bytes: tuples,
            len: count,
        });
    }
}

/// Put the tuples of the relation at `relation`, of the type `desc`
/// describes, in the canonical order, without duplicates: it then holds
/// them, in memory the statement owns.
///
/// # Safety
///
/// `relation` must point to a valid relation of the type `desc` describes,
/// whose tuples may be in any order.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_sort(relation: *mut Text, desc: *const TypeDesc) {
    // SAFETY: guaranteed by the caller.
    unsafe {
        let (tuples, count) = read_slice(relation.cast());
        let (bytes, len) = canonical(tuples, count, &*desc);
        relation.write_unaligned(Text { bytes, len });
    }
}

/// The `count` tuples at `tuples`, with the attributes of `desc`, sorted
/// canonically and without duplicates, copied into memory the current
/// statement owns: that memory and how many tuples it holds.
///
/// # Safety
///
/// As for [`trendy_rt_reshape`]'s result.
#[cfg(not(test))]
unsafe fn canonical(tuples: *const u8, count: usize, desc: &TypeDesc) -> (*mut u8, usize) {
    let size = desc.element_size;
    // SAFETY: guaranteed by the caller.
    unsafe {
        let order = core::slice::from_raw_parts_mut(allocate::<*const u8>(count), count);
        for (i, tuple) in order.iter_mut().enumerate() {
            *tuple = tuples.add(i * size);
        }
        order.sort_unstable_by(|a, b| compare_tuples(*a, *b, desc));
        let out = allocate_owned(count * size);
        let mut kept = 0;
        for i in 0..count {
            if i > 0 && compare_tuples(order[i - 1], order[i], desc).is_eq() {
                continue;
            }
            core::ptr::copy_nonoverlapping(order[i], out.add(kept * size), size);
            kept += 1;
        }
        free(order.as_mut_ptr().cast());
        (out, kept)
    }
}

/// The canonical order of two values of the type `desc` describes, which is
/// the compiler's (`hir::Value`'s): `false` before `true`, integers
/// numerically, approximate numbers by IEEE 754 totalOrder, text by its
/// bytes, tuples attribute by attribute in name order, and relations by their
/// tuples, lexicographically.
///
/// # Safety
///
/// `lhs` and `rhs` must point to valid values of the type `desc` describes.
#[must_use]
pub unsafe fn order(lhs: *const u8, rhs: *const u8, desc: &TypeDesc) -> core::cmp::Ordering {
    // SAFETY: guaranteed by the caller.
    unsafe {
        match desc.kind {
            KIND_BOOLEAN => (*lhs).cmp(&*rhs),
            KIND_INTEGER => lhs
                .cast::<i64>()
                .read_unaligned()
                .cmp(&rhs.cast::<i64>().read_unaligned()),
            KIND_APPROXIMATE => lhs
                .cast::<f64>()
                .read_unaligned()
                .total_cmp(&rhs.cast::<f64>().read_unaligned()),
            KIND_TEXT => {
                let (a, b) = (read_slice(lhs), read_slice(rhs));
                core::slice::from_raw_parts(a.0, a.1).cmp(core::slice::from_raw_parts(b.0, b.1))
            }
            KIND_TUPLE => compare_tuples(lhs, rhs, desc),
            KIND_RELATION => {
                let ((a, count), (b, other)) = (read_slice(lhs), read_slice(rhs));
                for i in 0..count.min(other) {
                    let offset = i * desc.element_size;
                    let ordering = compare_tuples(a.add(offset), b.add(offset), desc);
                    if ordering.is_ne() {
                        return ordering;
                    }
                }
                count.cmp(&other)
            }
            _ => core::cmp::Ordering::Equal,
        }
    }
}

/// The canonical order of two tuples with the attributes of `desc`.
///
/// # Safety
///
/// As for [`order`], with `desc` describing a tuple or relation.
unsafe fn compare_tuples(lhs: *const u8, rhs: *const u8, desc: &TypeDesc) -> core::cmp::Ordering {
    // SAFETY: guaranteed by the caller.
    unsafe {
        for attr in attributes(desc) {
            let ordering = order(lhs.add(attr.offset), rhs.add(attr.offset), &*attr.desc);
            if ordering.is_ne() {
                return ordering;
            }
        }
        core::cmp::Ordering::Equal
    }
}

/// Stop the program because of `failure`, one of the `FAIL_` constants:
/// write its message to stderr and exit with status 1.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn trendy_rt_fail(failure: u32) -> ! {
    let mut out = Stderr::default();
    // There is nowhere to report a failure to write to stderr.
    let _ = out.write_str(failure_message(failure));
    out.flush();
    // SAFETY: `exit` has no preconditions.
    unsafe { exit(1) }
}

/// A description of a type, emitted by the compiler.
#[repr(C)]
pub struct TypeDesc {
    /// One of the `KIND_` constants.
    pub kind: u8,
    /// The size of a value of this type, in bytes.
    pub size: usize,
    /// For a tuple or relation, its heading's attributes, sorted by name.
    /// For other kinds, `count` is 0 and this is not read.
    pub attributes: *const Attr,
    pub count: usize,
    /// For a relation, the size of each tuple in its array, in bytes.
    pub element_size: usize,
}

/// An attribute of a heading.
#[repr(C)]
pub struct Attr {
    /// The UTF-8 bytes of the attribute's name, which is empty for one the
    /// source can't name: one that a relation passed for an operator's
    /// parameter that `per` parameters refer to wraps its tuples' own
    /// attributes in, written `…`.
    pub name: *const u8,
    pub name_len: usize,
    /// Where the attribute is within a tuple, in bytes.
    pub offset: usize,
    pub desc: *const TypeDesc,
}

/// Print `value`, of the type `desc` describes, to stderr as one line of
/// trendy source.
///
/// # Safety
///
/// `value` must point to a valid value of the type `desc` describes, and
/// `desc` to a valid descriptor, as emitted by the compiler.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_debug_value(value: *const u8, desc: *const TypeDesc) {
    let mut out = Stderr::default();
    // SAFETY: guaranteed by the caller.
    let printed = unsafe { write_value(&mut out, value, &*desc) };
    // There is nowhere to report a failure to write to stderr.
    let _ = printed.and_then(|()| out.write_char('\n'));
    out.flush();
}

/// Store at `dst` the text [`trendy_rt_debug_value`] writes for `value`,
/// of the type `desc` describes, in memory the statement owns. Stops the
/// program if the memory can't be allocated.
///
/// # Safety
///
/// As for [`trendy_rt_debug_value`], and `dst` must point to memory for a
/// text value. The statement must have begun a scope.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_to_text(
    dst: *mut Text,
    value: *const u8,
    desc: *const TypeDesc,
) {
    // SAFETY: guaranteed by the caller.
    unsafe {
        // Too long to count is too long to allocate.
        let Ok(text) = text_of(value, &*desc, allocate_owned) else {
            trendy_rt_fail(FAIL_OUT_OF_MEMORY)
        };
        dst.write_unaligned(text);
    }
}

/// `value` as [`write_value`] writes it, in memory from `allocate`, which
/// takes its length: written once to count its bytes, then again into that
/// memory.
///
/// # Safety
///
/// As for [`write_value`], and `allocate` must return memory for as many
/// bytes as it is given.
///
/// # Errors
///
/// Returns an error if the length doesn't fit in a `usize`.
unsafe fn text_of(
    value: *const u8,
    desc: &TypeDesc,
    allocate: impl FnOnce(usize) -> *mut u8,
) -> Result<Text, fmt::Error> {
    let mut counted = Counted(0);
    // SAFETY, throughout: guaranteed by the caller.
    unsafe {
        write_value(&mut counted, value, desc)?;
        let len = counted.0;
        let bytes = allocate(len);
        let mut filled = Filled {
            bytes: core::slice::from_raw_parts_mut(bytes, len),
            len: 0,
        };
        write_value(&mut filled, value, desc)?;
        Ok(Text { bytes, len })
    }
}

/// Counts the bytes written to it.
struct Counted(usize);

impl Write for Counted {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.0 = self.0.checked_add(s.len()).ok_or(fmt::Error)?;
        Ok(())
    }
}

/// Writes into `bytes`, the first `len` of which it has filled.
struct Filled<'a> {
    bytes: &'a mut [u8],
    len: usize,
}

impl Write for Filled<'_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let end = self.len.checked_add(s.len()).ok_or(fmt::Error)?;
        let rest = self.bytes.get_mut(self.len..end).ok_or(fmt::Error)?;
        rest.copy_from_slice(s.as_bytes());
        self.len = end;
        Ok(())
    }
}

/// Write `value` as trendy source: tuples and relations with their
/// attributes in name order, and relations with their heading first, so the
/// output is a literal for the same value. An attribute without a name,
/// which the source can't name, is written `…`, last, without its type or
/// value, as the compiler writes its type.
///
/// # Safety
///
/// As for [`trendy_rt_debug_value`].
///
/// # Errors
///
/// Returns an error if `out` does.
pub unsafe fn write_value(out: &mut impl Write, value: *const u8, desc: &TypeDesc) -> fmt::Result {
    // SAFETY, throughout: the caller guarantees `value` holds a value of the
    // described type, and the descriptor's offsets and sizes are the
    // compiler's layout of that type.
    unsafe {
        match desc.kind {
            KIND_BOOLEAN => out.write_str(if *value == 0 { "false" } else { "true" }),
            KIND_INTEGER => write!(out, "{}", value.cast::<i64>().read_unaligned()),
            // The shortest exponent form that reads back as the same value,
            // as `hir::Approximate::literal` writes it.
            KIND_APPROXIMATE => write!(out, "{:e}", value.cast::<f64>().read_unaligned()),
            KIND_TEXT => {
                let text = read_text(value);
                out.write_char('"')?;
                trendy_text::escape(text, out)?;
                out.write_char('"')
            }
            KIND_TUPLE => {
                out.write_str("tuple ")?;
                write_tuple(out, value, desc)
            }
            KIND_RELATION => {
                out.write_str("relation ")?;
                write_heading(out, desc)?;
                out.write_char(' ')?;
                let (tuples, count) = read_slice(value);
                if count == 0 {
                    return out.write_str("{}");
                }
                out.write_str("{ ")?;
                for i in 0..count {
                    if i > 0 {
                        out.write_str(", ")?;
                    }
                    write_tuple(out, tuples.add(i * desc.element_size), desc)?;
                }
                out.write_str(" }")
            }
            _ => Err(fmt::Error),
        }
    }
}

/// Write the type `desc` describes, as trendy source.
///
/// # Safety
///
/// `desc` must be a valid descriptor.
unsafe fn write_type(out: &mut impl Write, desc: &TypeDesc) -> fmt::Result {
    match desc.kind {
        KIND_BOOLEAN => out.write_str("boolean"),
        KIND_INTEGER => out.write_str("integer"),
        KIND_APPROXIMATE => out.write_str("approximate"),
        KIND_TEXT => out.write_str("text"),
        KIND_TUPLE => {
            out.write_str("tuple ")?;
            // SAFETY: guaranteed by the caller.
            unsafe { write_heading(out, desc) }
        }
        KIND_RELATION => {
            out.write_str("relation ")?;
            // SAFETY: guaranteed by the caller.
            unsafe { write_heading(out, desc) }
        }
        _ => Err(fmt::Error),
    }
}

/// `{ name: type, … }` or `{}`.
///
/// # Safety
///
/// `desc` must be a valid tuple or relation descriptor.
unsafe fn write_heading(out: &mut impl Write, desc: &TypeDesc) -> fmt::Result {
    // SAFETY: guaranteed by the caller; a valid descriptor's attributes
    // are valid.
    unsafe { write_attributes(out, desc, |out, attr| write_type(out, &*attr.desc)) }
}

/// `{ name: value, … }` or `{}`, for the tuple at `value`.
///
/// # Safety
///
/// As for [`write_value`], with `desc` describing the tuple's heading.
unsafe fn write_tuple(out: &mut impl Write, value: *const u8, desc: &TypeDesc) -> fmt::Result {
    // SAFETY: guaranteed by the caller; the attribute's offset and
    // descriptor are its layout.
    unsafe {
        write_attributes(out, desc, |out, attr| {
            write_value(out, value.add(attr.offset), &*attr.desc)
        })
    }
}

/// `{ name: …, … }` or `{}`, for the attributes of `desc`, each written
/// after its name by `each`, and those without a name as one `…`, last.
///
/// # Safety
///
/// `desc` must be a valid tuple or relation descriptor, and `each` safe
/// to call with each of its attributes.
unsafe fn write_attributes<W: Write>(
    out: &mut W,
    desc: &TypeDesc,
    each: impl Fn(&mut W, &Attr) -> fmt::Result,
) -> fmt::Result {
    // SAFETY: guaranteed by the caller.
    let attributes = unsafe { attributes(desc) };
    if attributes.is_empty() {
        return out.write_str("{}");
    }
    out.write_str("{ ")?;
    let mut first = true;
    let mut unnamed = false;
    for attr in attributes {
        // SAFETY: a valid descriptor's attributes are valid.
        let name = unsafe { name(attr) };
        if name.is_empty() {
            unnamed = true;
            continue;
        }
        if !first {
            out.write_str(", ")?;
        }
        first = false;
        out.write_str(name)?;
        out.write_str(": ")?;
        each(out, attr)?;
    }
    if unnamed {
        out.write_str(if first { "…" } else { ", …" })?;
    }
    out.write_str(" }")
}

/// # Safety
///
/// `desc` must be a valid descriptor.
unsafe fn attributes(desc: &TypeDesc) -> &[Attr] {
    if desc.count == 0 {
        return &[];
    }
    // SAFETY: a valid descriptor's attributes point to `count` entries.
    unsafe { core::slice::from_raw_parts(desc.attributes, desc.count) }
}

/// The `len` values at `values`, which can be dangling, or misaligned
/// for `T`, when there are none.
///
/// # Safety
///
/// If `len` isn't 0, `values` must point to `len` valid values.
#[cfg(not(test))]
unsafe fn array<'a, T>(values: *const T, len: usize) -> &'a [T] {
    if len == 0 {
        return &[];
    }
    // SAFETY: guaranteed by the caller.
    unsafe { core::slice::from_raw_parts(values, len) }
}

/// # Safety
///
/// `attr` must be a valid attribute.
unsafe fn name(attr: &Attr) -> &str {
    // SAFETY: the compiler emits names as valid UTF-8.
    unsafe { core::str::from_utf8_unchecked(core::slice::from_raw_parts(attr.name, attr.name_len)) }
}

/// A `text` value: a pointer to UTF-8 bytes, then their length.
///
/// # Safety
///
/// `value` must point to a valid `text` value.
unsafe fn read_text<'a>(value: *const u8) -> &'a str {
    // SAFETY: a text value's bytes are valid UTF-8 for its length.
    unsafe {
        let (bytes, len) = read_slice(value);
        core::str::from_utf8_unchecked(core::slice::from_raw_parts(bytes, len))
    }
}

/// A pointer followed by a length, as in `text` and relation values.
///
/// # Safety
///
/// `value` must point to such a pair.
unsafe fn read_slice(value: *const u8) -> (*const u8, usize) {
    // SAFETY: guaranteed by the caller. Reads are unaligned, so they make no
    // assumption about where the value is stored.
    unsafe {
        let pointer = value.cast::<*const u8>().read_unaligned();
        let len = value
            .add(size_of::<*const u8>())
            .cast::<usize>()
            .read_unaligned();
        (pointer, len)
    }
}

/// Buffered output to file descriptor 2.
#[derive(Default)]
struct Stderr {
    buffer: Buffer,
}

struct Buffer {
    bytes: [u8; 512],
    len: usize,
}

impl Default for Buffer {
    fn default() -> Self {
        Buffer {
            bytes: [0; 512],
            len: 0,
        }
    }
}

impl Stderr {
    fn flush(&mut self) {
        let mut written = 0;
        while written < self.buffer.len {
            let rest = &self.buffer.bytes[written..self.buffer.len];
            // SAFETY: `rest` is a valid buffer of its length.
            let n = unsafe { write(2, rest.as_ptr(), rest.len()) };
            let Ok(n) = usize::try_from(n) else {
                break;
            };
            written += n;
        }
        self.buffer.len = 0;
    }
}

impl Write for Stderr {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for &byte in s.as_bytes() {
            if self.buffer.len == self.buffer.bytes.len() {
                self.flush();
            }
            self.buffer.bytes[self.buffer.len] = byte;
            self.buffer.len += 1;
        }
        Ok(())
    }
}

unsafe extern "C" {
    fn write(fd: i32, buf: *const u8, count: usize) -> isize;
    #[cfg(not(test))]
    fn abort() -> !;
    #[cfg(not(test))]
    fn exit(status: i32) -> !;
    #[cfg(not(test))]
    fn malloc(size: usize) -> *mut core::ffi::c_void;
    #[cfg(not(test))]
    fn free(pointer: *mut core::ffi::c_void);
}

#[cfg(not(test))]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    // SAFETY: `abort` has no preconditions.
    unsafe { abort() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scalar(kind: u8, size: usize) -> TypeDesc {
        TypeDesc {
            kind,
            size,
            attributes: core::ptr::null(),
            count: 0,
            element_size: 0,
        }
    }

    /// The value as `to_text` makes it, in memory of exactly its length,
    /// which is what `debug_value` writes.
    fn print(value: *const u8, desc: &TypeDesc) -> String {
        let mut written = String::new();
        let mut memory = Vec::new();
        // SAFETY: each test builds a value matching its descriptor, and the
        // vector has room for the bytes asked for.
        let text = unsafe {
            write_value(&mut written, value, desc).unwrap();
            text_of(value, desc, |len| {
                memory = vec![0; len];
                memory.as_mut_ptr()
            })
        }
        .unwrap();
        assert_eq!(text.len, memory.len());
        assert_eq!(memory, written.as_bytes());
        written
    }

    fn attr(name: &'static str, offset: usize, desc: &TypeDesc) -> Attr {
        Attr {
            name: name.as_ptr(),
            name_len: name.len(),
            offset,
            desc,
        }
    }

    #[test]
    fn persisted_values_point_only_into_their_arena() {
        // `relation { name: text } { { name: "Ada" }, { name: "李白" } }`.
        let text = scalar(KIND_TEXT, 16);
        let attrs = [attr("name", 0, &text)];
        let relation = TypeDesc {
            kind: KIND_RELATION,
            size: 16,
            attributes: attrs.as_ptr(),
            count: 1,
            element_size: 16,
        };
        let (ada, li) = ("Ada", "李白");
        let tuples = [
            [ada.as_ptr() as usize, ada.len()],
            [li.as_ptr() as usize, li.len()],
        ];
        let src = [tuples.as_ptr() as usize, 2];
        let mut measured = Arena::measuring();
        // SAFETY: the value matches its descriptor, and nothing is written
        // while measuring.
        unsafe {
            persist(
                core::ptr::null_mut(),
                src.as_ptr().cast(),
                &relation,
                &mut measured,
            );
        };
        assert_eq!(measured.used, 2 * 16 + ada.len() + li.len());

        let mut memory = vec![0_u128; measured.used.div_ceil(16)];
        let base: *mut u8 = memory.as_mut_ptr().cast();
        let mut arena = Arena { base, used: 0 };
        let mut dst = src;
        // SAFETY: `dst` is a copy of the value, and the arena has the room
        // measured.
        unsafe {
            persist(
                dst.as_mut_ptr().cast(),
                src.as_ptr().cast(),
                &relation,
                &mut arena,
            );
        };
        assert_eq!(arena.used, measured.used);
        assert_eq!(
            print(dst.as_ptr().cast(), &relation),
            print(src.as_ptr().cast(), &relation)
        );
        let inside =
            |address: usize| (base as usize..base as usize + arena.used).contains(&address);
        assert!(inside(dst[0]), "the tuples are copied");
        // SAFETY: the copied tuples are texts, in the arena.
        let copied = unsafe { core::slice::from_raw_parts(dst[0] as *const [usize; 2], 2) };
        assert!(copied.iter().all(|t| inside(t[0])), "so are their texts");
    }

    #[test]
    fn scalars() {
        assert_eq!(print([1_u8].as_ptr(), &scalar(KIND_BOOLEAN, 1)), "true");
        assert_eq!(
            print((-42_i64).to_ne_bytes().as_ptr(), &scalar(KIND_INTEGER, 8)),
            "-42"
        );
        let approximate = |r: f64| print(r.to_ne_bytes().as_ptr(), &scalar(KIND_APPROXIMATE, 8));
        assert_eq!(approximate(0.0), "0e0");
        assert_eq!(approximate(1.5e-3), "1.5e-3");
        assert_eq!(approximate(-0.0), "-0e0");
        assert_eq!(approximate(0.1 + 0.2), "3.0000000000000004e-1");
        assert_eq!(approximate(1e300), "1e300");
    }

    #[test]
    fn equality() {
        let approximate = scalar(KIND_APPROXIMATE, 8);
        let same = |a: f64, b: f64| unsafe {
            equal(
                a.to_ne_bytes().as_ptr(),
                b.to_ne_bytes().as_ptr(),
                &approximate,
            )
        };
        assert!(same(1.5, 1.5));
        assert!(!same(0.0, -0.0), "different values, by totalOrder");
        let text = scalar(KIND_TEXT, 16);
        let value = |t: &str| [t.as_ptr() as usize, t.len()];
        let same_text = |a: &str, b: &str| unsafe {
            equal(value(a).as_ptr().cast(), value(b).as_ptr().cast(), &text)
        };
        assert!(same_text("é", &String::from("é")));
        assert!(!same_text("ab", "abc"));
        assert!(same_text("", ""));
        // tuple { a: boolean, b: integer }: `a` at 0, `b` at 8.
        let boolean = scalar(KIND_BOOLEAN, 1);
        let integer = scalar(KIND_INTEGER, 8);
        let heading = [attr("a", 0, &boolean), attr("b", 8, &integer)];
        let tuple = TypeDesc {
            kind: KIND_TUPLE,
            size: 16,
            attributes: heading.as_ptr(),
            count: 2,
            element_size: 0,
        };
        let value = |a: u8, b: i64| {
            let mut bytes = [0_u8; 16];
            bytes[0] = a;
            bytes[8..].copy_from_slice(&b.to_ne_bytes());
            bytes
        };
        let same_tuple =
            |x: [u8; 16], y: [u8; 16]| unsafe { equal(x.as_ptr(), y.as_ptr(), &tuple) };
        assert!(same_tuple(value(1, 7), value(1, 7)));
        assert!(!same_tuple(value(1, 7), value(0, 7)));
        assert!(!same_tuple(value(1, 7), value(1, 8)));
    }

    #[test]
    fn text_is_escaped() {
        let text = "say \"hi\"\n";
        let value: [usize; 2] = [text.as_ptr() as usize, text.len()];
        assert_eq!(
            print(value.as_ptr().cast(), &scalar(KIND_TEXT, 16)),
            r#""say \"hi\"\n""#
        );
    }

    #[test]
    fn tuples_and_relations() {
        let boolean = scalar(KIND_BOOLEAN, 1);
        let integer = scalar(KIND_INTEGER, 8);
        // tuple { a: boolean, b: integer }: `a` at 0, `b` at 8, size 16.
        let heading = [attr("a", 0, &boolean), attr("b", 8, &integer)];
        let tuple = TypeDesc {
            kind: KIND_TUPLE,
            size: 16,
            attributes: heading.as_ptr(),
            count: 2,
            element_size: 0,
        };
        let one: [i64; 2] = [0, 25];
        assert_eq!(
            print(one.as_ptr().cast(), &tuple),
            "tuple { a: false, b: 25 }"
        );

        let relation = TypeDesc {
            kind: KIND_RELATION,
            size: 16,
            attributes: heading.as_ptr(),
            count: 2,
            element_size: 16,
        };
        let tuples: [i64; 4] = [0, 25, 1, 3];
        let value: [usize; 2] = [tuples.as_ptr() as usize, 2];
        assert_eq!(
            print(value.as_ptr().cast(), &relation),
            "relation { a: boolean, b: integer } { { a: false, b: 25 }, { a: true, b: 3 } }"
        );
        let empty: [usize; 2] = [tuples.as_ptr() as usize, 0];
        assert_eq!(
            print(empty.as_ptr().cast(), &relation),
            "relation { a: boolean, b: integer } {}"
        );
    }

    #[test]
    fn unnamed_attributes_are_written_last_as_an_ellipsis() {
        let integer = scalar(KIND_INTEGER, 8);
        // tuple { "": integer, b: integer }, as a `per` relation's tuples
        // wrap their own attributes in one the source can't name.
        let heading = [attr("", 0, &integer), attr("b", 8, &integer)];
        let relation = TypeDesc {
            kind: KIND_RELATION,
            size: 16,
            attributes: heading.as_ptr(),
            count: 2,
            element_size: 16,
        };
        let tuples: [i64; 4] = [1, 7, 2, 7];
        let value: [usize; 2] = [tuples.as_ptr() as usize, 2];
        assert_eq!(
            print(value.as_ptr().cast(), &relation),
            "relation { b: integer, … } { { b: 7, … }, { b: 7, … } }"
        );
        let alone = TypeDesc {
            kind: KIND_TUPLE,
            size: 8,
            attributes: heading.as_ptr(),
            count: 1,
            element_size: 0,
        };
        assert_eq!(print(tuples.as_ptr().cast(), &alone), "tuple { … }");
    }

    #[test]
    fn nested_relation_types_are_written_in_headings() {
        let integer = scalar(KIND_INTEGER, 8);
        let inner_heading = [attr("x", 0, &integer)];
        let inner = TypeDesc {
            kind: KIND_RELATION,
            size: 16,
            attributes: inner_heading.as_ptr(),
            count: 1,
            element_size: 8,
        };
        let outer_heading = [attr("r", 0, &inner)];
        let outer = TypeDesc {
            kind: KIND_RELATION,
            size: 16,
            attributes: outer_heading.as_ptr(),
            count: 1,
            element_size: 16,
        };
        let xs: [i64; 1] = [7];
        let inner_value: [usize; 2] = [xs.as_ptr() as usize, 1];
        let outer_value: [usize; 2] = [inner_value.as_ptr() as usize, 1];
        assert_eq!(
            print(outer_value.as_ptr().cast(), &outer),
            "relation { r: relation { x: integer } } { { r: relation { x: integer } { { x: 7 } } } }"
        );
    }
}
