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
//! longer, however many a loop makes. Scopes nest, as a relational
//! database's memory contexts do: the work on each tuple of a relational
//! operation, and each
//! call of a read-only operator, has a scope of its own, which hands what
//! outlives it to the one it is in ([`trendy_rt_hand_out`]) before it ends,
//! so that an operation over many tuples frees each one's work as it goes.
//!
//! # Failures
//!
//! The runtime never stops the program, but for an internal error. A
//! function that can fail makes its error, a value of the prelude's type
//! `error`, which waits where the compiler's table of errors says
//! (`Failures`, `fail_as`), and returns whether it failed, a `bool`,
//! which compiled code checks as it does a call's. Running out of memory
//! has no error, and writes its message as it happens. Inside, a failure is `Failed`, which a `Fallible`
//! function gives up through its callers. A function that fails changes
//! nothing but what the statement's scope holds, which ends as the
//! statement fails, so a buffer half-built is released with it, and the
//! function's own memory is a `Temp`, freed however it ends.

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
/// One character, laid out as a text of it is.
pub const KIND_CHARACTER: u8 = 9;
/// An integer stored in one byte, as a component of a type of the program's own whose
/// constraints keep it in that range.
pub const KIND_U8: u8 = 10;
/// As [`KIND_U8`], in two bytes.
pub const KIND_U16: u8 = 11;
/// As [`KIND_U8`], in four bytes.
pub const KIND_U32: u8 = 12;

/// The integer stored at `value` in the width of the kind `kind`.
///
/// # Safety
///
/// `value` must point to an integer of that kind.
unsafe fn narrow(value: *const u8, kind: u8) -> u32 {
    // SAFETY: guaranteed by the caller.
    unsafe {
        match kind {
            KIND_U8 => u32::from(*value),
            KIND_U16 => u32::from(value.cast::<u16>().read_unaligned()),
            _ => value.cast::<u32>().read_unaligned(),
        }
    }
}

/// Store at `dst` the code point `point`, as the integer of the kind `kind` it is
/// stored as: [`KIND_INTEGER`] or a narrower one.
///
/// # Safety
///
/// `dst` must point to memory for one.
#[cfg(not(test))]
unsafe fn write_number(dst: *mut u8, kind: u8, point: u32) {
    // SAFETY: guaranteed by the caller.
    unsafe {
        match kind {
            KIND_U32 => dst.cast::<u32>().write_unaligned(point),
            _ => dst.cast::<i64>().write_unaligned(i64::from(point)),
        }
    }
}

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
/// `shl`.
pub const ARITHMETIC_SHIFT_LEFT: u32 = 7;
/// `shr`.
pub const ARITHMETIC_SHIFT_RIGHT: u32 = 8;

/// A `text` value: the address of its UTF-8 bytes, and their number. The
/// compiler lays text out as this pair.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Text {
    pub bytes: *const u8,
    pub len: usize,
}

/// Where `load_csv` read a value, for a failure to write, laid out as the
/// compiler lays it out: the attribute or component it was read for, by
/// name, the file's path, and the record's line, counting from 1.
#[repr(C)]
pub struct Record {
    pub attribute: Text,
    pub path: Text,
    pub line: usize,
}

/// Store `lhs || rhs` at `dst`, in memory the statement owns. Fails if the
/// memory can't be allocated.
///
/// # Safety
///
/// `lhs` and `rhs` must point to valid text values, and `dst` to memory for
/// one.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_concat(
    dst: *mut Text,
    lhs: *const Text,
    rhs: *const Text,
) -> bool {
    // SAFETY: guaranteed by the caller.
    status(|| unsafe {
        let (lhs, rhs) = (lhs.read_unaligned(), rhs.read_unaligned());
        let Some(len) = lhs.len.checked_add(rhs.len) else {
            return Err(out_of_memory());
        };
        let bytes = allocate_buffer(len, core::ptr::null())?;
        core::ptr::copy_nonoverlapping(lhs.bytes, bytes, lhs.len);
        core::ptr::copy_nonoverlapping(rhs.bytes, bytes.add(lhs.len), rhs.len);
        dst.write_unaligned(Text { bytes, len });
        Ok(())
    })
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
            KIND_U8 | KIND_U16 | KIND_U32 => narrow(lhs, desc.kind) == narrow(rhs, desc.kind),
            KIND_TEXT | KIND_CHARACTER => {
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
                if count != other {
                    return false;
                }
                // What is one byte each is compared at once.
                if element.size == 1 && !element.points {
                    return core::slice::from_raw_parts(a, count)
                        == core::slice::from_raw_parts(b, count);
                }
                (0..count).all(|i| {
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
        if desc.integers > 0 {
            return compare_integers(lhs, rhs, desc.integers).is_eq();
        }
        attributes(desc)
            .iter()
            .all(|attr| equal(lhs.add(attr.offset), rhs.add(attr.offset), &*attr.desc))
    }
}

/// Store at `dst` the array of the elements of `lhs` then those of `rhs`, of the type
/// `desc` describes, in memory the statement's scope holds. Fails if the memory can't
/// be allocated.
///
/// # Safety
///
/// `lhs` and `rhs` must point to valid arrays of that type, `desc` to its descriptor,
/// and `dst` to memory for an array. The statement must have begun a scope.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_concat_arrays(
    dst: *mut Text,
    lhs: *const Text,
    rhs: *const Text,
    desc: *const TypeDesc,
) -> bool {
    // SAFETY: guaranteed by the caller; the new buffer has room for both.
    status(|| unsafe {
        let (lhs, rhs) = (lhs.read_unaligned(), rhs.read_unaligned());
        let desc = &*desc;
        let element = desc.element();
        let sizes = lhs
            .len
            .checked_add(rhs.len)
            .and_then(|len| Some((len, len.checked_mul(element.size)?)));
        let Some((len, size)) = sizes else {
            return Err(out_of_memory());
        };
        let bytes = allocate_buffer(size, desc)?;
        let first = lhs.len * element.size;
        core::ptr::copy_nonoverlapping(lhs.bytes, bytes, first);
        core::ptr::copy_nonoverlapping(rhs.bytes, bytes.add(first), rhs.len * element.size);
        adopt(bytes, len, element);
        dst.write_unaligned(Text { bytes, len });
        Ok(())
    })
}

/// Store at `dst` -1, 0 or 1, as `lhs` is before, equal to or after `rhs`, of the type
/// `desc` describes, in the order sorting does.
///
/// # Safety
///
/// `lhs` and `rhs` must point to valid values of that type, `desc` to its descriptor,
/// and `dst` to an `i64`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_compare(
    dst: *mut i64,
    lhs: *const u8,
    rhs: *const u8,
    desc: *const TypeDesc,
) {
    // SAFETY: guaranteed by the caller.
    unsafe {
        let ordering = order(lhs, rhs, &*desc);
        dst.write_unaligned(ordering as i64);
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
    /// How many bytes it has room for, so that a relation's holder can add
    /// tuples to it where nothing else holds it ([`trendy_rt_insert`]).
    pub room: usize,
}

/// [`Header::count`] of a buffer compiled into the program.
pub const IMMORTAL: u64 = u64::MAX;

/// The bytes of a character of one ASCII byte, after a header that says they are
/// never released, as a buffer compiled into a program is. Every such character
/// shares one, so that an array of them takes no memory but its own.
#[repr(C)]
struct AsciiCharacter {
    header: Header,
    byte: u8,
}

// SAFETY: nothing writes to one: its header's `count` is `IMMORTAL`, which no
// reference changes, and its `desc`, the only thing that keeps it from being `Sync`
// by itself, is null.
unsafe impl Sync for AsciiCharacter {}

static ASCII_CHARACTERS: [AsciiCharacter; 128] = {
    let mut characters = [const {
        AsciiCharacter {
            header: Header {
                count: IMMORTAL,
                len: 0,
                desc: core::ptr::null(),
                room: 1,
            },
            byte: 0,
        }
    }; 128];
    let mut byte = 0_u8;
    while byte < 128 {
        characters[byte as usize].byte = byte;
        byte += 1;
    }
    characters
};

/// The text value of `character`, if it is one ASCII byte, whose bytes no statement
/// needs to allocate or hold.
fn ascii_character(character: &str) -> Option<Text> {
    // One byte of UTF-8 is ASCII.
    match *character.as_bytes() {
        [byte] => Some(Text {
            bytes: &raw const ASCII_CHARACTERS[usize::from(byte)].byte,
            len: 1,
        }),
        _ => None,
    }
}

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
/// its arguments while the operator's statements run. Within a statement,
/// the work on one tuple, and a read-only operator's call, has a scope of
/// its own in the statement's, which what outlives it is handed out to
/// ([`trendy_rt_hand_out`]). Compiled code keeps each scope in a stack slot
/// laid out as this struct, which only the runtime reads.
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

/// Store at `dst` the address of the innermost scope, or null if there is
/// none, as a function that begins scopes starts, so that its failure exit
/// can end those it began ([`trendy_rt_end_since`]).
///
/// # Safety
///
/// `dst` must point to memory for an address.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_innermost(dst: *mut *mut Scope) {
    // SAFETY: guaranteed by the caller.
    unsafe { dst.write(CURRENT.load(core::sync::atomic::Ordering::Relaxed)) }
}

/// End, innermost first, as [`trendy_rt_end`] does, every scope begun
/// since the one whose address `saved` holds, which
/// [`trendy_rt_innermost`] stored, was the innermost: those a function
/// that fails began.
///
/// # Safety
///
/// `saved` must point to what [`trendy_rt_innermost`] stored as the
/// function began, every scope it has begun since must still be valid,
/// and those its callees began must have ended.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_end_since(saved: *const *mut Scope) {
    use core::sync::atomic::Ordering::Relaxed;
    // SAFETY: guaranteed by the caller: each scope from the innermost to
    // the one saved is valid, and is the innermost when it ends.
    unsafe {
        let saved = saved.read();
        while CURRENT.load(Relaxed) != saved {
            trendy_rt_end(CURRENT.load(Relaxed));
        }
    }
}

/// Make room in the current statement's scope for `more` references more,
/// so that holding them ([`hold_reserved`]) can't fail: a function that
/// hands the scope references it can't take back does, before it changes
/// anything. Fails if there is no memory for them.
///
/// # Safety
///
/// The statement must have begun a scope, if `more` isn't 0.
#[cfg(not(test))]
unsafe fn reserve(more: usize) -> Fallible {
    use core::sync::atomic::Ordering::Relaxed;
    // SAFETY: guaranteed by the caller.
    unsafe { reserve_in(CURRENT.load(Relaxed), more) }
}

/// Make room in `scope` for `more` references more, as [`reserve`] does in
/// the current one.
///
/// # Safety
///
/// `scope` must point to a scope that has begun and not ended, if `more`
/// isn't 0.
#[cfg(not(test))]
unsafe fn reserve_in(scope: *mut Scope, more: usize) -> Fallible {
    if more == 0 {
        return Ok(());
    }
    // SAFETY: guaranteed by the caller.
    unsafe {
        let scope = &mut *scope;
        let Some(needed) = scope.len.checked_add(more) else {
            return Err(out_of_memory());
        };
        if needed <= scope.capacity {
            return Ok(());
        }
        let capacity = needed.max(scope.capacity * 2).max(8);
        let Some(bytes) = capacity.checked_mul(size_of::<*mut u8>()) else {
            return Err(out_of_memory());
        };
        let grown = realloc(scope.buffers.cast(), bytes);
        if grown.is_null() {
            return Err(out_of_memory());
        }
        scope.buffers = grown.cast();
        scope.capacity = capacity;
        Ok(())
    }
}

/// Have the current statement's scope hold a reference to `buffer`, in
/// room [`reserve`] made, which it gives up when the statement ends. An
/// immortal buffer needs none.
///
/// # Safety
///
/// `buffer` must be a buffer, just past its header, whose reference the
/// scope takes over, and the scope must have room for it.
#[cfg(not(test))]
unsafe fn hold_reserved(buffer: *mut u8) {
    use core::sync::atomic::Ordering::Relaxed;
    // SAFETY: guaranteed by the caller.
    unsafe { hold_reserved_in(CURRENT.load(Relaxed), buffer) }
}

/// Have `scope` hold a reference to `buffer`, in room [`reserve_in`] made,
/// as [`hold_reserved`] does the current one.
///
/// # Safety
///
/// As for [`hold_reserved`], of `scope`.
#[cfg(not(test))]
unsafe fn hold_reserved_in(scope: *mut Scope, buffer: *mut u8) {
    // SAFETY: guaranteed by the caller.
    unsafe {
        if header(buffer).count == IMMORTAL {
            return;
        }
        let scope = &mut *scope;
        *scope.buffers.add(scope.len) = buffer;
        scope.len += 1;
    }
}

/// Have the current statement's scope hold a reference to `buffer`, as
/// [`hold_reserved`] does, making room for it. If there is no memory for
/// that, it gives the reference up and fails.
///
/// # Safety
///
/// `buffer` must be a buffer, just past its header, whose reference the
/// scope takes over. A statement that makes or replaces values that point
/// to buffers has begun a scope.
#[cfg(not(test))]
unsafe fn hold(buffer: *mut u8) -> Fallible {
    // SAFETY: guaranteed by the caller.
    unsafe {
        if let Err(failed) = reserve(1) {
            release_buffer(buffer);
            return Err(failed);
        }
        hold_reserved(buffer);
    }
    Ok(())
}

