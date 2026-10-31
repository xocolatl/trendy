//! The runtime library linked into every trendy executable.
//!
//! It is `no_std` and links only the platform's C library, so it stays
//! small. Of the crates outside the workspace, it uses only
//! `unicode-width`, for the columns a terminal gives a table's cell. In
//! the workspace it builds as an `rlib`, so that its tests run with `std`;
//! `trendyc`'s build script builds it again as a `staticlib`, with
//! `panic = "abort"`, and embeds that archive for linking.
//!
//! # Type descriptors
//!
//! Operators with an `any` parameter receive the value's address and a
//! [`TypeDesc`] for its type. The compiler emits descriptors as read-only
//! data, laid out exactly like these `#[repr(C)]` structs. Every size and
//! offset in a descriptor comes from the compiler, so the runtime never
//! repeats its layout rules.
//!
//! # Memory
//!
//! A text, a relation or an array points to a buffer, which values share,
//! after a [`Header`] that counts its references: those of the variables
//! whose values point to it, of the buffers whose tuples or elements do,
//! and of the [`Scope`] of the statement that made it, which gives its own
//! up when the statement ends. A buffer is freed when its last reference
//! is given up, so a value lasts as long as something holds it, and no
//! longer, however many a loop makes.

#![cfg_attr(not(test), no_std)]

use core::fmt::{self, Write};

mod csv;

use unicode_width::UnicodeWidthStr;

pub const KIND_BOOLEAN: u8 = 0;
pub const KIND_INTEGER: u8 = 1;
pub const KIND_APPROXIMATE: u8 = 2;
pub const KIND_TEXT: u8 = 3;
pub const KIND_TUPLE: u8 = 4;
pub const KIND_RELATION: u8 = 5;
pub const KIND_ARRAY: u8 = 6;
/// A type of the program's own, laid out as the tuple of its components.
pub const KIND_DEFINED: u8 = 7;
/// A type in a hierarchy, a union type or a subtype of one: a value is the
/// address of the `KIND_DEFINED` descriptor of its own type, then its
/// components, at the offset of the descriptor's one attribute, which has
/// no name or type.
pub const KIND_UNION: u8 = 8;

/// [`trendy_rt_integer_failed`]'s and [`trendy_rt_approximate_failed`]'s
/// operators: `+`.
pub const ARITHMETIC_ADD: u32 = 0;
/// `-`, of two operands.
pub const ARITHMETIC_SUBTRACT: u32 = 1;
/// `*`.
pub const ARITHMETIC_MULTIPLY: u32 = 2;
/// `/`, of approximate numbers.
pub const ARITHMETIC_DIVIDE: u32 = 3;
/// `div`, of integers.
pub const ARITHMETIC_DIV: u32 = 4;
/// `mod`, of integers.
pub const ARITHMETIC_MOD: u32 = 5;
/// `-`, of one operand.
pub const ARITHMETIC_NEGATE: u32 = 6;

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
            fail(Failure::OutOfMemory)
        };
        let bytes = allocate_buffer(len, core::ptr::null());
        core::ptr::copy_nonoverlapping(lhs.bytes, bytes, lhs.len);
        core::ptr::copy_nonoverlapping(rhs.bytes, bytes.add(lhs.len), rhs.len);
        dst.write_unaligned(Text { bytes, len });
    }
}

/// Store at `dst` 1 if `lhs` and `rhs`, of the type `desc` describes, are
/// equal, and 0 if not. Two values are equal exactly when they are the same
/// value: approximate numbers when they have the same bits (there is no NaN,
/// and one zero), tuples attribute by attribute, relations when they have
/// the same tuples, and arrays element by element. The compiler keeps every
/// relation's tuples in one canonical order, so equal relations have equal
/// arrays of tuples.
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
            KIND_TUPLE | KIND_DEFINED => tuples_equal(lhs, rhs, desc),
            // Of the same type, with equal components.
            KIND_UNION => {
                let ((a, at), (b, bt)) = (own_type(lhs, desc), own_type(rhs, desc));
                same_type(a, b) && equal(at, bt, a)
            }
            KIND_RELATION | KIND_ARRAY => {
                let ((a, count), (b, other)) = (read_slice(lhs), read_slice(rhs));
                let element = desc.element();
                count == other
                    && (0..count).all(|i| {
                        let offset = i * element.size;
                        equal(a.add(offset), b.add(offset), element)
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

/// The header just before every buffer that a value points to: each text's
/// bytes, each relation's tuples and each array's elements, which values
/// share. A value's pointer is just past it, so reading a value never
/// sees it. A buffer compiled into the program has [`IMMORTAL`] references,
/// and is never released.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Header {
    /// The references to the buffer: from variables, from other buffers'
    /// elements, and from statements' scopes, each of which releases its
    /// own when it ends.
    pub count: u64,
    /// How many of its elements, from the first, the buffer holds references
    /// through, to what they point to, which it releases when it is
    /// released: compiled code fills a buffer the runtime allocates, then
    /// has the runtime adopt its elements ([`trendy_rt_adopt`]).
    pub len: usize,
    /// The type of the relation or array whose tuples or elements the buffer
    /// holds, or null for a text's bytes, which point to nothing.
    pub desc: *const TypeDesc,
}

/// [`Header::count`] of a buffer compiled into the program.
pub const IMMORTAL: u64 = u64::MAX;

/// The size of a [`Header`]: a multiple of 8, so that the buffer after it
/// is aligned for any value.
#[cfg(not(test))]
const HEADER: usize = size_of::<Header>();

/// The scope of a statement, from [`trendy_rt_begin`] to [`trendy_rt_end`]:
/// a reference to each buffer that runtime functions make while it runs,
/// which no value outlives unless a variable takes a reference too, and to
/// each variable's old value that it replaces, which it may still point
/// into. It releases them all when it ends, so a value never needs
/// releasing on its own, however many times a loop made one.
///
/// Scopes nest as calls do: a statement that calls an update operator holds
/// its arguments while the operator's statements run. Compiled code keeps
/// each scope in a stack slot laid out as this struct, which only the
/// runtime reads.
#[cfg(not(test))]
#[repr(C)]
pub struct Scope {
    /// The buffers it holds a reference to, `len` of them, in memory for
    /// `capacity`, or null before the first.
    buffers: *mut *mut u8,
    len: usize,
    capacity: usize,
    /// The scope of the statement this one's operator was called from, or
    /// null.
    outer: *mut Scope,
}

/// The scope of the statement running now.
#[cfg(not(test))]
static CURRENT: core::sync::atomic::AtomicPtr<Scope> =
    core::sync::atomic::AtomicPtr::new(core::ptr::null_mut());

/// Begin the scope at `scope` of a statement, which holds a reference to
/// what runtime functions make until [`trendy_rt_end`] ends it.
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
            buffers: core::ptr::null_mut(),
            len: 0,
            capacity: 0,
            outer: CURRENT.load(Relaxed),
        });
    }
    CURRENT.store(scope, Relaxed);
}

/// End the scope at `scope`, releasing each buffer it holds a reference
/// to: the end of every text, relation and array the statement made that
/// no variable holds.
///
/// # Safety
///
/// `scope` must point to the innermost scope, which [`trendy_rt_begin`]
/// began.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_end(scope: *mut Scope) {
    use core::sync::atomic::Ordering::Relaxed;
    // SAFETY: guaranteed by the caller. Each buffer in the list is one the
    // scope holds a reference to, which it gives up once.
    unsafe {
        let scope = &*scope;
        for i in 0..scope.len {
            release_buffer(*scope.buffers.add(i));
        }
        if !scope.buffers.is_null() {
            free(scope.buffers.cast());
        }
        CURRENT.store(scope.outer, Relaxed);
    }
}

/// Have the current statement's scope hold a reference to `buffer`, which
/// it gives up when the statement ends. An immortal buffer needs none.
///
/// # Safety
///
/// `buffer` must be a buffer, just past its header, whose reference the
/// scope takes over. A statement that makes or replaces values that point
/// to buffers has begun a scope.
#[cfg(not(test))]
unsafe fn hold(buffer: *mut u8) {
    use core::sync::atomic::Ordering::Relaxed;
    // SAFETY: guaranteed by the caller.
    unsafe {
        if header(buffer).count == IMMORTAL {
            return;
        }
        let scope = &mut *CURRENT.load(Relaxed);
        if scope.len == scope.capacity {
            let capacity = (scope.capacity * 2).max(8);
            let Some(bytes) = capacity.checked_mul(size_of::<*mut u8>()) else {
                fail(Failure::OutOfMemory)
            };
            let grown = realloc(scope.buffers.cast(), bytes);
            if grown.is_null() {
                fail(Failure::OutOfMemory);
            }
            scope.buffers = grown.cast();
            scope.capacity = capacity;
        }
        *scope.buffers.add(scope.len) = buffer;
        scope.len += 1;
    }
}

/// The header of `buffer`.
///
/// # Safety
///
/// `buffer` must be a buffer, just past its header.
#[cfg(not(test))]
unsafe fn header(buffer: *const u8) -> Header {
    // SAFETY: guaranteed by the caller.
    unsafe { buffer.sub(HEADER).cast::<Header>().read_unaligned() }
}

/// Replace the header of `buffer` with `header`.
///
/// # Safety
///
/// `buffer` must be a buffer, just past its header, or memory for one.
#[cfg(not(test))]
unsafe fn set_header(buffer: *mut u8, header: Header) {
    // SAFETY: guaranteed by the caller.
    unsafe { buffer.sub(HEADER).cast::<Header>().write_unaligned(header) }
}

/// A new buffer for `bytes` bytes, of the relation or array `desc`
/// describes, or of a text's bytes if it is null, which the current
/// statement's scope holds the only reference to, holding references
/// through none of its elements yet. Stops the program if there is no
/// memory for it.
#[cfg(not(test))]
fn allocate_buffer(bytes: usize, desc: *const TypeDesc) -> *mut u8 {
    let buffer = new_buffer(bytes, desc);
    // SAFETY: the buffer is new, and its one reference the scope's.
    unsafe { hold(buffer) };
    buffer
}

/// A new buffer, as for [`allocate_buffer`], whose one reference is the
/// caller's to give to what holds it.
#[cfg(not(test))]
fn new_buffer(bytes: usize, desc: *const TypeDesc) -> *mut u8 {
    let Some(total) = bytes.checked_add(HEADER) else {
        fail(Failure::OutOfMemory)
    };
    // SAFETY: the block has room for the header and the bytes.
    unsafe {
        let buffer = allocate::<u8>(total).add(HEADER);
        set_header(
            buffer,
            Header {
                count: 1,
                len: 0,
                desc,
            },
        );
        buffer
    }
}

/// Call `f` with each buffer that the value at `value`, of the type `desc`
/// describes, points to directly: a text's, a relation's or an array's own,
/// and those of a tuple's attributes. A null pointer, a local variable's
/// before its first value and after its block, points to none.
///
/// # Safety
///
/// `value` must point to a valid value of the type `desc` describes, or one
/// with null pointers.
#[cfg(not(test))]
unsafe fn each_buffer(value: *const u8, desc: &TypeDesc, f: &mut impl FnMut(*mut u8)) {
    // SAFETY: guaranteed by the caller.
    unsafe {
        match desc.kind {
            KIND_TEXT | KIND_RELATION | KIND_ARRAY => {
                let (buffer, _) = read_slice(value);
                if !buffer.is_null() {
                    f(buffer.cast_mut());
                }
            }
            KIND_TUPLE | KIND_DEFINED => {
                for attr in attributes(desc) {
                    each_buffer(value.add(attr.offset), &*attr.desc, f);
                }
            }
            // A value with null pointers, before its variable's
            // declaration, has no type yet, and so none.
            KIND_UNION if !value.cast::<*const TypeDesc>().read_unaligned().is_null() => {
                let (own, components) = own_type(value, desc);
                each_buffer(components, own, f);
            }
            _ => {}
        }
    }
}

/// Whether a value of the type `desc` describes points to buffers.
///
/// # Safety
///
/// `desc` must be a valid descriptor.
#[cfg(not(test))]
unsafe fn points(desc: &TypeDesc) -> bool {
    // SAFETY: guaranteed by the caller.
    unsafe {
        match desc.kind {
            // Which of a hierarchy's types a value has is known only from it.
            KIND_TEXT | KIND_RELATION | KIND_ARRAY | KIND_UNION => true,
            KIND_TUPLE | KIND_DEFINED => attributes(desc).iter().any(|attr| points(&*attr.desc)),
            _ => false,
        }
    }
}

/// Take a reference to each buffer the value at `value`, of the type `desc`
/// describes, points to.
///
/// # Safety
///
/// As for [`each_buffer`].
#[cfg(not(test))]
unsafe fn retain(value: *const u8, desc: &TypeDesc) {
    // SAFETY: guaranteed by the caller.
    unsafe {
        each_buffer(value, desc, &mut |buffer| {
            let mut counted = header(buffer);
            if counted.count != IMMORTAL {
                counted.count += 1;
                set_header(buffer, counted);
            }
        });
    }
}

