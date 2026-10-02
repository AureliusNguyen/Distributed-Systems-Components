//! C ABI for atomvar.
//!
//! Conventions:
//! - Every fallible function returns an `atomvar_status` (0 = ATOMVAR_OK) and
//!   writes results through out-pointers, which must be non-NULL.
//!   `atomvar_last_error()` returns a thread-local message for the most recent
//!   failure on the calling thread.
//! - Handles are opaque and released with their `_free` function. A handle keeps
//!   its storage (heap cell or arena mapping) alive.
//! - Only operations are exposed, never raw value pointers, so C callers cannot
//!   perform non-atomic accesses.
//! - Orderings are `atomvar_ordering` values; ATOMVAR_SEQ_CST is the safe default.
//! - No panic crosses the boundary (ATOMVAR_E_INTERNAL instead).
//!
//! The C prototypes are produced by the same macros that define the functions
//! (see `header()`); a test checks include/atomvar.h against them.

#![allow(non_camel_case_types, clippy::missing_safety_doc)]

use atomvar_core::{self as core, Arena, Error, Ordering};
use std::cell::RefCell;
use std::ffi::{c_char, c_int, CStr, CString};
use std::panic::{catch_unwind, AssertUnwindSafe};

pub const ATOMVAR_OK: c_int = 0;
pub const ATOMVAR_E_INVALID_ARGUMENT: c_int = 1;
pub const ATOMVAR_E_INVALID_ORDERING: c_int = 2;
pub const ATOMVAR_E_INVALID_NAME: c_int = 3;
pub const ATOMVAR_E_NOT_FOUND: c_int = 4;
pub const ATOMVAR_E_TYPE_MISMATCH: c_int = 5;
pub const ATOMVAR_E_ARENA_FULL: c_int = 6;
pub const ATOMVAR_E_LAYOUT_MISMATCH: c_int = 7;
pub const ATOMVAR_E_FORKED_PROCESS: c_int = 8;
pub const ATOMVAR_E_UNSUPPORTED_CPU: c_int = 9;
pub const ATOMVAR_E_IO: c_int = 10;
pub const ATOMVAR_E_INTERNAL: c_int = 11;

thread_local! {
    static LAST_ERROR: RefCell<CString> = RefCell::new(CString::default());
}

fn set_error(msg: &str) {
    let c = CString::new(msg.replace('\0', " ")).unwrap_or_default();
    LAST_ERROR.with(|e| *e.borrow_mut() = c);
}

fn status_of(e: &Error) -> c_int {
    match e {
        Error::InvalidOrdering(_) => ATOMVAR_E_INVALID_ORDERING,
        Error::InvalidName(_) => ATOMVAR_E_INVALID_NAME,
        Error::InvalidArgument(_) => ATOMVAR_E_INVALID_ARGUMENT,
        Error::ArenaFull { .. } => ATOMVAR_E_ARENA_FULL,
        Error::TypeMismatch { .. } => ATOMVAR_E_TYPE_MISMATCH,
        Error::NotFound { .. } => ATOMVAR_E_NOT_FOUND,
        Error::LayoutMismatch(_) => ATOMVAR_E_LAYOUT_MISMATCH,
        Error::ForkedProcess => ATOMVAR_E_FORKED_PROCESS,
        Error::UnsupportedCpu => ATOMVAR_E_UNSUPPORTED_CPU,
        Error::Io(_) => ATOMVAR_E_IO,
    }
}

/// Runs `f`, converting errors and panics into status codes.
fn guard(f: impl FnOnce() -> core::Result<()>) -> c_int {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(Ok(())) => ATOMVAR_OK,
        Ok(Err(e)) => {
            set_error(&e.to_string());
            status_of(&e)
        }
        Err(_) => {
            set_error("internal panic in atomvar");
            ATOMVAR_E_INTERNAL
        }
    }
}