/// How many buffers the value at `value`, of the type `desc` describes,
/// points to directly, as [`each_buffer`] goes through them.
///
/// # Safety
///
/// As for [`each_buffer`].
#[cfg(not(test))]
unsafe fn buffers(value: *const u8, desc: &TypeDesc) -> usize {
    let mut count = 0;
    // SAFETY: guaranteed by the caller.
    unsafe { each_buffer(value, desc, &mut |_| count += 1) };
    count
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
/// through none of its elements yet. Fails if there is no memory for it.
#[cfg(not(test))]
fn allocate_buffer(bytes: usize, desc: *const TypeDesc) -> Fallible<*mut u8> {
    // SAFETY: a statement that makes a buffer has begun a scope, which
    // takes over the new buffer's one reference.
    unsafe {
        reserve(1)?;
        let buffer = new_buffer(bytes, desc)?;
        hold_reserved(buffer);
        Ok(buffer)
    }
}

/// A new buffer, as for [`allocate_buffer`], whose one reference is the
/// caller's to give to what holds it.
#[cfg(not(test))]
fn new_buffer(bytes: usize, desc: *const TypeDesc) -> Fallible<*mut u8> {
    let Some(total) = bytes.checked_add(HEADER) else {
        return Err(out_of_memory());
    };
    // SAFETY: the block has room for the header and the bytes.
    unsafe {
        let buffer = allocate::<u8>(total)?.add(HEADER);
        set_header(
            buffer,
            Header {
                count: 1,
                len: 0,
                desc,
                room: bytes,
            },
        );
        Ok(buffer)
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
    if !desc.points {
        return;
    }
    // SAFETY: guaranteed by the caller.
    unsafe {
        match desc.kind {
            KIND_TEXT | KIND_CHARACTER | KIND_RELATION | KIND_ARRAY => {
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
            KEPT.forget(buffer);
            let element = desc.element();
            if element.points {
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
/// `src`. The statement must have begun a scope. Fails, assigning nothing,
/// if there is no memory for the scope to hold the old value's references.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_assign(
    dst: *mut u8,
    src: *const u8,
    desc: *const TypeDesc,
) -> bool {
    // SAFETY: guaranteed by the caller.
    status(|| unsafe {
        let desc = &*desc;
        reserve(buffers(dst, desc))?;
        retain(src, desc);
        each_buffer(dst, desc, &mut |buffer| hold_reserved(buffer));
        core::ptr::copy_nonoverlapping(src, dst, desc.size);
        Ok(())
    })
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
/// Fails, keeping nothing, if there is no memory for the scope to hold
/// them.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_keep(value: *const u8, desc: *const TypeDesc) -> bool {
    // SAFETY: guaranteed by the caller.
    status(|| unsafe {
        let desc = &*desc;
        reserve(buffers(value, desc))?;
        retain(value, desc);
        each_buffer(value, desc, &mut |buffer| hold_reserved(buffer));
        Ok(())
    })
}

/// Have the scope the innermost one is in hold a reference to each buffer
/// the value at `value`, of the type `desc` describes, points to, until it
/// ends: what the innermost scope's work made that outlives it, as a
/// tuple's computed attributes do the scope of the work on that tuple, or a
/// read-only operator's result the scope of its call. Ending the innermost
/// scope then frees only what nothing else holds, so that a relational
/// operation's memory is its operands' and its result's, not its work's.
///
/// # Safety
///
/// `value` must point to a valid value of the type `desc` describes, and
/// `desc` to a valid descriptor. The innermost scope must be in another.
/// Fails, handing out nothing, if there is no memory for that one to hold
/// them.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_hand_out(value: *const u8, desc: *const TypeDesc) -> bool {
    use core::sync::atomic::Ordering::Relaxed;
    // SAFETY: guaranteed by the caller.
    status(|| unsafe {
        let desc = &*desc;
        let outer = (*CURRENT.load(Relaxed)).outer;
        reserve_in(outer, buffers(value, desc))?;
        retain(value, desc);
        each_buffer(value, desc, &mut |buffer| hold_reserved_in(outer, buffer));
        Ok(())
    })
}

/// Give up the references that the value at `value`, a local variable's of
/// the type `desc` describes, holds, at the end of its block, and make all of
/// it zero, so that the block, run again, has nothing to give up when it first
/// assigns it. Nothing points into what is freed: every statement that could
/// has ended, and a statement's scope holds its own references. Nothing reads
/// any of it from then on, numbers included, as lowering has copied them out.
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

/// Take a reference to each buffer the value at `value`, of the type `desc`
/// describes, points to, for a copy of it that is to hold them: a
/// `fail`'s error, which waits for what takes it.
///
/// # Safety
///
/// `value` must point to a valid value of the type `desc` describes, or
/// one with null pointers, and `desc` to a valid descriptor.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_retain(value: *const u8, desc: *const TypeDesc) {
    // SAFETY: guaranteed by the caller.
    unsafe { retain(value, &*desc) }
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
        let (buffer, len) = read_slice(value);
        adopt(buffer.cast_mut(), len, (*desc).element());
    }
}

/// Have `buffer` hold references through its first `len` tuples or
/// elements, of the type `element` describes, which something has written.
///
/// # Safety
///
/// `buffer` must hold them, each valid, and references through none of its
/// elements yet.
#[cfg(not(test))]
unsafe fn adopt(buffer: *mut u8, len: usize, element: &TypeDesc) {
    // SAFETY: guaranteed by the caller.
    unsafe {
        if element.points {
            for i in 0..len {
                retain(buffer.add(i * element.size), element);
            }
        }
        set_header(
            buffer,
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
/// scope. Fails, changing nothing, if there is no memory for the copy.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_unique(value: *mut Text, desc: *const TypeDesc) -> bool {
    // SAFETY: guaranteed by the caller.
    status(|| unsafe {
        let (buffer, len) = read_slice(value.cast());
        if header(buffer).count == 1 {
            return Ok(());
        }
        reserve(1)?;
        let copy = copied(buffer, len, &*desc)?;
        hold_reserved(buffer.cast_mut());
        value.write_unaligned(Text { bytes: copy, len });
        Ok(())
    })
}

/// Store at `dst` a copy of the array at `src`, of the type `desc`
/// describes, in a new buffer, which the current statement's scope holds
/// the only reference to, so that compiled code can change its elements in
/// place: an array with some elements changed, as a value.
///
/// # Safety
///
/// `src` must point to a valid array of the type `desc` describes, and
/// `dst` to memory for one. The statement must have begun a scope. Fails
/// if there is no memory for the copy.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_duplicate(
    dst: *mut Text,
    src: *const u8,
    desc: *const TypeDesc,
) -> bool {
    // SAFETY: guaranteed by the caller.
    status(|| unsafe {
        let (buffer, len) = read_slice(src);
        let copy = copied(buffer, len, &*desc)?;
        hold(copy)?;
        dst.write_unaligned(Text { bytes: copy, len });
        Ok(())
    })
}

/// A new buffer of copies of the `len` elements of `buffer`, an array's of
/// the type `desc` describes, which take references to what they point to,
/// and whose one reference is the caller's.
///
/// # Safety
///
/// `buffer` must hold `len` valid elements of the array's type.
#[cfg(not(test))]
unsafe fn copied(buffer: *const u8, len: usize, desc: &TypeDesc) -> Fallible<*mut u8> {
    // SAFETY: guaranteed by the caller.
    unsafe {
        let Some(bytes) = len.checked_mul(desc.element().size) else {
            return Err(out_of_memory());
        };
        copied_with_room(buffer, len, desc, bytes)
    }
}

/// As [`copied`], with room for `bytes` bytes in all.
///
/// # Safety
///
/// As for [`copied`], and `bytes` must be room for the `len` elements.
#[cfg(not(test))]
unsafe fn copied_with_room(
    buffer: *const u8,
    len: usize,
    desc: &TypeDesc,
    bytes: usize,
) -> Fallible<*mut u8> {
    // SAFETY: guaranteed by the caller.
    unsafe {
        let element = desc.element();
        let copy = new_buffer(bytes, desc)?;
        for i in 0..len {
            let at = i * element.size;
            copy_element(copy, copy.add(at), buffer.add(at), element);
        }
        Ok(copy)
    }
}

/// Memory for `count` values of type `T`, at least one byte, so that even
/// an empty text, relation or array has an address. `malloc`'s memory is aligned
/// for any type. Fails if there is none.
#[cfg(not(test))]
fn allocate<T>(count: usize) -> Fallible<*mut T> {
    let Some(size) = count.checked_mul(size_of::<T>()) else {
        return Err(out_of_memory());
    };
    // SAFETY: `malloc` has no preconditions.
    let memory = unsafe { malloc(size.max(1)) };
    if memory.is_null() {
        return Err(out_of_memory());
    }
    Ok(memory.cast())
}

/// How much memory [`SCRATCH`] has: enough that a relational operation that
/// takes less than the `malloc` and the `free` it saves fits in it.
#[cfg(not(test))]
const SCRATCH_BYTES: usize = 1 << 16;

/// The alignment of what [`SCRATCH`] gives, which is `malloc`'s: enough for
/// any type.
#[cfg(not(test))]
const SCRATCH_ALIGN: usize = 16;

/// Memory for the [`Temp`]s of the functions running now, one after another,
/// so that one costs a few instructions, where a relational operation over
/// a few tuples, made for each of many tuples, would otherwise spend much of
/// its time in `malloc` and `free`. One too big for what is left has its own
/// from `malloc`.
#[cfg(not(test))]
#[repr(C, align(16))]
struct Scratch {
    memory: core::cell::UnsafeCell<[core::mem::MaybeUninit<u8>; SCRATCH_BYTES]>,
    /// The bytes in use, from the start of `memory`.
    top: core::cell::Cell<usize>,
    /// How many `Temp`s hold some of it. It is all free when none does.
    held: core::cell::Cell<usize>,
}

// SAFETY: the program runs on one thread.
#[cfg(not(test))]
unsafe impl Sync for Scratch {}

/// The program's scratch memory, which takes no memory until it is used,
/// and no more than its size.
#[cfg(not(test))]
static SCRATCH: Scratch = Scratch {
    memory: core::cell::UnsafeCell::new([core::mem::MaybeUninit::uninit(); SCRATCH_BYTES]),
    top: core::cell::Cell::new(0),
    held: core::cell::Cell::new(0),
};

#[cfg(not(test))]
impl Scratch {
    /// `bytes` of memory, if there are that many left.
    fn take(&self, bytes: usize) -> Option<*mut u8> {
        let top = self.top.get();
        // Some, even of none, so that what it gives is within the memory.
        let rounded = bytes.max(1).checked_next_multiple_of(SCRATCH_ALIGN)?;
        if rounded > SCRATCH_BYTES - top {
            return None;
        }
        self.top.set(top + rounded);
        self.held.set(self.held.get() + 1);
        // SAFETY: `top` is within the memory, whose start is aligned, and
        // each `take` keeps it aligned.
        Some(unsafe { self.memory.get().cast::<u8>().add(top) })
    }

    /// Whether `memory` came from [`Scratch::take`].
    fn gave(&self, memory: *const u8) -> bool {
        let start = self.memory.get().cast::<u8>().cast_const();
        (start..start.wrapping_add(SCRATCH_BYTES)).contains(&memory)
    }

    /// Have back `memory`, of `bytes` or fewer, from [`Scratch::take`]: all
    /// of it once nothing holds any, and otherwise, if it is the last
    /// taken, as a function that makes one for each of many usually
    /// frees it, what it took.
    fn give_back(&self, memory: *const u8, bytes: usize) {
        self.held.set(self.held.get() - 1);
        let at = memory as usize - self.memory.get() as usize;
        if self.held.get() == 0 {
            self.top.set(0);
        } else if at + bytes.max(1).next_multiple_of(SCRATCH_ALIGN) == self.top.get() {
            self.top.set(at);
        }
    }
}

/// Memory for values of type `T`, from [`SCRATCH`] or else as [`allocate`]
/// gives it, for a function's own use while it runs, freed when it is
/// dropped, however the function ends: room for `len` of them, which are a
/// slice once written.
#[cfg(not(test))]
struct Temp<T> {
    memory: *mut T,
    len: usize,
}

#[cfg(not(test))]
impl<T> Temp<T> {
    /// Room for `len` values, not written yet. Fails if there is no memory
    /// for them.
    fn new(len: usize) -> Fallible<Self> {
        let scratch = len
            .checked_mul(size_of::<T>())
            .and_then(|bytes| SCRATCH.take(bytes));
        let memory = match scratch {
            Some(memory) => memory.cast(),
            None => allocate(len)?,
        };
        Ok(Temp { memory, len })
    }

    /// Room for `len` values, all zero bytes.
    fn zeroed(len: usize) -> Fallible<Self> {
        let temp = Temp::new(len)?;
        // SAFETY: the memory has room for `len` values.
        unsafe { core::ptr::write_bytes(temp.memory, 0, len) };
        Ok(temp)
    }

    /// Room for `len` values, the first of them as they were. Fails,
    /// leaving them as they are, if there is no memory for them.
    fn grow(&mut self, len: usize) -> Fallible {
        if SCRATCH.gave(self.memory.cast()) {
            let bigger = allocate::<T>(len)?;
            // SAFETY: both have room for the first `self.len`.
            unsafe { core::ptr::copy_nonoverlapping(self.memory, bigger, self.len) };
            SCRATCH.give_back(self.memory.cast(), self.len * size_of::<T>());
            self.memory = bigger;
            self.len = len;
            return Ok(());
        }
        let Some(bytes) = len.checked_mul(size_of::<T>()) else {
            return Err(out_of_memory());
        };
        // SAFETY: the memory is `allocate`'s, or `realloc`'s.
        let grown = unsafe { realloc(self.memory.cast(), bytes.max(1)) };
        if grown.is_null() {
            return Err(out_of_memory());
        }
        self.memory = grown.cast();
        self.len = len;
        Ok(())
    }

    /// Where the memory starts.
    fn ptr(&self) -> *mut T {
        self.memory
    }

    /// The values, which must all have been written.
    ///
    /// # Safety
    ///
    /// Each of the first `len` values must be valid.
    unsafe fn values(&mut self) -> &mut [T] {
        // SAFETY: guaranteed by the caller.
        unsafe { core::slice::from_raw_parts_mut(self.memory, self.len) }
    }
}

#[cfg(not(test))]
impl<T> Drop for Temp<T> {
    fn drop(&mut self) {
        if SCRATCH.gave(self.memory.cast()) {
            SCRATCH.give_back(self.memory.cast(), self.len * size_of::<T>());
        } else {
            // SAFETY: `allocate` gave the memory, which only this frees.
            unsafe { free(self.memory.cast()) }
        }
    }
}

/// Store at `dst` the relation at `src`, with each tuple reshaped into the
/// attributes `result` describes, in canonical order and without
/// duplicates, in memory the statement owns. `source` describes `src`,
/// and `offsets` gives, for each attribute of `result`, the offset in a
/// tuple of `src` of the attribute it comes from. Removing attributes is
/// reshaping, and can make tuples equal; renaming is too, and can change
/// their order. If `*orders` has [`LOOSE_RESULT`], which it can only if no two
/// can be equal, as of a renaming, they stay in the source's order instead. The
/// source can be in any order.
///
/// # Safety
///
/// `src` must point to a valid relation of the type `source` describes,
/// `result` and `source` to valid relation descriptors, `offsets` to one
/// offset per attribute of `result`, `orders` to a set of `LOOSE_` bits, and
/// `dst` to memory for a relation.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_reshape(
    dst: *mut Text,
    src: *const u8,
    result: *const TypeDesc,
    source: *const TypeDesc,
    offsets: *const usize,
    orders: *const usize,
) -> bool {
    // SAFETY: guaranteed by the caller; each size and offset is the layout
    // of the value it describes.
    status(|| unsafe {
        let (result, source) = (&*result, &*source);
        let (tuples, count) = read_slice(src);
        let size = result.element().size;
        let attributes = attributes(result);
        let offsets = core::slice::from_raw_parts(offsets, attributes.len());
        // Each tuple with only the kept attributes, in the source's order.
        let write = |projected: *mut u8| {
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
        };
        let (bytes, len) = if orders.read_unaligned() & LOOSE_RESULT == 0 {
            made_canonical(count, result, write)?
        } else {
            made_loose(count, result, write)?
        };
        dst.write_unaligned(Text { bytes, len });
        Ok(())
    })
}

/// In the `orders` of a join, a semijoin or a reshape: its left operand's tuples are in
/// no particular order, only all different. Such a relation is loose: it never reaches
/// anything that depends on canonical order, as `lir::Function::loose_read` checks.
pub const LOOSE_LHS: usize = 1;
/// As for [`LOOSE_LHS`], of its right operand.
pub const LOOSE_RHS: usize = 2;
/// In the `orders` of a join or a reshape: nothing depends on its result's order, so it
/// is left as it comes, rather than sorted.
pub const LOOSE_RESULT: usize = 4;

/// Put the relation at `relation`, of the type `desc` describes, whose tuples are all
/// different and hold references to what they point to, in canonical order: where they
/// are, if nothing else holds them and they are integers, and otherwise a copy, in memory
/// the statement owns, giving up the buffer it had if only the statement's scope holds it.
///
/// # Safety
///
/// `relation` must point to a valid relation of that type, in any order, that nothing else
/// points into, if only the current scope holds its buffer. The statement must have begun
/// a scope.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_put_in_order(
    relation: *mut Text,
    desc: *const TypeDesc,
) -> bool {
    // SAFETY: guaranteed by the caller.
    status(|| unsafe {
        let desc = &*desc;
        let (tuples, count) = read_slice(relation.cast());
        if in_canonical_order(tuples, count, desc) {
            return Ok(());
        }
        if sorts_in_place(desc) && only_in_scope(tuples.cast_mut()).is_some() {
            let (bytes, len) = sorted_in_place(tuples.cast_mut(), count, desc)?;
            relation.write_unaligned(Text { bytes, len });
            return Ok(());
        }
        let (bytes, len) = canonical(tuples, count, desc)?;
        relation.write_unaligned(Text { bytes, len });
        release_replaced(tuples.cast_mut());
        Ok(())
    })
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
) -> bool {
    // SAFETY: guaranteed by the caller.
    status(|| unsafe {
        let (bytes, len) = canonical(tuples, count.read_unaligned(), &*desc)?;
        dst.write_unaligned(Text { bytes, len });
        Ok(())
    })
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

/// Tuples of the operands of a join or a semijoin that agree on the common
/// attributes: those of the left operand from `lhs_start` to `lhs_end`, and those of
/// the right from `rhs_start` to `rhs_end`, in the orders [`Matches`] has them.
#[cfg(not(test))]
#[derive(Clone, Copy)]
struct Group {
    lhs_start: usize,
    lhs_end: usize,
    rhs_start: usize,
    rhs_end: usize,
}

/// Store at `dst` the relation `result` describes whose tuples are each
/// made of a tuple of the relation `lhs` and one of `rhs` that agree on
/// their common attributes (Algebra A's AND), in memory the statement owns: in
/// canonical order, unless `*orders` has [`LOOSE_RESULT`], and of operands each in
/// any order if it has [`LOOSE_LHS`] or [`LOOSE_RHS`]. `lhs_desc` and `rhs_desc` describe the operands. `sources` says where each attribute of `result` comes
/// from, and `common` lists the `*common_count` common attributes.
///
/// Both operands' tuples are sorted on the common attributes, then merged:
/// each run of tuples of one that agree on them pairs with the matching run
/// of the other. Of none in common, it is their product: made in canonical
/// order, if all one's attributes come before all the other's in name
/// order, and otherwise sorted.
///
/// # Safety
///
/// `lhs` and `rhs` must point to valid relations of the types `lhs_desc`
/// and `rhs_desc` describe, `result` to a valid descriptor of the relation
/// type with the attributes of both, `sources` to one source per attribute
/// of `result`, `common` to `*common_count` common attributes, `orders` to a set of
/// `LOOSE_` bits, and `dst` to memory for a relation.
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
    orders: *const usize,
) -> bool {
    // SAFETY: guaranteed by the caller; each size and offset is the layout
    // of the value it describes.
    status(|| unsafe {
        let (result, lhs_desc, rhs_desc) = (&*result, &*lhs_desc, &*rhs_desc);
        let ((lhs, lhs_count), (rhs, rhs_count)) = (read_slice(lhs), read_slice(rhs));
        let result_attrs = attributes(result);
        let sources = array(sources, result_attrs.len());
        let common = array(common, common_count.read_unaligned());
        let orders = orders.read_unaligned();
        if common.is_empty() {
            let (bytes, len) = product(
                Tuples {
                    at: lhs,
                    count: lhs_count,
                    size: lhs_desc.element().size,
                },
                Tuples {
                    at: rhs,
                    count: rhs_count,
                    size: rhs_desc.element().size,
                },
                result,
                sources,
                orders,
            )?;
            dst.write_unaligned(Text { bytes, len });
            return Ok(());
        }
        let (lhs, rhs) = (
            Operand::of(
                common,
                lhs,
                lhs_count,
                lhs_desc,
                true,
                orders & LOOSE_LHS != 0,
            ),
            Operand::of(
                common,
                rhs,
                rhs_count,
                rhs_desc,
                false,
                orders & LOOSE_RHS != 0,
            ),
        );
        let (mut lhs_pointers, mut rhs_pointers) = (None, None);
        let matches = Matches::of(common, lhs, rhs, &mut lhs_pointers, &mut rhs_pointers)?;
        let mut groups = Growing::<Group>::with_room(lhs.count.min(rhs.count))?;
        let mut total = 0_usize;
        matches.each_group(common, &mut |group| {
            let Some(sum) = (group.lhs_end - group.lhs_start)
                .checked_mul(group.rhs_end - group.rhs_start)
                .and_then(|n| n.checked_add(total))
            else {
                return Err(out_of_memory());
            };
            total = sum;
            groups.push(group)
        })?;

        let size = result.element().size;
        let write = |joined: *mut u8| {
            let mut k = 0;
            for at in 0..groups.len {
                let group = groups.at(at);
                for i in group.lhs_start..group.lhs_end {
                    let l = matches.lhs.at(i);
                    for j in group.rhs_start..group.rhs_end {
                        let r = matches.rhs.at(j);
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
        };
        let (bytes, len) = if orders & LOOSE_RESULT == 0 {
            made_canonical(total, result, write)?
        } else {
            made_loose(total, result, write)?
        };
        dst.write_unaligned(Text { bytes, len });
        Ok(())
    })
}

/// A relation's tuples, or some of them: `count` of them from `at`, each
/// `size` bytes.
#[cfg(not(test))]
#[derive(Clone, Copy)]
struct Tuples {
    at: *const u8,
    count: usize,
    size: usize,
}

#[cfg(not(test))]
impl Tuples {
    /// The tuple at position `i`.
    ///
    /// # Safety
    ///
    /// `i` must be less than `count`.
    unsafe fn tuple(self, i: usize) -> *const u8 {
        // SAFETY: guaranteed by the caller.
        unsafe { self.at.add(i * self.size) }
    }
}

/// The relation `result` describes of every tuple made of a tuple of `lhs`
/// and one of `rhs`, which have no attributes in common, in canonical
/// order, in memory the statement owns: that memory and how many tuples it
/// holds. `sources` says where each attribute of `result` comes from, in
/// name order. If all one operand's come before all the other's, each tuple
/// of that one, in its order, with each of the other's, in its order, is
/// the canonical order of the result, which is written as it is made, with
/// no sorting and no copy. Otherwise the tuples must be sorted, as they must if an
/// operand is in no particular order, unless `orders` has [`LOOSE_RESULT`]. They are
/// all different, as each operand's are.
///
/// # Safety
///
/// As for [`trendy_rt_and`], whose operands they are.
#[cfg(not(test))]
unsafe fn product(
    lhs: Tuples,
    rhs: Tuples,
    result: &TypeDesc,
    sources: &[Source],
    orders: usize,
) -> Fallible<(*mut u8, usize)> {
    // SAFETY: guaranteed by the caller; each size and offset is the layout
    // of the value it describes, and each tuple written has room.
    unsafe {
        let element = result.element();
        let Some(total) = lhs.count.checked_mul(rhs.count) else {
            return Err(out_of_memory());
        };
        let rhs_first = sources.first().is_some_and(|source| source.from == 1);
        let write = |to: *mut u8| {
            let (outer, inner) = if rhs_first { (rhs, lhs) } else { (lhs, rhs) };
            let attributes = attributes(result);
            for i in 0..outer.count {
                for j in 0..inner.count {
                    let (a, b) = (outer.tuple(i), inner.tuple(j));
                    let (l, r) = if rhs_first { (b, a) } else { (a, b) };
                    let made = to.add((i * inner.count + j) * element.size);
                    for (attr, source) in attributes.iter().zip(sources) {
                        let from = if source.from == 1 { r } else { l };
                        core::ptr::copy_nonoverlapping(
                            from.add(source.offset),
                            made.add(attr.offset),
                            (*attr.desc).size,
                        );
                    }
                }
            }
        };
        // The attributes change operand at most once if they don't interleave.
        let changes = sources
            .windows(2)
            .filter(|pair| pair[0].from != pair[1].from)
            .count();
        if orders & LOOSE_RESULT != 0 {
            return made_loose(total, result, write);
        }
        if changes > 1 || orders & (LOOSE_LHS | LOOSE_RHS) != 0 {
            return made_canonical(total, result, write);
        }
        made_in_order(total, result, write)
    }
}

/// Store at `dst` an index of the relation `rel`, which `rel_desc`
/// describes, on the `*common_count` attributes in `common`, each `lhs`
/// offset of which is where it is in a tuple of `rel`: an array, of the
/// type `index_desc` describes, of the positions of its tuples, from 0,
/// sorted by their values for those attributes, and those that agree in
/// canonical order, in memory the statement owns. [`trendy_rt_lookup`]
/// finds the tuples that agree with a tuple in it.
///
/// # Safety
///
/// `rel` must point to a valid relation of the type `rel_desc` describes,
/// `index_desc` to the descriptor of an array of integers, `common` to
/// `*common_count` attributes of `rel`'s tuples, and `dst` to memory for an
/// array.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_index(
    dst: *mut Text,
    rel: *const u8,
    rel_desc: *const TypeDesc,
    index_desc: *const TypeDesc,
    common: *const Common,
    common_count: *const usize,
) -> bool {
    // SAFETY: guaranteed by the caller; each offset is where an attribute
    // is in a tuple.
    status(|| unsafe {
        let (tuples, count) = read_slice(rel);
        let size = (*rel_desc).element().size;
        let common = array(common, common_count.read_unaligned());
        let Some(bytes) = count.checked_mul(size_of::<i64>()) else {
            return Err(out_of_memory());
        };
        let mut positions = Temp::<usize>::new(count)?;
        let positions = positions.values();
        for (i, position) in positions.iter_mut().enumerate() {
            *position = i;
        }
        // The relation is in canonical order, so the positions of tuples
        // that agree stay in it.
        let tuple = |i: usize| tuples.add(i * size);
        positions.sort_unstable_by(|&x, &y| {
            on_common(common, (tuple(x), true), (tuple(y), true)).then(x.cmp(&y))
        });
        let index = allocate_buffer(bytes, index_desc)?;
        for (k, &position) in positions.iter().enumerate() {
            let position =
                i64::try_from(position).unwrap_or_else(|_| internal_error("too many tuples"));
            index
                .add(k * size_of::<i64>())
                .cast::<i64>()
                .write_unaligned(position);
        }
        dst.write_unaligned(Text {
            bytes: index,
            len: count,
        });
        Ok(())
    })
}

/// Store at `dst` the relation `result` describes whose tuples are each
/// made of a tuple of the relation `rel` and the tuple `key` that agree on
/// their `*common_count` common attributes in `common`, as
/// [`trendy_rt_and`] makes them of `rel`, its left operand, and a relation
/// of `key` alone, its right, in memory the statement owns. `index` is
/// `rel`'s index on those attributes (see [`trendy_rt_index`]), in which
/// the tuples that agree with `key` are a run, found by binary search.
/// They are in canonical order, and so are the tuples made of them, as
/// `key`'s attributes are the same in each, so they need no sorting.
/// `rel_desc` describes `rel`, and `sources` says where each attribute of
/// `result` comes from: 0 for `rel` and 1 for `key`.
///
/// # Safety
///
/// `rel` must point to a valid relation of the type `rel_desc` describes,
/// `index` to its index on the `common` attributes, `key` to a valid
/// tuple, `result` to a valid descriptor of the relation type with the
/// attributes of both, `sources` to one source per attribute of `result`,
/// `common` to `*common_count` common attributes, and `dst` to memory for a
/// relation.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_lookup(
    dst: *mut Text,
    rel: *const u8,
    index: *const u8,
    key: *const u8,
    result: *const TypeDesc,
    rel_desc: *const TypeDesc,
    sources: *const Source,
    common: *const Common,
    common_count: *const usize,
) -> bool {
    // SAFETY: guaranteed by the caller; each size and offset is the layout
    // of the value it describes.
    status(|| unsafe {
        let result = &*result;
        let (tuples, _) = read_slice(rel);
        let (index, count) = read_slice(index);
        let size = (*rel_desc).element().size;
        let common = array(common, common_count.read_unaligned());
        // The tuple at position `k` of the index.
        let tuple = |k: usize| {
            let position = index
                .add(k * size_of::<i64>())
                .cast::<i64>()
                .read_unaligned();
            let i = usize::try_from(position).unwrap_or_else(|_| internal_error("a position"));
            tuples.add(i * size)
        };
        let agrees = |k: usize| on_common(common, (tuple(k), true), (key, false));
        let start = partition_point(0, count, |k| agrees(k).is_lt());
        let end = partition_point(start, count, |k| agrees(k).is_eq());
        let result_attrs = attributes(result);
        let sources = array(sources, result_attrs.len());
        let element = result.element();
        let found = end - start;
        let Some(bytes) = found.checked_mul(element.size) else {
            return Err(out_of_memory());
        };
        let made = Temp::<u8>::zeroed(element.size)?;
        let made = made.ptr();
        let out = allocate_buffer(bytes, result)?;
        for k in start..end {
            let from_rel = tuple(k);
            for (attr, source) in result_attrs.iter().zip(sources) {
                let from = if source.from == 1 { key } else { from_rel };
                core::ptr::copy_nonoverlapping(
                    from.add(source.offset),
                    made.add(attr.offset),
                    (*attr.desc).size,
                );
            }
            copy_element(out, out.add((k - start) * element.size), made, element);
        }
        dst.write_unaligned(Text {
            bytes: out,
            len: found,
        });
        Ok(())
    })
}

/// Store at `dst` the run of tuples of the relation at `src`, which `desc`
/// describes, whose attribute `*attribute`, from the first in name order,
/// is equal to the value at `value`, found by binary search: it is the
/// attribute they are sorted by, as those before it are the same in all of
/// them. It has no buffer of its own, only its tuples' address in `src`'s,
/// and their number, so it is only to be read while `src` holds them.
///
/// # Safety
///
/// `src` must point to a relation's tuples of the type `desc` describes, or
/// a run of them this made of an attribute before this one, `value` to a
/// valid value of the attribute's type, and `dst` to memory for a relation.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_equal_run(
    dst: *mut Text,
    src: *const u8,
    value: *const u8,
    desc: *const TypeDesc,
    attribute: *const usize,
) {
    // SAFETY: guaranteed by the caller.
    unsafe {
        let desc = &*desc;
        let (tuples, count) = read_slice(src);
        let size = desc.element().size;
        let attr = &attributes(desc)[attribute.read_unaligned()];
        let compared = |i: usize| order(tuples.add(i * size + attr.offset), value, &*attr.desc);
        let start = partition_point(0, count, |i| compared(i).is_lt());
        let end = partition_point(start, count, |i| compared(i).is_eq());
        dst.write_unaligned(Text {
            bytes: tuples.add(start * size),
            len: end - start,
        });
    }
}

/// The first of `start..end` for which `before` is false, all those before
/// it being those for which it is true, by binary search.
#[cfg(not(test))]
fn partition_point(start: usize, end: usize, before: impl Fn(usize) -> bool) -> usize {
    let (mut low, mut high) = (start, end);
    while low < high {
        let middle = low + (high - low) / 2;
        if before(middle) {
            low = middle + 1;
        } else {
            high = middle;
        }
    }
    low
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
) -> bool {
    // SAFETY: guaranteed by the caller; each size and offset is the layout
    // of the value it describes.
    status(|| unsafe {
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
        let Some(total) = total else {
            return Err(out_of_memory());
        };
        // For the tuple at hand, the index in each ungrouped relation of the
        // tuple being combined, counted up like an odometer.
        let mut at = Temp::<usize>::zeroed(ungrouped.len())?;
        let at = at.values();
        let (bytes, len) = made_canonical(total, result, |made| {
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
        })?;
        dst.write_unaligned(Text { bytes, len });
        Ok(())
    })
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
/// sorting. Fails if a disjoint union's operands, or disjoint changes',
/// share a tuple, or an included minus's right operand has one the left
/// lacks.
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
) -> bool {
    use core::cmp::Ordering;
    // SAFETY: guaranteed by the caller.
    status(|| unsafe {
        let desc = &*desc;
        let ((lhs, lhs_count), (rhs, rhs_count)) = (read_slice(lhs), read_slice(rhs));
        let combination = combination.read_unaligned();
        let minus = matches!(combination, COMBINE_MINUS | COMBINE_INCLUDED_MINUS);
        let size = desc.element().size;
        let Some(room) = lhs_count.checked_add(rhs_count) else {
            return Err(out_of_memory());
        };
        let Some(bytes) = room.checked_mul(size) else {
            return Err(out_of_memory());
        };
        let out = allocate_buffer(bytes, desc)?;
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
                        COMBINE_DISJOINT_UNION => {
                            return Err(tuple_failed(SHARED_TUPLE, l, tuple_desc));
                        }
                        COMBINE_DISJOINT_CHANGES => {
                            return Err(tuple_failed(TUPLE_REMOVED_TWICE, l, tuple_desc));
                        }
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
                        return Err(tuple_failed(MISSING_TUPLE, rhs.add(j * size), tuple_desc));
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
            return Err(tuple_failed(MISSING_TUPLE, rhs.add(j * size), tuple_desc));
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
        Ok(())
    })
}

/// Where `tuple` goes among the first `end` of the `tuples`, of a relation
/// `desc` describes: the place of the first that isn't less, and whether it
/// is equal. Looks back from the end, one tuple, then two, then four, and so
/// on, then between the last two it looked at, so finding the place of a tuple
/// that goes last takes one comparison, and of `n` tuples in order among `m`
/// no more than a merge would.
///
/// # Safety
///
/// `tuples` must point to at least `end` valid tuples, in canonical order, and
/// `tuple` to a valid one.
#[cfg(not(test))]
unsafe fn place_from_end(
    tuples: *const u8,
    end: usize,
    tuple: *const u8,
    desc: &TypeDesc,
) -> (usize, bool) {
    use core::cmp::Ordering;
    // Those before `lower` are less, and those from `upper` greater.
    let (mut lower, mut upper) = (0, end);
    let mut step = 1;
    // SAFETY: guaranteed by the caller; each place compared is below `end`.
    unsafe {
        let size = desc.element().size;
        while upper > 0 {
            let at = upper.saturating_sub(step);
            match compare_tuples(tuples.add(at * size), tuple, desc) {
                Ordering::Less => {
                    lower = at + 1;
                    break;
                }
                Ordering::Equal => return (at, true),
                Ordering::Greater => upper = at,
            }
            step = step.saturating_mul(2);
        }
        while lower < upper {
            let at = lower + (upper - lower) / 2;
            match compare_tuples(tuples.add(at * size), tuple, desc) {
                Ordering::Less => lower = at + 1,
                Ordering::Equal => return (at, true),
                Ordering::Greater => upper = at,
            }
        }
    }
    (lower, false)
}

/// Make the relation at `value`, of the type `desc` describes, with `len`
/// tuples, the only holder of a buffer with room for `bytes` bytes: as it is,
/// if nothing else holds it and it has the room; grown, if nothing else
/// holds it; or else a copy, whose tuples take references to what they point
/// to, and the holder's reference to the old buffer goes to the current
/// statement's scope, which may still point into it. It gets more room than
/// that as it grows, so that adding one tuple after another moves each a few
/// times at most.
///
/// # Safety
///
/// `value` must point to a valid relation of the type `desc` describes that
/// holds a reference to its buffer, or a null pointer. The statement must
/// have begun a scope. Fails, changing nothing, if there is no memory.
#[cfg(not(test))]
unsafe fn with_room(
    value: *mut Text,
    len: usize,
    bytes: usize,
    desc: &TypeDesc,
) -> Fallible<*mut u8> {
    // SAFETY: guaranteed by the caller; a block's memory starts with the
    // header, before the buffer.
    unsafe {
        let size = desc.element().size;
        let now = len.saturating_mul(size);
        let room = bytes.max(now.saturating_mul(2)).max(size.saturating_mul(4));
        let (buffer, _) = read_slice(value.cast());
        let buffer = buffer.cast_mut();
        if buffer.is_null() {
            return new_buffer(room, desc);
        }
        let held = header(buffer);
        if held.count != 1 {
            reserve(1)?;
            let copy = copied_with_room(buffer, len, desc, room)?;
            hold_reserved(buffer);
            return Ok(copy);
        }
        KEPT.forget(buffer);
        if held.room >= bytes {
            return Ok(buffer);
        }
        let Some(total) = room.checked_add(HEADER) else {
            return Err(out_of_memory());
        };
        let grown = realloc(buffer.sub(HEADER).cast(), total);
        if grown.is_null() {
            return Err(out_of_memory());
        }
        let buffer = grown.cast::<u8>().add(HEADER);
        set_header(buffer, Header { room, ..held });
        Ok(buffer)
    }
}

/// Add the tuples of the relation `tuples` to the relation variable at
/// `value`, both of the type `desc` describes, as `*combination`, a
/// `COMBINE_` constant of a union or a disjoint union, says: in the variable's
/// own buffer if nothing else holds it, moving only the tuples after each one
/// added, so that adding a few tuples doesn't copy the others, and in a copy
/// of it otherwise, as a value something else holds doesn't change. Tuples
/// `value` already has aren't added. Fails, changing nothing, if a disjoint
/// union's operands share a tuple, or there is no memory.
///
/// `tuples` can be in the variable's own buffer, as all of them are then in
/// the variable already, so nothing is added.
///
/// # Safety
///
/// `value` must point to a valid relation of the type `desc` describes that
/// holds a reference to its buffer, or a null pointer, `tuples` to a valid
/// relation of the type, and `combination` to a `COMBINE_` constant. The
/// statement must have begun a scope.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_insert(
    value: *mut Text,
    tuples: *const u8,
    desc: *const TypeDesc,
    combination: *const usize,
) -> bool {
    /// In a tuple's place, one the relation has already.
    const THERE: usize = usize::MAX;
    // SAFETY: guaranteed by the caller.
    status(|| unsafe {
        let desc = &*desc;
        let element = desc.element();
        let size = element.size;
        let (have, count) = read_slice(value.cast());
        let (added, added_count) = read_slice(tuples);
        // Where each goes, from the last, as they are in canonical order.
        let places = Temp::<usize>::new(added_count)?;
        let mut end = count;
        let mut new = 0;
        for j in (0..added_count).rev() {
            let tuple = added.add(j * size);
            let (at, equal) = place_from_end(have, end, tuple, desc);
            if equal && combination.read_unaligned() == COMBINE_DISJOINT_UNION {
                return Err(tuple_failed(SHARED_TUPLE, tuple, element));
            }
            places.ptr().add(j).write(if equal { THERE } else { at });
            new += usize::from(!equal);
            end = at;
        }
        if new == 0 {
            return Ok(());
        }
        let Some(bytes) = count
            .checked_add(new)
            .and_then(|total| total.checked_mul(size))
        else {
            return Err(out_of_memory());
        };
        let buffer = with_room(value, count, bytes, desc)?;
        // From the end, each to its place, and those after it to theirs.
        let mut end = count;
        let mut to = count + new;
        for j in (0..added_count).rev() {
            let at = places.ptr().add(j).read();
            if at == THERE {
                continue;
            }
            let after = end - at;
            to -= after;
            core::ptr::copy(buffer.add(at * size), buffer.add(to * size), after * size);
            to -= 1;
            let tuple = added.add(j * size);
            core::ptr::copy_nonoverlapping(tuple, buffer.add(to * size), size);
            retain(tuple, element);
            end = at;
        }
        set_header(
            buffer,
            Header {
                len: count + new,
                ..header(buffer)
            },
        );
        value.write_unaligned(Text {
            bytes: buffer,
            len: count + new,
        });
        Ok(())
    })
}

/// Take `gone` tuples from the relation variable at `value`, of the type
/// `desc` describes, those at the places of `ranges`, each from its first to
/// before its last, in order, none of them empty or touching: in the variable's
/// own buffer if nothing else holds it, which keeps its room, giving up what
/// they point to and moving those after them, and otherwise in a copy of the
/// others, as a value something else holds doesn't change, and the holder's
/// reference to the old buffer goes to the current statement's scope, which may
/// still point into it.
///
/// # Safety
///
/// `value` must point to a valid relation of the type `desc` describes that
/// holds a reference to its buffer, and `ranges` be of its places. The statement
/// must have begun a scope. Fails, changing nothing, if there is no memory for
/// a copy.
#[cfg(not(test))]
unsafe fn removed_from(
    value: *mut Text,
    desc: &TypeDesc,
    gone: usize,
    ranges: impl Iterator<Item = (usize, usize)>,
) -> Fallible {
    if gone == 0 {
        return Ok(());
    }
    // SAFETY: guaranteed by the caller; those moved are moved to where tuples
    // are given up or moved from already.
    unsafe {
        let element = desc.element();
        let size = element.size;
        let (buffer, count) = read_slice(value.cast());
        let buffer = buffer.cast_mut();
        let len = count - gone;
        let held = header(buffer);
        if held.count == 1 {
            KEPT.forget(buffer);
            let (mut kept, mut from) = (0, 0);
            for (first, last) in ranges {
                let before = first - from;
                core::ptr::copy(
                    buffer.add(from * size),
                    buffer.add(kept * size),
                    before * size,
                );
                kept += before;
                if element.points {
                    for i in first..last {
                        release(buffer.add(i * size), element);
                    }
                }
                from = last;
            }
            core::ptr::copy(
                buffer.add(from * size),
                buffer.add(kept * size),
                (count - from) * size,
            );
            set_header(buffer, Header { len, ..held });
            value.write_unaligned(Text { bytes: buffer, len });
            return Ok(());
        }
        let Some(bytes) = len.checked_mul(size) else {
            return Err(out_of_memory());
        };
        reserve(1)?;
        let copy = new_buffer(bytes, desc)?;
        let (mut kept, mut from) = (0, 0);
        let mut keep = |from: usize, to: usize| {
            for i in from..to {
                copy_element(copy, copy.add(kept * size), buffer.add(i * size), element);
                kept += 1;
            }
        };
        for (first, last) in ranges {
            keep(from, first);
            from = last;
        }
        keep(from, count);
        hold_reserved(buffer);
        value.write_unaligned(Text { bytes: copy, len });
        Ok(())
    }
}

/// Take the tuples of the relation `tuples` from the relation variable at
/// `value`, both of the type `desc` describes, as `*combination`, a `COMBINE_`
/// constant of a minus or an included minus, says (see `removed_from`).
/// Tuples `value` hasn't got aren't taken. Fails, changing nothing, if an
/// included minus's right operand has one, or there is no memory. The places
/// are found before anything moves, so `tuples` can be `value`'s own.
///
/// # Safety
///
/// `value` must point to a valid relation of the type `desc` describes that
/// holds a reference to its buffer, or a null pointer, `tuples` to a valid
/// relation of the type, and `combination` to a `COMBINE_` constant. The
/// statement must have begun a scope.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_remove(
    value: *mut Text,
    tuples: *const u8,
    desc: *const TypeDesc,
    combination: *const usize,
) -> bool {
    // SAFETY: guaranteed by the caller.
    status(|| unsafe {
        let desc = &*desc;
        let element = desc.element();
        let size = element.size;
        let (have, count) = read_slice(value.cast());
        let (taken, taken_count) = read_slice(tuples);
        // Each one's place, from the last, in the room from its end.
        let places = Temp::<usize>::new(taken_count)?;
        let mut end = count;
        let mut gone = 0;
        for j in (0..taken_count).rev() {
            let tuple = taken.add(j * size);
            let (at, equal) = place_from_end(have, end, tuple, desc);
            if equal {
                gone += 1;
                places.ptr().add(taken_count - gone).write(at);
            } else if combination.read_unaligned() == COMBINE_INCLUDED_MINUS {
                return Err(tuple_failed(MISSING_TUPLE, tuple, element));
            }
            end = at;
        }
        let places = array(places.ptr().add(taken_count - gone), gone);
        removed_from(value, desc, gone, ranges_of(places))
    })
}

/// The places `places`, in order, as ranges, each from its first to before its
/// last, with those side by side in one.
#[cfg(not(test))]
fn ranges_of(places: &[usize]) -> impl Iterator<Item = (usize, usize)> + '_ {
    let mut at = 0;
    core::iter::from_fn(move || {
        let first = *places.get(at)?;
        let mut last = first + 1;
        at += 1;
        while places.get(at) == Some(&last) {
            last += 1;
            at += 1;
        }
        Some((first, last))
    })
}

/// The place in the relation at `value` of the first tuple of `run`, a run of
/// it, as [`trendy_rt_equal_run`] makes one or the relation itself, of tuples of
/// `size` bytes.
///
/// # Safety
///
/// `run`'s tuples must be in `value`'s buffer, or both empty.
#[cfg(not(test))]
unsafe fn place_of_run(value: *const u8, run: *const u8, size: usize) -> usize {
    // SAFETY: guaranteed by the caller.
    unsafe {
        let (buffer, _) = read_slice(value);
        let (first, _) = read_slice(run);
        if size == 0 || first.is_null() {
            0
        } else {
            first.offset_from(buffer).unsigned_abs() / size
        }
    }
}

/// Take from the relation variable at `value`, of the type `desc` describes,
/// the tuples of `run`, a run of them (see `removed_from`).
///
/// # Safety
///
/// `value` must point to a valid relation of the type `desc` describes that
/// holds a reference to its buffer, or a null pointer, and `run` to a run of
/// its tuples, as for `place_of_run`. The statement must have begun a scope.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_remove_run(
    value: *mut Text,
    run: *const u8,
    desc: *const TypeDesc,
) -> bool {
    // SAFETY: guaranteed by the caller.
    status(|| unsafe {
        let desc = &*desc;
        let (_, gone) = read_slice(run);
        if gone == 0 {
            return Ok(());
        }
        let first = place_of_run(value.cast(), run, desc.element().size);
        removed_from(value, desc, gone, core::iter::once((first, first + gone)))
    })
}

/// Take from the relation variable at `value`, of the type `desc` describes,
/// the tuples of `run`, a run of them, with a non-zero byte at their position
/// in `marks` (see `removed_from`).
///
/// # Safety
///
/// As for [`trendy_rt_remove_run`], and `marks` must point to an array of as
/// many bytes as `run` has tuples.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_remove_marked(
    value: *mut Text,
    run: *const u8,
    marks: *const u8,
    desc: *const TypeDesc,
) -> bool {
    // SAFETY: guaranteed by the caller.
    status(|| unsafe {
        let desc = &*desc;
        let (marked, count) = read_slice(marks);
        let marks = array(marked, count);
        let gone = marks.iter().filter(|&&mark| mark != 0).count();
        if gone == 0 {
            return Ok(());
        }
        let first = place_of_run(value.cast(), run, desc.element().size);
        let mut at = 0;
        let ranges = core::iter::from_fn(|| {
            at += marks[at..].iter().position(|&mark| mark != 0)?;
            let start = at;
            at += marks[at..]
                .iter()
                .position(|&mark| mark == 0)
                .unwrap_or(count - at);
            Some((first + start, first + at))
        });
        removed_from(value, desc, gone, ranges)
    })
}

/// Store at `dst` the tuples of the relation `lhs`, which `lhs_desc`
/// describes, that have a match in the relation `rhs`, which `rhs_desc`
/// describes, on the `*common_count` attributes in `common`, or, if
/// `*negated` is 1, those that have none (the semijoin and the antijoin), in the
/// order they are in `lhs`, in memory the statement owns. Each operand can be in any
/// order if `*orders` has [`LOOSE_LHS`] or [`LOOSE_RHS`].
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
    orders: *const usize,
) -> bool {
    // SAFETY: guaranteed by the caller; each size and offset is the layout
    // of the value it describes.
    status(|| unsafe {
        let (lhs_desc, rhs_desc) = (&*lhs_desc, &*rhs_desc);
        let ((lhs, lhs_count), (rhs, rhs_count)) = (read_slice(lhs), read_slice(rhs));
        let common = array(common, common_count.read_unaligned());
        let negated = negated.read_unaligned() == 1;
        let orders = orders.read_unaligned();
        let size = lhs_desc.element().size;
        let (left, right) = (
            Operand::of(
                common,
                lhs,
                lhs_count,
                lhs_desc,
                true,
                orders & LOOSE_LHS != 0,
            ),
            Operand::of(
                common,
                rhs,
                rhs_count,
                rhs_desc,
                false,
                orders & LOOSE_RHS != 0,
            ),
        );
        let (mut lhs_pointers, mut rhs_pointers) = (None, None);
        let matches = Matches::of(common, left, right, &mut lhs_pointers, &mut rhs_pointers)?;
        // Which of the left operand's tuples have a match, by where they are.
        let mut marks = Temp::<u64>::zeroed(lhs_count / 64 + 1)?;
        let marks = marks.values();
        matches.each_group(common, &mut |group| {
            for i in group.lhs_start..group.lhs_end {
                let position = matches.lhs.at(i).offset_from_unsigned(lhs);
                // Tuples that take no memory are one at most, the first.
                let position = position.checked_div(size).unwrap_or(0);
                marks[position / 64] |= 1 << (position % 64);
            }
            Ok(())
        })?;
        // The bits of the tuples kept, a word at a time, those past the last clear.
        if negated {
            for word in marks.iter_mut() {
                *word = !*word;
            }
            marks[lhs_count / 64] &= (1 << (lhs_count % 64)) - 1;
        }
        let kept = marks.iter().map(|word| word.count_ones() as usize).sum();
        // In the order they are in, each stretch of them together.
        let (bytes, len) = made_in_order(kept, lhs_desc, |out| {
            let mut written = 0;
            let mut stretch: Option<(usize, usize)> = None;
            let mut copy = |(start, end): (usize, usize)| {
                let bytes = (end - start) * size;
                core::ptr::copy_nonoverlapping(lhs.add(start * size), out.add(written), bytes);
                written += bytes;
            };
            for (word, &bits) in marks.iter().enumerate() {
                let mut bits = bits;
                while bits != 0 {
                    let first = bits.trailing_zeros() as usize;
                    let run = (bits >> first).trailing_ones() as usize;
                    let (start, end) = (word * 64 + first, word * 64 + first + run);
                    stretch = match stretch {
                        Some((from, to)) if to == start => Some((from, end)),
                        Some(done) => {
                            copy(done);
                            Some((start, end))
                        }
                        None => Some((start, end)),
                    };
                    bits = if first + run == 64 {
                        0
                    } else {
                        bits & (u64::MAX << (first + run))
                    };
                }
            }
            if let Some(done) = stretch {
                copy(done);
            }
        })?;
        dst.write_unaligned(Text { bytes, len });
        Ok(())
    })
}

/// Store at `dst` the transitive closure of the relation at `src`, of two
/// attributes of one type, of the type `desc` describes, in memory the
/// statement owns: a tuple for each path through its tuples, each an arc
/// from its first attribute's value, in name order, to its second's, from
/// the path's first value to its last, as the compiler computes it
/// (`hir::Value::tclose`). Each value reaches what `reaching` says, in order,
/// so the tuples are written in canonical order, once each, with nothing to
/// sort. Fails if the memory can't be allocated.
///
/// # Safety
///
/// `src` must point to a valid relation of the type `desc` describes, a
/// relation of two attributes of one type, and `dst` to memory for one.
/// The statement must have begun a scope.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_tclose(
    dst: *mut Text,
    src: *const u8,
    desc: *const TypeDesc,
) -> bool {
    // SAFETY: guaranteed by the caller; each size and offset is the layout
    // of the value it describes.
    status(|| unsafe {
        let desc = &*desc;
        let element = desc.element();
        let [from, to] = attributes(element) else {
            internal_error("the transitive closure of a relation without two attributes");
        };
        let value = &*from.desc;
        let (tuples, count) = read_slice(src);
        let laid = Ordered::Laid {
            at: tuples,
            size: element.size,
        };
        let arcs = Arcs::of(count, laid, [from.offset, to.offset], value)?;
        let reached = reaching(&arcs)?;
        let numbered = arcs.into_values();
        let values = numbered.values();
        let total = (0..values.len()).try_fold(0_usize, |sum, number| {
            sum.checked_add(reached.of(number).len())
        });
        let Some(total) = total else {
            return Err(out_of_memory());
        };
        let (bytes, len) = made_in_order(total, desc, |made| {
            let mut k = 0;
            for (number, start) in values.iter().enumerate() {
                for &end in reached.of(number) {
                    let tuple = made.add(k * element.size);
                    core::ptr::copy_nonoverlapping(*start, tuple.add(from.offset), value.size);
                    core::ptr::copy_nonoverlapping(
                        values[end as usize],
                        tuple.add(to.offset),
                        value.size,
                    );
                    k += 1;
                }
            }
        })?;
        dst.write_unaligned(Text { bytes, len });
        Ok(())
    })
}

/// Store at `dst` the tuples of the transitive closure of the relation at `src`,
/// as for [`trendy_rt_tclose`], whose attribute `*attribute`, 0 or 1 by name order,
/// is the value at `value`, in memory the statement owns: following only paths from
/// that value, or to it, rather than making the whole closure (`closure_of`).
/// Fails if the memory can't be allocated.
///
/// # Safety
///
/// `src` must point to a valid relation of the type `desc` describes, a relation
/// of two attributes of one type, `value` to a valid value of it, if the relation
/// has a tuple, and `dst` to memory for a relation. The statement must have begun a
/// scope.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_tclose_of(
    dst: *mut Text,
    src: *const u8,
    value: *const u8,
    desc: *const TypeDesc,
    attribute: *const usize,
) -> bool {
    // SAFETY: guaranteed by the caller.
    status(|| unsafe {
        let forward = attribute.read_unaligned() == 0;
        let (bytes, len) = closure_of(src, &*desc, forward, &[value])?;
        dst.write_unaligned(Text { bytes, len });
        Ok(())
    })
}