/// Give up a reference to `buffer`: the last one frees it, after giving up
/// those it holds through its elements.
///
/// # Safety
///
/// `buffer` must be a buffer, just past its header, that the caller holds
/// a reference to.
#[cfg(not(test))]
unsafe fn release_buffer(buffer: *mut u8) {
    // SAFETY: guaranteed by the caller; a buffer's elements, up to its
    // header's `len`, are valid values of its element type.
    unsafe {
        let mut counted = header(buffer);
        if counted.count == IMMORTAL {
            return;
        }
        counted.count -= 1;
        if counted.count > 0 {
            set_header(buffer, counted);
            return;
        }
        if let Some(desc) = counted.desc.as_ref() {
            let element = desc.element();
            if points(element) {
                for i in 0..counted.len {
                    release(buffer.add(i * element.size), element);
                }
            }
        }
        free(buffer.sub(HEADER).cast());
    }
}

/// Give up a reference to each buffer the value at `value`, of the type
/// `desc` describes, points to.
///
/// # Safety
///
/// As for [`each_buffer`], and the caller must hold those references.
#[cfg(not(test))]
unsafe fn release(value: *const u8, desc: &TypeDesc) {
    // SAFETY: guaranteed by the caller.
    unsafe { each_buffer(value, desc, &mut |buffer| release_buffer(buffer)) }
}

/// Store at `dst`, a variable's value, the value at `src`, of the type
/// `desc` describes, taking a reference to each buffer it points to. The
/// variable's references to the buffers of its old value go to the current
/// statement's scope, which releases them when it ends, since the statement
/// may still point into them, as an `assign` block that swaps two variables
/// does.
///
/// # Safety
///
/// `src` must point to a valid value of the type `desc` describes, `desc`
/// to a valid descriptor, and `dst` to a variable's value, which holds
/// references to what it points to, or null pointers, and doesn't overlap
/// `src`. The statement must have begun a scope.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_assign(dst: *mut u8, src: *const u8, desc: *const TypeDesc) {
    // SAFETY: guaranteed by the caller.
    unsafe {
        let desc = &*desc;
        retain(src, desc);
        each_buffer(dst, desc, &mut |buffer| hold(buffer));
        core::ptr::copy_nonoverlapping(src, dst, desc.size);
    }
}

/// Have the current statement's scope hold a reference to each buffer the
/// value at `value`, of the type `desc` describes, points to, until it
/// ends: an update operator's argument, which the operator's assignments
/// must not free, since an argument can be the value of a variable the
/// operator assigns.
///
/// # Safety
///
/// `value` must point to a valid value of the type `desc` describes, and
/// `desc` to a valid descriptor. The statement must have begun a scope.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_keep(value: *const u8, desc: *const TypeDesc) {
    // SAFETY: guaranteed by the caller.
    unsafe {
        let desc = &*desc;
        retain(value, desc);
        each_buffer(value, desc, &mut |buffer| hold(buffer));
    }
}

/// Give up the references that the value at `value`, a local variable's of
/// the type `desc` describes, holds, at the end of its block, and make its
/// pointers null, so that the block, run again, has nothing to give up when
/// it first assigns it. Nothing points into what is freed: every statement
/// that could has ended, and a statement's scope holds its own references.
///
/// # Safety
///
/// `value` must point to a local variable's value of the type `desc`
/// describes, which holds references to what it points to, or null
/// pointers.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_release(value: *mut u8, desc: *const TypeDesc) {
    // SAFETY: guaranteed by the caller.
    unsafe {
        let desc = &*desc;
        release(value, desc);
        core::ptr::write_bytes(value, 0, desc.size);
    }
}

/// Take a reference, for the buffer of the relation or array at `value`,
/// of the type `desc` describes, to what each of its tuples or elements
/// points to: compiled code has filled the buffer, which the runtime
/// allocated ([`trendy_rt_allocate`]), so that it holds them.
///
/// # Safety
///
/// `value` must point to a valid relation or array of the type `desc`
/// describes, whose buffer [`trendy_rt_allocate`] made, holding references
/// through none of its elements.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_adopt(value: *const u8, desc: *const TypeDesc) {
    // SAFETY: guaranteed by the caller.
    unsafe {
        let element = (*desc).element();
        let (buffer, len) = read_slice(value);
        if points(element) {
            for i in 0..len {
                retain(buffer.add(i * element.size), element);
            }
        }
        set_header(
            buffer.cast_mut(),
            Header {
                len,
                ..header(buffer)
            },
        );
    }
}

/// Copy the tuple or element at `src`, of the type `desc` describes, to
/// `dst` in the buffer `buffer`, taking a reference for it to what the
/// tuple points to, and count one more element the buffer holds them
/// through.
///
/// # Safety
///
/// `src` must point to a valid value of the type `desc` describes, `dst` to
/// the buffer's next element after those it holds references through, and
/// `buffer` to a buffer the runtime allocated.
#[cfg(not(test))]
unsafe fn copy_element(buffer: *mut u8, dst: *mut u8, src: *const u8, desc: &TypeDesc) {
    // SAFETY: guaranteed by the caller.
    unsafe {
        core::ptr::copy_nonoverlapping(src, dst, desc.size);
        retain(src, desc);
        let counted = header(buffer);
        set_header(
            buffer,
            Header {
                len: counted.len + 1,
                ..counted
            },
        );
    }
}

/// Make the array at `value`, of the type `desc` describes, the only
/// holder of its buffer, so that compiled code can change its elements in
/// place: as it is, if nothing else holds the buffer, or else a copy of
/// it, whose elements take references to what they point to. The holder's
/// reference to the old buffer goes to the current statement's scope,
/// which may still point into it.
///
/// # Safety
///
/// `value` must point to a valid array of the type `desc` describes that
/// holds a reference to its buffer: a variable's value, or an element of a
/// buffer that only its holder holds. The statement must have begun a
/// scope.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_unique(value: *mut Text, desc: *const TypeDesc) {
    // SAFETY: guaranteed by the caller.
    unsafe {
        let (buffer, len) = read_slice(value.cast());
        if header(buffer).count == 1 {
            return;
        }
        let copy = copied(buffer, len, &*desc);
        hold(buffer.cast_mut());
        value.write_unaligned(Text { bytes: copy, len });
    }
}

/// Store at `dst` a copy of the array at `src`, of the type `desc`
/// describes, in a new buffer, which the current statement's scope holds
/// the only reference to, so that compiled code can change its elements in
/// place: an array with some elements changed, as a value.
///
/// # Safety
///
/// `src` must point to a valid array of the type `desc` describes, and
/// `dst` to memory for one. The statement must have begun a scope.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_duplicate(
    dst: *mut Text,
    src: *const u8,
    desc: *const TypeDesc,
) {
    // SAFETY: guaranteed by the caller.
    unsafe {
        let (buffer, len) = read_slice(src);
        let copy = copied(buffer, len, &*desc);
        hold(copy);
        dst.write_unaligned(Text { bytes: copy, len });
    }
}

/// A new buffer of copies of the `len` elements of `buffer`, an array's of
/// the type `desc` describes, which take references to what they point to,
/// and whose one reference is the caller's.
///
/// # Safety
///
/// `buffer` must hold `len` valid elements of the array's type.
#[cfg(not(test))]
unsafe fn copied(buffer: *const u8, len: usize, desc: &TypeDesc) -> *mut u8 {
    // SAFETY: guaranteed by the caller.
    unsafe {
        let element = desc.element();
        let Some(bytes) = len.checked_mul(element.size) else {
            fail(Failure::OutOfMemory)
        };
        let copy = new_buffer(bytes, desc);
        for i in 0..len {
            let at = i * element.size;
            copy_element(copy, copy.add(at), buffer.add(at), element);
        }
        copy
    }
}

