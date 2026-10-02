//! atomvar-core: lock-free atomic variables.
//!
//! Two backings share one set of handle types:
//! - heap: `AtomicI64::new(0)`, shared between threads of one process;
//! - arena: `Arena::open("jobs", None)?.i64("counter", 0)?`, a named variable in a
//!   shared-memory segment that any process (any language, via the C ABI) can open.
//!
//! Guarantees (see README): value operations are atomic and lock-free (no fallback
//! locks), not wait-free or single-instruction. Registry operations and arena
//! bootstrap take an OS lock. Default ordering everywhere in the bindings is SeqCst.

#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
compile_error!("atomvar v1 supports Linux x86_64 only");

#[cfg(not(target_feature = "cmpxchg16b"))]
compile_error!(
    "atomvar requires -C target-feature=+cmpxchg16b (set in Atomic-Variables/.cargo/config.toml)"
);

// No u128 operation may silently fall back to a global lock.
const _: () = assert!(portable_atomic::AtomicU128::is_always_lock_free());

#[doc(hidden)]
pub mod algo;
mod arena;
mod cpu;
mod error;
mod handles;
mod hooks;
mod layout;
mod ordering;
mod value;

pub use arena::{arena_exists, destroy_arena, validate_arena_name, Arena, VarInfo, DEFAULT_CAPACITY, MAX_CAPACITY};
pub use cpu::{cpu_supported, init};
pub use error::{Error, Result};
pub use handles::{AnyAtomic, AtomicBool, AtomicF64, AtomicI64, AtomicU128, AtomicU64};
pub use layout::{MAX_ARENA_NAME_LEN, MAX_VAR_NAME_LEN};
pub use ordering::Ordering;
pub use value::{Value, ValueType};