/// Store at `dst` the tuples of the transitive closure of the relation at `src`,
/// as for [`trendy_rt_tclose`], whose attribute `common` says is a value in the
/// relation at `values`, which `values_desc` describes, in memory the statement owns:
/// following only paths from those values, or to them, rather than making the whole
/// closure (`closure_of`). Fails if the memory can't be allocated.
///
/// # Safety
///
/// `src` must point to a valid relation of the type `desc` describes, a relation of
/// two attributes of one type, `values` to a valid relation, `common` to where an
/// attribute of that type is in each, `lhs` for the first, and `dst` to memory for a
/// relation. The statement must have begun a scope.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_tclose_of_each(
    dst: *mut Text,
    src: *const u8,
    values: *const u8,
    desc: *const TypeDesc,
    values_desc: *const TypeDesc,
    common: *const Common,
) -> bool {
    // SAFETY: guaranteed by the caller; each size and offset is the layout
    // of the value it describes.
    status(|| unsafe {
        let (desc, common) = (&*desc, &*common);
        let [first, _] = attributes(desc.element()) else {
            internal_error("the transitive closure of a relation without two attributes");
        };
        let value = &*common.desc;
        let (tuples, count) = read_slice(values);
        let size = (*values_desc).element().size;
        let mut starts = pointers(tuples.wrapping_add(common.rhs), count, size)?;
        let starts = starts.values();
        starts.sort_unstable_by(|a, b| order(*a, *b, value));
        let distinct = deduplicated(starts, value);
        let forward = common.lhs == first.offset;
        let (bytes, len) = closure_of(src, desc, forward, &starts[..distinct])?;
        dst.write_unaligned(Text { bytes, len });
        Ok(())
    })
}

/// The tuples of the transitive closure of the relation at `src`, which `desc`
/// describes, whose first attribute by name order is one of `starts`, if `forward`,
/// and otherwise whose second is, in memory the current statement owns, and how many
/// there are: following only paths from those values, or to them, rather than making
/// the whole closure. The tuples are in order by the first attribute, so paths from a
/// value find each arc by binary search, and paths to one sort them by the second
/// first. Fails if there is no memory.
///
/// # Safety
///
/// `src` must point to a valid relation `desc` describes, of two attributes of one
/// type, and each of `starts` to a valid value of it, if the relation has a tuple,
/// `starts` being in order and each only once.
#[cfg(not(test))]
unsafe fn closure_of(
    src: *const u8,
    desc: &TypeDesc,
    forward: bool,
    starts: &[*const u8],
) -> Fallible<(*mut u8, usize)> {
    // SAFETY: guaranteed by the caller; each size and offset is the layout
    // of the value it describes.
    unsafe {
        let element = desc.element();
        let [first, second] = attributes(element) else {
            internal_error("the transitive closure of a relation without two attributes");
        };
        let (tuples, count) = read_slice(src);
        if forward {
            let tuples = Ordered::Laid {
                at: tuples,
                size: element.size,
            };
            let paths = Paths::new(count, tuples, first, second)?;
            paths_written(paths, starts, desc, true)
        } else {
            let mut by_second = pointers(tuples, count, element.size)?;
            let by_second = by_second.values();
            by_second.sort_unstable_by(|a, b| {
                order(a.add(second.offset), b.add(second.offset), &*second.desc)
            });
            let paths = Paths::new(count, Ordered::Pointed(by_second), second, first)?;
            paths_written(paths, starts, desc, false)
        }
    }
}

/// The tuples of the relation `desc` describes for what `paths` find from `starts`,
/// in memory the current statement owns, and how many there are, in canonical order
/// as they are found if `in_order` says so. Fails if there is no memory.
///
/// # Safety
///
/// As for [`Paths::follow`], with `starts` in order and each only once, and `desc` must
/// be of tuples of the two attributes `paths` follows.
#[cfg(not(test))]
unsafe fn paths_written(
    mut paths: Paths<'_>,
    starts: &[*const u8],
    desc: &TypeDesc,
    in_order: bool,
) -> Fallible<(*mut u8, usize)> {
    // SAFETY: guaranteed by the caller; the first of each array that says so is
    // written.
    unsafe {
        let written = Written {
            desc,
            given: paths.from,
            other: paths.to,
        };
        if let [start] = *starts {
            let (ends, len) = paths.ends_from(start)?;
            let ends = core::slice::from_raw_parts(ends.ptr(), len);
            return written.tuples(starts, &[len], ends, true);
        }
        let arcs = paths.arcs_from(starts)?;
        let (kept, numbers, count) = numbered_starts(&arcs, starts, &*paths.from.desc)?;
        if count == 0 {
            return written.tuples(&[], &[], &[], true);
        }
        let numbers = core::slice::from_raw_parts(numbers.ptr(), count);
        let (ends, until) = ends_from_each(&arcs, numbers)?;
        written.tuples(
            core::slice::from_raw_parts(kept.ptr(), count),
            core::slice::from_raw_parts(until.ptr(), count),
            core::slice::from_raw_parts(ends.memory.ptr(), ends.len),
            in_order,
        )
    }
}

/// The tuples of a closure's that follow paths from values of one of its attributes,
/// `given`, to the other's, `other`.
#[cfg(not(test))]
struct Written<'a> {
    desc: &'a TypeDesc,
    given: &'a Attr,
    other: &'a Attr,
}

#[cfg(not(test))]
impl Written<'_> {
    /// A tuple for each of `starts` and each of `ends` up to where its `until` says,
    /// after those of the one before it, in memory the current statement owns, and
    /// how many there are. They are in canonical order as they come, if `in_order`
    /// says so, and otherwise are sorted, taking that memory twice over while they are.
    /// Fails if there is no memory.
    ///
    /// # Safety
    ///
    /// Each of `starts` and `ends` must point to a valid value of the attributes' type,
    /// and `until` have one for each of `starts`, none less than the one before it, and
    /// none more than there are of `ends`.
    unsafe fn tuples(
        &self,
        starts: &[*const u8],
        until: &[usize],
        ends: &[*const u8],
        in_order: bool,
    ) -> Fallible<(*mut u8, usize)> {
        // SAFETY: guaranteed by the caller; `made` has room for a tuple for each of
        // `ends`.
        unsafe {
            let size = self.desc.element().size;
            let value_size = (*self.given.desc).size;
            let write = |made: *mut u8| {
                let mut k = 0;
                for (start, until) in starts.iter().zip(until) {
                    while k < *until {
                        let tuple = made.add(k * size);
                        let at = tuple.add(self.given.offset);
                        core::ptr::copy_nonoverlapping(*start, at, value_size);
                        let at = tuple.add(self.other.offset);
                        core::ptr::copy_nonoverlapping(ends[k], at, value_size);
                        k += 1;
                    }
                }
            };
            if in_order {
                return made_in_order(ends.len(), self.desc, write);
            }
            let Some(bytes) = ends.len().checked_mul(size) else {
                return Err(out_of_memory());
            };
            let unsorted = Temp::<u8>::zeroed(bytes)?;
            write(unsorted.ptr());
            canonical(unsorted.ptr(), ends.len(), self.desc)
        }
    }
}

/// Those of `starts`, in order, that are among the values `arcs` numbers, as the others
/// reach nothing, their numbers, and how many there are. Fails if there is no memory.
///
/// # Safety
///
/// Each of `starts` must point to a valid value of the type `value` describes, as the
/// values of `arcs` must, and be in order.
#[cfg(not(test))]
unsafe fn numbered_starts(
    arcs: &Arcs,
    starts: &[*const u8],
    value: &TypeDesc,
) -> Fallible<(Temp<*const u8>, Temp<u32>, usize)> {
    // SAFETY: guaranteed by the caller; no more are kept than there are of `starts`.
    unsafe {
        let kept = Temp::<*const u8>::new(starts.len().max(1))?;
        let numbers = Temp::<u32>::new(starts.len().max(1))?;
        let values = core::slice::from_raw_parts(arcs.values.ptr(), arcs.count);
        let (mut count, mut at) = (0, 0);
        for &start in starts {
            at += values[at..].partition_point(|v| order(*v, start, value).is_lt());
            if at < values.len() && order(values[at], start, value).is_eq() {
                kept.ptr().add(count).write(start);
                numbers.ptr().add(count).write(number_of(at));
                count += 1;
            }
        }
        Ok((kept, numbers, count))
    }
}

/// What paths through `arcs` from each of `starts`, by their numbers, reach, in order,
/// one start's after another, and where each start's stop. Each batch of 64 starts is
/// one pass through the components their paths come to, a bit for each start in a word
/// for each value, so that a batch takes about as long as one start would, and nothing
/// is kept of what isn't one's. Fails if there is no memory.
#[cfg(not(test))]
fn ends_from_each(arcs: &Arcs, starts: &[u32]) -> Fallible<(Growing<*const u8>, Temp<usize>)> {
    let count = arcs.count;
    let Some(fields) = count.checked_mul(2) else {
        return Err(out_of_memory());
    };
    let mut of_value = Temp::<u32>::zeroed(count.max(1))?;
    let mut memory = Temp::<u32>::zeroed(fields + 1)?;
    // SAFETY: they are written, as zeros.
    let (members, member_starts) = unsafe { memory.values() }.split_at_mut(count);
    let mut found = Components {
        // SAFETY: as above.
        of_value: unsafe { of_value.values() },
        members,
        member_starts,
        count: 0,
    };
    walk_components(arcs, &mut found, |_, _| Ok(()))?;
    let mut seeded = Temp::<u64>::zeroed(count.max(1))?;
    let mut reached = Temp::<u64>::zeroed(count.max(1))?;
    let mut stamps = Temp::<u32>::zeroed(found.count.max(1))?;
    let mut components = Temp::<u32>::zeroed(found.count.max(1))?;
    let mut touched = Temp::<u32>::zeroed(count.max(1))?;
    // SAFETY: they are written, as zeros.
    let mut batch = unsafe {
        Batch {
            arcs,
            found: &found,
            seeded: seeded.values(),
            reached: reached.values(),
            stamps: stamps.values(),
            components: components.values(),
            touched: touched.values(),
        }
    };
    let until = Temp::<usize>::new(starts.len().max(1))?;
    let mut ends = Growing::<*const u8>::new()?;
    for (number, chunk) in starts.chunks(64).enumerate() {
        let values_reached = batch.reach(chunk, number_of(number + 1));
        let mut sizes = [0_usize; 64];
        let touched = &mut batch.touched[..values_reached];
        touched.sort_unstable();
        for &value in &*touched {
            each_bit(batch.reached[value as usize], |bit| sizes[bit] += 1);
        }
        let mut places = [0_usize; 64];
        let mut place = ends.len;
        for (bit, _) in chunk.iter().enumerate() {
            places[bit] = place;
            place += sizes[bit];
            // SAFETY: there are as many as there are of `starts`.
            unsafe { until.ptr().add(number * 64 + bit).write(place) };
        }
        ends.make_room(place)?;
        for &value in &*touched {
            let at = &mut batch.reached[value as usize];
            // SAFETY: each value is numbered, and there is room for the end of each
            // that `sizes` counted, as `places` keeps within.
            each_bit(*at, |bit| unsafe {
                let end = arcs.values.ptr().add(value as usize).read();
                ends.memory.ptr().add(places[bit]).write(end);
                places[bit] += 1;
            });
            *at = 0;
        }
        ends.len = place;
        for &start in chunk {
            batch.seeded[start as usize] = 0;
        }
    }
    Ok((ends, until))
}

/// The memory of [`ends_from_each`]'s passes, one for each batch of starts.
#[cfg(not(test))]
struct Batch<'a> {
    arcs: &'a Arcs,
    found: &'a Components<'a>,
    /// Of each value, the bit of the start of the batch it is, if it is one.
    seeded: &'a mut [u64],
    /// Of each value, a bit for each start of the batch whose paths reach it.
    reached: &'a mut [u64],
    /// Of each component, the last batch to come to it, and one more.
    stamps: &'a mut [u32],
    /// The components this batch's paths come to.
    components: &'a mut [u32],
    /// The values this batch's paths reach.
    touched: &'a mut [u32],
}

#[cfg(not(test))]
impl Batch<'_> {
    /// Come to the component of `value`, unless batch `stamp` has, after the first
    /// `come_to`: how many there now are.
    fn come_to(&mut self, value: u32, stamp: u32, come_to: usize) -> usize {
        let of = self.found.of(value);
        if self.stamps[of] == stamp {
            return come_to;
        }
        self.stamps[of] = stamp;
        self.components[come_to] = number_of(of);
        come_to + 1
    }

    /// Set a bit for each of `starts`, none of which has been in this batch, in what
    /// each reaches, and how many values those are, in `touched`. A component is
    /// found after those it leads to, so going through them backwards, each has all
    /// that lead to it first, and the word for all its values is all it hands on.
    fn reach(&mut self, starts: &[u32], stamp: u32) -> usize {
        let (arcs, found) = (self.arcs, self.found);
        let mut come_to = 0;
        for (bit, &start) in starts.iter().enumerate() {
            self.seeded[start as usize] = 1 << bit;
            come_to = self.come_to(start, stamp, come_to);
        }
        let mut next = 0;
        while next < come_to {
            let component = self.components[next] as usize;
            next += 1;
            for &member in found.members_of(component) {
                for &end in arcs.ends_of(member) {
                    come_to = self.come_to(end, stamp, come_to);
                }
            }
        }
        self.components[..come_to].sort_unstable_by(|a, b| b.cmp(a));
        let mut touches = 0;
        for at in 0..come_to {
            let members = found.members_of(self.components[at] as usize);
            let word = members.iter().fold(0, |word, &member| {
                word | self.seeded[member as usize] | self.reached[member as usize]
            });
            for &member in members {
                for &end in arcs.ends_of(member) {
                    let reached = &mut self.reached[end as usize];
                    if *reached == 0 {
                        self.touched[touches] = end;
                        touches += 1;
                    }
                    *reached |= word;
                }
            }
        }
        touches
    }
}

/// `act` with each bit that is set in `word`, from the lowest.
#[cfg(not(test))]
fn each_bit(mut word: u64, mut act: impl FnMut(usize)) {
    while word != 0 {
        act(word.trailing_zeros() as usize);
        word &= word - 1;
    }
}

/// A relation's tuples, in some order.
#[cfg(not(test))]
#[derive(Clone, Copy)]
enum Ordered<'a> {
    /// One after another, each `size` bytes, from `at`.
    Laid { at: *const u8, size: usize },
    /// Each where it says.
    Pointed(&'a [*const u8]),
    /// Laid out as for [`Ordered::Laid`], in the order of their positions.
    Positions {
        at: *const u8,
        size: usize,
        positions: &'a [u32],
    },
}

#[cfg(not(test))]
impl Ordered<'_> {
    /// The tuple numbered `i`.
    ///
    /// # Safety
    ///
    /// There must be one.
    #[inline]
    unsafe fn at(self, i: usize) -> *const u8 {
        // SAFETY: guaranteed by the caller. A merge reads these more than it does
        // anything else, so they aren't checked again.
        unsafe {
            match self {
                Ordered::Laid { at, size } => at.add(i * size),
                Ordered::Pointed(pointers) => *pointers.get_unchecked(i),
                Ordered::Positions {
                    at,
                    size,
                    positions,
                } => at.add(*positions.get_unchecked(i) as usize * size),
            }
        }
    }
}

/// A search through arcs, each followed once, from each value it comes to, however
/// many ways lead to it. Each is one of `count` of `tuples`, in order by the attribute
/// `from`, an arc from that attribute's value to the attribute `to`'s.
#[cfg(not(test))]
struct Paths<'a> {
    count: usize,
    tuples: Ordered<'a>,
    from: &'a Attr,
    to: &'a Attr,
    /// A bit for each tuple, set if it is the first of those from its value, and
    /// they have been followed.
    followed: Temp<u64>,
    /// The first tuple of those from each value the search has come to.
    waiting: Temp<usize>,
}

#[cfg(not(test))]
impl<'a> Paths<'a> {
    /// The search of `count` of `tuples`. Fails if there is no memory.
    fn new(count: usize, tuples: Ordered<'a>, from: &'a Attr, to: &'a Attr) -> Fallible<Self> {
        Ok(Paths {
            count,
            tuples,
            from,
            to,
            followed: Temp::zeroed(count / 64 + 1)?,
            waiting: Temp::new(count.max(1))?,
        })
    }

    /// The value at the attribute `from` of the tuple numbered `i`.
    ///
    /// # Safety
    ///
    /// `i` must be below `count`, and the tuples valid.
    unsafe fn source(&self, i: usize) -> *const u8 {
        // SAFETY: guaranteed by the caller.
        unsafe { self.tuples.at(i).add(self.from.offset) }
    }

    /// The first tuple of those from `value`, unless none are, or they have been
    /// followed, to follow next, as they now have been.
    ///
    /// # Safety
    ///
    /// The tuples must be valid, and `value` must point to a valid value of their
    /// attributes' type, if there are any.
    unsafe fn first_from(&self, value: *const u8) -> Option<usize> {
        // SAFETY: guaranteed by the caller; `followed` has a bit for each tuple.
        unsafe {
            let of_type = &*self.from.desc;
            let run = partition_point(0, self.count, |i| {
                order(self.source(i), value, of_type).is_lt()
            });
            if run == self.count || order(self.source(run), value, of_type).is_ne() {
                return None;
            }
            let word = self.followed.ptr().add(run / 64);
            let bit = 1_u64 << (run % 64);
            (word.read() & bit == 0).then(|| {
                word.write(word.read() | bit);
                run
            })
        }
    }

    /// Follow each arc that a path from one of `starts` goes through, once, giving
    /// `arc` the tuple of each: how many values were followed from, each the first of
    /// its tuples in `waiting`.
    ///
    /// # Safety
    ///
    /// The tuples must be valid, and each of `starts` must point to a valid value of
    /// their attributes' type, if there are any.
    unsafe fn follow(&self, starts: &[*const u8], mut arc: impl FnMut(*const u8)) -> usize {
        // SAFETY: guaranteed by the caller; each pointer is to a value of the
        // attributes' type, and no more than `count` are in `waiting`, as each value's
        // are followed once.
        unsafe {
            let value = &*self.from.desc;
            let waiting = self.waiting.ptr();
            let mut waits = 0;
            for start in starts {
                if let Some(run) = self.first_from(*start) {
                    waiting.add(waits).write(run);
                    waits += 1;
                }
            }
            let mut next = 0;
            while next < waits {
                let run = waiting.add(next).read();
                next += 1;
                let mut i = run;
                while i < self.count && order(self.source(i), self.source(run), value).is_eq() {
                    let tuple = self.tuples.at(i);
                    arc(tuple);
                    if let Some(run) = self.first_from(tuple.add(self.to.offset)) {
                        waiting.add(waits).write(run);
                        waits += 1;
                    }
                    i += 1;
                }
            }
            waits
        }
    }

    /// The values that paths from `start` reach, in order, once each, and how many,
    /// each where it is in its tuple, so that it lasts as long as they do. Fails if
    /// there is no memory.
    ///
    /// # Safety
    ///
    /// As for [`Paths::follow`].
    unsafe fn ends_from(&mut self, start: *const u8) -> Fallible<(Temp<*const u8>, usize)> {
        // SAFETY: guaranteed by the caller; no more than `count` arcs are followed.
        unsafe {
            let found = Temp::<*const u8>::new(self.count.max(1))?;
            let mut len = 0;
            let offset = self.to.offset;
            self.follow(&[start], |tuple| {
                found.ptr().add(len).write(tuple.add(offset));
                len += 1;
            });
            let value = &*self.from.desc;
            let values = core::slice::from_raw_parts_mut(found.ptr(), len);
            values.sort_unstable_by(|a, b| order(*a, *b, value));
            let kept = deduplicated(values, value);
            Ok((found, kept))
        }
    }

    /// The arcs that paths from `starts` go through, numbered, each value's together.
    /// Fails if there is no memory.
    ///
    /// # Safety
    ///
    /// As for [`Paths::follow`], and the relation must outlive what this gives.
    unsafe fn arcs_from(&mut self, starts: &[*const u8]) -> Fallible<Arcs> {
        // SAFETY: guaranteed by the caller; no more than `count` arcs are followed,
        // and their first tuples, in order, keep the tuples in order by `from`.
        unsafe {
            let mut arcs = 0;
            let waits = self.follow(starts, |_| arcs += 1);
            core::slice::from_raw_parts_mut(self.waiting.ptr(), waits).sort_unstable();
            let found = Temp::<*const u8>::new(arcs.max(1))?;
            let value = &*self.from.desc;
            let mut len = 0;
            for at in 0..waits {
                let run = self.waiting.ptr().add(at).read();
                let mut i = run;
                while i < self.count && order(self.source(i), self.source(run), value).is_eq() {
                    found.ptr().add(len).write(self.tuples.at(i));
                    len += 1;
                    i += 1;
                }
            }
            let found = core::slice::from_raw_parts(found.ptr(), len);
            Arcs::of(
                len,
                Ordered::Pointed(found),
                [self.from.offset, self.to.offset],
                value,
            )
        }
    }
}

/// How many values `values`, in order, have once each, moved to where they start.
///
/// # Safety
///
/// Each must point to a valid value of the type `value` describes.
#[cfg(not(test))]
unsafe fn deduplicated(values: &mut [*const u8], value: &TypeDesc) -> usize {
    // SAFETY: guaranteed by the caller.
    unsafe {
        let mut kept = 0;
        for i in 0..values.len() {
            if kept == 0 || order(values[kept - 1], values[i], value).is_ne() {
                values[kept] = values[i];
                kept += 1;
            }
        }
        kept
    }
}

/// The arcs of a relation of two attributes of one type, with its values
/// numbered in canonical order, so that in that order is how they compare. Its
/// numbers take 32 bits, as there are many, as a relation with more values takes
/// more memory for them than is likely to be there: it fails for lack of it.
#[cfg(not(test))]
struct Arcs {
    /// How many values there are.
    count: usize,
    /// Each value, by where it is in the relation, which outlives this.
    values: Temp<*const u8>,
    /// Where each value's arcs begin in `ends`, and then where the last ends.
    starts: Temp<usize>,
    /// The value each arc ends at, each value's in order.
    ends: Temp<u32>,
}

#[cfg(not(test))]
impl Arcs {
    /// The arcs of `len` of `tuples`, in canonical order, from the value of the type
    /// `value` describes at the first of `offsets` in each to the one at the second.
    /// Fails if there is no memory.
    ///
    /// # Safety
    ///
    /// There must be that many, in order by the first, and the relation must outlive
    /// this.
    unsafe fn of(
        len: usize,
        tuples: Ordered<'_>,
        offsets: [usize; 2],
        value: &TypeDesc,
    ) -> Fallible<Arcs> {
        let Some(both) = len.checked_mul(2) else {
            return Err(out_of_memory());
        };
        // SAFETY: guaranteed by the caller; each is a value of that type, and
        // there are at most `both` of them, which the memory has room for.
        unsafe {
            let end_of = |i: usize| tuples.at(i).add(offsets[1]);
            let by_end = Temp::<usize>::new(len.max(1))?;
            for i in 0..len {
                by_end.ptr().add(i).write(i);
            }
            let by_end = core::slice::from_raw_parts_mut(by_end.ptr(), len);
            by_end.sort_unstable_by(|a, b| order(end_of(*a), end_of(*b), value));
            let start_of = |i: usize| tuples.at(i).add(offsets[0]);
            let values = Temp::<*const u8>::new(both.max(1))?;
            let starts = Temp::<usize>::new(both + 1)?;
            let ends = Temp::<u32>::new(len.max(1))?;
            // The tuples' first values are in order, as the relation's are, and
            // the ends now are: the two go through the values together.
            let (mut count, mut i, mut j) = (0, 0, 0);
            while i < len || j < len {
                let next = if j == len
                    || (i < len && order(start_of(i), end_of(by_end[j]), value).is_le())
                {
                    start_of(i)
                } else {
                    end_of(by_end[j])
                };
                let Some(number) = u32::try_from(count).ok().filter(|n| *n < u32::MAX) else {
                    return Err(out_of_memory());
                };
                values.ptr().add(count).write(next);
                starts.ptr().add(count).write(i);
                while i < len && order(start_of(i), next, value).is_eq() {
                    i += 1;
                }
                while j < len && order(end_of(by_end[j]), next, value).is_eq() {
                    ends.ptr().add(by_end[j]).write(number);
                    j += 1;
                }
                count += 1;
            }
            starts.ptr().add(count).write(len);
            Ok(Arcs {
                count,
                values,
                starts,
                ends,
            })
        }
    }

    /// Each value, by its number, those of the arcs given up.
    fn into_values(self) -> Numbered {
        Numbered {
            count: self.count,
            values: self.values,
        }
    }

    /// The values the arcs from `value` end at, in order.
    fn ends_of(&self, value: u32) -> &[u32] {
        let value = value as usize;
        // SAFETY: each value's arcs are written, and `starts` says where.
        unsafe {
            let (start, end) = (
                *self.starts.ptr().add(value),
                *self.starts.ptr().add(value + 1),
            );
            core::slice::from_raw_parts(self.ends.ptr().add(start), end - start)
        }
    }
}

/// Each value of a relation's arcs, by its number (see [`Arcs`]).
#[cfg(not(test))]
struct Numbered {
    count: usize,
    values: Temp<*const u8>,
}

#[cfg(not(test))]
impl Numbered {
    /// Each value, by its number.
    fn values(&self) -> &[*const u8] {
        // SAFETY: the first `count` are written.
        unsafe { core::slice::from_raw_parts(self.values.ptr(), self.count) }
    }
}

/// Which values a path through arcs reaches from each, by the strongly
/// connected component each is in, as all of a component's reach the same.
#[cfg(not(test))]
struct Reached {
    /// The component of each value, and one more.
    components: Temp<u32>,
    /// Where each component's values begin in `values`, and then where the last
    /// ends.
    starts: Temp<usize>,
    /// The values each component's reach, in order.
    values: Numbers,
}

#[cfg(not(test))]
impl Reached {
    /// The values that `value` reaches, in order.
    fn of(&self, value: usize) -> &[u32] {
        // SAFETY: each value's component is written, and each component's values,
        // and `starts` says where.
        unsafe {
            let component = *self.components.ptr().add(value) as usize - 1;
            let (start, end) = (
                *self.starts.ptr().add(component),
                *self.starts.ptr().add(component + 1),
            );
            core::slice::from_raw_parts(self.values.memory.ptr().add(start), end - start)
        }
    }
}

/// Values, in memory of a function's own, which grows as they are added.
#[cfg(not(test))]
struct Growing<T> {
    memory: Temp<T>,
    len: usize,
}

/// Numbers, in memory of a function's own, which grows as they are added.
#[cfg(not(test))]
type Numbers = Growing<u32>;

#[cfg(not(test))]
impl<T: Copy> Growing<T> {
    /// None yet, with room for a few. Fails if there is no memory for them.
    fn new() -> Fallible<Self> {
        Self::with_room(4)
    }

    /// None yet, with room for `room` or a few. Fails if there is no memory for them.
    fn with_room(room: usize) -> Fallible<Self> {
        Ok(Growing {
            memory: Temp::new(room.max(4))?,
            len: 0,
        })
    }

    /// Have room for `len` values, the most yet. Fails if there is no memory for them.
    fn make_room(&mut self, len: usize) -> Fallible {
        if len > self.memory.len {
            self.memory
                .grow(len.max(self.memory.len.saturating_mul(2)))?;
        }
        Ok(())
    }

    /// Add `value`, after those so far. Fails if there is no memory for it.
    fn push(&mut self, value: T) -> Fallible {
        self.make_room(self.len.saturating_add(1))?;
        // SAFETY: the memory has room for one more.
        unsafe { self.memory.ptr().add(self.len).write(value) };
        self.len += 1;
        Ok(())
    }

    /// The value at `index`, one of those added.
    fn at(&self, index: usize) -> T {
        // SAFETY: the first `len` are written.
        unsafe { *self.memory.ptr().add(index) }
    }
}

/// `n`, which counts values, of which [`Arcs`] has made sure a number can be.
#[cfg(not(test))]
fn number_of(n: usize) -> u32 {
    u32::try_from(n).unwrap_or_else(|_| internal_error("too many values"))
}

/// What each value reaches by `arcs`, by the components of the graph (Tarjan's
/// algorithm, without recursion of its own, so that no path is too long for the
/// stack), found as those they lead to are done: Nuutila's. A component reaches
/// what its arcs' ends are in, and what those reach, itself among them if a cycle
/// is in it. An end already among those a component reaches needs no looking through,
/// as it got there with all it reaches, so the work is about what is written, and
/// no more if arcs go round in cycles or by many ways to one value. Fails if there
/// is no memory for them.
#[cfg(not(test))]
fn reaching(arcs: &Arcs) -> Fallible<Reached> {
    let count = arcs.count;
    let Some(fields) = count.checked_mul(3) else {
        return Err(out_of_memory());
    };
    let mut memory = Temp::<u32>::zeroed(fields + 1)?;
    // SAFETY: they are written, as zeros.
    let memory = unsafe { memory.values() };
    let (seen, memory) = memory.split_at_mut(count);
    let (members, member_starts) = memory.split_at_mut(count);
    let mut components = Temp::<u32>::zeroed(count.max(1))?;
    let mut starts = Temp::<usize>::zeroed(count + 1)?;
    let mut found = Components {
        // SAFETY: they are written, as zeros.
        of_value: unsafe { components.values() },
        members,
        member_starts,
        count: 0,
    };
    let mut reaches = Reaches {
        arcs,
        seen,
        // SAFETY: as above.
        starts: unsafe { starts.values() },
        values: Numbers {
            memory: Temp::new(count.max(4))?,
            len: 0,
        },
    };
    walk_components(arcs, &mut found, |found, component| {
        reaches.of_component(found, component)
    })?;
    let Reaches { values, .. } = reaches;
    Ok(Reached {
        components,
        starts,
        values,
    })
}

/// The strongly connected components of `arcs`, in `found`, each with those it leads to
/// before it (Tarjan's algorithm, without recursion of its own, so that no path is too
/// long for the stack), calling `done` with each as it is found. Fails if there is no
/// memory, or as `done` does.
#[cfg(not(test))]
fn walk_components(
    arcs: &Arcs,
    found: &mut Components<'_>,
    mut done: impl FnMut(&Components<'_>, usize) -> Fallible,
) -> Fallible {
    let count = arcs.count;
    let Some(fields) = count.checked_mul(5) else {
        return Err(out_of_memory());
    };
    let mut memory = Temp::<u32>::zeroed(fields.max(1))?;
    // SAFETY: they are written, as zeros.
    let memory = unsafe { memory.values() };
    let (number, memory) = memory.split_at_mut(count);
    let (low, memory) = memory.split_at_mut(count);
    let (next, memory) = memory.split_at_mut(count);
    let (calls, waiting) = memory.split_at_mut(count);
    let mut walk = Walk {
        number,
        low,
        calls,
        waiting,
        numbered: 0,
        depth: 0,
        waits: 0,
    };
    for root in 0..count {
        if walk.number[root] != 0 {
            continue;
        }
        walk.visit(number_of(root));
        while walk.depth > 0 {
            let at = walk.calls[walk.depth - 1];
            let here = at as usize;
            if let Some(&end) = arcs.ends_of(at).get(next[here] as usize) {
                next[here] += 1;
                let reached = end as usize;
                if walk.number[reached] == 0 {
                    walk.visit(end);
                } else if found.of_value[reached] == 0 {
                    walk.low[here] = walk.low[here].min(walk.number[reached]);
                }
                continue;
            }
            walk.depth -= 1;
            if walk.depth > 0 {
                let caller = walk.calls[walk.depth - 1] as usize;
                walk.low[caller] = walk.low[caller].min(walk.low[here]);
            }
            if walk.low[here] == walk.number[here] {
                let first = walk.waiting[..walk.waits]
                    .iter()
                    .rposition(|&value| value == at)
                    .unwrap_or_else(|| internal_error("a value that isn't waiting"));
                let component = found.add(&walk.waiting[first..walk.waits]);
                done(found, component)?;
                walk.waits = first;
            }
        }
    }
    Ok(())
}

/// Where [`walk_components`]'s walk through the arcs is.
#[cfg(not(test))]
struct Walk<'a> {
    /// The order each value was first come to in, from 1, or 0 if it hasn't been.
    number: &'a mut [u32],
    /// Of each value, the lowest number of a value it is known to lead back to,
    /// that has no component yet.
    low: &'a mut [u32],
    /// The values being walked from, `depth` of them, each by an arc from the one
    /// before.
    calls: &'a mut [u32],
    /// The values that have been come to and have no component yet, `waits` of
    /// them.
    waiting: &'a mut [u32],
    numbered: u32,
    depth: usize,
    waits: usize,
}

#[cfg(not(test))]
impl Walk<'_> {
    /// Come to `value`, for the first time.
    fn visit(&mut self, value: u32) {
        self.numbered += 1;
        self.number[value as usize] = self.numbered;
        self.low[value as usize] = self.numbered;
        self.calls[self.depth] = value;
        self.depth += 1;
        self.waiting[self.waits] = value;
        self.waits += 1;
    }
}

/// The components [`walk_components`] has found.
#[cfg(not(test))]
struct Components<'a> {
    /// The component each value is in, and one more, or 0 if it hasn't one yet.
    of_value: &'a mut [u32],
    /// The values of each component, one after another.
    members: &'a mut [u32],
    /// Where each component's begin in `members`, and then where the last ends.
    member_starts: &'a mut [u32],
    /// How many there are.
    count: usize,
}

#[cfg(not(test))]
impl Components<'_> {
    /// Add `members`, a component, and say which it is.
    fn add(&mut self, members: &[u32]) -> usize {
        let component = self.count;
        let stamp = number_of(component + 1);
        let member_start = self.member_starts[component] as usize;
        let member_end = member_start + members.len();
        self.members[member_start..member_end].copy_from_slice(members);
        self.member_starts[component + 1] = number_of(member_end);
        for &member in members {
            self.of_value[member as usize] = stamp;
        }
        self.count += 1;
        component
    }

    /// The component `value` is in, which must have one.
    fn of(&self, value: u32) -> usize {
        self.of_value[value as usize] as usize - 1
    }

    /// The values of `component`.
    fn members_of(&self, component: usize) -> &[u32] {
        let (first, last) = (
            self.member_starts[component],
            self.member_starts[component + 1],
        );
        &self.members[first as usize..last as usize]
    }
}

/// What the components [`reaching`]'s walk has found reach.
#[cfg(not(test))]
struct Reaches<'a> {
    arcs: &'a Arcs,
    /// Of each value, the last component to reach it, and one more.
    seen: &'a mut [u32],
    /// Where each component's reached values begin in `values`, and then where
    /// the last ends.
    starts: &'a mut [usize],
    values: Numbers,
}

