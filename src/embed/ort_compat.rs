//! Symbols the prebuilt ONNX Runtime archive expects from a newer C and C++
//! runtime than glibc 2.35 and GCC 12 provide.
//!
//! ort links a static ONNX Runtime compiled against glibc 2.38 and the GCC 13
//! libstdc++ headers. On an older system the link fails on `__isoc23_strtol`
//! and friends and on `basic_string::_M_replace_cold`, which only those
//! versions define. Defining them here lets the same archive link and run
//! against glibc 2.35 and libstdc++ 12. Where the system already has them,
//! these behave the same and take precedence only for this binary.

use std::ffi::{c_char, c_int, c_long, c_longlong, c_ulong, c_ulonglong, c_void};
use std::ptr;

unsafe extern "C" {
    fn strtol(s: *const c_char, end: *mut *mut c_char, base: c_int) -> c_long;
    fn strtoll(s: *const c_char, end: *mut *mut c_char, base: c_int) -> c_longlong;
    fn strtoul(s: *const c_char, end: *mut *mut c_char, base: c_int) -> c_ulong;
    fn strtoull(s: *const c_char, end: *mut *mut c_char, base: c_int) -> c_ulonglong;
    fn strtoll_l(
        s: *const c_char,
        end: *mut *mut c_char,
        base: c_int,
        locale: *mut c_void,
    ) -> c_longlong;
    fn strtoull_l(
        s: *const c_char,
        end: *mut *mut c_char,
        base: c_int,
        locale: *mut c_void,
    ) -> c_ulonglong;
    #[link_name = "_ZNSt8ios_base4InitC1Ev"]
    fn ios_base_init(this: *mut u8);
}

// glibc 2.38 headers route the strto* family to C23 versions, which differ
// only in also reading a `0b` prefix when the base is 0 or 2.