fn arg<T>(p: *const T, what: &str) -> core::Result<()> {
    if p.is_null() {
        Err(Error::InvalidArgument(format!("{what} is NULL")))
    } else {
        Ok(())
    }
}

fn order(o: c_int) -> core::Result<Ordering> {
    u8::try_from(o)
        .map_err(|_| Error::InvalidOrdering("unknown ordering value"))
        .and_then(Ordering::from_u8)
}

unsafe fn cstr<'a>(p: *const c_char, what: &str) -> core::Result<&'a str> {
    arg(p, what)?;
    CStr::from_ptr(p)
        .to_str()
        .map_err(|_| Error::InvalidName(format!("{what} is not valid UTF-8")))
}

/// 128-bit value split into halves (portable across C compilers).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct atomvar_uint128 {
    pub lo: u64,
    pub hi: u64,
}

impl From<u128> for atomvar_uint128 {
    fn from(v: u128) -> Self {
        atomvar_uint128 {
            lo: v as u64,
            hi: (v >> 64) as u64,
        }
    }
}
impl From<atomvar_uint128> for u128 {
    fn from(v: atomvar_uint128) -> Self {
        (v.hi as u128) << 64 | v.lo as u128
    }
}

/// Opaque arena handle.
pub struct atomvar_arena(Arena);

macro_rules! proto {
    ($($s:expr),* $(,)?) => { concat!($($s),*, "\n") };
}

// ---------------------------------------------------------------------------
// Library-level functions
// ---------------------------------------------------------------------------

const PROTOS_LIB: &str = concat!(
    proto!("/* Best-effort CPU diagnostic: NOT a guarantee (see README, u128 contract). */"),
    proto!("int atomvar_init(void);"),
    proto!(""),
    proto!("/* Message for the last failure on this thread; valid until the next failing call. */"),
    proto!("const char *atomvar_last_error(void);"),
    proto!(""),
    proto!("/* Opens or creates the named shared-memory arena. capacity 0 adopts an existing"),
    proto!("   arena's capacity (or creates one with the default); otherwise it must match. */"),
    proto!("int atomvar_arena_open(const char *name, uint32_t capacity, atomvar_arena **out);"),
    proto!("void atomvar_arena_close(atomvar_arena *arena);"),
    proto!("uint32_t atomvar_arena_capacity(const atomvar_arena *arena);"),
);

#[no_mangle]
pub extern "C" fn atomvar_init() -> c_int {
    guard(core::init)
}

#[no_mangle]
pub extern "C" fn atomvar_last_error() -> *const c_char {
    LAST_ERROR.with(|e| e.borrow().as_ptr())
}

#[no_mangle]
pub unsafe extern "C" fn atomvar_arena_open(
    name: *const c_char,
    capacity: u32,
    out: *mut *mut atomvar_arena,
) -> c_int {
    guard(|| {
        arg(out, "out")?;
        let name = cstr(name, "name")?;
        let cap = if capacity == 0 { None } else { Some(capacity) };
        let a = Arena::open(name, cap)?;
        *out = Box::into_raw(Box::new(atomvar_arena(a)));
        Ok(())
    })
}

#[no_mangle]
pub unsafe extern "C" fn atomvar_arena_close(arena: *mut atomvar_arena) {
    if !arena.is_null() {
        let _ = catch_unwind(AssertUnwindSafe(|| drop(Box::from_raw(arena))));
    }
}

#[no_mangle]
pub unsafe extern "C" fn atomvar_arena_capacity(arena: *const atomvar_arena) -> u32 {
    if arena.is_null() {
        0
    } else {
        (*arena).0.capacity()
    }
}

// ---------------------------------------------------------------------------
// Per-type functions. $t: type name, $Core: core handle, $rt: core value type,
// $ft: FFI value type, $ct: C spelling of $ft.
// ---------------------------------------------------------------------------