#[cfg(not(test))]
impl Reaches<'_> {
    /// What `component` reaches, in order, every other component it leads to having
    /// been found. Fails if there is no memory for them.
    fn of_component(&mut self, found: &Components<'_>, component: usize) -> Fallible {
        let stamp = number_of(component + 1);
        let start = self.values.len;
        let arcs = self.arcs;
        for &member in found.members_of(component) {
            for &end in arcs.ends_of(member) {
                if self.seen[end as usize] == stamp {
                    continue;
                }
                // Its own, if a cycle is in it, with nothing yet that it reaches.
                let of = found.of(end);
                for &member in found.members_of(of) {
                    self.add(member, stamp)?;
                }
                for i in self.starts[of]..self.starts[of + 1] {
                    self.add(self.values.at(i), stamp)?;
                }
            }
        }
        self.starts[component + 1] = self.values.len;
        // SAFETY: those from `start` on are written.
        unsafe {
            core::slice::from_raw_parts_mut(
                self.values.memory.ptr().add(start),
                self.values.len - start,
            )
        }
        .sort_unstable();
        Ok(())
    }

    /// Add `value` to what the component with the mark `stamp` reaches, unless it
    /// is there. Fails if there is no memory for it.
    fn add(&mut self, value: u32, stamp: u32) -> Fallible {
        if self.seen[value as usize] != stamp {
            self.seen[value as usize] = stamp;
            self.values.push(value)?;
        }
        Ok(())
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

/// Pointers to the `count` tuples or elements at `values`, each `size`
/// bytes, in order, in memory of the caller's own, for sorting. Fails if
/// there is no memory for them.
///
/// # Safety
///
/// `values` must point to `count` values of `size` bytes.
#[cfg(not(test))]
unsafe fn pointers(values: *const u8, count: usize, size: usize) -> Fallible<Temp<*const u8>> {
    let pointers = Temp::<*const u8>::new(count)?;
    for i in 0..count {
        // SAFETY: guaranteed by the caller; the memory has room for
        // `count` pointers.
        unsafe { pointers.ptr().add(i).write(values.add(i * size)) };
    }
    Ok(pointers)
}

/// An operand of a join or a semijoin.
#[cfg(not(test))]
#[derive(Clone, Copy)]
struct Operand {
    tuples: *const u8,
    count: usize,
    size: usize,
    /// Whether it is the left operand, which says where the common attributes are.
    is_lhs: bool,
    /// Whether its tuples are in order on the common attributes as they are: those
    /// attributes come first in name order, which is what they are sorted by.
    in_order: bool,
    /// How many integers its tuples are, if they can be sorted where they are, into that
    /// order: they are in no particular order, but would be if they were canonical, and
    /// only the current scope holds them. That takes less time and memory than sorting
    /// pointers to them.
    sorts_here: Option<usize>,
}

#[cfg(not(test))]
impl Operand {
    /// The `count` tuples at `tuples`, of the relation `desc` describes, as the left
    /// operand if `is_lhs` and otherwise the right, of an operation on `common`, in no
    /// particular order if `loose`.
    ///
    /// # Safety
    ///
    /// `desc` must describe a relation with those attributes, whose names are in the
    /// order `common` has them.
    unsafe fn of(
        common: &[Common],
        tuples: *const u8,
        count: usize,
        desc: &TypeDesc,
        is_lhs: bool,
        loose: bool,
    ) -> Operand {
        // SAFETY: guaranteed by the caller.
        let (attributes, size) = unsafe { (attributes(desc), desc.element().size) };
        // Two attributes are at one place only if the first takes no memory, so has
        // one value, which orders nothing.
        let leads = attributes
            .iter()
            .zip(common)
            .all(|(attr, c)| attr.offset == if is_lhs { c.lhs } else { c.rhs })
            && common.len() <= attributes.len();
        // SAFETY: guaranteed by the caller, which a relation of no tuples isn't asked of.
        let sorts_here = (loose
            && leads
            && count > 0
            && sorts_in_place(desc)
            && unsafe { only_in_scope(tuples.cast_mut()) }.is_some())
        .then_some(desc.integers);
        Operand {
            tuples,
            count,
            size,
            is_lhs,
            in_order: leads && !loose,
            sorts_here,
        }
    }

    /// Pointers to its tuples, in order on `common`, those that agree in the order they
    /// are in. Fails if there is no memory for them.
    ///
    /// # Safety
    ///
    /// Its tuples must be valid.
    unsafe fn sorted_on(self, common: &[Common]) -> Fallible<Temp<*const u8>> {
        // SAFETY: guaranteed by the caller; they are written, as `pointers` does.
        unsafe {
            let mut sorted = pointers(self.tuples, self.count, self.size)?;
            sorted.values().sort_unstable_by(|x, y| {
                on_common(common, (*x, self.is_lhs), (*y, self.is_lhs)).then(x.cmp(y))
            });
            Ok(sorted)
        }
    }

    /// Its tuples, as they are laid out.
    fn laid(self) -> Ordered<'static> {
        Ordered::Laid {
            at: self.tuples,
            size: self.size,
        }
    }

    /// Its tuples, in order on `common`: as they are, or otherwise through pointers to
    /// them, sorted, in `pointers`. Fails if there is no memory for them.
    ///
    /// # Safety
    ///
    /// Its tuples must be valid.
    unsafe fn ordered<'a>(
        self,
        common: &[Common],
        pointers: &'a mut Option<Temp<*const u8>>,
    ) -> Fallible<Ordered<'a>> {
        if self.in_order {
            return Ok(self.laid());
        }
        // SAFETY: guaranteed by the caller; only the current scope holds them, and what
        // reads them doesn't depend on the order they are in, which leaves them all there.
        unsafe {
            if let Some(integers) = self.sorts_here {
                KEPT.forget(self.tuples);
                sort_integers(self.tuples.cast_mut(), self.count, integers);
                return Ok(self.laid());
            }
            let sorted = pointers.insert(self.sorted_on(common)?);
            Ok(Ordered::Pointed(sorted.values()))
        }
    }
}

/// The first of `start..end` for which `before` is false, all those before it being
/// those for which it is true, looking from `start` by steps that double, so that it
/// takes about the logarithm of how far that is, rather than of all of it: one
/// comparison, if that is `start`.
#[cfg(not(test))]
fn gallop(start: usize, end: usize, before: impl Fn(usize) -> bool) -> usize {
    let (mut known, mut step) = (start, 1);
    loop {
        let next = known.saturating_add(step - 1).min(end);
        if next == end || !before(next) {
            return partition_point(known, next, before);
        }
        known = next + 1;
        step = step.saturating_mul(2);
    }
}

/// How to find the tuples of the two operands of a join or a semijoin that agree on the
/// common attributes. Where the larger is in order, as it is if those attributes come
/// first in it, or through an index it has earned ([`Kept`]), this gallops through it
/// from the smaller, so that the time follows the smaller. If the larger is much larger,
/// and in no such order, it is read as it is, never sorted, each tuple looking for its
/// match in the smaller. Otherwise both are sorted on them, and merged.
#[cfg(not(test))]
struct Matches<'a> {
    /// The left operand's tuples, in the order [`Group`]s say where they are in.
    lhs: Ordered<'a>,
    lhs_count: usize,
    /// The right operand's.
    rhs: Ordered<'a>,
    rhs_count: usize,
    /// If one is read as it is: that one, and the other, which has been sorted.
    scan: Option<(Operand, Operand)>,
}

/// How many times larger an operand must be to be read as it is, rather than sorted: with
/// less between them, what it takes to look for each of its tuples comes to more than
/// sorting them would have.
#[cfg(not(test))]
const SCANNED_FROM: usize = 8;

#[cfg(not(test))]
impl<'a> Matches<'a> {
    /// How to find those of `lhs` and `rhs`, on `common`, with `lhs_pointers` and
    /// `rhs_pointers` for what needs sorting. Fails if there is no memory.
    ///
    /// # Safety
    ///
    /// Their tuples must be valid, and `common` their attributes in common.
    unsafe fn of(
        common: &[Common],
        lhs: Operand,
        rhs: Operand,
        lhs_pointers: &'a mut Option<Temp<*const u8>>,
        rhs_pointers: &'a mut Option<Temp<*const u8>>,
    ) -> Fallible<Matches<'a>> {
        // SAFETY: guaranteed by the caller.
        unsafe {
            let larger = if lhs.count > rhs.count { lhs } else { rhs };
            let smaller = if lhs.count > rhs.count { rhs } else { lhs };
            if smaller.count == 0 {
                return Ok(Matches {
                    lhs: lhs.laid(),
                    lhs_count: lhs.count,
                    rhs: rhs.laid(),
                    rhs_count: rhs.count,
                    scan: None,
                });
            }
            let kept = if larger.in_order {
                None
            } else {
                KEPT.positions(common, larger, smaller.count)
            };
            let scanned =
                !larger.in_order && kept.is_none() && larger.count / SCANNED_FROM > smaller.count;
            let order = |side: Operand, pointers| match kept {
                Some(positions) if side.is_lhs == larger.is_lhs => Ok(Ordered::Positions {
                    at: side.tuples,
                    size: side.size,
                    positions,
                }),
                _ if scanned && side.is_lhs == larger.is_lhs => Ok(side.laid()),
                _ => side.ordered(common, pointers),
            };
            Ok(Matches {
                lhs: order(lhs, lhs_pointers)?,
                lhs_count: lhs.count,
                rhs: order(rhs, rhs_pointers)?,
                rhs_count: rhs.count,
                scan: scanned.then_some((larger, smaller)),
            })
        }
    }

    /// `found` with each run of tuples of one that agree with a run of the other. Fails
    /// if `found` does, or there is no memory.
    ///
    /// # Safety
    ///
    /// As for [`Matches::of`].
    unsafe fn each_group(
        &self,
        common: &[Common],
        found: &mut dyn FnMut(Group) -> Fallible,
    ) -> Fallible {
        // SAFETY: guaranteed by the caller.
        unsafe {
            match self.scan {
                Some((larger, smaller)) => self.scanning(common, larger, smaller, found),
                None => self.merging(common, found),
            }
        }
    }

    /// [`Matches::each_group`], both being in order on `common`. Merging compares
    /// little else, so the usual kinds of order are decided once rather than each time.
    ///
    /// # Safety
    ///
    /// As for [`Matches::of`].
    unsafe fn merging(
        &self,
        common: &[Common],
        found: &mut dyn FnMut(Group) -> Fallible,
    ) -> Fallible {
        let counts = (self.lhs_count, self.rhs_count);
        // SAFETY: guaranteed by the caller; each is of as many tuples as its count says.
        unsafe {
            match (self.lhs, self.rhs) {
                (Ordered::Pointed(lhs), Ordered::Pointed(rhs)) => merged(
                    common,
                    |i| *lhs.get_unchecked(i),
                    |j| *rhs.get_unchecked(j),
                    counts,
                    found,
                ),
                (
                    Ordered::Laid {
                        at: lhs,
                        size: lhs_size,
                    },
                    Ordered::Laid {
                        at: rhs,
                        size: rhs_size,
                    },
                ) => merged(
                    common,
                    |i| lhs.add(i * lhs_size),
                    |j| rhs.add(j * rhs_size),
                    counts,
                    found,
                ),
                (lhs, rhs) => merged(common, |i| lhs.at(i), |j| rhs.at(j), counts, found),
            }
        }
    }

    /// [`Matches::each_group`], reading `larger` as it is, each tuple looking in
    /// `smaller`, which is in order on `common`. A filter says of most that have no match
    /// that they haven't, without looking.
    ///
    /// # Safety
    ///
    /// As for [`Matches::of`].
    unsafe fn scanning(
        &self,
        common: &[Common],
        larger: Operand,
        smaller: Operand,
        found: &mut dyn FnMut(Group) -> Fallible,
    ) -> Fallible {
        use core::cmp::Ordering;
        // SAFETY: guaranteed by the caller.
        unsafe {
            let in_order = if smaller.is_lhs { self.lhs } else { self.rhs };
            let filter = Filter::of(common, smaller, in_order)?;
            for i in 0..larger.count {
                let tuple = larger.tuples.add(i * larger.size);
                if filter
                    .as_ref()
                    .is_some_and(|filter| !filter.may_have(common, tuple, larger.is_lhs))
                {
                    continue;
                }
                let against = |k: usize| {
                    on_common(
                        common,
                        (in_order.at(k), smaller.is_lhs),
                        (tuple, larger.is_lhs),
                    )
                };
                let start = partition_point(0, smaller.count, |k| against(k) == Ordering::Less);
                if start == smaller.count || against(start) != Ordering::Equal {
                    continue;
                }
                let end = gallop(start + 1, smaller.count, |k| against(k) == Ordering::Equal);
                found(if larger.is_lhs {
                    Group {
                        lhs_start: i,
                        lhs_end: i + 1,
                        rhs_start: start,
                        rhs_end: end,
                    }
                } else {
                    Group {
                        lhs_start: start,
                        lhs_end: end,
                        rhs_start: i,
                        rhs_end: i + 1,
                    }
                })?;
            }
            Ok(())
        }
    }
}

/// `found` with each run of the `counts.0` tuples `lhs` gives that agrees on `common`
/// with a run of the `counts.1` tuples `rhs` gives, both in order on them: each going on
/// from where the other has got to, by [`gallop`].
///
/// # Safety
///
/// `lhs` and `rhs` must give valid tuples of the left and the right operand.
#[cfg(not(test))]
unsafe fn merged(
    common: &[Common],
    lhs: impl Fn(usize) -> *const u8,
    rhs: impl Fn(usize) -> *const u8,
    (lhs_count, rhs_count): (usize, usize),
    found: &mut dyn FnMut(Group) -> Fallible,
) -> Fallible {
    use core::cmp::Ordering;
    // SAFETY: guaranteed by the caller.
    let between =
        |i: usize, j: usize| unsafe { on_common(common, (lhs(i), true), (rhs(j), false)) };
    let (mut i, mut j) = (0, 0);
    while i < lhs_count && j < rhs_count {
        match between(i, j) {
            Ordering::Less => i = gallop(i + 1, lhs_count, |k| between(k, j).is_lt()),
            Ordering::Greater => j = gallop(j + 1, rhs_count, |k| between(i, k).is_gt()),
            Ordering::Equal => {
                let lhs_end = gallop(i + 1, lhs_count, |k| between(k, j).is_eq());
                let rhs_end = gallop(j + 1, rhs_count, |k| between(i, k).is_eq());
                found(Group {
                    lhs_start: i,
                    lhs_end,
                    rhs_start: j,
                    rhs_end,
                })?;
                (i, j) = (lhs_end, rhs_end);
            }
        }
    }
    Ok(())
}

/// Which values of the common attributes a relation's tuples might have: a bit for
/// each of their hashes, a good many more bits than there are tuples, so that a value
/// none has rarely finds one set, which costs far less than looking for it.
#[cfg(not(test))]
struct Filter {
    bits: Temp<u64>,
    /// How far to shift a hash to leave the place of its bit.
    shift: u32,
}

/// How many tuples make a filter worth having: fewer are as quickly searched.
#[cfg(not(test))]
const FILTERED_FROM: usize = 4;

#[cfg(not(test))]
impl Filter {
    /// The filter of the `count` tuples of `of`, which are `found` in order, unless they
    /// are too few, or no common attribute is of a type this can hash, and so says
    /// nothing. Fails if there is no memory for it.
    ///
    /// # Safety
    ///
    /// As for [`Matches::of`].
    unsafe fn of(common: &[Common], of: Operand, found: Ordered<'_>) -> Fallible<Option<Filter>> {
        let hashes = |c: &Common| {
            // SAFETY: guaranteed by the caller.
            let kind = unsafe { (*c.desc).kind };
            matches!(kind, KIND_INTEGER | KIND_TEXT)
        };
        if of.count < FILTERED_FROM || !common.iter().any(hashes) {
            return Ok(None);
        }
        let Some(places) = of
            .count
            .checked_mul(16)
            .and_then(usize::checked_next_power_of_two)
        else {
            return Ok(None);
        };
        let mut bits = Temp::<u64>::zeroed((places / 64).max(1))?;
        let shift = 64 - places.trailing_zeros();
        // SAFETY: guaranteed by the caller; each bit's place is within them.
        unsafe {
            let words = bits.values();
            for k in 0..of.count {
                let place = place_in(hash_on(common, found.at(k), of.is_lhs), shift);
                words[place / 64] |= 1 << (place % 64);
            }
        }
        Ok(Some(Filter { bits, shift }))
    }

    /// Whether a tuple of the operand it isn't of, as the left operand if `is_lhs`, could
    /// agree with one of those it is of.
    ///
    /// # Safety
    ///
    /// `tuple` must be valid.
    unsafe fn may_have(&self, common: &[Common], tuple: *const u8, is_lhs: bool) -> bool {
        // SAFETY: guaranteed by the caller; the place is within them, as it was above.
        unsafe {
            let place = place_in(hash_on(common, tuple, is_lhs), self.shift);
            let word = self.bits.ptr().add(place / 64).read();
            (word >> (place % 64)) & 1 == 1
        }
    }
}

/// The place of the bit of `hash` in a [`Filter`] that shifts it by `shift`.
#[cfg(not(test))]
fn place_in(hash: u64, shift: u32) -> usize {
    usize::try_from(hash >> shift).unwrap_or_else(|_| internal_error("too many bits"))
}

/// A hash of the common attributes of `tuple`, the left operand's if `is_lhs`: equal
/// for tuples that agree on them, as it reads only integers and texts, whose equal
/// values have equal bytes. Its top bits are what vary most.
///
/// # Safety
///
/// `tuple` must be a valid tuple of that operand.
#[cfg(not(test))]
unsafe fn hash_on(common: &[Common], tuple: *const u8, is_lhs: bool) -> u64 {
    const MULTIPLIER: u64 = 0x9E37_79B9_7F4A_7C15;
    let mix = |hash: u64, word: u64| (hash ^ word).wrapping_mul(MULTIPLIER).rotate_left(29);
    let mut hash = 0;
    // SAFETY: guaranteed by the caller.
    unsafe {
        for c in common {
            let value = tuple.add(if is_lhs { c.lhs } else { c.rhs });
            match (*c.desc).kind {
                KIND_INTEGER => hash = mix(hash, value.cast::<u64>().read_unaligned()),
                KIND_TEXT => {
                    let (bytes, len) = read_slice(value);
                    for &byte in array(bytes, len) {
                        hash = mix(hash, u64::from(byte));
                    }
                    hash = mix(hash, len as u64);
                }
                _ => {}
            }
        }
    }
    hash.wrapping_mul(MULTIPLIER)
}

/// How many indexes the program keeps at a time, past which the one least recently
/// used goes.
#[cfg(not(test))]
const KEPT_INDEXES: usize = 8;

/// How many common attributes a kept index can be on.
#[cfg(not(test))]
const KEPT_ATTRIBUTES: usize = 4;

/// How many tuples a relation must have for an index to be worth keeping.
#[cfg(not(test))]
const KEPT_FROM: usize = 4096;

/// How many times its tuples must have been read through before a relation gets one,
/// as sorting them takes about as long as this: so a relation joined fewer times
/// never pays for one, and one that is joined more pays no more than twice what
/// knowing that from the start would have cost.
#[cfg(not(test))]
const KEPT_AFTER: usize = 16;

/// A relation's tuples' positions, in order on some of its attributes, which lasts as
/// long as the relation does, or what has been spent so far without one.
#[cfg(not(test))]
#[derive(Clone, Copy)]
struct KeptIndex {
    /// The relation's tuples, or null if this is of nothing.
    tuples: *const u8,
    count: usize,
    /// The attributes it is in order on: where each is in a tuple, and its type.
    on: [(usize, *const TypeDesc); KEPT_ATTRIBUTES],
    on_count: usize,
    /// How many tuples have been read for want of it.
    read: usize,
    /// The positions, `count` of them, in `malloc`'s memory, or null until it is made.
    positions: *mut u32,
    /// When it was last used, by [`Kept::clock`].
    used: u64,
}

/// The indexes the program keeps, of relations that are joined again and again with
/// far smaller ones, whose common attributes don't come first, so that each join would
/// otherwise read all their tuples.
///
/// One is keyed by the address of the tuples, so it must go when that stops meaning
/// the same tuples: when the buffer is freed ([`release_buffer`]), or changed where it
/// is ([`with_room`], [`removed_from`]), the only things that change a relation's
/// tuples once it has been made.
#[cfg(not(test))]
struct Kept {
    indexes: core::cell::UnsafeCell<[KeptIndex; KEPT_INDEXES]>,
    /// How many are of something, so that freeing a buffer needn't look if none is.
    in_use: core::cell::Cell<usize>,
    clock: core::cell::Cell<u64>,
}

// SAFETY: the program runs on one thread.
#[cfg(not(test))]
unsafe impl Sync for Kept {}

#[cfg(not(test))]
const NO_INDEX: KeptIndex = KeptIndex {
    tuples: core::ptr::null(),
    count: 0,
    on: [(0, core::ptr::null()); KEPT_ATTRIBUTES],
    on_count: 0,
    read: 0,
    positions: core::ptr::null_mut(),
    used: 0,
};

#[cfg(not(test))]
static KEPT: Kept = Kept {
    indexes: core::cell::UnsafeCell::new([NO_INDEX; KEPT_INDEXES]),
    in_use: core::cell::Cell::new(0),
    clock: core::cell::Cell::new(0),
};

#[cfg(not(test))]
impl Kept {
    /// The positions of `larger`'s tuples in order on `common`, if it has an index on
    /// them, or has now earned one, and the `smaller` tuples it is joined with are few
    /// enough for searching it to be quicker than reading it.
    ///
    /// # Safety
    ///
    /// Its tuples must be valid, and `common` its attributes in common with the other
    /// operand. Nothing else may be using what this has given before.
    unsafe fn positions(
        &self,
        common: &[Common],
        larger: Operand,
        smaller: usize,
    ) -> Option<&'static [u32]> {
        if larger.count < KEPT_FROM {
            return None;
        }
        // Galloping from each of the smaller, as far as they are apart.
        let apart = larger.count / smaller.max(1);
        let searches = smaller.saturating_mul(apart.ilog2() as usize + 1);
        if searches >= larger.count
            || common.len() > KEPT_ATTRIBUTES
            || u32::try_from(larger.count).is_err()
        {
            return None;
        }
        let mut on = [(0, core::ptr::null()); KEPT_ATTRIBUTES];
        for (place, c) in on.iter_mut().zip(common) {
            *place = (if larger.is_lhs { c.lhs } else { c.rhs }, c.desc);
        }
        // SAFETY: guaranteed by the caller; nothing else holds the table, and
        // what `positions` points to is written.
        unsafe {
            let indexes = &mut *self.indexes.get();
            let at = indexes.iter().position(|index| {
                index.tuples == larger.tuples
                    && index.count == larger.count
                    && index.on_count == common.len()
                    && index.on == on
            });
            let now = self.clock.get() + 1;
            self.clock.set(now);
            let index = if let Some(at) = at {
                &mut indexes[at]
            } else {
                let oldest = indexes
                    .iter_mut()
                    .min_by_key(|index| index.used)
                    .unwrap_or_else(|| internal_error("no kept indexes"));
                if oldest.tuples.is_null() {
                    self.in_use.set(self.in_use.get() + 1);
                } else if !oldest.positions.is_null() {
                    free(oldest.positions.cast());
                }
                *oldest = KeptIndex {
                    tuples: larger.tuples,
                    count: larger.count,
                    on,
                    on_count: common.len(),
                    ..NO_INDEX
                };
                oldest
            };
            index.used = now;
            if index.positions.is_null() {
                index.read = index.read.saturating_add(larger.count);
                if index.read < larger.count.saturating_mul(KEPT_AFTER) {
                    return None;
                }
                index.positions = sorted_positions(common, larger);
                // Without the memory, it goes on reading them, which needs none.
                index.read = 0;
                if index.positions.is_null() {
                    return None;
                }
            }
            Some(core::slice::from_raw_parts(index.positions, larger.count))
        }
    }

    /// Forget each index of the tuples at `tuples`, which are going, or changing.
    ///
    /// # Safety
    ///
    /// Nothing else may be using what this has given.
    unsafe fn forget(&self, tuples: *const u8) {
        if self.in_use.get() == 0 {
            return;
        }
        // SAFETY: guaranteed by the caller; nothing else holds the table.
        unsafe {
            for index in &mut *self.indexes.get() {
                if index.tuples == tuples {
                    if !index.positions.is_null() {
                        free(index.positions.cast());
                    }
                    *index = NO_INDEX;
                    self.in_use.set(self.in_use.get() - 1);
                }
            }
        }
    }
}

/// The positions of `operand`'s tuples, in order on `common`, those that agree in
/// the order they are in, in `malloc`'s memory, or null if there is no memory for them,
/// which fails nothing, as it can go on without them.
///
/// # Safety
///
/// Its tuples must be valid, and its count fit 32 bits, with more than one tuple.
#[cfg(not(test))]
unsafe fn sorted_positions(common: &[Common], operand: Operand) -> *mut u32 {
    // SAFETY: guaranteed by the caller; `malloc` has no preconditions, and there is a
    // position for each of the tuples, each within the relation.
    unsafe {
        let positions = malloc(operand.count * size_of::<u32>()).cast::<u32>();
        let pointers = malloc(operand.count * size_of::<*const u8>()).cast::<*const u8>();
        if !positions.is_null() && !pointers.is_null() {
            for i in 0..operand.count {
                pointers.add(i).write(operand.tuples.add(i * operand.size));
            }
            let sorted = core::slice::from_raw_parts_mut(pointers, operand.count);
            sorted.sort_unstable_by(|x, y| {
                on_common(common, (*x, operand.is_lhs), (*y, operand.is_lhs)).then(x.cmp(y))
            });
            for (i, pointer) in sorted.iter().enumerate() {
                let position = pointer.offset_from_unsigned(operand.tuples) / operand.size;
                let position =
                    u32::try_from(position).unwrap_or_else(|_| internal_error("too many tuples"));
                positions.add(i).write(position);
            }
            free(pointers.cast());
            return positions;
        }
        free(positions.cast());
        free(pointers.cast());
        core::ptr::null_mut()
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
) -> bool {
    // SAFETY: guaranteed by the caller.
    status(|| unsafe {
        let count = count.read_unaligned();
        let Some(bytes) = count.checked_mul((*desc).element().size) else {
            return Err(out_of_memory());
        };
        let tuples = allocate_buffer(bytes, desc)?;
        core::ptr::write_bytes(tuples, 0, bytes);
        dst.write_unaligned(Text {
            bytes: tuples,
            len: count,
        });
        Ok(())
    })
}

/// Put the tuples of the relation at `relation`, of the type `desc`
/// describes, in the canonical order, without duplicates: it then holds
/// them, in memory the statement owns. The buffer it had is sorted where it is
/// if its tuples are integers and only the current scope holds it, and
/// otherwise freed at once if only the current scope holds it, as the one
/// compiled code filled has nothing else pointing into it once it is replaced,
/// so that a relation sorted isn't two of it until the statement ends.
///
/// # Safety
///
/// `relation` must point to a valid relation of the type `desc` describes,
/// whose tuples may be in any order, and nothing but `relation` may point
/// into its buffer, if only the current scope holds that.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_sort(relation: *mut Text, desc: *const TypeDesc) -> bool {
    // SAFETY: guaranteed by the caller.
    status(|| unsafe {
        let (tuples, count) = read_slice(relation.cast());
        if sorts_in_place(&*desc) && only_in_scope(tuples.cast_mut()).is_some() {
            let (bytes, len) = sorted_in_place(tuples.cast_mut(), count, &*desc)?;
            relation.write_unaligned(Text { bytes, len });
            return Ok(());
        }
        if in_canonical_order(tuples, count, &*desc) {
            adopt(tuples.cast_mut(), count, (*desc).element());
            return Ok(());
        }
        let (bytes, len) = canonical(tuples, count, &*desc)?;
        relation.write_unaligned(Text { bytes, len });
        release_replaced(tuples.cast_mut());
        Ok(())
    })
}

/// Give up the current scope's reference to `buffer`, if it is the only
/// one, freeing it: a buffer the statement made, and has replaced, which
/// nothing points into any more. One that anything else holds, or that
/// the scope doesn't, is left as it is.
///
/// # Safety
///
/// `buffer` must be a buffer, just past its header, or null, and nothing
/// may point into it once it is freed but what holds a reference to it.
#[cfg(not(test))]
unsafe fn release_replaced(buffer: *mut u8) {
    use core::sync::atomic::Ordering::Relaxed;
    // SAFETY: guaranteed by the caller; the scope's list holds its `len`
    // buffers.
    unsafe {
        if let Some(i) = only_in_scope(buffer) {
            let scope = &mut *CURRENT.load(Relaxed);
            scope.len -= 1;
            *scope.buffers.add(i) = *scope.buffers.add(scope.len);
            release_buffer(buffer);
        }
    }
}

/// Give up the current scope's reference to what the value at `value`, of the
/// type `desc` describes, points to, where that is the only one: the operand of
/// a relational operation, once it has read it. Its result holds references of
/// its own to what it shares with the operand, and only the operation read the
/// operand, so nothing else points into what is freed, and what the operand holds
/// doesn't last until the statement ends. What anything else holds is left as it
/// is, as a variable's value is.
///
/// # Safety
///
/// `value` must point to a valid value of the type `desc` describes, which
/// nothing but what holds a reference to it reads from now on.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_drop_operand(value: *const u8, desc: *const TypeDesc) {
    // SAFETY: guaranteed by the caller.
    unsafe { each_buffer(value, &*desc, &mut |buffer| release_replaced(buffer)) };
}