/// Memory for `count` values of type `T`, at least one byte, so that even
/// an empty text, relation or array has an address. `malloc`'s memory is aligned
/// for any type. Stops the program if there is none.
#[cfg(not(test))]
fn allocate<T>(count: usize) -> *mut T {
    let Some(size) = count.checked_mul(size_of::<T>()) else {
        fail(Failure::OutOfMemory)
    };
    // SAFETY: `malloc` has no preconditions.
    let memory = unsafe { malloc(size.max(1)) };
    if memory.is_null() {
        fail(Failure::OutOfMemory);
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
        let size = result.element().size;
        let Some(bytes) = count.checked_mul(size) else {
            fail(Failure::OutOfMemory)
        };
        let attributes = attributes(result);
        let offsets = core::slice::from_raw_parts(offsets, attributes.len());
        // Each tuple with only the kept attributes, in the source's order.
        let projected = allocate::<u8>(bytes);
        core::ptr::write_bytes(projected, 0, bytes);
        for i in 0..count {
            let from = tuples.add(i * source.element().size);
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
        let lhs_sorted = sorted_on(common, true, lhs, lhs_count, lhs_desc.element().size);
        let rhs_sorted = sorted_on(common, false, rhs, rhs_count, rhs_desc.element().size);
        let (groups, total) = matching_runs(common, lhs_sorted, rhs_sorted);

        let size = result.element().size;
        let Some(bytes) = total.checked_mul(size) else {
            fail(Failure::OutOfMemory)
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
        let tuple = |i: usize| tuples.add(i * source.element().size);
        let total = (0..count)
            .map(|i| {
                ungrouped.iter().try_fold(1usize, |product, u| {
                    product.checked_mul(read_slice(tuple(i).add(u.offset)).1)
                })
            })
            .try_fold(0usize, |sum, n| sum.checked_add(n?));
        let size = result.element().size;
        let Some((total, bytes)) = total.and_then(|total| Some((total, total.checked_mul(size)?)))
        else {
            fail(Failure::OutOfMemory)
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
        let size = desc.element().size;
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

/// The start of the error [`trendy_rt_combine`] writes, before the tuple,
/// when an included minus's right operand has a tuple the left lacks.
#[cfg(not(test))]
const MISSING_TUPLE: &str = "error[R0007]: i_minus or i_delete removes a tuple that isn't there: ";

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
        let size = desc.element().size;
        let Some(room) = lhs_count.checked_add(rhs_count) else {
            fail(Failure::OutOfMemory)
        };
        let Some(bytes) = room.checked_mul(size) else {
            fail(Failure::OutOfMemory)
        };
        let out = allocate_buffer(bytes, desc);
        let tuple_desc = desc.element();
        let mut count = 0;
        let mut keep = |tuple: *const u8| {
            copy_element(out, out.add(count * size), tuple, tuple_desc);
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
                        COMBINE_DISJOINT_UNION => tuple_failed(
                            "error[R0006]: d_union or d_insert adds a tuple that is already there: ",
                            l,
                            tuple_desc,
                        ),
                        COMBINE_DISJOINT_CHANGES => tuple_failed(
                            "error[R0009]: two changes in an assign block remove the same tuple: ",
                            l,
                            tuple_desc,
                        ),
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
                        tuple_failed(MISSING_TUPLE, rhs.add(j * size), tuple_desc);
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
            tuple_failed(MISSING_TUPLE, rhs.add(j * size), tuple_desc);
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
        let lhs_sorted = sorted_on(common, true, lhs, lhs_count, lhs_desc.element().size);
        let rhs_sorted = sorted_on(common, false, rhs, rhs_count, rhs_desc.element().size);
        let size = lhs_desc.element().size;
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
                        fail(Failure::OutOfMemory)
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

/// Store at `dst` a relation or an array of the type `desc` describes with
/// `*count` tuples or elements, all zero bytes, in memory the statement
/// owns, for compiled code to fill in: a loop that computes each tuple of
/// its result, or each element of an array selector. Zeroing leaves the
/// padding in them zero, as everywhere else.
///
/// # Safety
///
/// `count` must point to a `usize`, `desc` to a valid relation or array
/// descriptor, and `dst` to memory for one.
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
        let Some(bytes) = count.checked_mul((*desc).element().size) else {
            fail(Failure::OutOfMemory)
        };
        let tuples = allocate_buffer(bytes, desc);
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

/// An attribute [`trendy_rt_load`] orders tuples by: its offset in a tuple,
/// its type, and whether in descending order, 1, or ascending, 0. The
/// compiler emits an array of these, laid out like this struct.
#[repr(C)]
pub struct OrderItem {
    pub offset: usize,
    pub desc: *const TypeDesc,
    pub descending: usize,
}

/// Part of an element of an array [`trendy_rt_load`] makes: `size` bytes
/// from offset `from` in a tuple of the relation, to offset `to` in the
/// element. The compiler emits an array of these, laid out like this
/// struct, one for each attribute an element keeps.
#[repr(C)]
pub struct Part {
    pub from: usize,
    pub to: usize,
    pub size: usize,
}

/// Store at `dst` the array, which `result` describes, of the tuples of the
/// relation `src`, which `source` describes, in the order of the `*count`
/// `items`: by the first, then, among tuples equal on it, by the next, and
/// so on, in memory the statement owns. Each element is made of the
/// `*part_count` `parts` of its tuple: the tuple without the attributes
/// removed, or the value of the only one left, and elements that turn out
/// equal are each there. Tuples equal on every item are in canonical
/// order, which the language leaves undefined.
///
/// # Safety
///
/// `src` must point to a valid relation of the type `source` describes,
/// `result` to a valid array descriptor, `items` to `*count` items, each
/// with the offset and type of one of the relation's attributes, and
/// `parts` to `*part_count` parts, which together make a valid element of
/// the array from a tuple.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_load(
    dst: *mut Text,
    src: *const u8,
    source: *const TypeDesc,
    result: *const TypeDesc,
    items: *const OrderItem,
    count: *const usize,
    parts: *const Part,
    part_count: *const usize,
) {
    // SAFETY: guaranteed by the caller.
    unsafe {
        let (source, result) = (&*source, &*result);
        let items = array(items, count.read_unaligned());
        let parts = array(parts, part_count.read_unaligned());
        let (tuples, len) = read_slice(src);
        let size = source.element().size;
        let order = core::slice::from_raw_parts_mut(allocate::<*const u8>(len), len);
        for (i, tuple) in order.iter_mut().enumerate() {
            *tuple = tuples.add(i * size);
        }
        // A relation's tuples are distinct, so no two are equal in
        // canonical order, and the sort has one result.
        order.sort_unstable_by(|a, b| {
            on_items(items, *a, *b).then_with(|| compare_tuples(*a, *b, source))
        });
        let element = result.element();
        let Some(bytes) = len.checked_mul(element.size) else {
            fail(Failure::OutOfMemory)
        };
        let out = allocate_buffer(bytes, result);
        // Each element is made here first, zeroed, as everywhere, so that
        // the padding in it is zero.
        let made = allocate::<u8>(element.size);
        core::ptr::write_bytes(made, 0, element.size);
        for (i, tuple) in order.iter().enumerate() {
            for part in parts {
                core::ptr::copy_nonoverlapping(tuple.add(part.from), made.add(part.to), part.size);
            }
            copy_element(out, out.add(i * element.size), made, element);
        }
        free(made.cast());
        free(order.as_mut_ptr().cast());
        dst.write_unaligned(Text { bytes: out, len });
    }
}

/// Store at `dst` the relation, which `result` describes, of the elements
/// of the array `src`, which `source` describes, in memory the statement
/// owns. Each element makes a tuple of the `*part_count` `parts` of it,
/// each `size` bytes from offset `from` in the element to offset `to` in
/// the tuple: its attributes, or itself, as the value of an attribute. At
/// each of the `*number_count` offsets `numbers`, none or one, the tuple
/// has the element's position, counting from 1, an integer. Tuples that
/// turn out equal are one.
///
/// # Safety
///
/// `src` must point to a valid array of the type `source` describes,
/// `result` to a valid relation descriptor, `parts` to `*part_count`
/// parts, and `numbers` to `*number_count` offsets of integer attributes,
/// which together make a valid tuple of the relation from an element.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_unload(
    dst: *mut Text,
    src: *const u8,
    source: *const TypeDesc,
    result: *const TypeDesc,
    parts: *const Part,
    part_count: *const usize,
    numbers: *const usize,
    number_count: *const usize,
) {
    // SAFETY: guaranteed by the caller.
    unsafe {
        let (source, result) = (&*source, &*result);
        let parts = array(parts, part_count.read_unaligned());
        let numbers = array(numbers, number_count.read_unaligned());
        let (elements, len) = read_slice(src);
        let element = source.element().size;
        let size = result.element().size;
        let Some(bytes) = len.checked_mul(size) else {
            fail(Failure::OutOfMemory)
        };
        // The tuples are made here first, zeroed, as everywhere, so that
        // the padding in them is zero, and share the elements' buffers
        // until `canonical` copies them, taking a reference to each.
        let tuples = allocate::<u8>(bytes);
        core::ptr::write_bytes(tuples, 0, bytes);
        for i in 0..len {
            let (from, to) = (elements.add(i * element), tuples.add(i * size));
            for part in parts {
                core::ptr::copy_nonoverlapping(from.add(part.from), to.add(part.to), part.size);
            }
            // An array of tuples of no attributes takes no memory, so only
            // the integers limit its positions.
            let Ok(position) = i64::try_from(i + 1) else {
                fail(Failure::IntegerOverflow)
            };
            for number in numbers {
                to.add(*number).cast::<i64>().write_unaligned(position);
            }
        }
        let (out, kept) = canonical(tuples, len, result);
        free(tuples.cast());
        dst.write_unaligned(Text {
            bytes: out,
            len: kept,
        });
    }
}

/// The order of two tuples by the `items` of a load.
///
/// # Safety
///
/// `x` and `y` must point to valid tuples with the items' attributes.
#[cfg(not(test))]
unsafe fn on_items(items: &[OrderItem], x: *const u8, y: *const u8) -> core::cmp::Ordering {
    for item in items {
        // SAFETY: guaranteed by the caller.
        let ordering = unsafe { order(x.add(item.offset), y.add(item.offset), &*item.desc) };
        let ordering = if item.descending == 1 {
            ordering.reverse()
        } else {
            ordering
        };
        if ordering.is_ne() {
            return ordering;
        }
    }
    core::cmp::Ordering::Equal
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
    // SAFETY: guaranteed by the caller.
    unsafe {
        let size = desc.element().size;
        let order = core::slice::from_raw_parts_mut(allocate::<*const u8>(count), count);
        for (i, tuple) in order.iter_mut().enumerate() {
            *tuple = tuples.add(i * size);
        }
        order.sort_unstable_by(|a, b| compare_tuples(*a, *b, desc));
        let out = allocate_buffer(count * size, desc);
        let mut kept = 0;
        for i in 0..count {
            if i > 0 && compare_tuples(order[i - 1], order[i], desc).is_eq() {
                continue;
            }
            copy_element(out, out.add(kept * size), order[i], desc.element());
            kept += 1;
        }
        free(order.as_mut_ptr().cast());
        (out, kept)
    }
}

/// The canonical order of two values of the type `desc` describes, which is
/// the compiler's (`hir::Value`'s): `false` before `true`, integers and
/// approximate numbers numerically, text by its bytes, tuples attribute by attribute in name order, and relations by their
/// tuples and arrays by their elements, lexicographically, a prefix first.
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
            // IEEE 754 totalOrder, which, without NaN or `-0.0`, is the
            // order of the values.
            KIND_APPROXIMATE => lhs
                .cast::<f64>()
                .read_unaligned()
                .total_cmp(&rhs.cast::<f64>().read_unaligned()),
            KIND_TEXT => {
                let (a, b) = (read_slice(lhs), read_slice(rhs));
                core::slice::from_raw_parts(a.0, a.1).cmp(core::slice::from_raw_parts(b.0, b.1))
            }
            KIND_TUPLE | KIND_DEFINED => compare_tuples(lhs, rhs, desc),
            // By their types' names, then their components.
            KIND_UNION => {
                let ((a, at), (b, bt)) = (own_type(lhs, desc), own_type(rhs, desc));
                if same_type(a, b) {
                    order(at, bt, a)
                } else {
                    type_name(a).cmp(type_name(b))
                }
            }
            KIND_RELATION | KIND_ARRAY => {
                let ((a, count), (b, other)) = (read_slice(lhs), read_slice(rhs));
                let element = desc.element();
                for i in 0..count.min(other) {
                    let offset = i * element.size;
                    let ordering = order(a.add(offset), b.add(offset), element);
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

/// Why the runtime stops the program where it has no values to write.
#[cfg(not(test))]
#[derive(Debug, Clone, Copy)]
enum Failure {
    /// An integer result out of range: a sum, a product, or a position.
    IntegerOverflow,
    /// An approximate result beyond the largest finite magnitude: a sum or
    /// a product.
    ApproximateOverflow,
    /// Memory for a result couldn't be allocated.
    OutOfMemory,
}

/// Stop the program because of `failure`.
#[cfg(not(test))]
fn fail(failure: Failure) -> ! {
    fail_with(|out| {
        out.write_str(match failure {
            Failure::IntegerOverflow => "error[R0001]: integer overflow\n",
            Failure::ApproximateOverflow => "error[R0001]: approximate overflow\n",
            Failure::OutOfMemory => "error[R0003]: out of memory\n",
        })
    })
}

/// Stop the program because `lhs op rhs`, of integers, has no result, `op`
/// being an `ARITHMETIC_` constant, writing the operation: a division by
/// zero (R0002), or else an overflow (R0001). A negation's `lhs` is 0.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn trendy_rt_integer_failed(op: u32, lhs: i64, rhs: i64) -> ! {
    arithmetic_failed(op, Number::Integer(lhs), Number::Integer(rhs))
}

/// As [`trendy_rt_integer_failed`], of approximate numbers.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn trendy_rt_approximate_failed(op: u32, lhs: f64, rhs: f64) -> ! {
    arithmetic_failed(op, Number::Approximate(lhs), Number::Approximate(rhs))
}

/// An operand of an arithmetic operation that has no result.
#[cfg(not(test))]
#[derive(Debug, Clone, Copy)]
enum Number {
    Integer(i64),
    Approximate(f64),
}

#[cfg(not(test))]
impl Number {
    fn is_zero(self) -> bool {
        match self {
            Number::Integer(n) => n == 0,
            // Equal to 0.0 for both zeros.
            Number::Approximate(x) => x == 0.0,
        }
    }

    fn is_negative(self) -> bool {
        match self {
            Number::Integer(n) => n < 0,
            Number::Approximate(x) => x < 0.0,
        }
    }
}

#[cfg(not(test))]
impl fmt::Display for Number {
    /// As trendy source writes it, as [`write_value`] does.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Number::Integer(n) => write!(f, "{n}"),
            Number::Approximate(x) => write!(f, "{x:e}"),
        }
    }
}

/// Stop the program because `lhs op rhs` has no result, as
/// [`trendy_rt_integer_failed`] says, writing the operation as trendy
/// source writes it, with a negative right operand in parentheses.
#[cfg(not(test))]
fn arithmetic_failed(op: u32, lhs: Number, rhs: Number) -> ! {
    let symbol = match op {
        ARITHMETIC_ADD => "+",
        ARITHMETIC_SUBTRACT | ARITHMETIC_NEGATE => "-",
        ARITHMETIC_MULTIPLY => "*",
        ARITHMETIC_DIVIDE => "/",
        ARITHMETIC_DIV => "div",
        ARITHMETIC_MOD => "mod",
        // The compiler and the runtime disagree: a bug.
        _ => fail_with(|out| {
            out.write_str("error[X0000]: internal error: unknown arithmetic operator\n")
        }),
    };
    let divides = matches!(op, ARITHMETIC_DIVIDE | ARITHMETIC_DIV | ARITHMETIC_MOD);
    let (error, outcome) = match rhs {
        _ if divides && rhs.is_zero() => ("R0002]: division by zero", "has no result"),
        Number::Integer(_) => (
            "R0001]: integer overflow",
            "doesn't fit in a 64-bit integer",
        ),
        Number::Approximate(_) => (
            "R0001]: approximate overflow",
            "is too large for an approximate number",
        ),
    };
    fail_with(|out| {
        write!(out, "error[{error}: ")?;
        if op == ARITHMETIC_NEGATE {
            out.write_str(symbol)?;
        } else {
            write!(out, "{lhs} {symbol} ")?;
        }
        if rhs.is_negative() {
            write!(out, "({rhs})")?;
        } else {
            write!(out, "{rhs}")?;
        }
        writeln!(out, " {outcome}")
    })
}

/// Stop the program because tuples being joined disagree on the common
/// attribute named `attribute`, a text, which is `lhs` in one and `rhs` in
/// the other, of the type `desc` describes, writing them (R0004).
///
/// # Safety
///
/// `attribute` must point to a valid text value, and `lhs` and `rhs` to
/// valid values of the type `desc` describes.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_tuple_mismatch(
    attribute: *const u8,
    lhs: *const u8,
    rhs: *const u8,
    desc: *const TypeDesc,
) -> ! {
    // SAFETY: guaranteed by the caller.
    unsafe {
        let (attribute, desc) = (read_text(attribute), &*desc);
        fail_with(|out| {
            write!(
                out,
                "error[R0004]: the tuples disagree on `{attribute}`, so they have no join: `{attribute}` is "
            )?;
            write_value(out, lhs, desc)?;
            out.write_str(" in one and ")?;
            write_value(out, rhs, desc)?;
            out.write_str(" in the other\n")
        })
    }
}

/// Stop the program because `value`, of the type of the program's own
/// `desc` describes, breaks its constraint written `constraint`, a text,
/// writing them (R0017).
///
/// # Safety
///
/// `value` must point to a valid value of the type `desc` describes, a
/// type of the program's own, and `constraint` to a valid text value.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_broken_constraint(
    value: *const u8,
    desc: *const TypeDesc,
    constraint: *const u8,
) -> ! {
    // SAFETY: guaranteed by the caller.
    unsafe {
        let (desc, constraint) = (&*desc, read_text(constraint));
        fail_with(|out| {
            out.write_str("error[R0017]: ")?;
            write_value(out, value, desc)?;
            writeln!(
                out,
                " isn't a value of `{}`: it breaks the constraint `{constraint}`",
                type_name(desc)
            )
        })
    }
}

