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
pub const KIND_RATIONAL: u8 = 2;
pub const KIND_TEXT: u8 = 3;
pub const KIND_TUPLE: u8 = 4;
pub const KIND_RELATION: u8 = 5;

/// An integer result out of range.
pub const FAIL_INTEGER_OVERFLOW: u32 = 0;
/// `/`, `div` or `mod` by zero.
pub const FAIL_DIVISION_BY_ZERO: u32 = 1;
/// A rational result beyond the largest finite magnitude.
pub const FAIL_RATIONAL_OVERFLOW: u32 = 2;
/// Memory for a result couldn't be allocated.
pub const FAIL_OUT_OF_MEMORY: u32 = 3;

/// The line written to stderr for each `FAIL_` constant, with the
/// compiler's error code, which `trendyc --explain` describes.
#[must_use]
pub fn failure_message(failure: u32) -> &'static str {
    match failure {
        FAIL_INTEGER_OVERFLOW => "error[R0001]: integer overflow\n",
        FAIL_DIVISION_BY_ZERO => "error[R0002]: division by zero\n",
        FAIL_RATIONAL_OVERFLOW => "error[R0001]: rational overflow\n",
        FAIL_OUT_OF_MEMORY => "error[R0003]: out of memory\n",
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

/// Store `lhs || rhs` at `dst`, in memory allocated for it, which
/// [`trendy_rt_free`] releases. Stops the program if the memory can't
/// be allocated.
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
        let bytes = allocate::<u8>(len);
        core::ptr::copy_nonoverlapping(lhs.bytes, bytes, lhs.len);
        core::ptr::copy_nonoverlapping(rhs.bytes, bytes.add(lhs.len), rhs.len);
        dst.write_unaligned(Text { bytes, len });
    }
}

/// Store at `dst` 1 if `lhs` and `rhs`, of the type `desc` describes, are
/// equal, and 0 if not. Two values are equal exactly when they are the same
/// value: rationals when they have the same bits (IEEE 754 totalOrder),
/// tuples attribute by attribute, and relations when they have the same
/// tuples. The compiler keeps every relation's tuples in one canonical
/// order, so equal relations have equal arrays.
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
            KIND_INTEGER | KIND_RATIONAL => {
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

/// Release the memory of a text or relation made by [`trendy_rt_concat`]
/// or [`trendy_rt_remove`]. Both are a pointer and a length.
///
/// # Safety
///
/// `value` must point to such a value, not yet released.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_free(value: *const Text) {
    // SAFETY: guaranteed by the caller.
    unsafe { free(value.read_unaligned().bytes.cast_mut().cast()) }
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

/// Store at `dst` the relation at `src` with only the attributes `result`
/// describes, in canonical order and without duplicates, in memory that
/// [`trendy_rt_free`] releases. `source` describes `src`, and `offsets`
/// gives, for each attribute of `result`, its offset in a tuple of `src`.
///
/// # Safety
///
/// `src` must point to a valid relation of the type `source` describes,
/// `result` and `source` to valid relation descriptors, `offsets` to one
/// offset per attribute of `result`, and `dst` to memory for a relation.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trendy_rt_remove(
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

/// The `count` tuples at `tuples`, with the attributes of `desc`, sorted
/// canonically and without duplicates, copied into new memory: the new
/// memory and how many tuples it holds.
///
/// # Safety
///
/// As for [`trendy_rt_remove`]'s result.
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
        let out = allocate::<u8>(count * size);
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
/// numerically, rationals by IEEE 754 totalOrder, text by its bytes, tuples
/// attribute by attribute in name order, and relations by their tuples,
/// lexicographically.
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
            KIND_RATIONAL => lhs
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
    /// The UTF-8 bytes of the attribute's name.
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

/// Write `value` as trendy source: tuples and relations with their
/// attributes in name order, and relations with their heading first, so the
/// output is a literal for the same value.
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
            KIND_RATIONAL => write!(out, "{:?}", value.cast::<f64>().read_unaligned()),
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
        KIND_RATIONAL => out.write_str("rational"),
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
    // SAFETY: guaranteed by the caller.
    let attributes = unsafe { attributes(desc) };
    if attributes.is_empty() {
        return out.write_str("{}");
    }
    out.write_str("{ ")?;
    for (i, attr) in attributes.iter().enumerate() {
        if i > 0 {
            out.write_str(", ")?;
        }
        // SAFETY: a valid descriptor's attributes are valid.
        unsafe {
            out.write_str(name(attr))?;
            out.write_str(": ")?;
            write_type(out, &*attr.desc)?;
        }
    }
    out.write_str(" }")
}

/// `{ name: value, … }` or `{}`, for the tuple at `value`.
///
/// # Safety
///
/// As for [`write_value`], with `desc` describing the tuple's heading.
unsafe fn write_tuple(out: &mut impl Write, value: *const u8, desc: &TypeDesc) -> fmt::Result {
    // SAFETY: guaranteed by the caller.
    let attributes = unsafe { attributes(desc) };
    if attributes.is_empty() {
        return out.write_str("{}");
    }
    out.write_str("{ ")?;
    for (i, attr) in attributes.iter().enumerate() {
        if i > 0 {
            out.write_str(", ")?;
        }
        // SAFETY: the attribute's offset and descriptor are its layout.
        unsafe {
            out.write_str(name(attr))?;
            out.write_str(": ")?;
            write_value(out, value.add(attr.offset), &*attr.desc)?;
        }
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

    fn print(value: *const u8, desc: &TypeDesc) -> String {
        let mut out = String::new();
        // SAFETY: each test builds a value matching its descriptor.
        unsafe { write_value(&mut out, value, desc) }.unwrap();
        out
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
        let rational = |r: f64| print(r.to_ne_bytes().as_ptr(), &scalar(KIND_RATIONAL, 8));
        assert_eq!(rational(0.0), "0.0");
        assert_eq!(rational(1.5e-3), "0.0015");
        assert_eq!(rational(1e300), "1e300");
    }

    #[test]
    fn equality() {
        let rational = scalar(KIND_RATIONAL, 8);
        let same = |a: f64, b: f64| unsafe {
            equal(
                a.to_ne_bytes().as_ptr(),
                b.to_ne_bytes().as_ptr(),
                &rational,
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