/// Where `buffer` is in the current scope's list, if the scope holds the only
/// reference to it.
///
/// # Safety
///
/// `buffer` must be a buffer, just past its header, or null.
#[cfg(not(test))]
unsafe fn only_in_scope(buffer: *mut u8) -> Option<usize> {
    use core::sync::atomic::Ordering::Relaxed;
    // SAFETY: guaranteed by the caller; the scope's list holds its `len`
    // buffers.
    unsafe {
        if buffer.is_null() || header(buffer).count != 1 {
            return None;
        }
        let scope = &*CURRENT.load(Relaxed);
        // From the end, where a buffer the statement made recently is.
        (0..scope.len)
            .rev()
            .find(|&i| *scope.buffers.add(i) == buffer)
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
) -> bool {
    // SAFETY: guaranteed by the caller.
    status(|| unsafe {
        let (source, result) = (&*source, &*result);
        let items = array(items, count.read_unaligned());
        let parts = array(parts, part_count.read_unaligned());
        let (tuples, len) = read_slice(src);
        let mut order = pointers(tuples, len, source.element().size)?;
        let order = order.values();
        // A relation's tuples are distinct, so no two are equal in
        // canonical order, and the sort has one result.
        order.sort_unstable_by(|a, b| {
            on_items(items, *a, *b).then_with(|| compare_tuples(*a, *b, source))
        });
        let element = result.element();
        let Some(bytes) = len.checked_mul(element.size) else {
            return Err(out_of_memory());
        };
        let out = allocate_buffer(bytes, result)?;
        // Each element is made here first, zeroed, as everywhere, so that
        // the padding in it is zero.
        let made = Temp::<u8>::zeroed(element.size)?;
        let made = made.ptr();
        for (i, tuple) in order.iter().enumerate() {
            for part in parts {
                core::ptr::copy_nonoverlapping(tuple.add(part.from), made.add(part.to), part.size);
            }
            copy_element(out, out.add(i * element.size), made, element);
        }
        dst.write_unaligned(Text { bytes: out, len });
        Ok(())
    })
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
) -> bool {
    // SAFETY: guaranteed by the caller.
    status(|| unsafe {
        let (source, result) = (&*source, &*result);
        let parts = array(parts, part_count.read_unaligned());
        let numbers = array(numbers, number_count.read_unaligned());
        let (elements, len) = read_slice(src);
        let element = source.element().size;
        let size = result.element().size;
        // An array of tuples of no attributes takes no memory, so only the
        // integers limit its positions.
        if i64::try_from(len).is_err() {
            return Err(overflowed("the position"));
        }
        let (out, kept) = made_canonical(len, result, |tuples| {
            let mut position = 0_i64;
            for i in 0..len {
                let (from, to) = (elements.add(i * element), tuples.add(i * size));
                for part in parts {
                    core::ptr::copy_nonoverlapping(from.add(part.from), to.add(part.to), part.size);
                }
                position += 1;
                for number in numbers {
                    to.add(*number).cast::<i64>().write_unaligned(position);
                }
            }
        })?;
        dst.write_unaligned(Text {
            bytes: out,
            len: kept,
        });
        Ok(())
    })
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
/// statement owns: that memory and how many tuples it holds. Fails if
/// there is no memory for them.
///
/// # Safety
///
/// As for [`trendy_rt_reshape`]'s result.
#[cfg(not(test))]
unsafe fn canonical(
    tuples: *const u8,
    count: usize,
    desc: &TypeDesc,
) -> Fallible<(*mut u8, usize)> {
    // SAFETY: guaranteed by the caller.
    unsafe {
        let size = desc.element().size;
        if sorts_in_place(desc) {
            let out = allocate_buffer(count * size, desc)?;
            core::ptr::copy_nonoverlapping(tuples, out, count * size);
            return sorted_in_place(out, count, desc);
        }
        if in_canonical_order(tuples, count, desc) {
            let out = allocate_buffer(count * size, desc)?;
            core::ptr::copy_nonoverlapping(tuples, out, count * size);
            adopt(out, count, desc.element());
            return Ok((out, count));
        }
        let mut order = pointers(tuples, count, size)?;
        let order = order.values();
        order.sort_unstable_by(|a, b| compare_tuples(*a, *b, desc));
        let out = allocate_buffer(count * size, desc)?;
        let mut kept = 0;
        for i in 0..count {
            if i > 0 && compare_tuples(order[i - 1], order[i], desc).is_eq() {
                continue;
            }
            copy_element(out, out.add(kept * size), order[i], desc.element());
            kept += 1;
        }
        Ok((out, kept))
    }
}

/// The relation `desc` describes of the `count` tuples that `write` writes at
/// the address it is given, in any order: sorted canonically and without
/// duplicates, in memory the current statement owns, as [`canonical`] gives
/// them. Tuples of integers are written in that memory and sorted there, and
/// others are written in temporary memory, zeroed, as everywhere, so that the
/// padding in them is zero, and share what they point to until [`canonical`]
/// copies them, taking a reference to each.
///
/// # Safety
///
/// `write` must write `count` valid tuples of that relation.
#[cfg(not(test))]
unsafe fn made_canonical(
    count: usize,
    desc: &TypeDesc,
    write: impl FnOnce(*mut u8),
) -> Fallible<(*mut u8, usize)> {
    // SAFETY: guaranteed by the caller.
    unsafe {
        let Some(bytes) = count.checked_mul(desc.element().size) else {
            return Err(out_of_memory());
        };
        if sorts_in_place(desc) {
            let out = allocate_buffer(bytes, desc)?;
            write(out);
            return sorted_in_place(out, count, desc);
        }
        let made = Temp::<u8>::zeroed(bytes)?;
        write(made.ptr());
        canonical(made.ptr(), count, desc)
    }
}

/// The relation `desc` describes of the `count` tuples that `write` writes at
/// the address it is given, zeroed, as everywhere, so that the padding in them is
/// zero, in canonical order and without duplicates, in memory the current
/// statement owns, which takes references to what they point to.
///
/// # Safety
///
/// `write` must write `count` valid tuples of that relation, in that order.
#[cfg(not(test))]
unsafe fn made_in_order(
    count: usize,
    desc: &TypeDesc,
    write: impl FnOnce(*mut u8),
) -> Fallible<(*mut u8, usize)> {
    // SAFETY: guaranteed by the caller.
    unsafe {
        let element = desc.element();
        let Some(bytes) = count.checked_mul(element.size) else {
            return Err(out_of_memory());
        };
        let out = allocate_buffer(bytes, desc)?;
        core::ptr::write_bytes(out, 0, bytes);
        write(out);
        adopt(out, count, element);
        Ok((out, count))
    }
}

/// The relation `desc` describes of the `count` tuples that `write` writes at the address
/// it is given, all different, in the order it writes them, for what doesn't depend on
/// that, in memory the current statement owns, which takes references to what they point
/// to. Integers have no padding to be zero, so aren't zeroed first.
///
/// # Safety
///
/// `write` must write `count` valid tuples of that relation, none of them equal.
#[cfg(not(test))]
unsafe fn made_loose(
    count: usize,
    desc: &TypeDesc,
    write: impl FnOnce(*mut u8),
) -> Fallible<(*mut u8, usize)> {
    // SAFETY: guaranteed by the caller.
    unsafe {
        if sorts_in_place(desc) {
            let Some(bytes) = count.checked_mul(desc.element().size) else {
                return Err(out_of_memory());
            };
            let out = allocate_buffer(bytes, desc)?;
            write(out);
            return Ok((out, count));
        }
        made_in_order(count, desc, write)
    }
}

/// Whether the tuples of the relation `desc` describes are sorted where they
/// are, rather than through pointers to them, which takes half the time or less,
/// and no memory for the pointers: they are integers, and few enough. Past that,
/// the gain is small, and each number of them takes code in every program.
#[cfg(not(test))]
fn sorts_in_place(desc: &TypeDesc) -> bool {
    (1..=8).contains(&desc.integers)
}

/// The `count` tuples at `out`, a buffer the current scope holds alone, of the
/// relation `desc` describes, of which [`sorts_in_place`] is true, sorted
/// canonically where they are, without duplicates: the memory that holds them,
/// which is smaller if they take less than half of it, as what a relation holds
/// lasts as long as it does, and how many there are. Fails if there is no memory.
///
/// # Safety
///
/// `out` must hold `count` valid tuples of that relation.
#[cfg(not(test))]
unsafe fn sorted_in_place(
    out: *mut u8,
    count: usize,
    desc: &TypeDesc,
) -> Fallible<(*mut u8, usize)> {
    // SAFETY: guaranteed by the caller.
    unsafe {
        let kept = if in_canonical_order(out, count, desc) {
            count
        } else {
            sort_integers(out, count, desc.integers)
        };
        // Integers point to nothing, so a buffer needs no references through
        // them.
        if kept >= count / 2 {
            return Ok((out, kept));
        }
        let bytes = kept * desc.element().size;
        let smaller = allocate_buffer(bytes, desc)?;
        core::ptr::copy_nonoverlapping(out, smaller, bytes);
        release_replaced(out);
        Ok((smaller, kept))
    }
}

/// Sort the `count` tuples at `tuples`, each of `integers` integers, where they
/// are, as [`compare_integers`] orders them, keeping one of those that are
/// equal: how many that leaves, first in the memory.
///
/// # Safety
///
/// `tuples` must point to `count` such tuples, aligned for integers, and
/// `integers` must be one of those [`sorts_in_place`] allows.
unsafe fn sort_integers(tuples: *mut u8, count: usize, integers: usize) -> usize {
    /// As for [`sort_integers`], of `N` integers.
    unsafe fn sort<const N: usize>(tuples: *mut u8, count: usize) -> usize {
        // SAFETY: guaranteed by the caller.
        let tuples = unsafe { core::slice::from_raw_parts_mut(tuples.cast::<[i64; N]>(), count) };
        tuples.sort_unstable();
        let mut kept = 0;
        for i in 0..count {
            if kept == 0 || tuples[kept - 1] != tuples[i] {
                tuples[kept] = tuples[i];
                kept += 1;
            }
        }
        kept
    }
    // SAFETY: guaranteed by the caller.
    unsafe {
        match integers {
            1 => sort::<1>(tuples, count),
            2 => sort::<2>(tuples, count),
            3 => sort::<3>(tuples, count),
            4 => sort::<4>(tuples, count),
            5 => sort::<5>(tuples, count),
            6 => sort::<6>(tuples, count),
            7 => sort::<7>(tuples, count),
            8 => sort::<8>(tuples, count),
            _ => unreachable!("`sorts_in_place` allows no others"),
        }
    }
}

/// Whether the `count` tuples at `tuples`, of the relation `desc` describes,
/// are in canonical order, without duplicates: so that they need no sorting,
/// as one tuple never does, nor those that something has kept in order.
/// Tuples out of order take few comparisons to find so, as this stops at
/// the first.
///
/// # Safety
///
/// `tuples` must point to `count` valid tuples of that relation.
#[cfg(not(test))]
unsafe fn in_canonical_order(tuples: *const u8, count: usize, desc: &TypeDesc) -> bool {
    // SAFETY: guaranteed by the caller.
    unsafe {
        let size = desc.element().size;
        (1..count).all(|i| {
            let (earlier, later) = (tuples.add((i - 1) * size), tuples.add(i * size));
            compare_tuples(earlier, later, desc).is_lt()
        })
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
#[inline]
pub unsafe fn order(lhs: *const u8, rhs: *const u8, desc: &TypeDesc) -> core::cmp::Ordering {
    // SAFETY: guaranteed by the caller.
    unsafe {
        // Sorting compares little else, so this is inlined into each loop
        // over attributes, without a call for each.
        if desc.kind == KIND_INTEGER {
            return lhs
                .cast::<i64>()
                .read_unaligned()
                .cmp(&rhs.cast::<i64>().read_unaligned());
        }
        order_otherwise(lhs, rhs, desc)
    }
}

/// [`order`] of values of any type but `integer`.
///
/// # Safety
///
/// As for [`order`].
unsafe fn order_otherwise(lhs: *const u8, rhs: *const u8, desc: &TypeDesc) -> core::cmp::Ordering {
    // SAFETY: guaranteed by the caller.
    unsafe {
        match desc.kind {
            KIND_BOOLEAN => (*lhs).cmp(&*rhs),
            // IEEE 754 totalOrder, which, without NaN or `-0.0`, is the
            // order of the values.
            KIND_APPROXIMATE => lhs
                .cast::<f64>()
                .read_unaligned()
                .total_cmp(&rhs.cast::<f64>().read_unaligned()),
            KIND_TEXT | KIND_CHARACTER => {
                let (a, b) = (read_slice(lhs), read_slice(rhs));
                core::slice::from_raw_parts(a.0, a.1).cmp(core::slice::from_raw_parts(b.0, b.1))
            }
            KIND_U8 | KIND_U16 | KIND_U32 => narrow(lhs, desc.kind).cmp(&narrow(rhs, desc.kind)),
            KIND_TUPLE | KIND_DEFINED => compare_tuples(lhs, rhs, desc),
            // By the names they are written with, then their components, as
            // `hir::DefinedValue` orders them: of a type and the subtypes a
            // constraint defines, written the same, in their components' order.
            KIND_UNION => {
                let ((a, at), (b, bt)) = (own_type(lhs, desc), own_type(rhs, desc));
                written_name(a)
                    .cmp(written_name(b))
                    .then_with(|| order(at, bt, a))
            }
            KIND_RELATION | KIND_ARRAY => {
                let ((a, count), (b, other)) = (read_slice(lhs), read_slice(rhs));
                let element = desc.element();
                // What is one byte each is compared at once, each as unsigned.
                if element.size == 1 && !element.points {
                    return core::slice::from_raw_parts(a, count)
                        .cmp(core::slice::from_raw_parts(b, other));
                }
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

/// The order of the `count` integers at `lhs` and those at `rhs`, one after
/// another: that of the first that differ.
///
/// # Safety
///
/// `lhs` and `rhs` must each point to `count` integers.
#[inline]
unsafe fn compare_integers(lhs: *const u8, rhs: *const u8, count: usize) -> core::cmp::Ordering {
    for i in 0..count {
        let at = i * size_of::<i64>();
        // SAFETY: guaranteed by the caller.
        let (a, b) = unsafe {
            (
                lhs.add(at).cast::<i64>().read_unaligned(),
                rhs.add(at).cast::<i64>().read_unaligned(),
            )
        };
        if a != b {
            return a.cmp(&b);
        }
    }
    core::cmp::Ordering::Equal
}

/// The canonical order of two tuples with the attributes of `desc`.
///
/// # Safety
///
/// As for [`order`], with `desc` describing a tuple or relation.
unsafe fn compare_tuples(lhs: *const u8, rhs: *const u8, desc: &TypeDesc) -> core::cmp::Ordering {
    // SAFETY: guaranteed by the caller.
    unsafe {
        if desc.integers > 0 {
            return compare_integers(lhs, rhs, desc.integers);
        }
        for attr in attributes(desc) {
            let ordering = order(lhs.add(attr.offset), rhs.add(attr.offset), &*attr.desc);
            if ordering.is_ne() {
                return ordering;
            }
        }
        core::cmp::Ordering::Equal
    }
}

/// A failure whose error is written, for which each function it happens
/// in gives up its work, up to the exported function, which returns that
/// it failed: compiled code then goes where its block's failures go.
#[cfg(not(test))]
#[must_use]
struct Failed;

/// What a function that can fail gives: its result, or [`Failed`]. A
/// function that fails changes nothing but what the statement's scope
/// holds, which ends as the statement fails.
#[cfg(not(test))]
type Fallible<T = ()> = Result<T, Failed>;

/// Whether `body` failed, as an exported function that can fail returns
/// it.
#[cfg(not(test))]
fn status(body: impl FnOnce() -> Fallible) -> bool {
    body().is_err()
}

/// Fail because memory for a result couldn't be allocated, with no error
/// waiting, which nothing takes (R0003): making one needs memory too, and
/// nothing after it can be relied on.
#[cfg(not(test))]
fn out_of_memory() -> Failed {
    write_failure(|out| out.write_str("error[R0003]: out of memory\n"));
    Failed
}

/// Fail with an `overflow` whose operation is `operation`, as its message
/// says it: an operation of no operands to write, as `the sum`.
#[cfg(not(test))]
fn overflowed(operation: &str) -> Failed {
    let Ok(operation) = Made::text(operation) else {
        return Failed;
    };
    // SAFETY: the operation is a valid text.
    unsafe { fail_as(OVERFLOW, &[("operation", operation.at())]) }
}

/// Fail with the error of `lhs op rhs`, of integers, which has no result,
/// `op` being an `ARITHMETIC_` constant: a `division_by_zero` (R0002), or
/// else an `overflow` (R0001), of the operation. A negation's `lhs` is 0.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn trendy_rt_integer_failed(op: u32, lhs: i64, rhs: i64) {
    arithmetic_failed(op, Number::Integer(lhs), Number::Integer(rhs));
}

/// As [`trendy_rt_integer_failed`], of approximate numbers.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn trendy_rt_approximate_failed(op: u32, lhs: f64, rhs: f64) {
    arithmetic_failed(op, Number::Approximate(lhs), Number::Approximate(rhs));
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

/// Fail with the error of `lhs op rhs`, which has no result, as
/// [`trendy_rt_integer_failed`] says, its operation as trendy source
/// writes it, a negative right operand in parentheses.
#[cfg(not(test))]
fn arithmetic_failed(op: u32, lhs: Number, rhs: Number) {
    let symbol = match op {
        ARITHMETIC_ADD => "+",
        ARITHMETIC_SUBTRACT | ARITHMETIC_NEGATE => "-",
        ARITHMETIC_MULTIPLY => "*",
        ARITHMETIC_DIVIDE => "/",
        ARITHMETIC_DIV => "div",
        ARITHMETIC_MOD => "mod",
        ARITHMETIC_SHIFT_LEFT => "shl",
        ARITHMETIC_SHIFT_RIGHT => "shr",
        _ => internal_error("unknown arithmetic operator"),
    };
    let divides = matches!(op, ARITHMETIC_DIVIDE | ARITHMETIC_DIV | ARITHMETIC_MOD);
    let kind = if divides && rhs.is_zero() {
        DIVISION_BY_ZERO
    } else {
        OVERFLOW
    };
    let Ok(operation) = Made::written(|out| {
        if op == ARITHMETIC_NEGATE {
            out.write_str(symbol)?;
        } else {
            write!(out, "{lhs} {symbol} ")?;
        }
        if rhs.is_negative() {
            write!(out, "({rhs})")
        } else {
            write!(out, "{rhs}")
        }
    }) else {
        return;
    };
    // SAFETY: the operation is a valid text.
    let Failed = unsafe { fail_as(kind, &[("operation", operation.at())]) };
}

/// Fail with the error of tuples being joined that disagree on the
/// common attribute named `attribute`, a text, which is `lhs` in one and
/// `rhs` in the other, of the type `desc` describes: a `tuple_mismatch`,
/// its values written as `debug_value` writes them (R0004).
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
) {
    // SAFETY: guaranteed by the caller.
    unsafe {
        let desc = &*desc;
        let (Ok(first), Ok(second)) = (Made::value(lhs, desc), Made::value(rhs, desc)) else {
            return;
        };
        let Failed = fail_as(
            TUPLE_MISMATCH,
            &[
                ("attribute", attribute),
                ("first", first.at()),
                ("second", second.at()),
            ],
        );
    }
}

/// Fail with the error of `value`, components `desc` describes, selected
/// of the type of the program's own named `ty`, a text, which break its
/// constraint written `constraint`, a text: a `broken_constraint`, the
/// value written as `debug_value` writes it (R0017).
///
/// # Safety
///
/// `value` must point to valid components `desc` describes, a type of the
/// program's own's, and `ty` and `constraint` to valid text values.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_broken_constraint(
    value: *const u8,
    desc: *const TypeDesc,
    ty: *const u8,
    constraint: *const u8,
) {
    // SAFETY: guaranteed by the caller.
    unsafe {
        let Ok(value) = Made::value(value, &*desc) else {
            return;
        };
        let Failed = fail_as(
            BROKEN_CONSTRAINT,
            &[
                ("value", value.at()),
                ("type", ty),
                ("constraint", constraint),
            ],
        );
    }
}

/// Fail with the error of `value`, of the type of the program's own
/// `desc` describes, which is of both of its subtypes defined by
/// constraints named `first` and `second`, texts, but a value has one most
/// specific type: an `overlapping_subtypes`, the value written as
/// `debug_value` writes it (R0020).
///
/// # Safety
///
/// `value` must point to a valid value of the type `desc` describes, and
/// `first` and `second` to valid text values.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_overlapping_subtypes(
    value: *const u8,
    desc: *const TypeDesc,
    first: *const u8,
    second: *const u8,
) {
    // SAFETY: guaranteed by the caller.
    unsafe {
        let Ok(value) = Made::value(value, &*desc) else {
            return;
        };
        let Failed = fail_as(
            OVERLAPPING_SUBTYPES,
            &[("value", value.at()), ("first", first), ("second", second)],
        );
    }
}

/// Fail with the error of a call of the operator named `operator`, a text,
/// with the arguments at `value`, a tuple `desc` describes, which break the
/// constraint written `constraint`, a text: a `broken_call_constraint`, the
/// call written as a call is, as `debug_value` writes a value (R0024).
///
/// # Safety
///
/// `value` must point to a valid tuple `desc` describes, and `operator` and
/// `constraint` to valid text values.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_broken_call_constraint(
    value: *const u8,
    desc: *const TypeDesc,
    operator: *const u8,
    constraint: *const u8,
) {
    // SAFETY: guaranteed by the caller.
    unsafe {
        let name = read_text(operator);
        let Ok(called) = Made::written(|mut out| write_call(&mut out, name, value, &*desc)) else {
            return;
        };
        let Failed = fail_as(
            BROKEN_CALL_CONSTRAINT,
            &[("called", called.at()), ("constraint", constraint)],
        );
    }
}

/// Fail with the error of `value`, of the type in a hierarchy `desc`
/// describes, which isn't a value of the type named `ty`, a text, that it
/// was treated as: a `not_of_type`, the value written as `debug_value`
/// writes it (R0019).
///
/// # Safety
///
/// `value` must point to a valid value of the type `desc` describes, and
/// `ty` to a valid text value.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_not_of_type(
    value: *const u8,
    desc: *const TypeDesc,
    ty: *const u8,
) {
    // SAFETY: guaranteed by the caller.
    unsafe {
        let Ok(value) = Made::value(value, &*desc) else {
            return;
        };
        let Failed = fail_as(NOT_OF_TYPE, &[("value", value.at()), ("type", ty)]);
    }
}

/// Fail with the error of a statement that left the variables so that the
/// constraint named `constraint`, a text, is false: its part written
/// `condition`, a text: a `constraint_violated` (R0023).
///
/// # Safety
///
/// `constraint` and `condition` must point to valid text values.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_constraint_violated(
    constraint: *const u8,
    condition: *const u8,
) {
    // SAFETY: guaranteed by the caller.
    let Failed = unsafe {
        fail_as(
            CONSTRAINT_VIOLATED,
            &[("constraint", constraint), ("condition", condition)],
        )
    };
}

/// As [`trendy_rt_constraint_violated`], with the relation of the tuples
/// that make the part false, written as `debug_value` writes it, `tuples`,
/// a text: a `constraint_violated_by` (R0023).
///
/// # Safety
///
/// `constraint`, `condition` and `tuples` must point to valid text values.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_constraint_violated_by(
    constraint: *const u8,
    condition: *const u8,
    tuples: *const u8,
) {
    // SAFETY: guaranteed by the caller.
    let Failed = unsafe {
        fail_as(
            CONSTRAINT_VIOLATED_BY,
            &[
                ("constraint", constraint),
                ("condition", condition),
                ("tuples", tuples),
            ],
        )
    };
}

/// Write the warning of a `warn` outside any transaction, whose message is
/// `message`, a text (R0022), escaped as [`trendy_rt_failed`] writes a
/// failure's, with where the program is, and carry on.
///
/// # Safety
///
/// `message` must point to a valid text value.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_warned(message: *const u8) {
    // SAFETY: guaranteed by the caller.
    let message = unsafe { read_text(message) };
    write_failure(|out| {
        out.write_str("warning[R0022]: ")?;
        trendy_text::escape_message(message, out)?;
        out.write_char('\n')
    });
}

/// Write the error at `error` that nothing took, a value of the prelude's
/// type `error`, whose message is `message`, a text, with the code of its
/// kind, if the language makes it ([`Failures`]), as `division_by_zero`'s
/// R0002, or else a program's own's, R0021. It is written as
/// [`trendy_text::escape_message`] writes it, its control characters and
/// bidirectional controls escaped, so that a message made from a file's
/// text can't change the terminal, or show other than what it says.
///
/// # Safety
///
/// `message` must point to a valid text value, and `error` to a valid
/// value of `error`.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_failed(message: *const u8, error: *const u8) {
    // SAFETY: guaranteed by the caller.
    unsafe {
        let (message, code) = (read_text(message), code_of(error));
        write_failure(|out| {
            write!(out, "error[{code}]: ")?;
            trendy_text::escape_message(message, out)?;
            out.write_char('\n')
        });
    }
}

/// Write the error at `error`, a value of the prelude's type `error`, that
/// `main` gives, whose message is `message`, a text, as a failure, if the
/// byte at `failed` is 1, or else as a warning, with the code of its kind,
/// as [`trendy_rt_failed`] writes it, and no site, as an error holds none.
///
/// # Safety
///
/// `message` must point to a valid text value, `error` to a valid value of
/// `error`, and `failed` to a byte.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_report(message: *const u8, error: *const u8, failed: *const u8) {
    // SAFETY: guaranteed by the caller.
    unsafe {
        let (message, code) = (read_text(message), code_of(error));
        let severity = if failed.read() == 1 {
            "error"
        } else {
            "warning"
        };
        let mut out = Stderr::default();
        // There is nowhere to report a failure to write to stderr.
        let _ = write!(out, "{severity}[{code}]: ")
            .and_then(|()| trendy_text::escape_message(message, &mut out))
            .and_then(|()| out.write_char('\n'));
        out.flush();
    }
}

/// The code of the error at `error`, a value of the prelude's type
/// `error`: its kind's, if the language makes it ([`Failures`]), as
/// `division_by_zero`'s R0002, or else a program's own's, R0021.
///
/// # Safety
///
/// `error` must point to a valid value of `error`.
#[cfg(not(test))]
unsafe fn code_of(error: *const u8) -> &'static str {
    // SAFETY: guaranteed by the caller, and the table's kinds are the
    // compiler's, which last as long as the program.
    unsafe {
        let own = error.cast::<*const TypeDesc>().read_unaligned();
        let failures = failures();
        let kinds = core::slice::from_raw_parts(failures.kinds, failures.count);
        kinds
            .iter()
            .find(|kind| core::ptr::eq(kind.desc, own))
            .map_or("R0021", |kind| read_text((&raw const kind.code).cast()))
    }
}

/// Fail with the error of `value`, components `desc` describes, which
/// `load_csv` read from the record `record` describes for a value of the
/// type of the program's own named `ty`, a text, and which break its
/// constraint written `constraint`, a text: a `broken_constraint_in_file`
/// (R0017).
///
/// # Safety
///
/// `value` must point to valid components `desc` describes, a type of the
/// program's own's, `ty` and `constraint` to valid text values, and
/// `record` to a valid [`Record`].
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_broken_constraint_in_file(
    value: *const u8,
    desc: *const TypeDesc,
    ty: *const u8,
    constraint: *const u8,
    record: *const Record,
) {
    // SAFETY: guaranteed by the caller.
    let Failed = unsafe {
        broken_constraint_in_file(None, value, &*desc, ty, constraint, &*record)
            .expect_err("with nothing to collect it, an error fails")
    };
}

/// Fail with the error of a relation that has `*count` tuples, not one,
/// where its only one is taken: a `not_one_tuple` (R0005).
///
/// # Safety
///
/// `count` must point to a `usize`.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_not_one_tuple(count: *const usize) {
    // SAFETY: guaranteed by the caller.
    let count = unsafe { count.read_unaligned() };
    // A relation's tuples are in memory, so fewer than any integer.
    let count = i64::try_from(count).unwrap_or(i64::MAX);
    // SAFETY: the count is an integer.
    let Failed = unsafe { fail_as(NOT_ONE_TUPLE, &[("count", (&raw const count).cast())]) };
}

/// Fail with the error of a relation that has no tuples, where the least
/// value of an attribute is taken: a `no_min_or_max` (R0010).
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn trendy_rt_no_min() {
    let Failed = no_min_or_max("min");
}

/// Fail with the error of a relation that has no tuples, where the
/// greatest value of an attribute is taken: a `no_min_or_max` (R0010).
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn trendy_rt_no_max() {
    let Failed = no_min_or_max("max");
}

/// Fail with a `no_min_or_max` of `aggregate`, `min` or `max`.
#[cfg(not(test))]
fn no_min_or_max(aggregate: &str) -> Failed {
    let Ok(aggregate) = Made::text(aggregate) else {
        return Failed;
    };
    // SAFETY: the aggregate is a valid text.
    unsafe { fail_as(NO_MIN_OR_MAX, &[("aggregate", aggregate.at())]) }
}

/// Fail with the error of an array of `*length` elements that has none at
/// `*position`: an `index_out_of_range` (R0011).
///
/// # Safety
///
/// `position` and `length` must point to integers.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_index_out_of_range(position: *const i64, length: *const i64) {
    // SAFETY: guaranteed by the caller.
    let (position, length) = unsafe { (position.read_unaligned(), length.read_unaligned()) };
    // SAFETY: the position and the length are integers.
    let Failed = unsafe {
        fail_as(
            INDEX_OUT_OF_RANGE,
            &[
                ("position", (&raw const position).cast()),
                ("length", (&raw const length).cast()),
            ],
        )
    };
}

/// Fail with the error of two changes to an array in an `assign` block
/// that set the element at the `*count` `positions`, from the outermost
/// array in, or, if `*nested` is 1, of one that sets it and another an
/// element in it: an `element_set_twice`, its element as trendy writes the
/// positions, as `[2][1]` (R0009).
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
) {
    // SAFETY: guaranteed by the caller.
    unsafe {
        let (count, within) = (count.read_unaligned(), nested.read());
        let Ok(element) = Made::written(|out| {
            for i in 0..count {
                write!(out, "[{}]", positions.add(i).read_unaligned())?;
            }
            Ok(())
        }) else {
            return;
        };
        let Failed = fail_as(
            ELEMENT_SET_TWICE,
            &[
                ("element", element.at()),
                ("within", (&raw const within).cast()),
            ],
        );
    }
}

/// Fail with the error of the kind at `kind` in the compiler's table of
/// the tuple at `tuple`, of the type `desc` describes, written as
/// `debug_value` writes it: a `shared_tuple`, a `missing_tuple` or a
/// `tuple_removed_twice`.
///
/// # Safety
///
/// `tuple` must point to a valid tuple of the type `desc` describes.
#[cfg(not(test))]
unsafe fn tuple_failed(kind: usize, tuple: *const u8, desc: &TypeDesc) -> Failed {
    // SAFETY: guaranteed by the caller.
    let Ok(tuple) = (unsafe { Made::value(tuple, desc) }) else {
        return Failed;
    };
    // SAFETY: the tuple is a valid text.
    unsafe { fail_as(kind, &[("value", tuple.at())]) }
}

/// Store at `shared`, a byte, 1 if the relation `value`, of the type `desc`
/// describes, has two tuples with the same values for the attributes of
/// `key`, the descriptor of its projection on one of its keys, and the
/// positions of two such tuples at `pair`, two `usize`s; else 0. Fails if
/// there is no memory to compare them in.
///
/// # Safety
///
/// `value` must point to a valid relation of the type `desc` describes,
/// `key` to the descriptor of a relation of some of its attributes,
/// `shared` to a byte, and `pair` to two `usize`s.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_key_shared(
    shared: *mut u8,
    pair: *mut usize,
    value: *const u8,
    desc: *const TypeDesc,
    key: *const TypeDesc,
) -> bool {
    // SAFETY: guaranteed by the caller; the key's attributes and the
    // tuples' positions are written before they are read.
    status(|| unsafe {
        shared.write(0);
        let (tuples, count) = read_slice(value);
        if count < 2 {
            return Ok(());
        }
        let (desc, key) = (&*desc, attributes(&*key));
        let element = desc.element();
        // The tuples' own attributes in the key, by name.
        let found = Temp::<*const Attr>::new(key.len())?;
        let mut len = 0;
        for attr in attributes(element) {
            if key.iter().any(|k| name(k) == name(attr)) {
                found.ptr().add(len).write(attr);
                len += 1;
            }
        }
        let compared = array(found.ptr(), len);
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
        let mut slots = Temp::<usize>::new(count)?;
        for i in 0..count {
            slots.ptr().add(i).write(i);
        }
        let positions = slots.values();
        positions.sort_unstable_by(|&i, &j| on_key(i, j).then(i.cmp(&j)));
        if let Some(&[first, second]) = positions
            .windows(2)
            .find(|pair| on_key(pair[0], pair[1]).is_eq())
        {
            shared.write(1);
            pair.write_unaligned(first);
            pair.add(1).write_unaligned(second);
        }
        Ok(())
    })
}

/// As [`trendy_rt_key_shared`], of a relation that has been without two such
/// tuples but for the tuples of the relation `added`, which it has now: only
/// they can share a key's values with another, so only they are looked for,
/// by a binary search where the key's attributes are the first in the tuples'
/// order, as those that share them are together then, and otherwise by going
/// through the tuples, either way without sorting them all.
///
/// # Safety
///
/// As for [`trendy_rt_key_shared`], and `added` must point to a valid relation
/// of the type `desc` describes, each of whose tuples `value` has.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_key_added(
    shared: *mut u8,
    pair: *mut usize,
    value: *const u8,
    desc: *const TypeDesc,
    key: *const TypeDesc,
    added: *const u8,
) -> bool {
    // SAFETY: guaranteed by the caller; the key's attributes are written
    // before they are read.
    status(|| unsafe {
        shared.write(0);
        let (tuples, count) = read_slice(value);
        let (added, added_count) = read_slice(added);
        let (desc, key) = (&*desc, attributes(&*key));
        let element = desc.element();
        let found = Temp::<*const Attr>::new(key.len())?;
        let mut len = 0;
        // Whether they are the first attributes, in the tuples' order.
        let mut first = true;
        let mut others = false;
        for attr in attributes(element) {
            if key.iter().any(|k| name(k) == name(attr)) {
                found.ptr().add(len).write(attr);
                len += 1;
                first &= !others;
            } else {
                others = true;
            }
        }
        let compared = array(found.ptr(), len);
        let size = element.size;
        let on_key = |a: *const u8, b: *const u8| {
            compared
                .iter()
                .map(|&attr| order(a.add((*attr).offset), b.add((*attr).offset), &*(*attr).desc))
                .find(|o| o.is_ne())
                .unwrap_or(core::cmp::Ordering::Equal)
        };
        for j in 0..added_count {
            let tuple = added.add(j * size);
            let two = if first {
                // The first of those with its values for the key.
                let (mut lower, mut upper) = (0, count);
                while lower < upper {
                    let at = lower + (upper - lower) / 2;
                    if on_key(tuples.add(at * size), tuple).is_lt() {
                        lower = at + 1;
                    } else {
                        upper = at;
                    }
                }
                (lower + 1 < count && on_key(tuples.add((lower + 1) * size), tuple).is_eq())
                    .then_some((lower, lower + 1))
            } else {
                let mut sharing =
                    (0..count).filter(|&i| on_key(tuples.add(i * size), tuple).is_eq());
                sharing.next().zip(sharing.next())
            };
            if let Some((a, b)) = two {
                shared.write(1);
                pair.write_unaligned(a);
                pair.add(1).write_unaligned(b);
                return Ok(());
            }
        }
        Ok(())
    })
}

/// Fail with the error of the tuples at the positions `pair` of the
/// relation `value`, of the type `desc` describes, about to be assigned to
/// the variable named `variable`, a text, which have the same values for
/// the attributes of `key`, the descriptor of its projection on one of the
/// variable's keys: a `key_violated` (R0008).
///
/// # Safety
///
/// As for [`trendy_rt_key_shared`], with `pair` the positions it found,
/// and `variable` a valid text value.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_key_violated(
    value: *const u8,
    desc: *const TypeDesc,
    key: *const TypeDesc,
    pair: *const usize,
    variable: *const u8,
) {
    // SAFETY: guaranteed by the caller.
    unsafe {
        let made = |write: &dyn Fn(&mut dyn Write) -> fmt::Result| Made::written(write);
        let Ok([key, first, second]) = key_violation(value, &*desc, &*key, pair, made) else {
            return;
        };
        let Failed = fail_as(
            KEY_VIOLATED,
            &[
                ("variable", variable),
                ("key", key.at()),
                ("first", first.at()),
                ("second", second.at()),
            ],
        );
    }
}

/// As [`trendy_rt_key_violated`], of the relation that is the component
/// named `component`, a text, of a value selected of the type named `ty`,
/// a text, with the attributes of `key` one of the component's keys: a
/// `component_key_violated`.
///
/// # Safety
///
/// As for [`trendy_rt_key_violated`], with `ty` and `component` valid text
/// values.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_component_key_violated(
    value: *const u8,
    desc: *const TypeDesc,
    key: *const TypeDesc,
    pair: *const usize,
    ty: *const u8,
    component: *const u8,
) {
    // SAFETY: guaranteed by the caller.
    unsafe {
        let made = |write: &dyn Fn(&mut dyn Write) -> fmt::Result| Made::written(write);
        let Ok([key, first, second]) = key_violation(value, &*desc, &*key, pair, made) else {
            return;
        };
        let Failed = fail_as(
            COMPONENT_KEY_VIOLATED,
            &[
                ("type", ty),
                ("component", component),
                ("key", key.at()),
                ("first", first.at()),
                ("second", second.at()),
            ],
        );
    }
}

/// As [`trendy_rt_component_key_violated`], of a value `load_csv` read
/// from the record `record` describes: a `component_key_violated_in_file`.
///
/// # Safety
///
/// As for [`trendy_rt_component_key_violated`], with `record` pointing to
/// a valid [`Record`].
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_component_key_violated_in_file(
    value: *const u8,
    desc: *const TypeDesc,
    key: *const TypeDesc,
    pair: *const usize,
    ty: *const u8,
    component: *const u8,
    record: *const Record,
) {
    // SAFETY: guaranteed by the caller; the names are copied, and the
    // error takes references of its own.
    let Failed = unsafe {
        let of = ComponentOf {
            ty: ty.cast::<Text>().read_unaligned(),
            component: component.cast::<Text>().read_unaligned(),
        };
        component_key_violated_in_file(None, value, &*desc, &*key, pair, &of, &*record)
            .expect_err("with nothing to collect it, an error fails")
    };
}