/// Stop the program because `value`, of the type of the program's own
/// `desc` describes, which `load_csv` read for the attribute or component
/// named `attribute`, a text, from the record on line `*line` of the file
/// at `path`, a text, breaks its constraint written `constraint`, a text,
/// writing them (R0017).
///
/// # Safety
///
/// `value` must point to a valid value of the type `desc` describes, a
/// type of the program's own, `constraint`, `attribute` and `path` to
/// valid text values, and `line` to a `usize`.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_broken_constraint_in_file(
    value: *const u8,
    desc: *const TypeDesc,
    constraint: *const u8,
    attribute: *const u8,
    path: *const u8,
    line: *const usize,
) -> ! {
    // SAFETY: guaranteed by the caller.
    unsafe {
        let desc = &*desc;
        let (constraint, attribute) = (read_text(constraint), read_text(attribute));
        let (path, line) = (read_text(path), line.read_unaligned());
        fail_with(|out| {
            write!(out, "error[R0017]: line {line} of \"")?;
            trendy_text::escape(path, out)?;
            write!(out, "\" gives `{attribute}` ")?;
            write_value(out, value, desc)?;
            writeln!(
                out,
                ", which isn't a value of `{}`: it breaks the constraint `{constraint}`",
                type_name(desc)
            )
        })
    }
}

/// Stop the program because a relation has `*count` tuples, not one,
/// where its only one is taken, writing the count (R0005).
///
/// # Safety
///
/// `count` must point to a `usize`.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_not_one_tuple(count: *const usize) -> ! {
    // SAFETY: guaranteed by the caller.
    let count = unsafe { count.read_unaligned() };
    fail_with(|out| {
        out.write_str("error[R0005]: the relation has ")?;
        match count {
            0 => out.write_str("no tuples")?,
            _ => write!(out, "{count} tuples")?,
        }
        out.write_str(", but exactly one is taken\n")
    })
}

/// Stop the program because a relation has no tuples, where the least
/// value of an attribute is taken (R0010).
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn trendy_rt_no_min() -> ! {
    fail_with(|out| out.write_str("error[R0010]: the relation has no tuples, so it has no min\n"))
}

/// Stop the program because a relation has no tuples, where the greatest
/// value of an attribute is taken (R0010).
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn trendy_rt_no_max() -> ! {
    fail_with(|out| out.write_str("error[R0010]: the relation has no tuples, so it has no max\n"))
}

/// Stop the program because an array of `*length` elements has none at
/// `*position`, writing them (R0011).
///
/// # Safety
///
/// `position` and `length` must point to integers.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_index_out_of_range(
    position: *const i64,
    length: *const i64,
) -> ! {
    // SAFETY: guaranteed by the caller.
    let (position, length) = unsafe { (position.read_unaligned(), length.read_unaligned()) };
    fail_with(|out| {
        out.write_str("error[R0011]: ")?;
        match length {
            _ if position < 1 => write!(
                out,
                "an array has no element at position {position}: positions count from 1"
            )?,
            0 => write!(
                out,
                "the array is empty, so it has no element at position {position}"
            )?,
            1 => write!(
                out,
                "the array has 1 element, so none at position {position}"
            )?,
            _ => write!(
                out,
                "the array has {length} elements, so none at position {position}"
            )?,
        }
        out.write_char('\n')
    })
}

/// Stop the program because two changes to an array in an `assign` block
/// set the element at the `*count` `positions`, from the outermost array
/// in, or, if `*nested` is 1, one sets it and the other an element in it,
/// writing the positions (R0009).
///
/// # Safety
///
/// `positions` must point to `*count` integers, `count` to a `usize`, and
/// `nested` to a byte.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_element_conflict(
    positions: *const i64,
    count: *const usize,
    nested: *const u8,
) -> ! {
    // SAFETY: guaranteed by the caller.
    unsafe {
        let (count, nested) = (count.read_unaligned(), nested.read() == 1);
        fail_with(|out| {
            if nested {
                out.write_str("error[R0009]: one change in an assign block sets the element at ")?;
            } else {
                out.write_str(
                    "error[R0009]: two changes in an assign block set the same element, at ",
                )?;
            }
            for i in 0..count {
                write!(out, "[{}]", positions.add(i).read_unaligned())?;
            }
            if nested {
                out.write_str(", and another an element in it")?;
            }
            out.write_char('\n')
        })
    }
}

/// Stop the program, writing `message`, then the tuple at `tuple`, of the
/// type `desc` describes.
///
/// # Safety
///
/// `tuple` must point to a valid tuple of the type `desc` describes.
#[cfg(not(test))]
unsafe fn tuple_failed(message: &str, tuple: *const u8, desc: &TypeDesc) -> ! {
    fail_with(|out| {
        out.write_str(message)?;
        // SAFETY: guaranteed by the caller.
        unsafe { write_value(out, tuple, desc)? };
        out.write_char('\n')
    })
}

/// Stop the program if the relation `value`, of the type `desc` describes,
/// about to be assigned to the variable named `variable`, a text, has two
/// tuples with the same values for the attributes of `key`, the
/// descriptor of its projection on one of its keys, writing the variable,
/// the key and the two tuples (R0008).
///
/// # Safety
///
/// `value` must point to a valid relation of the type `desc` describes,
/// `key` to the descriptor of a relation of some of its attributes, and
/// `variable` to a valid text value.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_check_key(
    value: *const u8,
    desc: *const TypeDesc,
    key: *const TypeDesc,
    variable: *const u8,
) {
    // SAFETY: guaranteed by the caller; `allocate` gives memory for the
    // key's attributes and the tuples' positions, which nothing else
    // holds, freed once they are compared.
    unsafe {
        let (tuples, count) = read_slice(value);
        if count < 2 {
            return;
        }
        let (desc, key) = (&*desc, attributes(&*key));
        let element = desc.element();
        // The tuples' own attributes in the key, by name.
        let found = allocate::<*const Attr>(key.len());
        let mut len = 0;
        for attr in attributes(element) {
            if key.iter().any(|k| name(k) == name(attr)) {
                found.add(len).write(attr);
                len += 1;
            }
        }
        let compared = array(found, len);
        let on_key = |i: usize, j: usize| {
            let (a, b) = (tuples.add(i * element.size), tuples.add(j * element.size));
            compared
                .iter()
                .map(|&attr| order(a.add((*attr).offset), b.add((*attr).offset), &*(*attr).desc))
                .find(|o| o.is_ne())
                .unwrap_or(core::cmp::Ordering::Equal)
        };
        // The tuples' positions, in their keys' order, which puts tuples
        // with the same values for it next to each other.
        let slots = allocate::<usize>(count);
        for i in 0..count {
            slots.add(i).write(i);
        }
        let positions = core::slice::from_raw_parts_mut(slots, count);
        positions.sort_unstable_by(|&i, &j| on_key(i, j).then(i.cmp(&j)));
        let shared = positions
            .windows(2)
            .find(|pair| on_key(pair[0], pair[1]).is_eq());
        if let Some(&[first, second]) = shared {
            let variable = read_text(variable);
            fail_with(|out| {
                if key.is_empty() {
                    write!(
                        out,
                        "error[R0008]: `{variable}` has two tuples, but its key {{}} allows at most one: "
                    )?;
                } else {
                    write!(
                        out,
                        "error[R0008]: two tuples of `{variable}` have the same values for its key {{ "
                    )?;
                    for (i, attr) in key.iter().enumerate() {
                        if i > 0 {
                            out.write_str(", ")?;
                        }
                        out.write_str(name(attr))?;
                    }
                    out.write_str(" }: ")?;
                }
                write_value(out, tuples.add(first * element.size), element)?;
                out.write_str(" and ")?;
                write_value(out, tuples.add(second * element.size), element)?;
                out.write_char('\n')
            });
        }
        free(slots.cast());
        free(found.cast());
    }
}

/// Where the program is in its source, a statement's place or a value's,
/// laid out as the compiler emits it.
#[repr(C)]
pub struct Site {
    /// The source file's path, as the compiler was given it, or
    /// `<prelude>`.
    pub path: Text,
    /// Counting from 1.
    pub line: usize,
    /// Counting from 1, in characters.
    pub column: usize,
}

/// The site the program is at, which compiled code sets before each
/// statement runs, and each value of a value block's is computed, and
/// again after each call, as the callee sets its own; null before the
/// first. A failure writes it.
#[unsafe(export_name = "trendy_rt_site")]
pub static SITE: core::sync::atomic::AtomicPtr<Site> =
    core::sync::atomic::AtomicPtr::new(core::ptr::null_mut());

/// Stop the program, writing the message `write` writes to stderr, a line
/// that starts with the compiler's error code, then where the program is,
/// as a compiler's error says, ` --> path:line:column`, and exiting with
/// status 1.
#[cfg(not(test))]
fn fail_with(write: impl FnOnce(&mut Stderr) -> fmt::Result) -> ! {
    let mut out = Stderr::default();
    // There is nowhere to report a failure to write to stderr.
    let _ = write(&mut out);
    // SAFETY: compiled code sets the site to one it emits, read-only data
    // whose path is UTF-8.
    let site = unsafe {
        SITE.load(core::sync::atomic::Ordering::Relaxed)
            .as_ref()
            .map(|site| {
                (
                    read_text((&raw const site.path).cast()),
                    site.line,
                    site.column,
                )
            })
    };
    if let Some((path, line, column)) = site {
        let _ = writeln!(out, " --> {path}:{line}:{column}");
    }
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
    /// For a tuple or relation, its heading's attributes, and for a type of
    /// the program's own, its components, sorted by name. For other kinds,
    /// `count` is 0 and this is not read.
    pub attributes: *const Attr,
    pub count: usize,
    /// For a relation, its tuples' type, and for an array, its elements':
    /// what each element of its memory is. For other kinds, null.
    pub element: *const TypeDesc,
    /// For a type of the program's own, the UTF-8 bytes of its name, whose
    /// components are its attributes. For other kinds, `name_len` is 0.
    pub name: *const u8,
    pub name_len: usize,
}

impl TypeDesc {
    /// The type of each element of a relation's or an array's memory: its
    /// tuples' or its elements'.
    ///
    /// # Safety
    ///
    /// The descriptor must be a valid relation or array descriptor.
    unsafe fn element(&self) -> &TypeDesc {
        // SAFETY: guaranteed by the caller.
        unsafe { &*self.element }
    }
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

/// Print the relation `value`, of the type `desc` describes, to stderr as
/// a table, as [`write_table`] writes it. Stops the program if there is no
/// memory for its columns' widths.
///
/// # Safety
///
/// As for [`trendy_rt_debug_value`], of a relation.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_debug_table(value: *const u8, desc: *const TypeDesc) {
    // SAFETY: guaranteed by the caller; `allocate` gives memory for a width
    // of each attribute, aligned, which nothing else holds, freed once
    // the table is written.
    unsafe {
        let desc = &*desc;
        let widths = allocate::<usize>(desc.count);
        let mut out = Stderr::default();
        // There is nowhere to report a failure to write to stderr.
        let _ = write_table(
            &mut out,
            value,
            desc,
            core::slice::from_raw_parts_mut(widths, desc.count),
        );
        out.flush();
        free(widths.cast());
    }
}

/// Store at `dst` the sum of the attribute at `index`, its position by
/// name, of the tuples of the relation `src`, of the type `desc` describes:
/// the exact sum of the integers or approximate numbers, rounded once, as
/// the compiler sums them. Stops the program if the sum is beyond its
/// type's range.
///
/// # Safety
///
/// `src` must point to a valid relation of the type `desc` describes,
/// `index` to a position among its attributes, of one that is an integer
/// or an approximate number, and `dst` to memory for one.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_sum(
    dst: *mut u8,
    src: *const u8,
    desc: *const TypeDesc,
    index: *const usize,
) {
    // SAFETY: guaranteed by the caller.
    unsafe {
        let column = Column::new(src, &*desc, index.read_unaligned());
        if column.kind == KIND_INTEGER {
            let mut sum = trendy_numeric::IntegerSum::default();
            column.values().for_each(|n| sum.add(n));
            store_integer(dst, sum.total());
        } else {
            let mut sum = trendy_numeric::ApproximateSum::default();
            column.values().for_each(|x| sum.add(x));
            store_approximate(dst, sum.total());
        }
    }
}