macro_rules! common {
    ($t:ident, $Core:ident, $rt:ty, $ft:ty, $ct:literal) => {
        paste::paste! {
            /// Opaque handle.
            pub struct [<atomvar_ $t>](core::$Core);

            const [<PROTOS_ $t:upper _COMMON>]: &str = concat!(
                proto!("/* ---------------- ", stringify!($t), " ---------------- */"),
                proto!("int atomvar_", stringify!($t), "_new(", $ct, " init, atomvar_", stringify!($t), " **out);"),
                proto!("/* Opens or creates; init is used only when this call creates the variable. */"),
                proto!("int atomvar_arena_", stringify!($t), "(const atomvar_arena *arena, const char *name, ", $ct, " init, atomvar_", stringify!($t), " **out);"),
                proto!("/* Opens an existing variable (ATOMVAR_E_NOT_FOUND otherwise). */"),
                proto!("int atomvar_arena_open_", stringify!($t), "(const atomvar_arena *arena, const char *name, atomvar_", stringify!($t), " **out);"),
                proto!("void atomvar_", stringify!($t), "_free(atomvar_", stringify!($t), " *h);"),
                proto!("int atomvar_", stringify!($t), "_load(const atomvar_", stringify!($t), " *h, int order, ", $ct, " *out);"),
                proto!("int atomvar_", stringify!($t), "_store(const atomvar_", stringify!($t), " *h, ", $ct, " value, int order);"),
                proto!("int atomvar_", stringify!($t), "_swap(const atomvar_", stringify!($t), " *h, ", $ct, " value, int order, ", $ct, " *old);"),
                proto!("/* Strong CAS. *previous receives the value seen; *exchanged tells whether it was replaced. */"),
                proto!("int atomvar_", stringify!($t), "_compare_exchange(const atomvar_", stringify!($t), " *h, ", $ct, " expected, ", $ct, " desired, int success, int failure, ", $ct, " *previous, bool *exchanged);"),
            );

            #[no_mangle]
            pub unsafe extern "C" fn [<atomvar_ $t _new>](init: $ft, out: *mut *mut [<atomvar_ $t>]) -> c_int {
                guard(|| {
                    arg(out, "out")?;
                    *out = Box::into_raw(Box::new([<atomvar_ $t>](core::$Core::new(<$rt>::from(init)))));
                    Ok(())
                })
            }

            #[no_mangle]
            pub unsafe extern "C" fn [<atomvar_arena_ $t>](
                arena: *const atomvar_arena,
                name: *const c_char,
                init: $ft,
                out: *mut *mut [<atomvar_ $t>],
            ) -> c_int {
                guard(|| {
                    arg(arena, "arena")?;
                    arg(out, "out")?;
                    let name = cstr(name, "name")?;
                    let h = (*arena).0.$t(name, <$rt>::from(init))?;
                    *out = Box::into_raw(Box::new([<atomvar_ $t>](h)));
                    Ok(())
                })
            }

            #[no_mangle]
            pub unsafe extern "C" fn [<atomvar_arena_open_ $t>](
                arena: *const atomvar_arena,
                name: *const c_char,
                out: *mut *mut [<atomvar_ $t>],
            ) -> c_int {
                guard(|| {
                    arg(arena, "arena")?;
                    arg(out, "out")?;
                    let name = cstr(name, "name")?;
                    let h = (*arena).0.[<open_ $t>](name)?;
                    *out = Box::into_raw(Box::new([<atomvar_ $t>](h)));
                    Ok(())
                })
            }

            #[no_mangle]
            pub unsafe extern "C" fn [<atomvar_ $t _free>](h: *mut [<atomvar_ $t>]) {
                if !h.is_null() {
                    let _ = catch_unwind(AssertUnwindSafe(|| drop(Box::from_raw(h))));
                }
            }

            #[no_mangle]
            pub unsafe extern "C" fn [<atomvar_ $t _load>](h: *const [<atomvar_ $t>], o: c_int, out: *mut $ft) -> c_int {
                guard(|| {
                    arg(h, "h")?;
                    arg(out, "out")?;
                    *out = <$ft>::from((*h).0.load(order(o)?)?);
                    Ok(())
                })
            }

            #[no_mangle]
            pub unsafe extern "C" fn [<atomvar_ $t _store>](h: *const [<atomvar_ $t>], v: $ft, o: c_int) -> c_int {
                guard(|| {
                    arg(h, "h")?;
                    (*h).0.store(<$rt>::from(v), order(o)?)
                })
            }

            #[no_mangle]
            pub unsafe extern "C" fn [<atomvar_ $t _swap>](h: *const [<atomvar_ $t>], v: $ft, o: c_int, old: *mut $ft) -> c_int {
                guard(|| {
                    arg(h, "h")?;
                    arg(old, "old")?;
                    *old = <$ft>::from((*h).0.swap(<$rt>::from(v), order(o)?));
                    Ok(())
                })
            }

            #[no_mangle]
            pub unsafe extern "C" fn [<atomvar_ $t _compare_exchange>](
                h: *const [<atomvar_ $t>],
                expected: $ft,
                desired: $ft,
                success: c_int,
                failure: c_int,
                previous: *mut $ft,
                exchanged: *mut bool,
            ) -> c_int {
                guard(|| {
                    arg(h, "h")?;
                    arg(previous, "previous")?;
                    arg(exchanged, "exchanged")?;
                    let r = (*h).0.compare_exchange(
                        <$rt>::from(expected),
                        <$rt>::from(desired),
                        order(success)?,
                        order(failure)?,
                    )?;
                    let (p, ok) = match r {
                        Ok(p) => (p, true),
                        Err(p) => (p, false),
                    };
                    *previous = <$ft>::from(p);
                    *exchanged = ok;
                    Ok(())
                })
            }
        }
    };
}