/// The texts of a key violation, its error's components, each made by
/// `make` from what writes it: the attributes of `key`, the descriptor of
/// the relation's projection on one of its keys, by name, separated by
/// commas, and the tuples at the positions `pair` of the relation
/// `value`, of the type `desc` describes, as `debug_value` writes them.
///
/// # Safety
///
/// `value` must point to a valid relation of the type `desc` describes,
/// `key` be the descriptor of its projection on one of its keys, and
/// `pair` point to two positions of its tuples.
#[cfg(not(test))]
unsafe fn key_violation<T>(
    value: *const u8,
    desc: &TypeDesc,
    key: &TypeDesc,
    pair: *const usize,
    make: impl Fn(&dyn Fn(&mut dyn Write) -> fmt::Result) -> Fallible<T>,
) -> Fallible<[T; 3]> {
    // SAFETY: guaranteed by the caller.
    unsafe {
        let names = make(&|out| {
            for (i, attr) in attributes(key).iter().enumerate() {
                if i > 0 {
                    out.write_str(", ")?;
                }
                out.write_str(name(attr))?;
            }
            Ok(())
        })?;
        let (tuples, _) = read_slice(value);
        let element = desc.element();
        let tuple = |i: usize| {
            let at = tuples.add(pair.add(i).read_unaligned() * element.size);
            make(&|mut out| write_value(&mut out, at, element))
        };
        Ok([names, tuple(0)?, tuple(1)?])
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

/// A call a failure passed through, which the function it failed in
/// records as its failure exit runs: the site it was called at, and the
/// function's name, a text.
#[cfg(not(test))]
#[derive(Clone, Copy)]
struct Frame {
    site: *const Site,
    function: *const Text,
}

/// The calls the failure under way has passed through, innermost first,
/// in memory from `malloc`, which the program keeps, and how many more it
/// had no memory for, the outermost.
#[cfg(not(test))]
struct Trace {
    frames: *mut Frame,
    len: usize,
    capacity: usize,
    lost: usize,
}

/// The failure's trace, which only the thread the program runs on uses.
#[cfg(not(test))]
struct Traced(core::cell::UnsafeCell<Trace>);

// SAFETY: the program runs on one thread.
#[cfg(not(test))]
unsafe impl Sync for Traced {}

#[cfg(not(test))]
impl Traced {
    /// Where the trace is, which no two references to may be live at once.
    fn get(&self) -> *mut Trace {
        self.0.get()
    }
}

/// The failure's trace.
#[cfg(not(test))]
static TRACE: Traced = Traced(core::cell::UnsafeCell::new(Trace {
    frames: core::ptr::null_mut(),
    len: 0,
    capacity: 0,
    lost: 0,
}));

/// Record that the failure under way passes through a call of the
/// function named `function`, a text, made at the site `*site` holds, as
/// the function's failure exit runs: none for one the runtime called,
/// whose site is null. It can't fail: a call there is no memory to record
/// is counted.
///
/// # Safety
///
/// `site` must point to a site's address, or null, and `function` to a
/// valid text value that lasts as long as the program.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_trace(site: *const *const Site, function: *const Text) {
    // SAFETY: guaranteed by the caller; the trace is only this thread's.
    unsafe {
        let site = site.read();
        if site.is_null() {
            return;
        }
        let trace = &mut *TRACE.get();
        if trace.lost == 0 && trace.len == trace.capacity {
            let capacity = (trace.capacity * 2).max(8);
            let grown = capacity
                .checked_mul(size_of::<Frame>())
                .map_or(core::ptr::null_mut(), |bytes| {
                    realloc(trace.frames.cast(), bytes)
                });
            if !grown.is_null() {
                trace.frames = grown.cast();
                trace.capacity = capacity;
            }
        }
        if trace.lost == 0 && trace.len < trace.capacity {
            trace.frames.add(trace.len).write(Frame { site, function });
            trace.len += 1;
        } else {
            trace.lost += 1;
        }
    }
}

/// Store at `dst` how many calls the trace has recorded so far, for
/// [`trendy_rt_trace_since`].
///
/// # Safety
///
/// `dst` must point to memory for a `usize`.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_traced(dst: *mut usize) {
    // SAFETY: guaranteed by the caller; the trace is only this thread's.
    unsafe { dst.write_unaligned((*TRACE.get()).len) }
}

/// Forget the first `*since` calls the trace recorded, keeping those
/// recorded after [`trendy_rt_traced`] counted them: those of a failure
/// that replaces the one they were for, as one writing the other's
/// message.
///
/// # Safety
///
/// `since` must point to what [`trendy_rt_traced`] stored, with no calls
/// forgotten since.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_trace_since(since: *const usize) {
    // SAFETY: guaranteed by the caller; the trace is only this thread's,
    // and holds `len` frames.
    unsafe {
        let trace = &mut *TRACE.get();
        let since = since.read_unaligned().min(trace.len);
        core::ptr::copy(trace.frames.add(since), trace.frames, trace.len - since);
        trace.len -= since;
    }
}

/// Forget the calls the trace recorded after the first `*since`, which
/// [`trendy_rt_traced`] counted: those of a failure a transaction takes,
/// which is written nowhere.
///
/// # Safety
///
/// `since` must point to what [`trendy_rt_traced`] stored, with no calls
/// forgotten since.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_untrace(since: *const usize) {
    // SAFETY: guaranteed by the caller; the trace is only this thread's.
    unsafe {
        let trace = &mut *TRACE.get();
        trace.len = since.read_unaligned().min(trace.len);
        trace.lost = 0;
    }
}

/// Write the calls the trace recorded, after the failure's error, each
/// as a note, `= note: in `f`, called at path:line:column`, innermost
/// first, and forget them.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn trendy_rt_write_trace() {
    let mut out = Stderr::default();
    // SAFETY: the trace is only this thread's, holds `len` frames, and
    // each points to a site and a name compiled into the program, whose
    // texts are UTF-8.
    unsafe {
        let trace = &mut *TRACE.get();
        // Calls that went too deep are too many to write: the innermost, and
        // the outermost, say what they were doing.
        let (first, last) = if trace.len > 2 * TRACE_WRITTEN {
            (TRACE_WRITTEN, trace.len - TRACE_WRITTEN)
        } else {
            (trace.len, trace.len)
        };
        let write_frame = |out: &mut Stderr, i: usize| {
            let frame = trace.frames.add(i).read();
            let site = &*frame.site;
            let path = read_text((&raw const site.path).cast());
            let function = read_text(frame.function.cast());
            // There is nowhere to report a failure to write to stderr.
            let _ = writeln!(
                out,
                "  = note: in `{function}`, called at {path}:{}:{}",
                site.line, site.column
            );
        };
        for i in 0..first {
            write_frame(&mut out, i);
        }
        if last > first {
            let _ = writeln!(
                out,
                "  = note: and {} calls more, the outermost:",
                last - first
            );
        }
        for i in last..trace.len {
            write_frame(&mut out, i);
        }
        if trace.lost > 0 {
            let _ = writeln!(out, "  = note: and {} calls more", trace.lost);
        }
        (trace.len, trace.lost) = (0, 0);
    }
    out.flush();
}

/// How many calls a trace writes of each end, of more than twice as many.
#[cfg(not(test))]
const TRACE_WRITTEN: usize = 8;

/// How much stack the program runs on, which only the calls it makes touch:
/// what a recursion with work left after each call can use, before it fails
/// (R0025), and what the system doesn't reserve until it is used.
#[cfg(not(test))]
const STACK_BYTES: usize = 1 << 30;

/// How much of it a recursive function leaves as it starts: for what it calls
/// that isn't recursive, whose depth the program's text bounds, for the
/// runtime's functions, and for the failure when there is too little.
#[cfg(not(test))]
const STACK_MARGIN: usize = 8 << 20;

/// The lowest the stack pointer goes as a recursive function starts, which
/// compiled code compares it to ([`trendy_rt_too_deep`]).
#[unsafe(export_name = "trendy_rt_stack_limit")]
pub static STACK_LIMIT: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

/// Room for a `pthread_attr_t`, which is opaque, of any system's.
#[cfg(not(test))]
#[repr(C, align(16))]
struct PthreadAttr([u8; 128]);

/// The program, on the thread [`trendy_rt_run`] made for it, whose stack it
/// sets the limit of: `entry`, the program's entry function, whose failing
/// is what it returns.
#[cfg(not(test))]
extern "C" fn run_on_stack(entry: *mut core::ffi::c_void) -> *mut core::ffi::c_void {
    let top = core::ptr::addr_of!(entry) as usize;
    STACK_LIMIT.store(
        top - (STACK_BYTES - STACK_MARGIN),
        core::sync::atomic::Ordering::Relaxed,
    );
    // SAFETY: it is the entry function `trendy_rt_run` was given.
    let entry: extern "C" fn() -> u8 = unsafe { core::mem::transmute(entry) };
    core::ptr::null_mut::<core::ffi::c_void>().wrapping_add(usize::from(entry()))
}

/// Run the program, whose entry function is `entry`, on a thread of its own,
/// the only one that does anything, with a stack `STACK_BYTES` long, as the
/// main thread's is whatever the system says, and small: a recursion with work
/// left after each call can then go millions of calls deep. The status to exit
/// with: 1 if it failed, and 0 if not.
///
/// # Safety
///
/// `entry` must be a function that takes nothing and returns whether it failed.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_run(entry: extern "C" fn() -> u8) -> i32 {
    // SAFETY: each is given what it asks for, of memory that outlives it.
    unsafe {
        let mut attr = core::mem::MaybeUninit::<PthreadAttr>::uninit();
        let mut thread = 0;
        let started = pthread_attr_init(attr.as_mut_ptr()) == 0
            && pthread_attr_setstacksize(attr.as_mut_ptr(), STACK_BYTES) == 0
            && pthread_create(
                &raw mut thread,
                attr.as_ptr(),
                run_on_stack,
                entry as *mut _,
            ) == 0;
        if !started {
            let mut out = Stderr::default();
            // There is nowhere to report a failure to write to stderr.
            let _ = writeln!(out, "error: no memory for the program's stack");
            out.flush();
            return 1;
        }
        let mut failed = core::ptr::null_mut();
        pthread_join(thread, &raw mut failed);
        i32::from(!failed.is_null())
    }
}

/// Fail with the error of calls that have used up the stack, a `too_deep`
/// (R0025): compiled code compares the stack pointer to [`STACK_LIMIT`] as a
/// recursive function starts.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn trendy_rt_too_deep() -> bool {
    let bytes = i64::try_from(STACK_BYTES).unwrap_or(i64::MAX);
    // SAFETY: the component is a valid integer.
    status(|| unsafe { Err(fail_as(TOO_DEEP, &[("bytes", (&raw const bytes).cast())])) })
}

/// The site the program is at, which compiled code sets before each
/// statement runs, and each value of a value block's is computed, and
/// again after each call, as the callee sets its own; null before the
/// first. A failure writes it.
#[unsafe(export_name = "trendy_rt_site")]
pub static SITE: core::sync::atomic::AtomicPtr<Site> =
    core::sync::atomic::AtomicPtr::new(core::ptr::null_mut());

/// Stop the program because the compiler and the runtime disagree, a
/// compiler bug, which nothing should catch, writing `message`.
#[cfg(not(test))]
fn internal_error(message: &str) -> ! {
    write_failure(|out| writeln!(out, "error[X0000]: internal error: {message}"));
    // SAFETY: `exit` has no preconditions.
    unsafe { exit(1) }
}

/// Write the error of a failure, or a `warn`'s warning, to stderr: the
/// message `write` writes, a line that starts with the compiler's code,
/// then where the program is, as a compiler's error says,
/// ` --> path:line:column`. The
/// program then fails: compiled code goes on to where its block's
/// failures go.
#[cfg(not(test))]
fn write_failure(write: impl FnOnce(&mut Stderr) -> fmt::Result) {
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
}

/// A description of a type, emitted by the compiler.
#[repr(C)]
pub struct TypeDesc {
    /// One of the `KIND_` constants.
    pub kind: u8,
    /// Whether a value of this type can point to buffers: whether it is or
    /// has a text, a relation or an array, or, in a hierarchy, one of its
    /// types' does. One that can't takes no walking when a reference to
    /// what it points to would be taken or given up.
    pub points: bool,
    /// How many integers a value of this type, or a tuple of a relation of
    /// this type, is, if it is nothing else, one after another in the
    /// order they compare, and otherwise 0. Comparing them takes no walking
    /// through their descriptors.
    pub integers: usize,
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
    /// components are its attributes, and of the name its values are
    /// written with, in the stored representation: of the type that
    /// declares it, its own or, of a subtype defined by constraints, its
    /// supertype's. For other kinds, the lengths are 0.
    pub name: *const u8,
    pub name_len: usize,
    pub written: *const u8,
    pub written_len: usize,
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
/// a table, as [`write_table`] writes it. Fails if there is no memory for
/// its columns' widths.
///
/// # Safety
///
/// As for [`trendy_rt_debug_value`], of a relation.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_debug_table(value: *const u8, desc: *const TypeDesc) -> bool {
    // SAFETY: guaranteed by the caller; the widths are memory for a width
    // of each attribute, which `write_table` writes before it reads.
    status(|| unsafe {
        let desc = &*desc;
        let mut widths = Temp::<usize>::zeroed(desc.count)?;
        let mut out = Stderr::default();
        // There is nowhere to report a failure to write to stderr.
        let _ = write_table(&mut out, value, desc, widths.values());
        out.flush();
        Ok(())
    })
}

/// Store at `dst` the sum of the attribute at `index`, its position by
/// name, of the tuples of the relation `src`, of the type `desc` describes:
/// the exact sum of the integers or approximate numbers, rounded once, as
/// the compiler sums them. Fails if the sum is beyond its type's range.
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
) -> bool {
    // SAFETY: guaranteed by the caller.
    status(|| unsafe {
        let column = Column::new(src, &*desc, index.read_unaligned());
        if column.kind == KIND_INTEGER {
            let mut sum = trendy_numeric::IntegerSum::default();
            column.values().for_each(|n| sum.add(n));
            store_integer(dst, sum.total(), "the sum")
        } else {
            let mut sum = trendy_numeric::ApproximateSum::default();
            column.values().for_each(|x| sum.add(x));
            store_approximate(dst, sum.total(), "the sum")
        }
    })
}

/// Store at `dst` the product of the attribute at `index`, as
/// [`trendy_rt_sum`] stores the sum: exact, rounded once, as the compiler
/// multiplies. A product of approximate numbers too near halfway between two
/// for 128 bits to tell which is worked out in full, in memory it frees.
/// Fails if the product is beyond its type's range, or that memory can't
/// be allocated.
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
) -> bool {
    // SAFETY: guaranteed by the caller.
    status(|| unsafe {
        let column = Column::new(src, &*desc, index.read_unaligned());
        if column.kind == KIND_INTEGER {
            let mut product = trendy_numeric::IntegerProduct::default();
            column.values().for_each(|n| product.multiply(n));
            return store_integer(dst, product.total(), "the product");
        }
        let mut product = trendy_numeric::ApproximateProduct::default();
        column.values().for_each(|x| product.multiply(x));
        let total = match product.total() {
            trendy_numeric::Rounded::Nearest(total) => total,
            trendy_numeric::Rounded::Exact(exact) => {
                let mut memory = Temp::<u64>::zeroed(exact.limbs())?;
                exact.total(column.values(), memory.values())
            }
        };
        store_approximate(dst, total, "the product")
    })
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

/// Store the integer `total` at `dst`, or fail if there is none, being out
/// of range: an `overflow` of `operation`, as `the sum`.
///
/// # Safety
///
/// `dst` must point to memory for an integer.
#[cfg(not(test))]
unsafe fn store_integer(dst: *mut u8, total: Option<i64>, operation: &str) -> Fallible {
    let Some(total) = total else {
        return Err(overflowed(operation));
    };
    // SAFETY: guaranteed by the caller.
    unsafe { dst.cast::<i64>().write_unaligned(total) };
    Ok(())
}

/// Store the approximate number `total` at `dst`, or fail if there is
/// none, being beyond the largest finite: an `overflow` of `operation`, as
/// `the sum`.
///
/// # Safety
///
/// `dst` must point to memory for an approximate number.
#[cfg(not(test))]
unsafe fn store_approximate(dst: *mut u8, total: Option<f64>, operation: &str) -> Fallible {
    let Some(total) = total else {
        return Err(overflowed(operation));
    };
    // SAFETY: guaranteed by the caller.
    unsafe { dst.cast::<f64>().write_unaligned(total) };
    Ok(())
}

/// Store at `dst` the text [`trendy_rt_debug_value`] writes for `value`,
/// of the type `desc` describes, in memory the statement owns. Fails if
/// the memory can't be allocated.
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
) -> bool {
    // SAFETY: guaranteed by the caller.
    status(|| unsafe {
        dst.write_unaligned(written(value, &*desc)?);
        Ok(())
    })
}

/// The text [`trendy_rt_debug_value`] writes for `value`, of the type
/// `desc` describes, in memory the statement's scope holds. Fails if the
/// memory can't be allocated, as for a text too long to count.
///
/// # Safety
///
/// As for [`trendy_rt_debug_value`]. The statement must have begun a
/// scope.
#[cfg(not(test))]
unsafe fn written(value: *const u8, desc: &TypeDesc) -> Fallible<Text> {
    // SAFETY: guaranteed by the caller.
    unsafe {
        text_of(
            value,
            desc,
            |bytes| allocate_buffer(bytes, core::ptr::null()),
            out_of_memory,
        )
    }
}

/// Store at `dst` the integer the text `from` writes, as `parse_integer`
/// reads one, and the compiler with it. Fails if the text writes none
/// (R0014).
///
/// # Safety
///
/// `from` must point to a valid text value, and `dst` to memory for an
/// integer.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_parse_integer(dst: *mut i64, from: *const Text) -> bool {
    // SAFETY: guaranteed by the caller.
    status(|| unsafe {
        let text = read_text(from.cast());
        let n =
            trendy_numeric::parse_integer(text).map_err(|why| unparsed(text, "integer", why))?;
        dst.write_unaligned(n);
        Ok(())
    })
}

/// Store at `dst` the approximate number nearest the decimal number the
/// text `from` writes, as `parse_approximate` reads one, and the compiler
/// with it. Fails if the text writes none (R0014).
///
/// # Safety
///
/// `from` must point to a valid text value, and `dst` to memory for an
/// approximate number.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_parse_approximate(dst: *mut f64, from: *const Text) -> bool {
    // SAFETY: guaranteed by the caller.
    status(|| unsafe {
        let text = read_text(from.cast());
        let x = trendy_numeric::parse_approximate(text)
            .map_err(|why| unparsed(text, "approximate", why))?;
        dst.write_unaligned(x);
        Ok(())
    })
}

/// Store at `dst` the square root of `*x`, as `sqrt` computes it, and the
/// compiler with it. Fails if `*x` is negative, which has none (R0018).
///
/// # Safety
///
/// `x` must point to an approximate number, and `dst` to memory for one.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_sqrt(dst: *mut f64, x: *const f64) -> bool {
    // SAFETY: guaranteed by the caller.
    let x = unsafe { x.read_unaligned() };
    status(|| {
        // SAFETY: the operand is an approximate number.
        let root = trendy_numeric::sqrt(x)
            .ok_or_else(|| unsafe { fail_as(NO_SQUARE_ROOT, &[("of", (&raw const x).cast())]) })?;
        // SAFETY: guaranteed by the caller.
        unsafe { dst.write_unaligned(root) };
        Ok(())
    })
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
/// reads one, and the compiler with it. Fails if the text writes none
/// (R0014).
///
/// # Safety
///
/// `from` must point to a valid text value, and `dst` to memory for a
/// boolean.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_parse_boolean(dst: *mut u8, from: *const Text) -> bool {
    // SAFETY: guaranteed by the caller.
    status(|| unsafe {
        let text = read_text(from.cast());
        let b = trendy_text::parse_boolean(text).map_err(|why| unparsed(text, "boolean", why))?;
        dst.write(u8::from(b));
        Ok(())
    })
}

/// Store at `dst` the array of the code points of the text `from`, in
/// order, of the type `desc` describes, whose elements are a code point's
/// stored components, `{ value: integer }`, its number, in memory the
/// statement owns, as `to_code_points` gives them, and the compiler with
/// it. Fails if the memory can't be allocated.
///
/// # Safety
///
/// `from` must point to a valid text value, `desc` to a valid descriptor
/// of that array type, and `dst` to memory for an array. The statement
/// must have begun a scope.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_to_code_points(
    dst: *mut Text,
    from: *const Text,
    desc: *const TypeDesc,
) -> bool {
    // SAFETY: guaranteed by the caller; each element has room for the
    // components its descriptor describes.
    status(|| unsafe {
        let text = read_text(from.cast());
        let desc = &*desc;
        let element = desc.element();
        let [number] = attributes(element) else {
            unreachable!("a code point's stored components are its number");
        };
        let count = text.chars().count();
        let Some(size) = count.checked_mul(element.size) else {
            return Err(out_of_memory());
        };
        let points = allocate_buffer(size, desc)?;
        for (i, c) in text.chars().enumerate() {
            let at = points.add(i * element.size + number.offset);
            write_number(at, (*number.desc).kind, u32::from(c));
        }
        dst.write_unaligned(Text {
            bytes: points,
            len: count,
        });
        Ok(())
    })
}

/// Store at `dst` the array of the characters of the text `from`, in order, of the type
/// `desc` describes, in memory the statement owns, as `to_characters` gives them, and
/// the compiler with it. Fails if the memory can't be allocated.
///
/// # Safety
///
/// `from` must point to a valid text value, `desc` to a valid descriptor of that array
/// type, and `dst` to memory for an array. The statement must have begun a scope.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_to_characters(
    dst: *mut Text,
    from: *const Text,
    desc: *const TypeDesc,
) -> bool {
    // SAFETY: guaranteed by the caller; each element has room for a text value.
    status(|| unsafe {
        let text = read_text(from.cast());
        let desc = &*desc;
        let element = desc.element();
        let count = trendy_text::characters(text).count();
        let Some(size) = count.checked_mul(element.size) else {
            return Err(out_of_memory());
        };
        let characters = allocate_buffer(size, desc)?;
        for (i, character) in trendy_text::characters(text).enumerate() {
            let value = match ascii_character(character) {
                Some(value) => value,
                None => scope_text(character)?,
            };
            copy_element(
                characters,
                characters.add(i * element.size),
                (&raw const value).cast(),
                element,
            );
        }
        dst.write_unaligned(Text {
            bytes: characters,
            len: count,
        });
        Ok(())
    })
}

/// Store at `dst` the text of the characters of the array `from`, in order, in memory
/// the statement owns, as `from_characters` gives it, and the compiler with it. Fails
/// if the memory can't be allocated.
///
/// # Safety
///
/// `from` must point to a valid array of characters, and `dst` to memory for a text
/// value. The statement must have begun a scope.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_from_characters(dst: *mut Text, from: *const Text) -> bool {
    // SAFETY: guaranteed by the caller; an array of characters is an array of what a
    // text value is laid out as.
    status(|| unsafe {
        let array = from.read_unaligned();
        let character = |i: usize| read_slice(array.bytes.add(i * size_of::<Text>()));
        let mut len = 0usize;
        for i in 0..array.len {
            len = len.checked_add(character(i).1).ok_or_else(out_of_memory)?;
        }
        let bytes = allocate_buffer(len, core::ptr::null())?;
        let mut at = 0;
        for i in 0..array.len {
            let (written, written_len) = character(i);
            core::ptr::copy_nonoverlapping(written, bytes.add(at), written_len);
            at += written_len;
        }
        dst.write_unaligned(Text { bytes, len });
        Ok(())
    })
}

/// Store at `dst` the text of the code points of the array `from`, in
/// order, of the type `desc` describes, whose elements are a code point's
/// stored components, as [`trendy_rt_to_code_points`] makes them, in memory
/// the statement owns, as `from_code_points` gives it, and the compiler
/// with it. Fails if the memory can't be allocated.
///
/// # Safety
///
/// `from` must point to a valid array of that type, `desc` to its
/// descriptor, and `dst` to memory for a text value. The statement must
/// have begun a scope.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_from_code_points(
    dst: *mut Text,
    from: *const Text,
    desc: *const TypeDesc,
) -> bool {
    // SAFETY: guaranteed by the caller; each code point is a Unicode scalar
    // value, as `code_point`'s constraint says.
    status(|| unsafe {
        let array = from.read_unaligned();
        let element = (*desc).element();
        let [number] = attributes(element) else {
            unreachable!("a code point's stored components are its number");
        };
        let point = |i: usize| {
            let at = array.bytes.add(i * element.size + number.offset);
            let n = match (*number.desc).kind {
                KIND_U32 => Some(narrow(at, KIND_U32)),
                _ => u32::try_from(at.cast::<i64>().read_unaligned()).ok(),
            };
            n.and_then(char::from_u32)
                .unwrap_or_else(|| internal_error("a code point is no Unicode scalar value"))
        };
        let mut len = 0usize;
        for i in 0..array.len {
            len = len
                .checked_add(point(i).len_utf8())
                .ok_or_else(out_of_memory)?;
        }
        let bytes = allocate_buffer(len, core::ptr::null())?;
        let mut at = 0;
        for i in 0..array.len {
            let mut buffer = [0; 4];
            let encoded = point(i).encode_utf8(&mut buffer);
            core::ptr::copy_nonoverlapping(encoded.as_ptr(), bytes.add(at), encoded.len());
            at += encoded.len();
        }
        dst.write_unaligned(Text { bytes, len });
        Ok(())
    })
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
}

/// Fail because `text` isn't a value of the type named `ty`, as `why`
/// says: an `unparsable` (R0014).
#[cfg(not(test))]
fn unparsed(text: &str, ty: &str, why: trendy_text::Unparsable) -> Failed {
    let (Ok(text), Ok(ty)) = (Made::text(text), Made::text(ty)) else {
        return Failed;
    };
    let out_of_range = u8::from(why == trendy_text::Unparsable::OutOfRange);
    // SAFETY: the text and the type are valid texts, and whether it is out
    // of range a boolean.
    unsafe {
        fail_as(
            UNPARSABLE,
            &[
                ("text", text.at()),
                ("type", ty.at()),
                ("out_of_range", (&raw const out_of_range).cast()),
            ],
        )
    }
}

/// Store at `dst` the array of the lines of the text file at `path`, in
/// the file's order, of the type `desc` describes, `array text`, in memory
/// the statement owns: each line without the line feed, or the carriage
/// return and line feed, that ends it. The last line needs no ending.
/// Fails if the file can't be read (R0012), or isn't UTF-8 text (R0013),
/// or the memory can't be allocated.
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
) -> bool {
    // SAFETY: guaranteed by the caller.
    status(|| unsafe {
        let path = read_text(path.cast());
        // With no errors to give its failure to, a file that can't be read
        // fails.
        let lines = match file_or_error(path, None)? {
            Some((_contents, text)) => lines_of(text, &*desc)?,
            None => lines_of("", &*desc)?,
        };
        dst.write_unaligned(lines);
        Ok(())
    })
}

/// As [`trendy_rt_load_text_file`], but rather than failing for a file
/// that can't be read, or isn't UTF-8 text, store at `dst` no lines, and
/// at `errors` its one error, an `unreadable_file` or a `not_utf8`, and
/// none if the file is read. Fails if the memory can't be allocated.
///
/// # Safety
///
/// As for [`trendy_rt_load_text_file`], and `errors` must point to a valid
/// [`Errors`] of the types [`UNREADABLE_FILE`] and [`NOT_UTF8`] say.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_load_text_file_with_errors(
    dst: *mut Text,
    path: *const Text,
    errors: *mut Errors,
    desc: *const TypeDesc,
) -> bool {
    // SAFETY: guaranteed by the caller.
    status(|| unsafe {
        let path = read_text(path.cast());
        let mut found = Found::new((*errors).relation);
        let lines = match file_or_error(path, Some(&mut found))? {
            Some((_contents, text)) => lines_of(text, &*desc)?,
            None => lines_of("", &*desc)?,
        };
        let found = found.finish()?;
        dst.write_unaligned(lines);
        (&raw mut (*errors).found).write_unaligned(found);
        Ok(())
    })
}

/// Write the array of texts at `lines` to the text file at `path`, each ending
/// in a line feed, so that [`trendy_rt_load_text_file`] reads them back. Fails
/// if the file can't be written (R0026), if a line wouldn't read back as one
/// (R0027), before anything is written, or if the memory can't be allocated.
///
/// # Safety
///
/// `path` must point to a valid text value, and `lines` to a valid array of
/// them. The statement must have begun a scope.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_save_text_file(path: *const Text, lines: *const Text) -> bool {
    // SAFETY: guaranteed by the caller.
    status(|| unsafe { save_text_file(read_text(path.cast()), lines, None) })
}

/// As [`trendy_rt_save_text_file`], but rather than failing for a file that
/// can't be written, store at `errors` its one error, an `unwritable_file`,
/// and none if the file is written. A line that wouldn't read back as one
/// still fails, as that is a mistake in the call.
///
/// # Safety
///
/// As for [`trendy_rt_save_text_file`], and `errors` must point to a valid
/// [`Errors`].
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_save_text_file_with_errors(
    path: *const Text,
    lines: *const Text,
    errors: *mut Errors,
) -> bool {
    // SAFETY: guaranteed by the caller.
    status(|| unsafe {
        let mut found = Found::new((*errors).relation);
        save_text_file(read_text(path.cast()), lines, Some(&mut found))?;
        let found = found.finish()?;
        (&raw mut (*errors).found).write_unaligned(found);
        Ok(())
    })
}

/// What [`trendy_rt_save_text_file`] and
/// [`trendy_rt_save_text_file_with_errors`] do: with `found`, why the file can't
/// be written goes there; without it, that fails.
///
/// # Safety
///
/// As for [`trendy_rt_save_text_file`], and `found`'s descriptor must be valid.
#[cfg(not(test))]
unsafe fn save_text_file(path: &str, lines: *const Text, found: Option<&mut Found>) -> Fallible {
    // SAFETY: guaranteed by the caller: an array of texts has them one after
    // another, each a `Text`, as the compiler lays them out.
    unsafe {
        let (elements, count) = read_slice(lines.cast());
        let line = |i: usize| read_text(elements.add(i * size_of::<Text>()));
        if let Some(i) = (0..count).find(|&i| !reads_back_as_one_line(line(i))) {
            let (path, position) = (
                scope_text(path)?,
                i64::try_from(i.saturating_add(1)).unwrap_or(i64::MAX),
            );
            return Err(fail_as(
                NOT_ONE_LINE,
                &[
                    ("path", (&raw const path).cast()),
                    ("line", (&raw const position).cast()),
                ],
            ));
        }
        let written = try_write_file(path, |out| {
            for i in 0..count {
                out.put(line(i).as_bytes())?;
                out.put(b"\n")?;
            }
            Ok(())
        })?;
        if let Err(reason) = written {
            let (path, reason) = (scope_text(path)?, scope_text(reason)?);
            give(
                found,
                UNWRITABLE_FILE,
                &[
                    ("path", (&raw const path).cast()),
                    ("reason", (&raw const reason).cast()),
                ],
            )?;
        }
        Ok(())
    }
}

/// Whether `line`, followed by a line feed, reads back as itself and no more,
/// as [`lines_of`] reads a file: with no line feed in it, and no carriage
/// return at its end, which would make those two one line end.
#[cfg(not(test))]
fn reads_back_as_one_line(line: &str) -> bool {
    !line.contains('\n') && !line.ends_with('\r')
}

/// The text of the file at `path`, and the memory it is in, which it
/// lasts as long as, or `None`, after giving `found` why the file can't be
/// read, or isn't UTF-8 text: an `unreadable_file` (R0012) or a `not_utf8`
/// (R0013), which, with no `found`, it fails with ([`give`]). Fails too if
/// the memory for them can't be allocated.
///
/// # Safety
///
/// `found`'s descriptor must be valid. The statement must have begun a
/// scope.
#[cfg(not(test))]
unsafe fn file_or_error<'a>(
    path: &str,
    mut found: Option<&mut Found>,
) -> Fallible<Option<(Temp<u8>, &'a str)>> {
    // SAFETY: guaranteed by the caller; the text is in the memory, which
    // moving the memory's owner doesn't move.
    unsafe {
        let contents = match try_read_file(path)? {
            Ok(read) => read,
            Err(reason) => {
                let (path, reason) = (scope_text(path)?, scope_text(reason)?);
                give(
                    found,
                    UNREADABLE_FILE,
                    &[
                        ("path", (&raw const path).cast()),
                        ("reason", (&raw const reason).cast()),
                    ],
                )?;
                return Ok(None);
            }
        };
        let bytes = core::slice::from_raw_parts(contents.ptr(), contents.len);
        match try_file_text(bytes) {
            Ok(text) => Ok(Some((contents, text))),
            Err(line) => {
                let path = scope_text(path)?;
                let line = i64::try_from(line).unwrap_or(i64::MAX);
                give(
                    found.take(),
                    NOT_UTF8,
                    &[
                        ("path", (&raw const path).cast()),
                        ("line", (&raw const line).cast()),
                    ],
                )?;
                Ok(None)
            }
        }
    }
}

/// The array of the lines of `text`, of the type `desc` describes,
/// `array text`, in memory the statement owns: each line without the line
/// feed, or the carriage return and line feed, that ends it.
///
/// # Safety
///
/// `desc` must be a valid descriptor of that array type. The statement
/// must have begun a scope. Fails if the memory can't be allocated.
#[cfg(not(test))]
unsafe fn lines_of(text: &str, desc: &TypeDesc) -> Fallible<Text> {
    // SAFETY: guaranteed by the caller.
    unsafe {
        let element = desc.element();
        let count = text.lines().count();
        let Some(size) = count.checked_mul(element.size) else {
            return Err(out_of_memory());
        };
        let lines = allocate_buffer(size, desc)?;
        for (i, line) in text.lines().enumerate() {
            let value = scope_text(line)?;
            copy_element(
                lines,
                lines.add(i * element.size),
                (&raw const value).cast(),
                element,
            );
        }
        Ok(Text {
            bytes: lines,
            len: count,
        })
    }
}