/// Store at `dst` the product of the attribute at `index`, as
/// [`trendy_rt_sum`] stores the sum: exact, rounded once, as the compiler
/// multiplies. A product of approximate numbers too near halfway between two
/// for 128 bits to tell which is worked out in full, in memory it frees.
/// Stops the program if the product is beyond its type's range, or that
/// memory can't be allocated.
///
/// # Safety
///
/// As for [`trendy_rt_sum`].
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_product(
    dst: *mut u8,
    src: *const u8,
    desc: *const TypeDesc,
    index: *const usize,
) {
    // SAFETY: guaranteed by the caller.
    unsafe {
        let column = Column::new(src, &*desc, index.read_unaligned());
        if column.kind == KIND_INTEGER {
            let mut product = trendy_numeric::IntegerProduct::default();
            column.values().for_each(|n| product.multiply(n));
            store_integer(dst, product.total());
            return;
        }
        let mut product = trendy_numeric::ApproximateProduct::default();
        column.values().for_each(|x| product.multiply(x));
        let total = match product.total() {
            trendy_numeric::Rounded::Nearest(total) => total,
            trendy_numeric::Rounded::Exact(exact) => {
                let limbs = exact.limbs();
                let memory = allocate::<u64>(limbs);
                core::ptr::write_bytes(memory, 0, limbs);
                let total = exact.total(
                    column.values(),
                    core::slice::from_raw_parts_mut(memory, limbs),
                );
                free(memory.cast());
                total
            }
        };
        store_approximate(dst, total);
    }
}

/// The values of one attribute of the tuples of a relation, each `size`
/// bytes from the last, at `offset` in each.
#[cfg(not(test))]
struct Column {
    tuples: *const u8,
    count: usize,
    size: usize,
    offset: usize,
    /// The attribute's type, one of the `KIND_` constants.
    kind: u8,
}

#[cfg(not(test))]
impl Column {
    /// The attribute at `index`, its position by name, of the tuples of
    /// the relation `src`, of the type `desc` describes.
    ///
    /// # Safety
    ///
    /// `src` must point to a valid relation of that type, and `index` be a
    /// position among its attributes.
    unsafe fn new(src: *const u8, desc: &TypeDesc, index: usize) -> Column {
        // SAFETY: guaranteed by the caller.
        unsafe {
            let attribute = &attributes(desc)[index];
            let (tuples, count) = read_slice(src);
            Column {
                tuples,
                count,
                size: desc.element().size,
                offset: attribute.offset,
                kind: (*attribute.desc).kind,
            }
        }
    }

    /// The values, which must be of type `T`.
    ///
    /// # Safety
    ///
    /// `T` must be the attribute's type, and the relation still valid.
    unsafe fn values<T: Copy>(&self) -> impl Iterator<Item = T> + Clone {
        let (tuples, size, offset) = (self.tuples, self.size, self.offset);
        // SAFETY: guaranteed by the caller of `new`, and of this.
        (0..self.count)
            .map(move |i| unsafe { tuples.add(i * size + offset).cast::<T>().read_unaligned() })
    }
}

/// Store the integer `total` at `dst`, or stop the program if there is
/// none, being out of range.
///
/// # Safety
///
/// `dst` must point to memory for an integer.
#[cfg(not(test))]
unsafe fn store_integer(dst: *mut u8, total: Option<i64>) {
    let Some(total) = total else {
        fail(Failure::IntegerOverflow)
    };
    // SAFETY: guaranteed by the caller.
    unsafe { dst.cast::<i64>().write_unaligned(total) }
}

/// Store the approximate number `total` at `dst`, or stop the program if
/// there is none, being beyond the largest finite.
///
/// # Safety
///
/// `dst` must point to memory for an approximate number.
#[cfg(not(test))]
unsafe fn store_approximate(dst: *mut u8, total: Option<f64>) {
    let Some(total) = total else {
        fail(Failure::ApproximateOverflow)
    };
    // SAFETY: guaranteed by the caller.
    unsafe { dst.cast::<f64>().write_unaligned(total) }
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
        let Ok(text) = text_of(value, &*desc, |bytes| {
            allocate_buffer(bytes, core::ptr::null())
        }) else {
            fail(Failure::OutOfMemory)
        };
        dst.write_unaligned(text);
    }
}

/// Store at `dst` the integer the text `from` writes, as `parse_integer`
/// reads one, and the compiler with it. Stops the program if the text
/// writes none (R0014).
///
/// # Safety
///
/// `from` must point to a valid text value, and `dst` to memory for an
/// integer.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_parse_integer(dst: *mut i64, from: *const Text) {
    // SAFETY: guaranteed by the caller.
    unsafe {
        let text = read_text(from.cast());
        match trendy_numeric::parse_integer(text) {
            Ok(n) => dst.write_unaligned(n),
            Err(why) => unparsable(text, "an integer", why, None),
        }
    }
}

/// Store at `dst` the approximate number nearest the decimal number the
/// text `from` writes, as `parse_approximate` reads one, and the compiler
/// with it. Stops the program if the text writes none (R0014).
///
/// # Safety
///
/// `from` must point to a valid text value, and `dst` to memory for an
/// approximate number.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_parse_approximate(dst: *mut f64, from: *const Text) {
    // SAFETY: guaranteed by the caller.
    unsafe {
        let text = read_text(from.cast());
        match trendy_numeric::parse_approximate(text) {
            Ok(x) => dst.write_unaligned(x),
            Err(why) => unparsable(text, "an approximate number", why, None),
        }
    }
}

/// Store at `dst` the square root of `*x`, as `sqrt` computes it, and the
/// compiler with it. Stops the program if `*x` is negative, which has none
/// (R0018).
///
/// # Safety
///
/// `x` must point to an approximate number, and `dst` to memory for one.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_sqrt(dst: *mut f64, x: *const f64) {
    // SAFETY: guaranteed by the caller.
    let x = unsafe { x.read_unaligned() };
    match trendy_numeric::sqrt(x) {
        // SAFETY: guaranteed by the caller.
        Some(root) => unsafe { dst.write_unaligned(root) },
        None => fail_with(|out| writeln!(out, "error[R0018]: {x:e} has no square root")),
    }
}

/// Store at `dst` the sine of `*x`, in radians, as `sin` computes it, and
/// the compiler with it.
///
/// # Safety
///
/// `x` must point to an approximate number, and `dst` to memory for one.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_sin(dst: *mut f64, x: *const f64) {
    // SAFETY: guaranteed by the caller.
    unsafe { dst.write_unaligned(trendy_numeric::sin(x.read_unaligned())) };
}

/// Store at `dst` the cosine of `*x`, in radians, as `cos` computes it,
/// and the compiler with it.
///
/// # Safety
///
/// `x` must point to an approximate number, and `dst` to memory for one.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_cos(dst: *mut f64, x: *const f64) {
    // SAFETY: guaranteed by the caller.
    unsafe { dst.write_unaligned(trendy_numeric::cos(x.read_unaligned())) };
}

/// Store at `dst` the angle, in radians, from the positive x axis to the
/// point (`*x`, `*y`), as `atan2` computes it, and the compiler with it.
///
/// # Safety
///
/// `y` and `x` must point to approximate numbers, and `dst` to memory for
/// one.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_atan2(dst: *mut f64, y: *const f64, x: *const f64) {
    // SAFETY: guaranteed by the caller.
    unsafe {
        let angle = trendy_numeric::atan2(y.read_unaligned(), x.read_unaligned());
        dst.write_unaligned(angle);
    }
}

/// Store at `dst` the boolean the text `from` writes, as `parse_boolean`
/// reads one, and the compiler with it. Stops the program if the text
/// writes none (R0014).
///
/// # Safety
///
/// `from` must point to a valid text value, and `dst` to memory for a
/// boolean.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_parse_boolean(dst: *mut u8, from: *const Text) {
    // SAFETY: guaranteed by the caller.
    unsafe {
        let text = read_text(from.cast());
        match trendy_text::parse_boolean(text) {
            Ok(b) => dst.write(u8::from(b)),
            Err(why) => unparsable(text, "a boolean", why, None),
        }
    }
}

/// Where a field of a CSV file is, for an attribute: in a column, counting
/// from 0, on a line, of the file at a path.
#[cfg(not(test))]
#[derive(Clone, Copy)]
struct Place<'a> {
    attribute: LeafName<'a>,
    column: usize,
    line: usize,
    path: &'a str,
    /// The file's CSV text, if its first record is a header, which names
    /// the column.
    header: Option<&'a str>,
    /// Whether the field is in the file's first record, which isn't a
    /// header.
    first: bool,
}

/// Stop the program because `text`, at `place` if it is a CSV file's
/// field, isn't `a`, a value of its type, as `why` says.
#[cfg(not(test))]
fn unparsable(text: &str, a: &str, why: trendy_text::Unparsable, place: Option<Place>) -> ! {
    fail_with(|out| {
        out.write_str("error[R0014]: \"")?;
        trendy_text::escape(text, out)?;
        out.write_char('"')?;
        if let Some(place) = place {
            write!(out, ", in column {}", place.column + 1)?;
            if let Some(field) = place
                .header
                .and_then(|text| csv::header(text).nth(place.column))
            {
                out.write_str(" (\"")?;
                // SAFETY: the statement has begun a scope, as
                // `trendy_rt_load_csv`'s caller guarantees.
                trendy_text::escape(unsafe { field_value(field) }, out)?;
                out.write_str("\")")?;
            }
            write!(out, " on line {} of \"", place.line)?;
            trendy_text::escape(place.path, out)?;
            out.write_str("\",")?;
        }
        match why {
            trendy_text::Unparsable::Malformed => write!(out, " isn't {a}")?,
            trendy_text::Unparsable::OutOfRange => write!(out, " is out of {a}'s range")?,
        }
        if let Some(place) = place {
            write!(out, ", the type of `{}`", place.attribute)?;
            if place.first {
                write!(
                    out,
                    "; if line {} is a header, `header: true` skips it",
                    place.line
                )?;
            }
        }
        out.write_char('\n')
    })
}

/// Store at `dst` the array of the lines of the text file at `path`, in
/// the file's order, of the type `desc` describes, `array text`, in memory
/// the statement owns: each line without the line feed, or the carriage
/// return and line feed, that ends it. The last line needs no ending.
/// Stops the program if the file can't be read (R0012), or isn't UTF-8
/// text (R0013), or the memory can't be allocated.
///
/// # Safety
///
/// `path` must point to a valid text value, `desc` to a valid descriptor
/// of that array type, and `dst` to memory for an array. The statement
/// must have begun a scope.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_load_text_file(
    dst: *mut Text,
    path: *const Text,
    desc: *const TypeDesc,
) {
    // SAFETY: guaranteed by the caller.
    unsafe {
        let path = read_text(path.cast());
        let (contents, len) = read_file(path);
        let text = file_text(path, array(contents, len));
        let desc = &*desc;
        let element = desc.element();
        let count = text.lines().count();
        let Some(size) = count.checked_mul(element.size) else {
            fail(Failure::OutOfMemory)
        };
        let lines = allocate_buffer(size, desc);
        for (i, line) in text.lines().enumerate() {
            let bytes = allocate_buffer(line.len(), core::ptr::null());
            core::ptr::copy_nonoverlapping(line.as_ptr(), bytes, line.len());
            let value = Text {
                bytes,
                len: line.len(),
            };
            copy_element(
                lines,
                lines.add(i * element.size),
                (&raw const value).cast(),
                element,
            );
        }
        free(contents.cast());
        dst.write_unaligned(Text {
            bytes: lines,
            len: count,
        });
    }
}

/// `bytes`, the contents of the file at `path`, as text. Stops the program
/// if they aren't UTF-8 (R0013).
#[cfg(not(test))]
fn file_text<'a>(path: &str, bytes: &'a [u8]) -> &'a str {
    match core::str::from_utf8(bytes) {
        Ok(text) => text,
        Err(error) => {
            let before = &bytes[..error.valid_up_to()];
            let line = before.split(|&b| b == b'\n').count();
            fail_with(|out| {
                write!(out, "error[R0013]: line {line} of \"")?;
                trendy_text::escape(path, out)?;
                out.write_str("\" isn't UTF-8 text\n")
            })
        }
    }
}