macro_rules! cas_weak {
    ($t:ident, $rt:ty, $ft:ty, $ct:literal) => {
        paste::paste! {
            const [<PROTOS_ $t:upper _WEAK>]: &str = concat!(
                proto!("/* Weak CAS: may fail spuriously; use inside a retry loop. */"),
                proto!("int atomvar_", stringify!($t), "_compare_exchange_weak(const atomvar_", stringify!($t), " *h, ", $ct, " expected, ", $ct, " desired, int success, int failure, ", $ct, " *previous, bool *exchanged);"),
            );

            #[no_mangle]
            pub unsafe extern "C" fn [<atomvar_ $t _compare_exchange_weak>](
                h: *const [<atomvar_ $t>],
                expected: $ft,
                desired: $ft,
                success: c_int,
                failure: c_int,
                previous: *mut $ft,
                exchanged: *mut bool,
            ) -> c_int {
                guard(|| {
                    arg(h, "h")?;
                    arg(previous, "previous")?;
                    arg(exchanged, "exchanged")?;
                    let r = (*h).0.compare_exchange_weak(
                        <$rt>::from(expected),
                        <$rt>::from(desired),
                        order(success)?,
                        order(failure)?,
                    )?;
                    let (p, ok) = match r {
                        Ok(p) => (p, true),
                        Err(p) => (p, false),
                    };
                    *previous = <$ft>::from(p);
                    *exchanged = ok;
                    Ok(())
                })
            }
        }
    };
}

/// Read-modify-write ops of shape `(h, value, order, *out)`. fetch_* write the
/// old value; *_and_get write the new value.
macro_rules! rmw {
    ($t:ident, $rt:ty, $ft:ty, $ct:literal, [$($op:ident),* $(,)?]) => {
        paste::paste! {
            const [<PROTOS_ $t:upper _RMW>]: &str = concat!(
                proto!("/* fetch_*: *out = value before the op. *_and_get: *out = value after the op. */"),
                $(proto!("int atomvar_", stringify!($t), "_", stringify!($op), "(const atomvar_", stringify!($t), " *h, ", $ct, " value, int order, ", $ct, " *out);"),)*
            );

            $(
                #[no_mangle]
                pub unsafe extern "C" fn [<atomvar_ $t _ $op>](h: *const [<atomvar_ $t>], v: $ft, o: c_int, out: *mut $ft) -> c_int {
                    guard(|| {
                        arg(h, "h")?;
                        arg(out, "out")?;
                        *out = <$ft>::from((*h).0.$op(<$rt>::from(v), order(o)?));
                        Ok(())
                    })
                }
            )*
        }
    };
}