/// A text value of `text`'s bytes, in a buffer the statement's scope
/// holds. Fails if the memory can't be allocated.
#[cfg(not(test))]
fn scope_text(text: &str) -> Fallible<Text> {
    let bytes = allocate_buffer(text.len(), core::ptr::null())?;
    // SAFETY: the buffer has room for the bytes.
    unsafe { core::ptr::copy_nonoverlapping(text.as_ptr(), bytes, text.len()) };
    Ok(Text {
        bytes,
        len: text.len(),
    })
}

/// A text value of what `write` writes, in a buffer the statement's scope
/// holds. Fails if the memory can't be allocated.
#[cfg(not(test))]
fn scope_written(write: impl Fn(&mut dyn Write) -> fmt::Result) -> Fallible<Text> {
    let len = counted(&write)?;
    let text = Text {
        bytes: allocate_buffer(len, core::ptr::null())?,
        len,
    };
    filled(&write, &text)?;
    Ok(text)
}

/// How many bytes `write` writes. Writing to memory fails only if they
/// can't be counted, and then there are too many to allocate.
#[cfg(not(test))]
fn counted(write: &impl Fn(&mut dyn Write) -> fmt::Result) -> Fallible<usize> {
    let mut counted = Counted(0);
    write(&mut counted).map_err(|_| out_of_memory())?;
    Ok(counted.0)
}

/// Write what `write` writes into `text`'s bytes, which [`counted`]
/// counted.
#[cfg(not(test))]
fn filled(write: &impl Fn(&mut dyn Write) -> fmt::Result, text: &Text) -> Fallible {
    // SAFETY: the buffer has room for what is counted.
    let mut filled = Filled {
        bytes: unsafe { core::slice::from_raw_parts_mut(text.bytes.cast_mut(), text.len) },
        len: 0,
    };
    write(&mut filled).map_err(|_| out_of_memory())
}

/// A text a failure makes for its error, whose one reference is its own,
/// given up when it is dropped, after the error takes its own
/// ([`fail_as`]): held by no scope, which a statement that makes nothing
/// hasn't begun.
#[cfg(not(test))]
struct Made(Text);

#[cfg(not(test))]
impl Made {
    /// `text`. Fails if there is no memory for it.
    fn text(text: &str) -> Fallible<Made> {
        Made::written(|out| out.write_str(text))
    }

    /// What `write` writes. Fails if there is no memory for it.
    fn written(write: impl Fn(&mut dyn Write) -> fmt::Result) -> Fallible<Made> {
        let len = counted(&write)?;
        let made = Made(Text {
            bytes: new_buffer(len, core::ptr::null())?,
            len,
        });
        filled(&write, &made.0)?;
        Ok(made)
    }

    /// The value at `value`, of the type `desc` describes, as
    /// `debug_value` writes it. Fails if there is no memory for it.
    ///
    /// # Safety
    ///
    /// `value` must point to a valid value of the type `desc` describes.
    unsafe fn value(value: *const u8, desc: &TypeDesc) -> Fallible<Made> {
        // SAFETY: guaranteed by the caller.
        Made::written(|mut out| unsafe { write_value(&mut out, value, desc) })
    }

    /// Where it is, as a component's value.
    fn at(&self) -> *const u8 {
        (&raw const self.0).cast()
    }
}

#[cfg(not(test))]
impl Drop for Made {
    fn drop(&mut self) {
        // SAFETY: the reference is its own.
        unsafe { release_buffer(self.0.bytes.cast_mut()) }
    }
}

/// Where a load that gives its errors puts them, as the compiler lays it
/// out: the relation of them, `relation { error: error }`, and its
/// descriptor's address. Their types are the compiler's table's
/// ([`Failures`]).
#[repr(C)]
pub struct Errors {
    pub found: Text,
    pub relation: *const TypeDesc,
}

/// What the compiler tells the runtime, once, as the program starts
/// ([`trendy_rt_failures`]), about the errors the language makes, values of
/// the subtypes of the prelude's type `error`: where a failure's error
/// waits for what takes it, a transaction or the program's entry, the
/// program's own variable `__failure`, null when none waits, and where its
/// site does, `__failure_site`; the type's descriptor; and each kind's,
/// [`Kind`], in `hir::LanguageError::ALL`'s order, which [`KINDS`] names.
#[repr(C)]
pub struct Failures {
    pub error: *mut u8,
    pub site: *mut usize,
    pub desc: *const TypeDesc,
    pub kinds: *const Kind,
    pub count: usize,
}

/// A kind of error the language makes, in the compiler's table
/// ([`Failures`]): its type's name, its components' descriptor, as its
/// selector writes them, and the code a failure with it is written with
/// when nothing takes it.
#[repr(C)]
pub struct Kind {
    pub name: Text,
    pub desc: *const TypeDesc,
    pub code: Text,
}

/// The names of the kinds of error the runtime makes, each at its place
/// in the compiler's table, which [`trendy_rt_failures`] checks.
pub const KINDS: [&str; 33] = [
    "unreadable_file",
    "not_utf8",
    "malformed_csv",
    "missing_column",
    "unreadable_field",
    "broken_constraint_in_file",
    "component_key_violated_in_file",
    "overlapping_subtypes_in_file",
    "overflow",
    "division_by_zero",
    "not_one_tuple",
    "index_out_of_range",
    "tuple_mismatch",
    "shared_tuple",
    "missing_tuple",
    "key_violated",
    "component_key_violated",
    "tuple_removed_twice",
    "element_set_twice",
    "no_min_or_max",
    "unparsable",
    "broken_constraint",
    "no_square_root",
    "not_of_type",
    "overlapping_subtypes",
    "broken_call_constraint",
    "too_deep",
    "column_shared",
    "column_below_one",
    "constraint_violated",
    "constraint_violated_by",
    "unwritable_file",
    "not_one_line",
];

/// The place in [`KINDS`] of the kind named `name`, found when the runtime
/// is compiled: a name that isn't one doesn't compile.
const fn place(name: &str) -> usize {
    let mut i = 0;
    while i < KINDS.len() {
        if same(KINDS[i].as_bytes(), name.as_bytes()) {
            return i;
        }
        i += 1;
    }
    panic!("not a kind of error the runtime makes");
}

/// Whether `a` and `b` are the same bytes, as `==` would say, when the
/// runtime is compiled.
const fn same(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

/// The place of `unreadable_file` in the compiler's table of errors.
pub const UNREADABLE_FILE: usize = place("unreadable_file");
/// `not_utf8`'s, as [`UNREADABLE_FILE`] is.
pub const NOT_UTF8: usize = place("not_utf8");
/// `unwritable_file`'s, as [`UNREADABLE_FILE`] is.
pub const UNWRITABLE_FILE: usize = place("unwritable_file");
/// `not_one_line`'s, as [`UNREADABLE_FILE`] is.
pub const NOT_ONE_LINE: usize = place("not_one_line");
/// `malformed_csv`'s, as [`UNREADABLE_FILE`] is.
pub const MALFORMED_CSV: usize = place("malformed_csv");
/// `missing_column`'s, as [`UNREADABLE_FILE`] is.
pub const MISSING_COLUMN: usize = place("missing_column");
/// `unreadable_field`'s, as [`UNREADABLE_FILE`] is.
pub const UNREADABLE_FIELD: usize = place("unreadable_field");
/// `broken_constraint_in_file`'s, as [`UNREADABLE_FILE`] is.
pub const BROKEN_CONSTRAINT_IN_FILE: usize = place("broken_constraint_in_file");
/// `component_key_violated_in_file`'s, as [`UNREADABLE_FILE`] is.
pub const COMPONENT_KEY_VIOLATED_IN_FILE: usize = place("component_key_violated_in_file");
/// `overlapping_subtypes_in_file`'s, as [`UNREADABLE_FILE`] is.
pub const OVERLAPPING_SUBTYPES_IN_FILE: usize = place("overlapping_subtypes_in_file");
/// `overflow`'s, as [`UNREADABLE_FILE`] is.
pub const OVERFLOW: usize = place("overflow");
/// `division_by_zero`'s, as [`UNREADABLE_FILE`] is.
pub const DIVISION_BY_ZERO: usize = place("division_by_zero");
/// `not_one_tuple`'s, as [`UNREADABLE_FILE`] is.
pub const NOT_ONE_TUPLE: usize = place("not_one_tuple");
/// `index_out_of_range`'s, as [`UNREADABLE_FILE`] is.
pub const INDEX_OUT_OF_RANGE: usize = place("index_out_of_range");
/// `tuple_mismatch`'s, as [`UNREADABLE_FILE`] is.
pub const TUPLE_MISMATCH: usize = place("tuple_mismatch");
/// `shared_tuple`'s, as [`UNREADABLE_FILE`] is.
pub const SHARED_TUPLE: usize = place("shared_tuple");
/// `missing_tuple`'s, as [`UNREADABLE_FILE`] is.
pub const MISSING_TUPLE: usize = place("missing_tuple");
/// `key_violated`'s, as [`UNREADABLE_FILE`] is.
pub const KEY_VIOLATED: usize = place("key_violated");
/// `component_key_violated`'s, as [`UNREADABLE_FILE`] is.
pub const COMPONENT_KEY_VIOLATED: usize = place("component_key_violated");
/// `tuple_removed_twice`'s, as [`UNREADABLE_FILE`] is.
pub const TUPLE_REMOVED_TWICE: usize = place("tuple_removed_twice");
/// `element_set_twice`'s, as [`UNREADABLE_FILE`] is.
pub const ELEMENT_SET_TWICE: usize = place("element_set_twice");
/// `no_min_or_max`'s, as [`UNREADABLE_FILE`] is.
pub const NO_MIN_OR_MAX: usize = place("no_min_or_max");
/// `unparsable`'s, as [`UNREADABLE_FILE`] is.
pub const UNPARSABLE: usize = place("unparsable");
/// `broken_constraint`'s, as [`UNREADABLE_FILE`] is.
pub const BROKEN_CONSTRAINT: usize = place("broken_constraint");
/// `no_square_root`'s, as [`UNREADABLE_FILE`] is.
pub const NO_SQUARE_ROOT: usize = place("no_square_root");
/// `not_of_type`'s, as [`UNREADABLE_FILE`] is.
pub const NOT_OF_TYPE: usize = place("not_of_type");
/// `overlapping_subtypes`'s, as [`UNREADABLE_FILE`] is.
pub const OVERLAPPING_SUBTYPES: usize = place("overlapping_subtypes");
/// `broken_call_constraint`'s, as [`UNREADABLE_FILE`] is.
pub const BROKEN_CALL_CONSTRAINT: usize = place("broken_call_constraint");
/// `too_deep`'s, as [`UNREADABLE_FILE`] is.
pub const TOO_DEEP: usize = place("too_deep");
/// `column_shared`'s, as [`UNREADABLE_FILE`] is.
pub const COLUMN_SHARED: usize = place("column_shared");
/// `column_below_one`'s, as [`UNREADABLE_FILE`] is.
pub const COLUMN_BELOW_ONE: usize = place("column_below_one");
/// `constraint_violated`'s, as [`UNREADABLE_FILE`] is.
pub const CONSTRAINT_VIOLATED: usize = place("constraint_violated");
/// `constraint_violated_by`'s, as [`UNREADABLE_FILE`] is.
pub const CONSTRAINT_VIOLATED_BY: usize = place("constraint_violated_by");

/// The compiler's table of errors, null until the program's entry gives
/// it ([`trendy_rt_failures`]).
#[cfg(not(test))]
static FAILURES: core::sync::atomic::AtomicPtr<Failures> =
    core::sync::atomic::AtomicPtr::new(core::ptr::null_mut());

/// Take the compiler's table of errors, `failures`, which lasts as long as
/// the program, checking that each kind is where the runtime knows it
/// ([`KINDS`]): if not, the compiler and the runtime disagree (X0000).
///
/// # Safety
///
/// `failures` must point to a valid [`Failures`], whose places are the
/// program's variables, which last as long as it does.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_failures(failures: *mut Failures) {
    // SAFETY: guaranteed by the caller.
    unsafe {
        let table = &*failures;
        let kinds = core::slice::from_raw_parts(table.kinds, table.count);
        let agree = kinds.len() == KINDS.len()
            && kinds
                .iter()
                .zip(KINDS)
                .all(|(kind, name)| read_text((&raw const kind.name).cast()) == name);
        if !agree {
            internal_error("the compiler's table of errors isn't the runtime's");
        }
        FAILURES.store(failures, core::sync::atomic::Ordering::Relaxed);
    }
}

/// The compiler's table of errors, which the program's entry gives as it
/// starts.
#[cfg(not(test))]
fn failures() -> &'static Failures {
    let failures = FAILURES.load(core::sync::atomic::Ordering::Relaxed);
    // SAFETY: the table lasts as long as the program.
    unsafe { failures.as_ref() }.unwrap_or_else(|| internal_error("no table of errors was given"))
}

/// The kind at `kind` in the compiler's table of errors.
#[cfg(not(test))]
fn kind(kind: usize) -> &'static Kind {
    // SAFETY: the table has the kinds `KINDS` names, as
    // `trendy_rt_failures` checks, and each place is one of theirs.
    unsafe { &*failures().kinds.add(kind) }
}

/// Write at `dst` the error of the kind at `kind` in the compiler's table,
/// a value of the type `desc` describes, the prelude's `error`, whose
/// components are `components`, each by name, a pointer to a value of its
/// type, copied: the kind's descriptor, then the components, as its
/// selector writes them. What they point to is held as the components
/// were.
///
/// # Safety
///
/// `dst` must point to memory for a value `desc` describes, and
/// `components` be the kind's, each pointing to a valid value.
#[cfg(not(test))]
unsafe fn make_error(dst: *mut u8, desc: &TypeDesc, kind: usize, components: &[(&str, *const u8)]) {
    // SAFETY: guaranteed by the caller.
    unsafe {
        let [within] = attributes(desc) else {
            unreachable!("a value in a hierarchy has its components in one attribute");
        };
        let own = self::kind(kind).desc;
        dst.cast::<*const TypeDesc>().write_unaligned(own);
        for component in attributes(&*own) {
            let (_, src) = components
                .iter()
                .find(|(n, _)| *n == name(component))
                .expect("the compiler and the runtime agree on an error's components");
            let at = dst.add(within.offset + component.offset);
            core::ptr::copy_nonoverlapping(*src, at, (*component.desc).size);
        }
    }
}

/// Fail with the error of the kind at `kind` in the compiler's table,
/// whose components are `components`, as [`make_error`] takes them, which
/// the failing statement's scope holds what they point to of: the error
/// waits, with the site the program is at, for what takes it, a
/// transaction or the program's entry, which writes it, as a `fail`'s
/// does. It takes references of its own before any an error waiting has
/// are given up. With no memory for it, the program fails as it does for
/// none, with no error.
///
/// # Safety
///
/// `components` must be the kind's, each pointing to a valid value.
#[cfg(not(test))]
unsafe fn fail_as(kind: usize, components: &[(&str, *const u8)]) -> Failed {
    let failures = failures();
    // SAFETY: the table's places and descriptors are the compiler's, and
    // the rest is guaranteed by the caller.
    unsafe {
        let desc = &*failures.desc;
        let Ok(made) = Temp::<u8>::zeroed(desc.size) else {
            return Failed;
        };
        make_error(made.ptr(), desc, kind, components);
        retain(made.ptr(), desc);
        release(failures.error, desc);
        core::ptr::copy_nonoverlapping(made.ptr(), failures.error, desc.size);
        let site = SITE.load(core::sync::atomic::Ordering::Relaxed);
        failures.site.write_unaligned(site as usize);
    }
    Failed
}

/// Give the error of the kind at `kind` in the compiler's table, whose
/// components are `components`, as [`make_error`] takes them, which the
/// statement's scope holds what they point to of: to `found`, a load's
/// errors, which collect it, or, with none, fail with it ([`fail_as`]).
///
/// # Safety
///
/// `found`'s descriptor must be valid, and `components` the kind's, each
/// pointing to a valid value.
#[cfg(not(test))]
unsafe fn give(
    found: Option<&mut Found>,
    kind: usize,
    components: &[(&str, *const u8)],
) -> Fallible {
    // SAFETY: guaranteed by the caller.
    unsafe {
        match found {
            Some(found) => found.add(kind, components),
            None => Err(fail_as(kind, components)),
        }
    }
}

/// A relation-valued component of a value, as the compiler lays its names
/// out: the name of the value's type, and the component's.
#[repr(C)]
pub struct ComponentOf {
    pub ty: Text,
    pub component: Text,
}

/// Add to the errors `errors` has that the tuples at the positions `pair`
/// of the relation `value`, of the type `desc` describes, the component
/// `of` names of a value read from the record `record` describes, share
/// their values for `key`, the descriptor of its projection on one of its
/// keys: a `component_key_violated_in_file`, its tuples written as
/// `debug_value` writes them.
///
/// # Safety
///
/// As for [`trendy_rt_broken_constraint_error`], with `value`, `desc`,
/// `key` and `pair` as for [`trendy_rt_key_violated`], and `of` pointing
/// to a valid [`ComponentOf`].
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_component_key_error(
    errors: *mut Errors,
    value: *const u8,
    desc: *const TypeDesc,
    key: *const TypeDesc,
    pair: *const usize,
    of: *const ComponentOf,
    record: *const Record,
) -> bool {
    // SAFETY: guaranteed by the caller.
    status(|| unsafe {
        let mut found = Found::of(&*errors)?;
        let of = &*of;
        component_key_violated_in_file(Some(&mut found), value, &*desc, &*key, pair, of, &*record)?;
        (&raw mut (*errors).found).write_unaligned(found.finish()?);
        Ok(())
    })
}

/// Give `found` that the tuples at the positions `pair` of the relation
/// `value`, of the type `desc` describes, the component `of` names of a
/// value read from the record `record` describes, share their values for
/// `key`, the descriptor of its projection on one of its keys: a
/// `component_key_violated_in_file`, its tuples written as `debug_value`
/// writes them, which, with no `found`, it fails with ([`give`]).
///
/// # Safety
///
/// As for [`give`], with `value`, `desc`, `key` and `pair` as for
/// [`trendy_rt_key_violated`]. The statement must have begun a scope.
#[cfg(not(test))]
unsafe fn component_key_violated_in_file(
    found: Option<&mut Found>,
    value: *const u8,
    desc: &TypeDesc,
    key: &TypeDesc,
    pair: *const usize,
    of: &ComponentOf,
    record: &Record,
) -> Fallible {
    // SAFETY: guaranteed by the caller.
    unsafe {
        let scoped = |write: &dyn Fn(&mut dyn Write) -> fmt::Result| scope_written(write);
        let [names, first, second] = key_violation(value, desc, key, pair, scoped)?;
        let line = i64::try_from(record.line).unwrap_or(i64::MAX);
        give(
            found,
            COMPONENT_KEY_VIOLATED_IN_FILE,
            &[
                ("path", (&raw const record.path).cast()),
                ("line", (&raw const line).cast()),
                ("attribute", (&raw const record.attribute).cast()),
                ("type", (&raw const of.ty).cast()),
                ("component", (&raw const of.component).cast()),
                ("key", (&raw const names).cast()),
                ("first", (&raw const first).cast()),
                ("second", (&raw const second).cast()),
            ],
        )
    }
}

/// Add to the errors `errors` has that `value`, of the type of the
/// program's own `desc` describes, read from the record `record`
/// describes, is of both of its subtypes defined by constraints named
/// `first` and `second`, texts, but a value has one most specific type: an
/// `overlapping_subtypes_in_file`, written as `debug_value` writes it.
///
/// # Safety
///
/// As for [`trendy_rt_broken_constraint_error`], with `first` and `second`
/// pointing to valid text values.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_overlapping_subtypes_error(
    errors: *mut Errors,
    value: *const u8,
    desc: *const TypeDesc,
    first: *const Text,
    second: *const Text,
    record: *const Record,
) -> bool {
    // SAFETY: guaranteed by the caller.
    status(|| unsafe {
        let mut found = Found::of(&*errors)?;
        let written = written(value, &*desc)?;
        let record = &*record;
        let line = i64::try_from(record.line).unwrap_or(i64::MAX);
        found.add(
            OVERLAPPING_SUBTYPES_IN_FILE,
            &[
                ("path", (&raw const record.path).cast()),
                ("line", (&raw const line).cast()),
                ("attribute", (&raw const record.attribute).cast()),
                ("value", (&raw const written).cast()),
                ("first", first.cast()),
                ("second", second.cast()),
            ],
        )?;
        (&raw mut (*errors).found).write_unaligned(found.finish()?);
        Ok(())
    })
}

/// Add to the errors `errors` has that `value`, whose components the
/// descriptor `desc` describes, read from the record `record` describes,
/// isn't a value of the type named `ty`, a text: it breaks its constraint
/// written `constraint`, a text, as `load_csv`'s caller finds once it has
/// read the record: a `broken_constraint_in_file`, written as
/// `debug_value` writes it.
///
/// # Safety
///
/// `errors` must point to valid [`Errors`] of the types
/// [`UNREADABLE_FILE`] and those after it say, `value` to valid components
/// `desc` describes, `ty` and `constraint` to valid text values, and
/// `record` to a valid [`Record`]. The statement must have begun a scope.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_broken_constraint_error(
    errors: *mut Errors,
    value: *const u8,
    desc: *const TypeDesc,
    ty: *const Text,
    constraint: *const Text,
    record: *const Record,
) -> bool {
    // SAFETY: guaranteed by the caller.
    status(|| unsafe {
        let mut found = Found::of(&*errors)?;
        broken_constraint_in_file(
            Some(&mut found),
            value,
            &*desc,
            ty.cast(),
            constraint.cast(),
            &*record,
        )?;
        (&raw mut (*errors).found).write_unaligned(found.finish()?);
        Ok(())
    })
}

/// Give `found` that `value`, whose components `desc` describes, read from
/// the record `record` describes, isn't a value of the type named `ty`, a
/// text: it breaks its constraint written `constraint`, a text: a
/// `broken_constraint_in_file`, written as `debug_value` writes it, which,
/// with no `found`, it fails with ([`give`]).
///
/// # Safety
///
/// As for [`give`], with `value` pointing to valid components `desc`
/// describes, and `ty` and `constraint` to valid text values. The
/// statement must have begun a scope.
#[cfg(not(test))]
unsafe fn broken_constraint_in_file(
    found: Option<&mut Found>,
    value: *const u8,
    desc: &TypeDesc,
    ty: *const u8,
    constraint: *const u8,
    record: &Record,
) -> Fallible {
    // SAFETY: guaranteed by the caller.
    unsafe {
        let written = written(value, desc)?;
        let line = i64::try_from(record.line).unwrap_or(i64::MAX);
        give(
            found,
            BROKEN_CONSTRAINT_IN_FILE,
            &[
                ("path", (&raw const record.path).cast()),
                ("line", (&raw const line).cast()),
                ("attribute", (&raw const record.attribute).cast()),
                ("value", (&raw const written).cast()),
                ("type", ty),
                ("constraint", constraint),
            ],
        )
    }
}

/// The errors a load finds, as it finds them: their tuples, `{ error: … }`,
/// one after another, in memory from `malloc`, freed when it is dropped,
/// of the relation's type an [`Errors`]'s descriptor describes.
#[cfg(not(test))]
struct Found {
    relation: *const TypeDesc,
    /// Null before the first.
    tuples: *mut u8,
    count: usize,
    capacity: usize,
}

#[cfg(not(test))]
impl Found {
    /// None yet, of the relation's type `relation` describes.
    fn new(relation: *const TypeDesc) -> Found {
        Found {
            relation,
            tuples: core::ptr::null_mut(),
            count: 0,
            capacity: 0,
        }
    }

    /// Those `errors` has, to add to. Fails if there is no memory for
    /// them.
    ///
    /// # Safety
    ///
    /// `errors` must be valid.
    unsafe fn of(errors: &Errors) -> Fallible<Found> {
        // SAFETY: guaranteed by the caller.
        unsafe {
            let mut found = Found::new(errors.relation);
            let size = found.relation().element().size;
            for i in 0..errors.found.len {
                let src = errors.found.bytes.add(i * size);
                core::ptr::copy_nonoverlapping(src, found.room()?, size);
                found.count += 1;
            }
            Ok(found)
        }
    }

    /// Room for one more error's tuple, after those found, zeroed. Fails
    /// if there is no memory for it.
    ///
    /// # Safety
    ///
    /// The descriptors must be valid.
    unsafe fn room(&mut self) -> Fallible<*mut u8> {
        // SAFETY: guaranteed by the caller.
        unsafe {
            let tuple = self.relation().element();
            if self.count == self.capacity {
                let capacity = self.capacity.saturating_mul(2).max(4);
                let Some(bytes) = capacity.checked_mul(tuple.size) else {
                    return Err(out_of_memory());
                };
                let grown = realloc(self.tuples.cast(), bytes.max(1));
                if grown.is_null() {
                    return Err(out_of_memory());
                }
                self.tuples = grown.cast();
                self.capacity = capacity;
            }
            let room = self.tuples.add(self.count * tuple.size);
            core::ptr::write_bytes(room, 0, tuple.size);
            Ok(room)
        }
    }

    /// The relation's descriptor, the compiler's, which lasts as long as
    /// the program.
    ///
    /// # Safety
    ///
    /// The descriptor must be valid.
    unsafe fn relation(&self) -> &'static TypeDesc {
        // SAFETY: guaranteed by the caller.
        unsafe { &*self.relation }
    }

    /// Add an error of the kind at `kind` in the compiler's table, whose
    /// components are `components`, as [`make_error`] takes them, which
    /// the statement's scope holds what they point to of.
    ///
    /// # Safety
    ///
    /// The descriptor must be valid, and `components` the kind's.
    unsafe fn add(&mut self, kind: usize, components: &[(&str, *const u8)]) -> Fallible {
        // SAFETY: guaranteed by the caller.
        unsafe {
            let tuple = self.relation().element();
            let made = self.room()?;
            let [attribute] = attributes(tuple) else {
                unreachable!("a relation of errors has one attribute");
            };
            make_error(
                made.add(attribute.offset),
                &*attribute.desc,
                kind,
                components,
            );
            self.count += 1;
            Ok(())
        }
    }

    /// The relation of the errors found, in canonical order, in memory the
    /// statement owns, which takes references to their texts. Fails if
    /// there is no memory for it.
    ///
    /// # Safety
    ///
    /// The descriptors must be valid. The statement must have begun a
    /// scope.
    unsafe fn finish(self) -> Fallible<Text> {
        // SAFETY: guaranteed by the caller.
        unsafe {
            let (bytes, len) = canonical(self.tuples, self.count, self.relation())?;
            Ok(Text { bytes, len })
        }
    }
}

#[cfg(not(test))]
impl Drop for Found {
    fn drop(&mut self) {
        // SAFETY: the tuples are `realloc`'s, or null, which only this
        // frees.
        unsafe { free(self.tuples.cast()) }
    }
}

/// `bytes`, the contents of a file, as text, or else the line of the
/// first byte that isn't UTF-8.
#[cfg(not(test))]
fn try_file_text(bytes: &[u8]) -> Result<&str, usize> {
    core::str::from_utf8(bytes).map_err(|error| {
        let before = &bytes[..error.valid_up_to()];
        before.split(|&b| b == b'\n').count()
    })
}

/// Store at `dst` the array of the records of the CSV file at `path`, in
/// the file's order, after the first if `header` is true, a header, of the
/// type `desc` describes, an array of tuples, in memory the statement
/// owns, and at `lines` the line each record starts on, an array of
/// `usize`s, in memory the statement owns too. Each attribute of the
/// tuples is the field in the column that `columns` gives it, counting
/// from 1: text as it is, and an integer, an approximate number, a boolean
/// or a character as `parse_integer`, `parse_approximate`, `parse_boolean`
/// and `parse_character` read one. A value of a type of the program's own is read as its
/// components, and a tuple, of the components of another possible
/// representation of one, as its attributes, whose columns `columns` gives
/// as a tuple of them; the caller converts those, and checks the type's
/// constraints. Columns it doesn't give are skipped. Fails if a column
/// isn't one of the file's, or two attributes or components have one
/// (R0016), the file can't be read (R0012), isn't UTF-8 text (R0013) or
/// CSV (R0015), a field isn't a value of its type (R0014), or the memory
/// can't be allocated.
///
/// # Safety
///
/// `path` must point to a valid text value, `header` to a valid boolean,
/// `columns` to valid [`Columns`] of the attributes of the tuples `desc`
/// describes, each an integer, or a tuple of the same shape for the
/// components of a value of a type of the program's own, or the attributes
/// of a tuple, and `desc` to a valid descriptor of an array of tuples whose
/// attributes, and those values' components, are text, integers,
/// approximate numbers, booleans or such values, and `dst` and `lines` to
/// memory for an array. The statement must have begun a scope.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_load_csv(
    dst: *mut Text,
    path: *const Text,
    header: *const u8,
    columns: *const Columns,
    lines: *mut Text,
    desc: *const TypeDesc,
) -> bool {
    // SAFETY: guaranteed by the caller.
    status(|| unsafe {
        let file = CsvFile {
            path: read_text(path.cast()),
            header: *header != 0,
            columns: &*columns,
        };
        let (rows, starts) = load_csv(&file, &*desc, None)?;
        dst.write_unaligned(rows);
        lines.write_unaligned(starts);
        Ok(())
    })
}

/// As [`trendy_rt_load_csv`], but rather than failing for a file it can't
/// read, store no records at `dst`, and at `errors` its
/// error, an `unreadable_file`, a `not_utf8` or a `missing_column`, and for
/// a record it can't read, skip it, giving its error there, a
/// `malformed_csv` or an `unreadable_field`, and none if every record is
/// read. A quoted field that isn't closed ends the records read. Two
/// attributes or components of one column, or one of a column below 1,
/// mistakes of the call's, still fail it, as memory that can't be
/// allocated does.
///
/// # Safety
///
/// As for [`trendy_rt_load_csv`], and `errors` must point to a valid
/// [`Errors`] of the types [`UNREADABLE_FILE`] and those after it say.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_load_csv_with_errors(
    dst: *mut Text,
    path: *const Text,
    header: *const u8,
    columns: *const Columns,
    lines: *mut Text,
    errors: *mut Errors,
    desc: *const TypeDesc,
) -> bool {
    // SAFETY: guaranteed by the caller.
    status(|| unsafe {
        let file = CsvFile {
            path: read_text(path.cast()),
            header: *header != 0,
            columns: &*columns,
        };
        let mut found = Found::new((*errors).relation);
        let (rows, starts) = load_csv(&file, &*desc, Some(&mut found))?;
        let found = found.finish()?;
        dst.write_unaligned(rows);
        lines.write_unaligned(starts);
        (&raw mut (*errors).found).write_unaligned(found);
        Ok(())
    })
}

/// The columns a CSV file's records are read from, as `load_csv`'s caller
/// lays them out: a tuple of their positions, and its type's descriptor.
#[repr(C)]
pub struct Columns {
    pub tuple: *const u8,
    pub desc: *const TypeDesc,
}

/// A CSV file to read: where it is, whether its first record is a header,
/// and the columns its records' tuples are read from.
#[cfg(not(test))]
struct CsvFile<'a> {
    path: &'a str,
    header: bool,
    columns: &'a Columns,
}

/// What [`trendy_rt_load_csv`] and [`trendy_rt_load_csv_with_errors`] do,
/// giving the records and the lines they start on: with `found`, what the
/// file can't be read for goes there, and so does what each record can't,
/// which is skipped; without it, the first fails.
///
/// # Safety
///
/// As for [`trendy_rt_load_csv`], and `found`'s descriptors as for
/// [`trendy_rt_load_csv_with_errors`].
#[cfg(not(test))]
unsafe fn load_csv(
    file: &CsvFile,
    desc: &TypeDesc,
    mut found: Option<&mut Found>,
) -> Fallible<(Text, Text)> {
    // SAFETY: guaranteed by the caller.
    unsafe {
        let element = desc.element();
        // Each scalar read, and its column, as `columns` gives them.
        let mut leaves = Temp::<Leaf>::new(scalars(element))?;
        let mut next = 0;
        gather(
            element,
            0,
            file.columns.tuple,
            &*file.columns.desc,
            leaves.ptr(),
            &mut next,
            element,
        )?;
        let leaves = leaves.values();
        distinct_columns(leaves, element)?;
        let read = file_or_error(file.path, found.as_deref_mut())?;
        // The file's records, and how many fields each has, unless it has
        // a failure of its own, which `found` has.
        let Some((_contents, text)) = read else {
            return no_records(desc);
        };
        let records = csv_records(text, file.path, file.header, found.is_none())?;
        let present = columns_present(
            leaves,
            element,
            file.path,
            records.fields,
            found.as_deref_mut(),
        )?;
        if !present {
            return no_records(desc);
        }
        let reading = Reading {
            file,
            text,
            leaves,
            element,
            records,
        };
        reading.records(desc, found)
    }
}

/// No records of the type `desc` describes, an array of tuples, and no
/// lines they start on, in memory the statement owns. Fails if there is no
/// memory for them.
#[cfg(not(test))]
fn no_records(desc: &TypeDesc) -> Fallible<(Text, Text)> {
    Ok((
        Text {
            bytes: allocate_buffer(0, desc)?,
            len: 0,
        },
        Text {
            bytes: allocate_buffer(0, core::ptr::null())?,
            len: 0,
        },
    ))
}

/// Whether the CSV file at `path`, whose records have `fields` fields,
/// has every column of `leaves`, the scalars its records' tuples, which
/// `element` describes, are read as. One it hasn't goes to `found`, if
/// given, and else fails (R0016).
///
/// # Safety
///
/// As for [`Found::add`]. The statement must have begun a scope.
#[cfg(not(test))]
unsafe fn columns_present(
    leaves: &[Leaf],
    element: &TypeDesc,
    path: &str,
    fields: usize,
    found: Option<&mut Found>,
) -> Fallible<bool> {
    let Some(i) = leaves.iter().position(|l| l.column >= fields) else {
        return Ok(true);
    };
    // A file with no records has no columns to lack.
    if fields == 0 {
        return Ok(true);
    }
    let named = LeafName {
        desc: element,
        leaf: i,
    };
    let attribute = scope_written(|out| write!(out, "{named}"))?;
    let (path, column, columns) = (
        scope_text(path)?,
        i64::try_from(leaves[i].column.saturating_add(1)).unwrap_or(i64::MAX),
        i64::try_from(fields).unwrap_or(i64::MAX),
    );
    // SAFETY: guaranteed by the caller.
    unsafe {
        give(
            found,
            MISSING_COLUMN,
            &[
                ("path", (&raw const path).cast()),
                ("attribute", (&raw const attribute).cast()),
                ("column", (&raw const column).cast()),
                ("columns", (&raw const columns).cast()),
            ],
        )?;
    }
    Ok(false)
}