/// Store at `dst` the array of the records of the CSV file at `path`, in
/// the file's order, after the first if `header` is true, a header, of the
/// type `desc` describes, an array of tuples, in memory the statement
/// owns, and at `lines` the line each record starts on, an array of
/// `usize`s, in memory the statement owns too. Each attribute of the
/// tuples is the field in the column that `columns`, a tuple
/// `columns_desc` describes, gives it, counting from 1: text as it is, and
/// an integer, an approximate number or a boolean as `parse_integer`,
/// `parse_approximate` and `parse_boolean` read one. A value of a type of
/// the program's own is read as its components, and a tuple, of the
/// components of another possible representation of one, as its
/// attributes, whose columns `columns` gives as a tuple of them; the caller
/// converts those, and checks the type's constraints. Columns it
/// doesn't give are skipped. Stops the program if a column isn't one of
/// the file's, or two attributes or components have one (R0016), the file
/// can't be read (R0012), isn't UTF-8 text (R0013) or CSV (R0015), a field
/// isn't a value of its type (R0014), or the memory can't be allocated.
///
/// # Safety
///
/// `path` must point to a valid text value, `header` to a valid boolean,
/// `columns` to a valid tuple and `columns_desc` to its type's descriptor,
/// of the attributes of the tuples `desc` describes, each an integer, or a
/// tuple of the same shape for the components of a value of a type of the
/// program's own, or the attributes of a tuple, and `desc` to a valid
/// descriptor of an array of tuples whose attributes, and those values'
/// components, are text, integers, approximate numbers, booleans or such
/// values, and `dst` and `lines` to
/// memory for an array. The statement must have begun a scope.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_load_csv(
    dst: *mut Text,
    path: *const Text,
    header: *const u8,
    columns: *const u8,
    columns_desc: *const TypeDesc,
    lines: *mut Text,
    desc: *const TypeDesc,
) {
    // SAFETY: guaranteed by the caller.
    unsafe {
        let path = read_text(path.cast());
        let header = *header != 0;
        let desc = &*desc;
        let element = desc.element();
        // Each scalar read, and its column, as `columns` gives them.
        let count_leaves = scalars(element);
        let leaves_memory = allocate::<Leaf>(count_leaves);
        let mut next = 0;
        gather(
            element,
            0,
            columns,
            &*columns_desc,
            leaves_memory,
            &mut next,
            element,
        );
        let leaves = array(leaves_memory, count_leaves);
        let named = |leaf| LeafName {
            desc: element,
            leaf,
        };
        distinct_columns(leaves, element);
        let (contents, len) = read_file(path);
        let text = file_text(path, array(contents, len));
        let Records { count, fields } = csv_records(text, path, header);
        if let Some(i) = leaves.iter().position(|l| l.column >= fields)
            && fields > 0
        {
            columns_fail(|out| {
                write!(
                    out,
                    "`{}` is read from column {}, but \"",
                    named(i),
                    leaves[i].column.saturating_add(1)
                )?;
                trendy_text::escape(path, out)?;
                let plural = if fields == 1 { "" } else { "s" };
                write!(out, "\" has {fields} column{plural}")
            });
        }
        let Some(size) = count.checked_mul(element.size) else {
            fail(Failure::OutOfMemory)
        };
        let rows = allocate_buffer(size, desc);
        let Some(lines_size) = count.checked_mul(size_of::<usize>()) else {
            fail(Failure::OutOfMemory)
        };
        let starts = allocate_buffer(lines_size, core::ptr::null());
        // Each record's tuple, made here, then copied into the array.
        let tuple = allocate::<u8>(element.size);
        // Every field after the header's, if there is one, as
        // `csv_records` found the file CSV.
        let mut all = csv::Fields::new(text).flatten();
        if header {
            all.by_ref().find(|f| f.last);
        }
        let (mut i, mut j, mut first, mut start) = (0, 0, !header, 0);
        for field in all {
            if j == 0 {
                start = field.line;
            }
            if let Some(k) = leaves.iter().position(|l| l.column == j) {
                let leaf = leaves[k];
                let place = Place {
                    attribute: named(k),
                    column: j,
                    line: field.line,
                    path,
                    header: header.then_some(text),
                    first,
                };
                read_field(field, tuple.add(leaf.offset), &*leaf.desc, place);
            }
            j += 1;
            if field.last {
                copy_element(rows, rows.add(i * element.size), tuple, element);
                starts
                    .add(i * size_of::<usize>())
                    .cast::<usize>()
                    .write_unaligned(start);
                i += 1;
                j = 0;
                first = false;
            }
        }
        free(tuple.cast());
        free(leaves_memory.cast());
        free(contents.cast());
        dst.write_unaligned(Text {
            bytes: rows,
            len: count,
        });
        lines.write_unaligned(Text {
            bytes: starts,
            len: count,
        });
    }
}

/// Stop the program if two of `leaves`, the scalars the records' tuples,
/// which `element` describes, are read as, have one column (R0016).
#[cfg(not(test))]
fn distinct_columns(leaves: &[Leaf], element: &TypeDesc) {
    let named = |leaf| LeafName {
        desc: element,
        leaf,
    };
    for (i, leaf) in leaves.iter().enumerate() {
        if let Some(first) = leaves[..i].iter().position(|l| l.column == leaf.column) {
            columns_fail(|out| {
                write!(
                    out,
                    "`{}` and `{}` are both read from column {}",
                    named(first),
                    named(i),
                    leaf.column + 1
                )
            });
        }
    }
}

/// A scalar a CSV file's field is read into: an attribute of the records'
/// tuples, or a component of a value of a type of the program's own in
/// one, at `offset` in the tuple, of the type `desc` describes, from the
/// column `column`, counting from 0.
#[cfg(not(test))]
#[derive(Clone, Copy)]
struct Leaf {
    offset: usize,
    desc: *const TypeDesc,
    column: usize,
}

/// Whether a value of the type `desc` describes, in a CSV file's record, is
/// read from several fields, each a scalar inside it: a value of a type of
/// the program's own, from its components, or a tuple, from the components
/// of another of its possible representations than the stored one, which
/// compiled code converts.
#[cfg(not(test))]
fn read_from_fields(desc: &TypeDesc) -> bool {
    desc.kind == KIND_DEFINED || desc.kind == KIND_TUPLE
}

/// How many scalars a tuple, or a value of a type of the program's own,
/// that `desc` describes is read as: one for each attribute or component,
/// and as many as its scalars are for a value read from several fields
/// (see [`read_from_fields`]).
///
/// # Safety
///
/// `desc` must be a valid descriptor of such a value.
#[cfg(not(test))]
unsafe fn scalars(desc: &TypeDesc) -> usize {
    // SAFETY: guaranteed by the caller.
    unsafe {
        attributes(desc)
            .iter()
            .map(|attr| {
                let inner = &*attr.desc;
                if read_from_fields(inner) {
                    scalars(inner)
                } else {
                    1
                }
            })
            .sum()
    }
}

/// Write at `leaves`, from the `next`th on, the scalars a tuple, or a value
/// of a type of the program's own, that `values` describes is read as, at
/// `base` in the records' tuples, which `element` describes, each with the
/// column `columns` gives it: a tuple `columns_desc` describes, of the
/// same shape, whose attributes are integers counting from 1, or such
/// tuples for the scalars of a value read from several fields (see
/// [`read_from_fields`]). Stops the program if a position is below 1
/// (R0016).
///
/// # Safety
///
/// `values` and `columns_desc` must be valid descriptors of that shape,
/// `columns` must point to a value of the latter, and `leaves` to room for
/// the scalars.
#[cfg(not(test))]
unsafe fn gather(
    values: &TypeDesc,
    base: usize,
    columns: *const u8,
    columns_desc: &TypeDesc,
    leaves: *mut Leaf,
    next: &mut usize,
    element: &TypeDesc,
) {
    // SAFETY: guaranteed by the caller.
    unsafe {
        for (attr, position) in attributes(values).iter().zip(attributes(columns_desc)) {
            let inner = &*attr.desc;
            let at = columns.add(position.offset);
            if read_from_fields(inner) {
                gather(
                    inner,
                    base + attr.offset,
                    at,
                    &*position.desc,
                    leaves,
                    next,
                    element,
                );
                continue;
            }
            let n = at.cast::<i64>().read_unaligned();
            if n < 1 {
                let name = LeafName {
                    desc: element,
                    leaf: *next,
                };
                columns_fail(|out| {
                    write!(
                        out,
                        "`{name}` is read from column {n}, but columns count from 1"
                    )
                });
            }
            leaves.add(*next).write(Leaf {
                offset: base + attr.offset,
                desc: attr.desc,
                column: usize::try_from(n - 1).unwrap_or(usize::MAX),
            });
            *next += 1;
        }
    }
}

/// The name of the `leaf`th scalar a CSV file's records' tuples, which
/// `desc` describes, are read as, counting as [`gather`] does: an
/// attribute's, or a component's after its value's, as `t.c` reads it.
#[cfg(not(test))]
#[derive(Clone, Copy)]
struct LeafName<'a> {
    desc: &'a TypeDesc,
    leaf: usize,
}

#[cfg(not(test))]
impl fmt::Display for LeafName<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (mut desc, mut leaf, mut dot) = (self.desc, self.leaf, "");
        // SAFETY: `trendy_rt_load_csv`'s caller gives valid descriptors.
        unsafe {
            'values: loop {
                for attr in attributes(desc) {
                    let inner = &*attr.desc;
                    let count = if read_from_fields(inner) {
                        scalars(inner)
                    } else {
                        1
                    };
                    if leaf < count {
                        write!(f, "{dot}{}", name(attr))?;
                        if !read_from_fields(inner) {
                            return Ok(());
                        }
                        (desc, dot) = (inner, ".");
                        continue 'values;
                    }
                    leaf -= count;
                }
                unreachable!("a leaf is one of the tuples' scalars");
            }
        }
    }
}

/// Stop the program because `columns` doesn't give each attribute a
/// column of the file, of its own, as `write` says (R0016).
#[cfg(not(test))]
fn columns_fail(write: impl FnOnce(&mut Stderr) -> fmt::Result) -> ! {
    fail_with(|out| {
        out.write_str("error[R0016]: ")?;
        write(out)?;
        out.write_char('\n')
    })
}

/// A CSV file's records to load, and how many fields each has.
#[cfg(not(test))]
struct Records {
    count: usize,
    /// The fields of each record, the first's, or 0 if there are none.
    fields: usize,
}

/// The records of the CSV text `text`, of the file at `path`, after its
/// first, a header, if `header` is true. Stops the program if it isn't
/// CSV, or a record has another number of fields than the first
/// (R0015).
#[cfg(not(test))]
fn csv_records(text: &str, path: &str, header: bool) -> Records {
    let (mut records, mut fields, mut line) = (0_usize, 0, 0);
    // The first record's fields and line.
    let mut first: Option<(usize, usize)> = None;
    for field in csv::Fields::new(text) {
        let field = field.unwrap_or_else(|csv::NotCsv { why, line }| {
            malformed(path, line, |out| {
                out.write_str(match why {
                    csv::Malformed::StrayQuote => "has a quote in a field that isn't quoted",
                    csv::Malformed::AfterQuote => {
                        "has more after a quoted field's closing quote than a comma or a line end"
                    }
                    csv::Malformed::Unclosed => "starts a quoted field that isn't closed",
                })
            })
        });
        if fields == 0 {
            line = field.line;
        }
        fields += 1;
        if !field.last {
            continue;
        }
        match first {
            None => first = Some((fields, line)),
            Some((expected, _)) if expected == fields => {}
            Some((expected, first_line)) => malformed(path, line, |out| {
                let plural = if fields == 1 { "" } else { "s" };
                write!(out, "has {fields} field{plural}, but ")?;
                if header {
                    write!(out, "the header has {expected}")
                } else {
                    write!(out, "line {first_line} has {expected}")
                }
            }),
        }
        records += 1;
        fields = 0;
    }
    Records {
        count: if header {
            records.saturating_sub(1)
        } else {
            records
        },
        fields: first.map_or(0, |(fields, _)| fields),
    }
}

/// Stop the program because the CSV file at `path` isn't CSV on line
/// `line`, as `write` says (R0015).
#[cfg(not(test))]
fn malformed(path: &str, line: usize, write: impl FnOnce(&mut Stderr) -> fmt::Result) -> ! {
    fail_with(|out| {
        write!(out, "error[R0015]: line {line} of \"")?;
        trendy_text::escape(path, out)?;
        out.write_str("\" ")?;
        write(out)?;
        out.write_char('\n')
    })
}

/// The value of the CSV field `field`: its text as written, or, with its
/// doubled quotes made one, a copy in memory the statement's scope holds.
///
/// # Safety
///
/// The statement must have begun a scope, which holds the copy until it
/// ends, as long as the value is used.
#[cfg(not(test))]
unsafe fn field_value(field: csv::Field<'_>) -> &str {
    if let Some(written) = field.as_written() {
        return written;
    }
    let len = field.len();
    let bytes = allocate_buffer(len, core::ptr::null());
    // SAFETY: the buffer has room for the value's bytes, and they are the
    // field's UTF-8, with some quotes left out.
    unsafe {
        let value = core::slice::from_raw_parts_mut(bytes, len);
        field.copy_to(value);
        core::str::from_utf8_unchecked(value)
    }
}