common!(i64, AtomicI64, i64, i64, "int64_t");
cas_weak!(i64, i64, i64, "int64_t");
rmw!(i64, i64, i64, "int64_t", [fetch_add, fetch_sub, fetch_and, fetch_or, fetch_xor, fetch_max, fetch_min, add_and_get, sub_and_get]);

common!(u64, AtomicU64, u64, u64, "uint64_t");
cas_weak!(u64, u64, u64, "uint64_t");
rmw!(u64, u64, u64, "uint64_t", [fetch_add, fetch_sub, fetch_and, fetch_or, fetch_xor, fetch_max, fetch_min, add_and_get, sub_and_get]);

common!(bool, AtomicBool, bool, bool, "bool");
cas_weak!(bool, bool, bool, "bool");
rmw!(bool, bool, bool, "bool", [fetch_and, fetch_or, fetch_xor, fetch_nand]);

common!(f64, AtomicF64, f64, f64, "double");
rmw!(f64, f64, f64, "double", [fetch_add, fetch_sub, add_and_get]);

common!(u128, AtomicU128, u128, atomvar_uint128, "atomvar_uint128");
cas_weak!(u128, u128, atomvar_uint128, "atomvar_uint128");

// f64 exact-bits access (NaN payloads, signed zero).
const PROTOS_F64_BITS: &str = concat!(
    proto!("/* Exact IEEE-754 bit patterns; compare_exchange on doubles is also bitwise. */"),
    proto!("int atomvar_f64_load_bits(const atomvar_f64 *h, int order, uint64_t *out);"),
    proto!("int atomvar_f64_store_bits(const atomvar_f64 *h, uint64_t bits, int order);"),
    proto!("int atomvar_f64_compare_exchange_bits(const atomvar_f64 *h, uint64_t expected, uint64_t desired, int success, int failure, uint64_t *previous, bool *exchanged);"),
);

#[no_mangle]
pub unsafe extern "C" fn atomvar_f64_load_bits(h: *const atomvar_f64, o: c_int, out: *mut u64) -> c_int {
    guard(|| {
        arg(h, "h")?;
        arg(out, "out")?;
        *out = (*h).0.load_bits(order(o)?)?;
        Ok(())
    })
}

#[no_mangle]
pub unsafe extern "C" fn atomvar_f64_store_bits(h: *const atomvar_f64, bits: u64, o: c_int) -> c_int {
    guard(|| {
        arg(h, "h")?;
        (*h).0.store_bits(bits, order(o)?)
    })
}

#[no_mangle]
pub unsafe extern "C" fn atomvar_f64_compare_exchange_bits(
    h: *const atomvar_f64,
    expected: u64,
    desired: u64,
    success: c_int,
    failure: c_int,
    previous: *mut u64,
    exchanged: *mut bool,
) -> c_int {
    guard(|| {
        arg(h, "h")?;
        arg(previous, "previous")?;
        arg(exchanged, "exchanged")?;
        let (p, ok) = match (*h)
            .0
            .compare_exchange_bits(expected, desired, order(success)?, order(failure)?)?
        {
            Ok(p) => (p, true),
            Err(p) => (p, false),
        };
        *previous = p;
        *exchanged = ok;
        Ok(())
    })
}

// ---------------------------------------------------------------------------
// Header assembly
// ---------------------------------------------------------------------------