/// A CSV file's records being read: the file, its text, the scalars its
/// records' tuples, which `element` describes, are read as, and its
/// records, as [`csv_records`] found them.
#[cfg(not(test))]
struct Reading<'a> {
    file: &'a CsvFile<'a>,
    text: &'a str,
    leaves: &'a [Leaf],
    element: &'a TypeDesc,
    records: Records,
}

#[cfg(not(test))]
impl Reading<'_> {
    /// The records, an array of the type `desc` describes, and the line
    /// each starts on, in memory the statement owns. With `found`, each
    /// record it can't read is skipped, its error there; without, the
    /// first fails.
    ///
    /// # Safety
    ///
    /// As for [`load_csv`].
    unsafe fn records(
        &self,
        desc: &TypeDesc,
        mut found: Option<&mut Found>,
    ) -> Fallible<(Text, Text)> {
        // SAFETY: guaranteed by the caller.
        unsafe {
            let (element, count) = (self.element, self.records.count);
            let Some(size) = count.checked_mul(element.size) else {
                return Err(out_of_memory());
            };
            let rows = allocate_buffer(size, desc)?;
            let Some(lines_size) = count.checked_mul(size_of::<usize>()) else {
                return Err(out_of_memory());
            };
            let starts = allocate_buffer(lines_size, core::ptr::null())?;
            // Each record's tuple, made here, then copied into the array.
            let tuple = Temp::<u8>::new(element.size)?;
            let tuple = tuple.ptr();
            let mut all = csv::Fields::new(self.text);
            // A record the errors skip, and why.
            let mut bad: Option<BadRecord> = None;
            let (mut i, mut j, mut start) = (0, 0, 0);
            // The header, then the records.
            let mut in_header = self.file.header;
            while let Some(field) = all.next() {
                let field = match field {
                    Ok(field) => field,
                    Err(csv::NotCsv { why, line }) => {
                        let found = found
                            .as_deref_mut()
                            .expect("without errors, the file is CSV, as `csv_records` found it");
                        malformed_error(Some(found), self.file.path, line, |out| {
                            out.write_str(malformed_words(why))
                        })?;
                        all.skip_record(why);
                        (j, bad, in_header) = (0, None, false);
                        continue;
                    }
                };
                if j == 0 {
                    start = field.line;
                }
                if !in_header && bad.is_none() {
                    bad = self.field(field, j, tuple, found.is_some())?;
                }
                j += 1;
                if !field.last {
                    continue;
                }
                if in_header {
                    in_header = false;
                } else if let Some(found) = found.as_deref_mut()
                    && (j != self.records.fields || bad.is_some())
                {
                    self.skipped(found, bad, j, start)?;
                } else {
                    copy_element(rows, rows.add(i * element.size), tuple, element);
                    starts
                        .add(i * size_of::<usize>())
                        .cast::<usize>()
                        .write_unaligned(start);
                    i += 1;
                }
                (j, bad) = (0, None);
            }
            Ok((
                Text {
                    bytes: rows,
                    len: i,
                },
                Text {
                    bytes: starts,
                    len: i,
                },
            ))
        }
    }

    /// Read `field`, in the column `column` of a record, into `tuple`, if a
    /// scalar is read from it: what is wrong with it, if `skips`, and else,
    /// for a field that isn't a value of its type, fail with an
    /// `unreadable_field` (R0014).
    ///
    /// # Safety
    ///
    /// As for [`load_csv`]; `tuple` must point to memory for a record's
    /// tuple.
    unsafe fn field<'a>(
        &self,
        field: csv::Field<'a>,
        column: usize,
        tuple: *mut u8,
        skips: bool,
    ) -> Fallible<Option<BadRecord<'a>>>
    where
        Self: 'a,
    {
        let Some(k) = self.leaves.iter().position(|l| l.column == column) else {
            return Ok(None);
        };
        let leaf = self.leaves[k];
        // SAFETY: guaranteed by the caller.
        unsafe {
            let leaf_desc = &*leaf.desc;
            let Err(why) = try_read_field(field, tuple.add(leaf.offset), leaf_desc)? else {
                return Ok(None);
            };
            let value = field_value(field)?;
            let place = Place {
                attribute: LeafName {
                    desc: self.element,
                    leaf: k,
                },
                column,
                line: field.line,
                path: self.file.path,
            };
            if !skips {
                // With no errors to give it to, it fails.
                field_error(None, value, leaf_desc.kind, why, &place)?;
            }
            Ok(Some(BadRecord::Field {
                text: value,
                kind: leaf_desc.kind,
                why,
                place,
            }))
        }
    }

    /// Add to `found` why the record starting on `start`, with `fields`
    /// fields, is skipped: another number of fields than the file's, or
    /// else `bad`.
    ///
    /// # Safety
    ///
    /// As for [`Found::add`]. The statement must have begun a scope.
    unsafe fn skipped(
        &self,
        found: &mut Found,
        bad: Option<BadRecord>,
        fields: usize,
        start: usize,
    ) -> Fallible {
        // SAFETY: guaranteed by the caller.
        unsafe {
            if fields != self.records.fields {
                malformed_error(Some(found), self.file.path, start, |out| {
                    let (header, first) = (self.file.header, self.records.first_line);
                    field_count_words(out, fields, self.records.fields, header, first)
                })
            } else if let Some(BadRecord::Field {
                text,
                kind,
                why,
                place,
            }) = bad
            {
                field_error(Some(found), text, kind, why, &place)
            } else {
                Ok(())
            }
        }
    }
}

/// A record of a CSV file that its call's errors skip, and why: a field
/// that isn't a value of its type, `text`, of the kind `kind`, as `why`
/// says, at `place`.
#[cfg(not(test))]
#[derive(Clone, Copy)]
enum BadRecord<'a> {
    Field {
        text: &'a str,
        kind: u8,
        why: trendy_text::Unparsable,
        place: Place<'a>,
    },
}

/// Give `found` that the CSV file at `path` isn't CSV on `line`, as
/// `write` says, a `malformed_csv`, which, with no `found`, it fails with
/// ([`give`]).
///
/// # Safety
///
/// As for [`give`]. The statement must have begun a scope.
#[cfg(not(test))]
unsafe fn malformed_error(
    found: Option<&mut Found>,
    path: &str,
    line: usize,
    write: impl Fn(&mut dyn Write) -> fmt::Result,
) -> Fallible {
    let (path, problem) = (scope_text(path)?, scope_written(write)?);
    let line = i64::try_from(line).unwrap_or(i64::MAX);
    // SAFETY: guaranteed by the caller.
    unsafe {
        give(
            found,
            MALFORMED_CSV,
            &[
                ("path", (&raw const path).cast()),
                ("line", (&raw const line).cast()),
                ("problem", (&raw const problem).cast()),
            ],
        )
    }
}

/// Give `found` that `text`, at `place`, isn't a value of its type, of the
/// kind `kind`, as `why` says, an `unreadable_field`, which, with no
/// `found`, it fails with ([`give`]).
///
/// # Safety
///
/// As for [`give`]. The statement must have begun a scope.
#[cfg(not(test))]
unsafe fn field_error(
    found: Option<&mut Found>,
    text: &str,
    kind: u8,
    why: trendy_text::Unparsable,
    place: &Place,
) -> Fallible {
    let ty = match kind {
        KIND_INTEGER => "integer",
        KIND_APPROXIMATE => "approximate",
        KIND_BOOLEAN => "boolean",
        KIND_CHARACTER => "character",
        kind => unreachable!("only a number, a boolean or a character can't be read: {kind}"),
    };
    let (path, attribute, ty, text) = (
        scope_text(place.path)?,
        scope_written(|out| write!(out, "{}", place.attribute))?,
        scope_text(ty)?,
        scope_text(text)?,
    );
    let (line, column, out_of_range) = (
        i64::try_from(place.line).unwrap_or(i64::MAX),
        i64::try_from(place.column.saturating_add(1)).unwrap_or(i64::MAX),
        u8::from(why == trendy_text::Unparsable::OutOfRange),
    );
    // SAFETY: guaranteed by the caller.
    unsafe {
        give(
            found,
            UNREADABLE_FIELD,
            &[
                ("path", (&raw const path).cast()),
                ("line", (&raw const line).cast()),
                ("column", (&raw const column).cast()),
                ("attribute", (&raw const attribute).cast()),
                ("type", (&raw const ty).cast()),
                ("text", (&raw const text).cast()),
                ("out_of_range", (&raw const out_of_range).cast()),
            ],
        )
    }
}

/// Fail if two of `leaves`, the scalars the records' tuples, which
/// `element` describes, are read as, have one column: a `column_shared`
/// (R0016).
///
/// # Safety
///
/// The statement must have begun a scope.
#[cfg(not(test))]
unsafe fn distinct_columns(leaves: &[Leaf], element: &TypeDesc) -> Fallible {
    let named = |leaf| {
        scope_written(|out| {
            write!(
                out,
                "{}",
                LeafName {
                    desc: element,
                    leaf
                }
            )
        })
    };
    for (i, leaf) in leaves.iter().enumerate() {
        if let Some(first) = leaves[..i].iter().position(|l| l.column == leaf.column) {
            let (first, second) = (named(first)?, named(i)?);
            let column = i64::try_from(leaf.column.saturating_add(1)).unwrap_or(i64::MAX);
            // SAFETY: the names are valid texts, and the column an
            // integer.
            return Err(unsafe {
                fail_as(
                    COLUMN_SHARED,
                    &[
                        ("first", (&raw const first).cast()),
                        ("second", (&raw const second).cast()),
                        ("column", (&raw const column).cast()),
                    ],
                )
            });
        }
    }
    Ok(())
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
/// [`read_from_fields`]). Fails if a position is below 1, a
/// `column_below_one` (R0016).
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
) -> Fallible {
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
                )?;
                continue;
            }
            let n = at.cast::<i64>().read_unaligned();
            if n < 1 {
                let name = LeafName {
                    desc: element,
                    leaf: *next,
                };
                let attribute = scope_written(|out| write!(out, "{name}"))?;
                return Err(fail_as(
                    COLUMN_BELOW_ONE,
                    &[
                        ("attribute", (&raw const attribute).cast()),
                        ("column", (&raw const n).cast()),
                    ],
                ));
            }
            leaves.add(*next).write(Leaf {
                offset: base + attr.offset,
                desc: attr.desc,
                column: usize::try_from(n - 1).unwrap_or(usize::MAX),
            });
            *next += 1;
        }
    }
    Ok(())
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

/// A CSV file's records to load, and how many fields each has.
#[cfg(not(test))]
struct Records {
    /// How many there are, or, as an errors skip some, at most.
    count: usize,
    /// The fields of each record, the first's, or 0 if there are none.
    fields: usize,
    /// The line the first record starts on.
    first_line: usize,
}

/// The records of the CSV text `text`, of the file at `path`, after its
/// first, a header, if `header` is true. If `stops`, it fails if it isn't
/// CSV, or a record has another number of fields than the first, a
/// `malformed_csv` (R0015); otherwise each record that isn't CSV, or has
/// another number, counts, as one an errors skip.
///
/// # Safety
///
/// The statement must have begun a scope.
#[cfg(not(test))]
unsafe fn csv_records(text: &str, path: &str, header: bool, stops: bool) -> Fallible<Records> {
    let (mut records, mut fields, mut line) = (0_usize, 0, 0);
    // The first record's fields and line.
    let mut first: Option<(usize, usize)> = None;
    let mut all = csv::Fields::new(text);
    while let Some(field) = all.next() {
        let field = match field {
            Ok(field) => field,
            Err(csv::NotCsv { why, line }) => {
                if stops {
                    // SAFETY: guaranteed by the caller.
                    unsafe {
                        malformed_error(None, path, line, |out| {
                            out.write_str(malformed_words(why))
                        })?;
                    }
                }
                all.skip_record(why);
                (records, fields) = (records + 1, 0);
                continue;
            }
        };
        if fields == 0 {
            line = field.line;
        }
        fields += 1;
        if !field.last {
            continue;
        }
        match first {
            None => first = Some((fields, line)),
            Some((expected, first_line)) if expected != fields && stops => {
                // SAFETY: guaranteed by the caller.
                unsafe {
                    malformed_error(None, path, line, |out| {
                        field_count_words(out, fields, expected, header, first_line)
                    })?;
                }
            }
            Some(_) => {}
        }
        records += 1;
        fields = 0;
    }
    Ok(Records {
        count: if header {
            records.saturating_sub(1)
        } else {
            records
        },
        fields: first.map_or(0, |(fields, _)| fields),
        first_line: first.map_or(0, |(_, line)| line),
    })
}

/// What a CSV file's record isn't CSV for, `why`, after "line n of the
/// file".
#[cfg(not(test))]
fn malformed_words(why: csv::Malformed) -> &'static str {
    match why {
        csv::Malformed::StrayQuote => "has a quote in a field that isn't quoted",
        csv::Malformed::AfterQuote => {
            "has more after a quoted field's closing quote than a comma or a line end"
        }
        csv::Malformed::Unclosed => "starts a quoted field that isn't closed",
    }
}

/// Write that a CSV file's record has `fields` fields, but its first, the
/// header if `header`, or else the record on `first_line`, has `expected`,
/// after "line n of the file".
#[cfg(not(test))]
fn field_count_words(
    out: &mut dyn Write,
    fields: usize,
    expected: usize,
    header: bool,
    first_line: usize,
) -> fmt::Result {
    let plural = if fields == 1 { "" } else { "s" };
    write!(out, "has {fields} field{plural}, but ")?;
    if header {
        write!(out, "the header has {expected}")
    } else {
        write!(out, "line {first_line} has {expected}")
    }
}

/// The value of the CSV field `field`: its text as written, or, with its
/// doubled quotes made one, a copy in memory the statement's scope holds.
/// Fails if there is no memory for the copy.
///
/// # Safety
///
/// The statement must have begun a scope, which holds the copy until it
/// ends, as long as the value is used.
#[cfg(not(test))]
unsafe fn field_value(field: csv::Field<'_>) -> Fallible<&str> {
    if let Some(written) = field.as_written() {
        return Ok(written);
    }
    let len = field.len();
    let bytes = allocate_buffer(len, core::ptr::null())?;
    // SAFETY: the buffer has room for the value's bytes, and they are the
    // field's UTF-8, with some quotes left out.
    unsafe {
        let value = core::slice::from_raw_parts_mut(bytes, len);
        field.copy_to(value);
        Ok(core::str::from_utf8_unchecked(value))
    }
}

/// Read the CSV file's field `field` into `dst`, as a value of the type
/// `desc` describes: text, an integer, an approximate number, a boolean or a
/// character; or why it isn't one. Fails if the memory for a text can't be allocated.
///
/// # Safety
///
/// `dst` must point to memory for a value of that type. The statement must
/// have begun a scope, which holds a text's bytes.
#[cfg(not(test))]
unsafe fn try_read_field(
    field: csv::Field,
    dst: *mut u8,
    desc: &TypeDesc,
) -> Fallible<Result<(), trendy_text::Unparsable>> {
    // SAFETY: guaranteed by the caller.
    unsafe {
        let value = field_value(field)?;
        Ok(match desc.kind {
            KIND_TEXT => {
                dst.cast::<Text>().write_unaligned(scope_text(value)?);
                Ok(())
            }
            KIND_INTEGER => {
                trendy_numeric::parse_integer(value).map(|n| dst.cast::<i64>().write_unaligned(n))
            }
            KIND_APPROXIMATE => trendy_numeric::parse_approximate(value)
                .map(|x| dst.cast::<f64>().write_unaligned(x)),
            KIND_BOOLEAN => trendy_text::parse_boolean(value).map(|b| dst.write(u8::from(b))),
            KIND_CHARACTER if trendy_text::is_one_character(value) => {
                let character = match ascii_character(value) {
                    Some(character) => character,
                    None => scope_text(value)?,
                };
                dst.cast::<Text>().write_unaligned(character);
                Ok(())
            }
            KIND_CHARACTER => Err(trendy_text::Unparsable::Malformed),
            kind => unreachable!("`load_csv` reads no values of kind {kind}"),
        })
    }
}

/// The bytes of the file at `path`, in memory of the caller's own, its
/// length theirs, or else the reason it can't be read, the system's.
/// Fails only if the memory can't be allocated.
#[cfg(not(test))]
fn try_read_file(path: &str) -> Fallible<Result<Temp<u8>, &'static str>> {
    let c_path = match c_string(path)? {
        Ok(c_path) => c_path,
        Err(reason) => return Ok(Err(reason)),
    };
    // SAFETY: the path ends in a NUL; the descriptor is read, then closed, and
    // `read` writes at most the room left in the memory.
    unsafe {
        let fd = open(c_path.ptr(), O_RDONLY);
        if fd < 0 {
            return Ok(Err(reason()));
        }
        let read_all = |contents: &mut Temp<u8>| -> Fallible<Result<usize, &'static str>> {
            let mut len = 0;
            loop {
                if len == contents.len {
                    let Some(grown) = contents.len.checked_mul(2) else {
                        return Err(out_of_memory());
                    };
                    contents.grow(grown)?;
                }
                let room = contents.len - len;
                let Ok(n) = usize::try_from(read(fd, contents.ptr().add(len), room)) else {
                    return Ok(Err(reason()));
                };
                if n == 0 {
                    return Ok(Ok(len));
                }
                len += n;
            }
        };
        let read = Temp::<u8>::new(4096).and_then(|mut contents| {
            Ok(read_all(&mut contents)?.map(|len| {
                contents.len = len;
                contents
            }))
        });
        close(fd);
        read
    }
}

/// `path` as `open` takes it, a C string, which ends at the first NUL, so that
/// one with a NUL in it is no path, for that reason. Fails if there is no memory
/// for it.
#[cfg(not(test))]
fn c_string(path: &str) -> Fallible<Result<Temp<u8>, &'static str>> {
    if path.contains('\0') {
        return Ok(Err("a path can't contain the character U+0000"));
    }
    let Some(terminated) = path.len().checked_add(1) else {
        return Err(out_of_memory());
    };
    let c_path = Temp::<u8>::new(terminated)?;
    // SAFETY: the memory is for the path's bytes and a NUL.
    unsafe {
        core::ptr::copy_nonoverlapping(path.as_ptr(), c_path.ptr(), path.len());
        *c_path.ptr().add(path.len()) = 0;
    }
    Ok(Ok(c_path))
}

/// How much of a file [`FileOut`] holds before it writes it.
#[cfg(not(test))]
const FILE_BUFFER: usize = 64 * 1024;

/// A file being written, with what hasn't been yet.
#[cfg(not(test))]
struct FileOut {
    fd: i32,
    buffer: Temp<u8>,
    held: usize,
}

#[cfg(not(test))]
impl FileOut {
    /// Add `bytes` to what is written, or say why they can't be.
    fn put(&mut self, mut bytes: &[u8]) -> Result<(), &'static str> {
        while !bytes.is_empty() {
            if self.held == self.buffer.len {
                self.flush()?;
            }
            let taken = bytes.len().min(self.buffer.len - self.held);
            // SAFETY: there is room for them after what the buffer holds.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    bytes.as_ptr(),
                    self.buffer.ptr().add(self.held),
                    taken,
                );
            }
            self.held += taken;
            bytes = &bytes[taken..];
        }
        Ok(())
    }

    /// Write what the buffer holds, as many calls of `write` as it takes.
    fn flush(&mut self) -> Result<(), &'static str> {
        let mut written = 0;
        while written < self.held {
            // SAFETY: the buffer holds that many bytes after those written.
            let n = unsafe { write(self.fd, self.buffer.ptr().add(written), self.held - written) };
            match usize::try_from(n) {
                Ok(0) => return Err("the system wrote nothing"),
                Ok(n) => written += n,
                Err(_) => return Err(reason()),
            }
        }
        self.held = 0;
        Ok(())
    }
}

/// Write the file at `path`, made if there is none and emptied if there is,
/// with what `fill` puts in it, or say why it can't be: as far as `fill` got,
/// if it can't partway. Fails if there is no memory for that.
#[cfg(not(test))]
fn try_write_file(
    path: &str,
    fill: impl FnOnce(&mut FileOut) -> Result<(), &'static str>,
) -> Fallible<Result<(), &'static str>> {
    let c_path = match c_string(path)? {
        Ok(c_path) => c_path,
        Err(reason) => return Ok(Err(reason)),
    };
    let buffer = Temp::<u8>::new(FILE_BUFFER)?;
    // SAFETY: the path ends in a NUL, and the descriptor is closed once.
    unsafe {
        let fd = open(c_path.ptr(), O_WRITE_ANEW, NEW_FILE_MODE);
        if fd < 0 {
            return Ok(Err(reason()));
        }
        let mut out = FileOut {
            fd,
            buffer,
            held: 0,
        };
        let written = fill(&mut out).and_then(|()| out.flush());
        // A system can say only now that it couldn't keep what was written,
        // but that isn't the first reason, if there is one already.
        let closed = close(fd);
        Ok(written.and_then(|()| if closed < 0 { Err(reason()) } else { Ok(()) }))
    }
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
/// Returns `allocate`'s error, or `too_long`'s if the length doesn't fit
/// in a `usize`.
unsafe fn text_of<E>(
    value: *const u8,
    desc: &TypeDesc,
    allocate: impl FnOnce(usize) -> Result<*mut u8, E>,
    too_long: impl Fn() -> E,
) -> Result<Text, E> {
    let mut counted = Counted(0);
    // SAFETY, throughout: guaranteed by the caller.
    unsafe {
        write_value(&mut counted, value, desc).map_err(|_| too_long())?;
        let len = counted.0;
        let bytes = allocate(len)?;
        let mut filled = Filled {
            bytes: core::slice::from_raw_parts_mut(bytes, len),
            len: 0,
        };
        // As much as was counted, which fits.
        write_value(&mut filled, value, desc).map_err(|_| too_long())?;
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
            KIND_U8 | KIND_U16 | KIND_U32 => write!(out, "{}", narrow(value, desc.kind)),
            // The shortest exponent form that reads back as the same value,
            // as `hir::Approximate::literal` writes it.
            KIND_APPROXIMATE => write!(out, "{:e}", value.cast::<f64>().read_unaligned()),
            KIND_TEXT => {
                let text = read_text(value);
                out.write_char('"')?;
                trendy_text::escape(text, out)?;
                out.write_char('"')
            }
            KIND_CHARACTER => {
                out.write_char('\'')?;
                trendy_text::escape_character(read_text(value), out)?;
                out.write_char('\'')
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
            KIND_DEFINED if is_byte(written_name(desc)) => match byte_number(value, desc) {
                Some(byte) => write!(out, "x'{byte:02x}'"),
                // What failed to be one, as an error says.
                None => write_call(out, written_name(desc), value, desc),
            },
            KIND_DEFINED => write_call(out, written_name(desc), value, desc),
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
                // Bytes, of an `array byte` out of a hierarchy, as a literal
                // writes them.
                if element.kind == KIND_DEFINED && is_byte(type_name(element)) {
                    out.write_str("x\"")?;
                    for i in 0..count {
                        let byte = byte_number(elements.add(i * element.size), element);
                        write!(out, "{:02x}", byte.ok_or(fmt::Error)?)?;
                    }
                    return out.write_char('"');
                }
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
        if matches!(desc.kind, KIND_TEXT | KIND_CHARACTER) {
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
        KIND_INTEGER | KIND_U8 | KIND_U16 | KIND_U32 => out.write_str("integer"),
        KIND_APPROXIMATE => out.write_str("approximate"),
        KIND_TEXT => out.write_str("text"),
        KIND_CHARACTER => out.write_str("character"),
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

/// A call of `operator` with the arguments at `value`, as a call is written:
/// `operator{name: value, …}`, in the attributes' order.
///
/// # Safety
///
/// As for [`write_value`], with `desc` describing the arguments' heading.
unsafe fn write_call(
    out: &mut impl Write,
    operator: &str,
    value: *const u8,
    desc: &TypeDesc,
) -> fmt::Result {
    out.write_str(operator)?;
    out.write_char('{')?;
    // SAFETY: guaranteed by the caller; each attribute's offset and
    // descriptor are its layout.
    unsafe {
        for (i, attr) in attributes(desc).iter().enumerate() {
            if i > 0 {
                out.write_str(", ")?;
            }
            write!(out, "{}: ", name(attr))?;
            write_value(out, value.add(attr.offset), &*attr.desc)?;
        }
    }
    out.write_char('}')
}

/// Whether the name a type is called or written by is what the program calls
/// the prelude's `byte`: its name, unless another unit declares one (see
/// `hir::Library::is_byte`).
fn is_byte(name: &str) -> bool {
    name == "byte" || name == "trendy.prelude::byte"
}

/// The number of the byte at `value`, a value of the type `desc`, stored in one
/// byte, or as an integer in a hierarchy, or as what a value selected of the type's
/// components was before they were checked, if it is out of its range.
///
/// # Safety
///
/// As for [`write_value`], with `desc` describing a `byte`.
unsafe fn byte_number(value: *const u8, desc: &TypeDesc) -> Option<u8> {
    // SAFETY: guaranteed by the caller; its one attribute's offset and
    // descriptor are its layout.
    unsafe {
        let number = &attributes(desc)[0];
        let at = value.add(number.offset);
        if (*number.desc).kind == KIND_U8 {
            Some(*at)
        } else {
            u8::try_from(at.cast::<i64>().read_unaligned()).ok()
        }
    }
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

/// The name a value of the type of the program's own that `desc` describes is
/// written with.
///
/// # Safety
///
/// `desc` must be a valid descriptor of such a type.
unsafe fn written_name(desc: &TypeDesc) -> &str {
    // SAFETY: the compiler emits names as valid UTF-8.
    unsafe {
        core::str::from_utf8_unchecked(core::slice::from_raw_parts(desc.written, desc.written_len))
    }
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

/// `open`'s flags to write a file, made if there is none and emptied if there
/// is: `O_WRONLY | O_CREAT | O_TRUNC`, which each system numbers its own way.
#[cfg(all(not(test), target_vendor = "apple"))]
const O_WRITE_ANEW: i32 = 0x0001 | 0x0200 | 0x0400;
/// As for Apple's systems.
#[cfg(all(not(test), target_os = "linux"))]
const O_WRITE_ANEW: i32 = 0o1 | 0o100 | 0o1000;

/// The permissions of a file made, which the process's umask narrows.
#[cfg(not(test))]
const NEW_FILE_MODE: u32 = 0o666;

unsafe extern "C" {
    #[cfg(not(test))]
    fn pthread_attr_init(attr: *mut PthreadAttr) -> i32;
    #[cfg(not(test))]
    fn pthread_attr_setstacksize(attr: *mut PthreadAttr, size: usize) -> i32;
    #[cfg(not(test))]
    fn pthread_create(
        thread: *mut usize,
        attr: *const PthreadAttr,
        start: extern "C" fn(*mut core::ffi::c_void) -> *mut core::ffi::c_void,
        arg: *mut core::ffi::c_void,
    ) -> i32;
    #[cfg(not(test))]
    fn pthread_join(thread: usize, result: *mut *mut core::ffi::c_void) -> i32;
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

    #[test]
    fn every_ascii_character_shares_bytes_never_released() {
        assert_eq!(
            core::mem::offset_of!(AsciiCharacter, byte),
            size_of::<Header>()
        );
        for byte in 0..128_u8 {
            let written = [byte];
            let character = core::str::from_utf8(&written).unwrap();
            let value = ascii_character(character).unwrap();
            let (bytes, len) = (value.bytes, value.len);
            assert_eq!(len, 1);
            // SAFETY: it points to its entry's byte, after its header.
            unsafe {
                assert_eq!(*bytes, byte);
                let header = bytes
                    .sub(size_of::<Header>())
                    .cast::<Header>()
                    .read_unaligned();
                assert_eq!(header.count, IMMORTAL);
                assert!(header.desc.is_null());
            }
            // The same bytes, however many times it is asked for.
            assert_eq!(ascii_character(character).unwrap().bytes, bytes);
        }
        // Anything else has bytes of its own.
        for character in ["é", "🇸🇪", "\r\n", "e\u{301}", ""] {
            assert!(ascii_character(character).is_none(), "{character}");
        }
    }

    fn scalar(kind: u8, size: usize) -> TypeDesc {
        TypeDesc {
            kind,
            points: matches!(kind, KIND_TEXT | KIND_CHARACTER),
            integers: 0,
            size,
            attributes: core::ptr::null(),
            count: 0,
            element: core::ptr::null(),
            name: core::ptr::null(),
            name_len: 0,
            written: core::ptr::null(),
            written_len: 0,
        }
    }

    /// Whether a value with `attributes` can point to buffers.
    fn any_points(attributes: &[Attr]) -> bool {
        // SAFETY: each test's descriptors outlive it.
        attributes.iter().any(|attr| unsafe { (*attr.desc).points })
    }

    /// A tuple of `size` bytes with `attributes`.
    fn tuple_of(attributes: &[Attr], size: usize) -> TypeDesc {
        TypeDesc {
            kind: KIND_TUPLE,
            points: any_points(attributes),
            integers: 0,
            size,
            attributes: attributes.as_ptr(),
            count: attributes.len(),
            element: core::ptr::null(),
            name: core::ptr::null(),
            name_len: 0,
            written: core::ptr::null(),
            written_len: 0,
        }
    }

    /// A type of the program's own named `name`, with the components
    /// `attributes`, of `size` bytes.
    fn defined_of(name: &'static str, attributes: &[Attr], size: usize) -> TypeDesc {
        TypeDesc {
            kind: KIND_DEFINED,
            points: any_points(attributes),
            integers: 0,
            size,
            attributes: attributes.as_ptr(),
            count: attributes.len(),
            element: core::ptr::null(),
            name: name.as_ptr(),
            name_len: name.len(),
            written: name.as_ptr(),
            written_len: name.len(),
        }
    }

    /// A relation whose tuples are `tuple`s.
    #[test]
    fn a_character_is_written_in_its_own_quotes() {
        let written = |character: &str| {
            let value = [character.as_ptr() as usize, character.len()];
            print(value.as_ptr().cast(), &scalar(KIND_CHARACTER, 16))
        };
        assert_eq!(written("a"), "'a'");
        assert_eq!(written("🇸🇪"), "'🇸🇪'");
        assert_eq!(written("'"), r"'\''");
        assert_eq!(written("\""), "'\"'");
        assert_eq!(written("\r\n"), r"'\r\n'");
    }

    fn relation_of(tuple: &TypeDesc) -> TypeDesc {
        TypeDesc {
            kind: KIND_RELATION,
            points: true,
            integers: 0,
            size: 16,
            attributes: tuple.attributes,
            count: tuple.count,
            element: tuple,
            name: core::ptr::null(),
            name_len: 0,
            written: core::ptr::null(),
            written_len: 0,
        }
    }

    /// An array whose elements are `element`s.
    fn array_of(element: &TypeDesc) -> TypeDesc {
        TypeDesc {
            kind: KIND_ARRAY,
            points: true,
            integers: 0,
            size: 16,
            attributes: core::ptr::null(),
            count: 0,
            element,
            name: core::ptr::null(),
            name_len: 0,
            written: core::ptr::null(),
            written_len: 0,
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
            text_of(
                value,
                desc,
                |len| {
                    memory = vec![0; len];
                    Ok(memory.as_mut_ptr())
                },
                || fmt::Error,
            )
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
    fn tuples_of_integers_sort_where_they_are_as_they_compare() {
        // Signed, by the first that differs, one of those equal kept.
        let mut pairs: [[i64; 2]; 6] = [
            [1, 2],
            [-1, 5],
            [1, -2],
            [i64::MAX, i64::MIN],
            [1, 2],
            [i64::MIN, i64::MAX],
        ];
        // SAFETY: they are six tuples of two integers each, aligned for them.
        let kept = unsafe { sort_integers(pairs.as_mut_ptr().cast(), 6, 2) };
        assert_eq!(
            pairs[..kept],
            [
                [i64::MIN, i64::MAX],
                [-1, 5],
                [1, -2],
                [1, 2],
                [i64::MAX, i64::MIN],
            ]
        );
        let mut same = [[7_i64]; 5];
        // SAFETY: as above, of one integer each.
        assert_eq!(unsafe { sort_integers(same.as_mut_ptr().cast(), 5, 1) }, 1);
        // Each number of them `sorts_in_place` allows.
        for integers in 1..=8 {
            let tuple = |first: i64| {
                let mut tuple = [0_i64; 8];
                tuple[integers - 1] = first;
                tuple
            };
            let mut tuples = [tuple(3), tuple(-3), tuple(3), tuple(0)];
            let mut flat: Vec<i64> = tuples
                .iter_mut()
                .flat_map(|t| t[..integers].to_vec())
                .collect();
            // SAFETY: as above, of that many integers each.
            let kept = unsafe { sort_integers(flat.as_mut_ptr().cast(), 4, integers) };
            assert_eq!(kept, 3, "{integers}");
            let last: Vec<i64> = flat
                .chunks(integers)
                .take(kept)
                .map(|t| t[integers - 1])
                .collect();
            assert_eq!(last, [-3, 0, 3], "{integers}");
        }
    }

    #[test]
    fn tuples_of_integers_compare_as_their_attributes_do() {
        let integer = scalar(KIND_INTEGER, 8);
        let attributes = [attr("a", 0, &integer), attr("b", 8, &integer)];
        let walked = tuple_of(&attributes, 16);
        let words = TypeDesc {
            integers: 2,
            ..tuple_of(&attributes, 16)
        };
        let values = [i64::MIN, -1, 0, 1, i64::MAX];
        for a in values {
            for b in values {
                for c in values {
                    for d in values {
                        let (x, y): ([i64; 2], [i64; 2]) = ([a, b], [c, d]);
                        let (x, y) = (x.as_ptr().cast(), y.as_ptr().cast());
                        // SAFETY: each is two integers, as both describe.
                        unsafe {
                            assert_eq!(order(x, y, &words), order(x, y, &walked));
                            assert_eq!(equal(x, y, &words), equal(x, y, &walked));
                        }
                    }
                }
            }
        }
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