/// Write at `dst` the value of the CSV field `field`, at `place`, of the
/// type `desc` describes: text, an integer, an approximate number or a
/// boolean. Stops the program if it isn't one of that type (R0014).
///
/// # Safety
///
/// `dst` must point to memory for a value of that type. The statement must
/// have begun a scope, which holds a text's bytes.
#[cfg(not(test))]
unsafe fn read_field(field: csv::Field, dst: *mut u8, desc: &TypeDesc, place: Place) {
    // SAFETY: guaranteed by the caller.
    unsafe {
        let value = field_value(field);
        let failed = |a, why| unparsable(value, a, why, Some(place));
        match desc.kind {
            KIND_TEXT => {
                let bytes = allocate_buffer(value.len(), core::ptr::null());
                core::ptr::copy_nonoverlapping(value.as_ptr(), bytes, value.len());
                dst.cast::<Text>().write_unaligned(Text {
                    bytes,
                    len: value.len(),
                });
            }
            KIND_INTEGER => match trendy_numeric::parse_integer(value) {
                Ok(n) => dst.cast::<i64>().write_unaligned(n),
                Err(why) => failed("an integer", why),
            },
            KIND_APPROXIMATE => match trendy_numeric::parse_approximate(value) {
                Ok(x) => dst.cast::<f64>().write_unaligned(x),
                Err(why) => failed("an approximate number", why),
            },
            KIND_BOOLEAN => match trendy_text::parse_boolean(value) {
                Ok(b) => dst.write(u8::from(b)),
                Err(why) => failed("a boolean", why),
            },
            kind => unreachable!("`load_csv` reads no values of kind {kind}"),
        }
    }
}

/// The bytes of the file at `path`, and their number, in memory from
/// `malloc`, which the caller frees. Stops the program if the file can't
/// be read (R0012), or the memory can't be allocated.
#[cfg(not(test))]
fn read_file(path: &str) -> (*mut u8, usize) {
    // `open` takes the path as a C string, which ends at the first NUL.
    if path.contains('\0') {
        cant_read(path, "a path can't contain the character U+0000");
    }
    let Some(terminated) = path.len().checked_add(1) else {
        fail(Failure::OutOfMemory)
    };
    // SAFETY: the memory is for the path's bytes and a NUL, read by `open`
    // and then freed; the descriptor is read, then closed, and `read`
    // writes at most the room left in the memory.
    unsafe {
        let c_path = allocate::<u8>(terminated);
        core::ptr::copy_nonoverlapping(path.as_ptr(), c_path, path.len());
        *c_path.add(path.len()) = 0;
        let fd = open(c_path, O_RDONLY);
        free(c_path.cast());
        if fd < 0 {
            cant_read(path, reason());
        }
        let mut capacity = 4096;
        let mut contents = allocate::<u8>(capacity);
        let mut len = 0;
        loop {
            if len == capacity {
                let Some(grown) = capacity.checked_mul(2) else {
                    fail(Failure::OutOfMemory)
                };
                contents = realloc(contents.cast(), grown).cast();
                if contents.is_null() {
                    fail(Failure::OutOfMemory);
                }
                capacity = grown;
            }
            let Ok(n) = usize::try_from(read(fd, contents.add(len), capacity - len)) else {
                cant_read(path, reason());
            };
            if n == 0 {
                break;
            }
            len += n;
        }
        close(fd);
        (contents, len)
    }
}

/// Stop the program because the file at `path` can't be read, for
/// `reason`.
#[cfg(not(test))]
fn cant_read(path: &str, reason: &str) -> ! {
    fail_with(|out| {
        out.write_str("error[R0012]: can't read \"")?;
        trendy_text::escape(path, out)?;
        writeln!(out, "\": {reason}")
    })
}

/// The system's description of why the last call of the C library that
/// failed did, such as "No such file or directory".
#[cfg(not(test))]
fn reason() -> &'static str {
    // SAFETY: `errno_location` is the calling thread's `errno`, and
    // `strerror` returns a C string, which no other call here replaces.
    unsafe {
        let description = core::ffi::CStr::from_ptr(strerror(*errno_location()));
        description.to_str().unwrap_or("an unknown error")
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
/// attributes in name order, relations with their heading first, and an
/// empty array with its elements' type, so the output is a literal for the
/// same value. An attribute without a name,
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
            // As its own type's.
            KIND_UNION => {
                let (own, components) = own_type(value, desc);
                write_value(out, components, own)
            }
            // Its selector's invocation, as a call is written.
            KIND_DEFINED => {
                out.write_str(type_name(desc))?;
                out.write_char('{')?;
                for (i, attr) in attributes(desc).iter().enumerate() {
                    if i > 0 {
                        out.write_str(", ")?;
                    }
                    write!(out, "{}: ", name(attr))?;
                    write_value(out, value.add(attr.offset), &*attr.desc)?;
                }
                out.write_char('}')
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
                    write_tuple(out, tuples.add(i * desc.element().size), desc)?;
                }
                out.write_str(" }")
            }
            KIND_ARRAY => {
                let (elements, count) = read_slice(value);
                let element = desc.element();
                if count == 0 {
                    out.write_str("array ")?;
                    write_type(out, element)?;
                    return out.write_str(" []");
                }
                out.write_str("array [")?;
                for i in 0..count {
                    if i > 0 {
                        out.write_str(", ")?;
                    }
                    write_value(out, elements.add(i * element.size), element)?;
                }
                out.write_char(']')
            }
            _ => Err(fmt::Error),
        }
    }
}

/// Write the relation at `value` as a table: its attributes' names, in name
/// order, then a line for each tuple, in the relation's order, with
/// borders of box-drawing characters. A cell holds its value as
/// [`write_value`] writes it, but a text without quotes, as
/// [`trendy_text::escape_bare`] writes it. A column is as wide as its
/// widest cell, in a terminal's columns, with numbers to the right and
/// anything else to the left. The attributes without a name, which the
/// source can't name, are one column, last, of `…`, as [`write_value`]
/// writes them.
///
/// # Safety
///
/// As for [`write_value`], with `desc` describing a relation, and `widths`
/// with room for a width of each of its attributes.
///
/// # Errors
///
/// Returns an error if `out` does.
pub unsafe fn write_table<W: Write>(
    out: &mut W,
    value: *const u8,
    desc: &TypeDesc,
    widths: &mut [usize],
) -> fmt::Result {
    // SAFETY, throughout: guaranteed by the caller; each tuple is the
    // element's size apart, and each attribute at its offset in it.
    unsafe {
        let attributes = attributes(desc);
        let (tuples, count) = read_slice(value);
        let size = desc.element().size;
        let tuple = |i: usize| tuples.add(i * size);
        for (attr, width) in attributes.iter().zip(widths.iter_mut()) {
            if name(attr).is_empty() {
                continue;
            }
            let mut widest = Measured(0);
            widest.write_str(name(attr))?;
            for i in 0..count {
                let mut measured = Measured(0);
                write_cell(&mut measured, tuple(i), Some(attr))?;
                widest.0 = widest.0.max(measured.0);
            }
            *width = widest.0;
        }
        let widths = &*widths;
        border(out, columns(attributes, widths), ('┌', '┬', '┐'))?;
        row(out, columns(attributes, widths), |out, attr, width| {
            let mut measured = Measured(0);
            let heading = attr.map_or("…", |a| name(a));
            measured.write_str(heading)?;
            out.write_str(heading)?;
            pad(out, width, measured.0)
        })?;
        border(out, columns(attributes, widths), ('├', '┼', '┤'))?;
        for i in 0..count {
            row(out, columns(attributes, widths), |out, attr, width| {
                let mut measured = Measured(0);
                write_cell(&mut measured, tuple(i), attr)?;
                let number =
                    attr.is_some_and(|a| matches!((*a.desc).kind, KIND_INTEGER | KIND_APPROXIMATE));
                if number {
                    pad(out, width, measured.0)?;
                    write_cell(out, tuple(i), attr)
                } else {
                    write_cell(out, tuple(i), attr)?;
                    pad(out, width, measured.0)
                }
            })?;
        }
        border(out, columns(attributes, widths), ('└', '┴', '┘'))
    }
}

/// A table's columns: the named attributes among `attributes`, each with
/// its width in `widths`, then, if there are any without a name, one
/// column of `…` for them, `None`.
///
/// # Safety
///
/// The attributes must be valid.
unsafe fn columns<'a>(
    attributes: &'a [Attr],
    widths: &'a [usize],
) -> impl Iterator<Item = (Option<&'a Attr>, usize)> {
    // SAFETY: guaranteed by the caller.
    let named = |attr: &Attr| unsafe { !name(attr).is_empty() };
    let unnamed = attributes.iter().any(|attr| !named(attr));
    attributes
        .iter()
        .zip(widths)
        .filter(move |(attr, _)| named(attr))
        .map(|(attr, &width)| (Some(attr), width))
        .chain(unnamed.then_some((None, "…".width())))
}

/// A table's border, across `columns`, from `left` to `right`, with
/// `middle` between two columns.
fn border<'a>(
    out: &mut impl Write,
    columns: impl Iterator<Item = (Option<&'a Attr>, usize)>,
    (left, middle, right): (char, char, char),
) -> fmt::Result {
    out.write_char(left)?;
    for (j, (_, width)) in columns.enumerate() {
        if j > 0 {
            out.write_char(middle)?;
        }
        for _ in 0..width + 2 {
            out.write_char('─')?;
        }
    }
    out.write_char(right)?;
    out.write_char('\n')
}