const HEADER_PRELUDE: &str = r#"/* atomvar.h - C ABI for atomvar (lock-free atomic variables in shared memory).
 *
 * GENERATED by `cargo run -p atomvar-ffi --bin atomvar-gen-header`; do not edit.
 *
 * Guarantees: value operations are atomic and lock-free (no fallback locks),
 * not wait-free or single-instruction. Registry operations (open/create by name)
 * take an OS lock. Supported: Linux x86_64 with CMPXCHG16B. Processes must be
 * started fresh (spawn / fork+exec); a plain fork() child gets
 * ATOMVAR_E_FORKED_PROCESS from registry calls.
 *
 * All out-pointers must be non-NULL. Link with -latomvar.
 */
#ifndef ATOMVAR_H
#define ATOMVAR_H

#include <stdbool.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef enum atomvar_status {
    ATOMVAR_OK = 0,
    ATOMVAR_E_INVALID_ARGUMENT = 1,
    ATOMVAR_E_INVALID_ORDERING = 2,
    ATOMVAR_E_INVALID_NAME = 3,
    ATOMVAR_E_NOT_FOUND = 4,
    ATOMVAR_E_TYPE_MISMATCH = 5,
    ATOMVAR_E_ARENA_FULL = 6,
    ATOMVAR_E_LAYOUT_MISMATCH = 7,
    ATOMVAR_E_FORKED_PROCESS = 8,
    ATOMVAR_E_UNSUPPORTED_CPU = 9,
    ATOMVAR_E_IO = 10,
    ATOMVAR_E_INTERNAL = 11
} atomvar_status;

typedef enum atomvar_ordering {
    ATOMVAR_RELAXED = 0,
    ATOMVAR_ACQUIRE = 1,
    ATOMVAR_RELEASE = 2,
    ATOMVAR_ACQ_REL = 3,
    ATOMVAR_SEQ_CST = 4
} atomvar_ordering;

typedef struct atomvar_uint128 {
    uint64_t lo;
    uint64_t hi;
} atomvar_uint128;

typedef struct atomvar_arena atomvar_arena;
typedef struct atomvar_i64 atomvar_i64;
typedef struct atomvar_u64 atomvar_u64;
typedef struct atomvar_bool atomvar_bool;
typedef struct atomvar_f64 atomvar_f64;
typedef struct atomvar_u128 atomvar_u128;

"#;

const HEADER_EPILOGUE: &str = r#"
#ifdef __cplusplus
}
#endif

#endif /* ATOMVAR_H */
"#;

const SECTIONS: &[&str] = &[
    PROTOS_LIB,
    PROTOS_I64_COMMON,
    PROTOS_I64_WEAK,
    PROTOS_I64_RMW,
    PROTOS_U64_COMMON,
    PROTOS_U64_WEAK,
    PROTOS_U64_RMW,
    PROTOS_BOOL_COMMON,
    PROTOS_BOOL_WEAK,
    PROTOS_BOOL_RMW,
    PROTOS_F64_COMMON,
    PROTOS_F64_RMW,
    PROTOS_F64_BITS,
    PROTOS_U128_COMMON,
    PROTOS_U128_WEAK,
];

/// The complete C header, generated from the function-defining macros.
pub fn header() -> String {
    let mut s = String::from(HEADER_PRELUDE);
    for (i, sec) in SECTIONS.iter().enumerate() {
        if i > 0 {
            s.push('\n');
        }
        s.push_str(sec);
    }
    s.push_str(HEADER_EPILOGUE);
    s
}

#[cfg(test)]
mod tests {
    #[test]
    fn committed_header_matches_generated() {
        let committed = include_str!("../include/atomvar.h");
        assert!(
            committed == super::header(),
            "include/atomvar.h is stale: run `cargo run -p atomvar-ffi --bin atomvar-gen-header > crates/atomvar-ffi/include/atomvar.h`"
        );
    }
}