#[unsafe(no_mangle)]
unsafe extern "C" fn __isoc23_strtol(
    s: *const c_char,
    end: *mut *mut c_char,
    base: c_int,
) -> c_long {
    unsafe { strtol(s, end, base) }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn __isoc23_strtoll(
    s: *const c_char,
    end: *mut *mut c_char,
    base: c_int,
) -> c_longlong {
    unsafe { strtoll(s, end, base) }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn __isoc23_strtoul(
    s: *const c_char,
    end: *mut *mut c_char,
    base: c_int,
) -> c_ulong {
    unsafe { strtoul(s, end, base) }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn __isoc23_strtoull(
    s: *const c_char,
    end: *mut *mut c_char,
    base: c_int,
) -> c_ulonglong {
    unsafe { strtoull(s, end, base) }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn __isoc23_strtoll_l(
    s: *const c_char,
    end: *mut *mut c_char,
    base: c_int,
    locale: *mut c_void,
) -> c_longlong {
    unsafe { strtoll_l(s, end, base, locale) }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn __isoc23_strtoull_l(
    s: *const c_char,
    end: *mut *mut c_char,
    base: c_int,
    locale: *mut c_void,
) -> c_ulonglong {
    unsafe { strtoull_l(s, end, base, locale) }
}

/// `std::string::_M_replace_cold`, the in-place half of `replace` when the
/// source lies inside the string, which GCC 13 moved out of line. `p` is the
/// start of the replaced range, `len1` its length, `s` and `len2` the source,
/// and `how_much` the length of the tail after the range. Ported from
/// libstdc++'s basic_string.tcc; every copy is a memmove, which covers its
/// memcpy calls too.
unsafe fn replace_cold<T: Copy>(p: *mut T, len1: usize, s: *const T, len2: usize, how_much: usize) {
    unsafe {
        if len2 != 0 && len2 <= len1 {
            ptr::copy(s, p, len2);
        }
        if how_much != 0 && len1 != len2 {
            ptr::copy(p.add(len1), p.add(len2), how_much);
        }
        if len2 > len1 {
            let end1 = p.add(len1).cast_const();
            if s.add(len2) <= end1 {
                ptr::copy(s, p, len2);
            } else if s >= end1 {
                // The source was in the tail, which just moved len2 - len1 on.
                let poff = s.offset_from(p) as usize + (len2 - len1);
                ptr::copy(p.add(poff), p, len2);
            } else {
                // The source straddles the end of the range: its left part did
                // not move, its right part moved along with the tail.
                let nleft = end1.offset_from(s) as usize;
                ptr::copy(s, p, nleft);
                ptr::copy(p.add(len2), p.add(nleft), len2 - nleft);
            }
        }
    }
}

#[unsafe(export_name = "_ZNSt7__cxx1112basic_stringIcSt11char_traitsIcESaIcEE15_M_replace_coldEPcmPKcmm")]
unsafe extern "C" fn string_replace_cold(
    _this: *mut c_void,
    p: *mut c_char,
    len1: usize,
    s: *const c_char,
    len2: usize,
    how_much: usize,
) {
    unsafe { replace_cold(p, len1, s, len2, how_much) }
}

#[unsafe(export_name = "_ZNSt7__cxx1112basic_stringIwSt11char_traitsIwESaIwEE15_M_replace_coldEPwmPKwmm")]
unsafe extern "C" fn wstring_replace_cold(
    _this: *mut c_void,
    p: *mut i32,
    len1: usize,
    s: *const i32,
    len2: usize,
    how_much: usize,
) {
    unsafe { replace_cold(p, len1, s, len2, how_much) }
}

/// GCC 13's `<iostream>` no longer constructs the standard streams in every
/// file that includes it. It references this symbol instead, so that only a
/// libstdc++ that builds the streams itself satisfies the link. An older
/// libstdc++ does not, and the first write to `std::cerr` crashes, so
/// `INIT_STREAMS` builds them.
#[unsafe(export_name = "_ZSt21ios_base_library_initv")]
extern "C" fn ios_base_library_init() {}

/// Runs before `main` and, being in a numbered `.init_array` section, before
/// the archive's own static constructors. The constructor counts references,
/// so a libstdc++ that already built the streams only increments the count.
#[used]
#[unsafe(link_section = ".init_array.00099")]
static INIT_STREAMS: extern "C" fn() = init_streams;

extern "C" fn init_streams() {
    // std::ios_base::Init is an empty class: one byte, never read.
    static mut INIT: u8 = 0;
    unsafe { ios_base_init(&raw mut INIT) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Replaces `len1` bytes at `pos` with `len2` bytes read from the same
    /// string at `src`, in place, as `std::string::replace` would.
    fn replace_in_place(text: &str, pos: usize, len1: usize, src: usize, len2: usize) -> String {
        let mut buf = text.as_bytes().to_vec();
        buf.resize(text.len() + len2, 0);
        let how_much = text.len() - pos - len1;
        let base = buf.as_mut_ptr();
        unsafe {
            replace_cold(
                base.add(pos),
                len1,
                base.add(src).cast_const(),
                len2,
                how_much,
            )
        };
        buf.truncate(text.len() - len1 + len2);
        String::from_utf8(buf).unwrap()
    }

    fn replace_copied(text: &str, pos: usize, len1: usize, src: usize, len2: usize) -> String {
        let source = &text[src..src + len2];
        format!("{}{}{}", &text[..pos], source, &text[pos + len1..])
    }

    #[test]
    fn replace_cold_matches_a_copy_of_the_source_wherever_it_lies() {
        let text = "abcdefgh";
        for (pos, len1, src, len2) in [
            (1, 4, 2, 2), // shorter, from inside the range
            (1, 2, 5, 2), // same length, from the tail
            (4, 1, 0, 3), // longer, from before the range
            (1, 2, 3, 3), // longer, from the tail
            (1, 2, 2, 4), // longer, straddling the end of the range
            (0, 0, 0, 3), // insertion of the string's own start
        ] {
            assert_eq!(
                replace_in_place(text, pos, len1, src, len2),
                replace_copied(text, pos, len1, src, len2),
                "replace({pos}, {len1}) with [{src}..+{len2}]"
            );
        }
    }
}