/// A table's line across `columns`, each cell written `width` wide by
/// `cell`, between bars.
fn row<'a, W: Write>(
    out: &mut W,
    columns: impl Iterator<Item = (Option<&'a Attr>, usize)>,
    mut cell: impl FnMut(&mut W, Option<&'a Attr>, usize) -> fmt::Result,
) -> fmt::Result {
    out.write_char('│')?;
    for (j, (attr, width)) in columns.enumerate() {
        if j > 0 {
            out.write_char('│')?;
        }
        out.write_char(' ')?;
        cell(out, attr, width)?;
        out.write_char(' ')?;
    }
    out.write_char('│')?;
    out.write_char('\n')
}

/// Spaces after `written` columns, up to `width`.
fn pad(out: &mut impl Write, width: usize, written: usize) -> fmt::Result {
    for _ in written..width {
        out.write_char(' ')?;
    }
    Ok(())
}

/// The cell of the attribute `attr` of the tuple at `tuple`, as
/// [`write_table`] writes it, or `…`, of the attributes without a name.
///
/// # Safety
///
/// As for [`write_value`], with `attr` an attribute of the tuple's type.
unsafe fn write_cell(out: &mut impl Write, tuple: *const u8, attr: Option<&Attr>) -> fmt::Result {
    let Some(attr) = attr else {
        return out.write_char('…');
    };
    // SAFETY: guaranteed by the caller.
    unsafe {
        let (value, desc) = (tuple.add(attr.offset), &*attr.desc);
        if desc.kind == KIND_TEXT {
            trendy_text::escape_bare(read_text(value), out)
        } else {
            write_value(out, value, desc)
        }
    }
}

/// Counts the columns of a terminal that what is written to it takes, by
/// Unicode's rules for East Asian width (UAX #11): two for a wide
/// character, such as `李`, and none for a combining one.
struct Measured(usize);

impl Write for Measured {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.0 = self.0.saturating_add(s.width());
        Ok(())
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
        // SAFETY: guaranteed by the caller.
        KIND_DEFINED | KIND_UNION => out.write_str(unsafe { type_name(desc) }),
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
        KIND_ARRAY => {
            out.write_str("array ")?;
            // SAFETY: guaranteed by the caller.
            unsafe { write_type(out, desc.element()) }
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

/// The descriptor of the own type of the value at `value`, of the type in
/// a hierarchy `desc` describes, and where its components are.
///
/// # Safety
///
/// `value` must point to a valid value of that type, and `desc` must be a
/// valid `KIND_UNION` descriptor.
unsafe fn own_type<'d>(value: *const u8, desc: &TypeDesc) -> (&'d TypeDesc, *const u8) {
    // SAFETY: guaranteed by the caller.
    unsafe {
        let own = &*value.cast::<*const TypeDesc>().read_unaligned();
        (own, value.add(attributes(desc)[0].offset))
    }
}

/// Whether two own types' descriptors, `KIND_DEFINED` ones, describe the
/// same type, whose name tells it.
///
/// # Safety
///
/// Both must be valid descriptors.
unsafe fn same_type(a: &TypeDesc, b: &TypeDesc) -> bool {
    // SAFETY: guaranteed by the caller.
    core::ptr::eq(a, b) || unsafe { type_name(a) == type_name(b) }
}

/// The name of the type of the program's own that `desc` describes.
///
/// # Safety
///
/// `desc` must be a valid descriptor of such a type, or of a type in a
/// hierarchy.
unsafe fn type_name(desc: &TypeDesc) -> &str {
    // SAFETY: the compiler emits names as valid UTF-8.
    unsafe { core::str::from_utf8_unchecked(core::slice::from_raw_parts(desc.name, desc.name_len)) }
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

/// A pointer followed by a length, as in `text`, relation and array
/// values.
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

/// `open`'s flag to open a file for reading only, 0 on every system.
#[cfg(not(test))]
const O_RDONLY: i32 = 0;

unsafe extern "C" {
    fn write(fd: i32, buf: *const u8, count: usize) -> isize;
    #[cfg(not(test))]
    fn open(path: *const u8, flags: i32, ...) -> i32;
    #[cfg(not(test))]
    fn read(fd: i32, buf: *mut u8, count: usize) -> isize;
    #[cfg(not(test))]
    fn close(fd: i32) -> i32;
    #[cfg(not(test))]
    fn strerror(errnum: i32) -> *const core::ffi::c_char;
    /// The address of the calling thread's `errno`, which C reaches
    /// through a macro, under each system's own name.
    #[cfg(not(test))]
    #[cfg_attr(target_vendor = "apple", link_name = "__error")]
    #[cfg_attr(target_os = "linux", link_name = "__errno_location")]
    fn errno_location() -> *mut i32;
    #[cfg(not(test))]
    fn abort() -> !;
    #[cfg(not(test))]
    fn exit(status: i32) -> !;
    #[cfg(not(test))]
    fn malloc(size: usize) -> *mut core::ffi::c_void;
    #[cfg(not(test))]
    fn free(pointer: *mut core::ffi::c_void);
    #[cfg(not(test))]
    fn realloc(pointer: *mut core::ffi::c_void, size: usize) -> *mut core::ffi::c_void;
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
            element: core::ptr::null(),
            name: core::ptr::null(),
            name_len: 0,
        }
    }

    /// A tuple of `size` bytes with `attributes`.
    fn tuple_of(attributes: &[Attr], size: usize) -> TypeDesc {
        TypeDesc {
            kind: KIND_TUPLE,
            size,
            attributes: attributes.as_ptr(),
            count: attributes.len(),
            element: core::ptr::null(),
            name: core::ptr::null(),
            name_len: 0,
        }
    }

    /// A type of the program's own named `name`, with the components
    /// `attributes`, of `size` bytes.
    fn defined_of(name: &'static str, attributes: &[Attr], size: usize) -> TypeDesc {
        TypeDesc {
            kind: KIND_DEFINED,
            size,
            attributes: attributes.as_ptr(),
            count: attributes.len(),
            element: core::ptr::null(),
            name: name.as_ptr(),
            name_len: name.len(),
        }
    }

    /// A relation whose tuples are `tuple`s.
    fn relation_of(tuple: &TypeDesc) -> TypeDesc {
        TypeDesc {
            kind: KIND_RELATION,
            size: 16,
            attributes: tuple.attributes,
            count: tuple.count,
            element: tuple,
            name: core::ptr::null(),
            name_len: 0,
        }
    }

    /// An array whose elements are `element`s.
    fn array_of(element: &TypeDesc) -> TypeDesc {
        TypeDesc {
            kind: KIND_ARRAY,
            size: 16,
            attributes: core::ptr::null(),
            count: 0,
            element,
            name: core::ptr::null(),
            name_len: 0,
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
    fn scalars() {
        assert_eq!(print([1_u8].as_ptr(), &scalar(KIND_BOOLEAN, 1)), "true");
        assert_eq!(
            print((-42_i64).to_ne_bytes().as_ptr(), &scalar(KIND_INTEGER, 8)),
            "-42"
        );
        let approximate = |r: f64| print(r.to_ne_bytes().as_ptr(), &scalar(KIND_APPROXIMATE, 8));
        assert_eq!(approximate(0.0), "0e0");
        assert_eq!(approximate(1.5e-3), "1.5e-3");
        assert_eq!(approximate(-1.5e-3), "-1.5e-3");
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
        assert!(!same(1.5, -1.5));
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
        let tuple = tuple_of(&heading, 16);
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
        // array integer: element by element, and as long.
        let integers = array_of(&integer);
        let same_array = |x: &[i64], y: &[i64]| unsafe {
            let (x, y) = (
                [x.as_ptr() as usize, x.len()],
                [y.as_ptr() as usize, y.len()],
            );
            equal(x.as_ptr().cast(), y.as_ptr().cast(), &integers)
        };
        assert!(same_array(&[1, 2], &[1, 2]));
        assert!(!same_array(&[1, 2], &[2, 1]), "in order");
        assert!(!same_array(&[1, 2], &[1, 2, 3]));
        assert!(same_array(&[], &[]));
    }

    #[test]
    fn arrays_are_ordered_element_by_element_a_prefix_first() {
        let integer = scalar(KIND_INTEGER, 8);
        let integers = array_of(&integer);
        let ordering = |x: &[i64], y: &[i64]| unsafe {
            let (x, y) = (
                [x.as_ptr() as usize, x.len()],
                [y.as_ptr() as usize, y.len()],
            );
            order(x.as_ptr().cast(), y.as_ptr().cast(), &integers)
        };
        assert!(ordering(&[1, 9], &[2]).is_lt());
        assert!(ordering(&[1], &[1, 0]).is_lt());
        assert!(ordering(&[], &[0]).is_lt());
        assert!(ordering(&[3, 1], &[3, 1]).is_eq());
    }

    #[test]
    fn arrays_are_written_as_selectors() {
        let integer = scalar(KIND_INTEGER, 8);
        let integers = array_of(&integer);
        let rainfall: [i64; 3] = [1200, 950, 220];
        let value: [usize; 2] = [rainfall.as_ptr() as usize, 3];
        assert_eq!(
            print(value.as_ptr().cast(), &integers),
            "array [1200, 950, 220]"
        );
        let empty: [usize; 2] = [rainfall.as_ptr() as usize, 0];
        assert_eq!(print(empty.as_ptr().cast(), &integers), "array integer []");
        // array array integer, of none: its elements' type in full.
        let nested = array_of(&integers);
        assert_eq!(
            print(empty.as_ptr().cast(), &nested),
            "array array integer []"
        );
        let rows: [[usize; 2]; 2] = [value, empty];
        let value: [usize; 2] = [rows.as_ptr() as usize, 2];
        assert_eq!(
            print(value.as_ptr().cast(), &nested),
            "array [array [1200, 950, 220], array integer []]"
        );
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
        let tuple = tuple_of(&heading, 16);
        let one: [i64; 2] = [0, 25];
        assert_eq!(
            print(one.as_ptr().cast(), &tuple),
            "tuple { a: false, b: 25 }"
        );

        let relation = relation_of(&tuple);
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
    fn a_programs_own_types_values_are_written_as_their_selectors_invocations() {
        let approximate = scalar(KIND_APPROXIMATE, 8);
        // type position { { latitude: approximate, longitude: approximate } }
        let components = [
            attr("latitude", 0, &approximate),
            attr("longitude", 8, &approximate),
        ];
        let position = defined_of("position", &components, 16);
        let nairobi: [f64; 2] = [-1.29, 36.82];
        assert_eq!(
            print(nairobi.as_ptr().cast(), &position),
            "position{latitude: -1.29e0, longitude: 3.682e1}"
        );
        // Of no components, and inside a tuple, whose type it names.
        let unit = defined_of("unit", &[], 0);
        assert_eq!(print([0_u8; 0].as_ptr(), &unit), "unit{}");
        let heading = [attr("at", 0, &position)];
        let tuple = tuple_of(&heading, 16);
        assert_eq!(
            print(nairobi.as_ptr().cast(), &tuple),
            "tuple { at: position{latitude: -1.29e0, longitude: 3.682e1} }"
        );
        let empty: [usize; 2] = [8, 0];
        assert_eq!(
            print(empty.as_ptr().cast(), &array_of(&position)),
            "array position []"
        );
        // Equal when their components are.
        let lima: [f64; 2] = [-12.05, -77.04];
        // SAFETY: both are values of the type.
        let same = |a: &[f64; 2], b: &[f64; 2]| unsafe {
            equal(a.as_ptr().cast(), b.as_ptr().cast(), &position)
        };
        assert!(same(&nairobi, &[-1.29, 36.82]));
        assert!(!same(&nairobi, &lima));
    }

    #[test]
    fn unnamed_attributes_are_written_last_as_an_ellipsis() {
        let integer = scalar(KIND_INTEGER, 8);
        // tuple { "": integer, b: integer }, as a `per` relation's tuples
        // wrap their own attributes in one the source can't name.
        let heading = [attr("", 0, &integer), attr("b", 8, &integer)];
        let tuple = tuple_of(&heading, 16);
        let relation = relation_of(&tuple);
        let tuples: [i64; 4] = [1, 7, 2, 7];
        let value: [usize; 2] = [tuples.as_ptr() as usize, 2];
        assert_eq!(
            print(value.as_ptr().cast(), &relation),
            "relation { b: integer, … } { { b: 7, … }, { b: 7, … } }"
        );
        let alone = tuple_of(&heading[..1], 8);
        assert_eq!(print(tuples.as_ptr().cast(), &alone), "tuple { … }");
    }

    /// The relation as `debug_table` writes it.
    fn table(value: *const u8, desc: &TypeDesc) -> String {
        let mut written = String::new();
        let mut widths = vec![0; desc.count];
        // SAFETY: each test builds a value matching its descriptor.
        unsafe { write_table(&mut written, value, desc, &mut widths) }.unwrap();
        written
    }

    /// A `text` value of `text`, which must outlive it.
    fn text_value(text: &str) -> [usize; 2] {
        [text.as_ptr() as usize, text.len()]
    }

    #[test]
    fn tables() {
        let integer = scalar(KIND_INTEGER, 8);
        let text = scalar(KIND_TEXT, 16);
        let boolean = scalar(KIND_BOOLEAN, 1);
        let inner_tuple = tuple_of(&[], 0);
        // relation { city: text, flag: tuple {}, people: integer, seen: boolean }
        let heading = [
            attr("city", 0, &text),
            attr("flag", 16, &inner_tuple),
            attr("people", 16, &integer),
            attr("seen", 24, &boolean),
        ];
        let relation = relation_of(&tuple_of(&heading, 32));
        let (lima, xian, quoted) = ("Lima", "西安", "\"Q\"\n");
        let row = |city: [usize; 2], people: i64, seen: bool| -> [usize; 4] {
            [
                city[0],
                city[1],
                usize::from_ne_bytes(people.to_ne_bytes()),
                usize::from(seen),
            ]
        };
        let tuples = [
            row(text_value(lima), 10_000_000, true),
            row(text_value(xian), 13, false),
            row(text_value(quoted), -1, false),
        ];
        let value: [usize; 2] = [tuples.as_ptr() as usize, 3];
        // Numbers to the right; wide characters take two columns; a
        // text's control characters are escaped, and its quotes kept.
        assert_eq!(
            table(value.as_ptr().cast(), &relation),
            "\
┌───────┬──────────┬──────────┬───────┐
│ city  │ flag     │ people   │ seen  │
├───────┼──────────┼──────────┼───────┤
│ Lima  │ tuple {} │ 10000000 │ true  │
│ 西安  │ tuple {} │       13 │ false │
│ \"Q\"\\n │ tuple {} │       -1 │ false │
└───────┴──────────┴──────────┴───────┘
"
        );
        let empty: [usize; 2] = [tuples.as_ptr() as usize, 0];
        assert_eq!(
            table(empty.as_ptr().cast(), &relation),
            "\
┌──────┬──────┬────────┬──────┐
│ city │ flag │ people │ seen │
├──────┼──────┼────────┼──────┤
└──────┴──────┴────────┴──────┘
"
        );
    }

    #[test]
    fn tables_of_no_attributes() {
        let relation = relation_of(&tuple_of(&[], 0));
        let tuples = [0_u8; 1];
        let dee: [usize; 2] = [tuples.as_ptr() as usize, 1];
        assert_eq!(
            table(dee.as_ptr().cast(), &relation),
            "┌┐\n││\n├┤\n││\n└┘\n"
        );
        let dum: [usize; 2] = [tuples.as_ptr() as usize, 0];
        assert_eq!(table(dum.as_ptr().cast(), &relation), "┌┐\n││\n├┤\n└┘\n");
    }

    #[test]
    fn tables_put_unnamed_attributes_last_as_an_ellipsis() {
        let integer = scalar(KIND_INTEGER, 8);
        let heading = [attr("", 0, &integer), attr("b", 8, &integer)];
        let relation = relation_of(&tuple_of(&heading, 16));
        let tuples: [i64; 2] = [1, 7];
        let value: [usize; 2] = [tuples.as_ptr() as usize, 1];
        assert_eq!(
            table(value.as_ptr().cast(), &relation),
            "┌───┬───┐\n│ b │ … │\n├───┼───┤\n│ 7 │ … │\n└───┴───┘\n"
        );
    }

    #[test]
    fn nested_relation_types_are_written_in_headings() {
        let integer = scalar(KIND_INTEGER, 8);
        let inner_heading = [attr("x", 0, &integer)];
        let inner_tuple = tuple_of(&inner_heading, 8);
        let inner = relation_of(&inner_tuple);
        let outer_heading = [attr("r", 0, &inner)];
        let outer_tuple = tuple_of(&outer_heading, 16);
        let outer = relation_of(&outer_tuple);
        let xs: [i64; 1] = [7];
        let inner_value: [usize; 2] = [xs.as_ptr() as usize, 1];
        let outer_value: [usize; 2] = [inner_value.as_ptr() as usize, 1];
        assert_eq!(
            print(outer_value.as_ptr().cast(), &outer),
            "relation { r: relation { x: integer } } { { r: relation { x: integer } { { x: 7 } } } }"
        );
    }
}
