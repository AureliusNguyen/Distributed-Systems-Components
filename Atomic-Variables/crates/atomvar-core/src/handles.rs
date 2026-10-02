//! Typed handles. Each wraps a 16-byte-aligned value cell that lives either on
//! the heap (in-process) or in an arena slot (shared memory), and views it as
//! exactly one atomic type for its whole lifetime.

use crate::algo;
use crate::arena::ArenaInner;
use crate::error::Result;
use crate::layout::ValueCell;
use crate::ordering::Ordering;
use crate::value::{Value, ValueType};
use std::cell::UnsafeCell;
use std::fmt;
use std::ptr::NonNull;
use std::sync::atomic as sa;
use std::sync::Arc;

/// Keeps the storage alive for as long as any handle exists.
#[derive(Clone)]
pub(crate) enum Owner {
    Heap(#[allow(dead_code)] Arc<ValueCell>),
    Arena(#[allow(dead_code)] Arc<ArenaInner>),
}

#[derive(Clone)]
struct Cell {
    ptr: NonNull<u8>,
    _owner: Owner,
}

// SAFETY: the cell is only accessed through atomic operations.
unsafe impl Send for Cell {}
unsafe impl Sync for Cell {}

impl Cell {
    fn heap(init: u128, store: impl FnOnce(*mut u8, u128)) -> Self {
        let owner = Arc::new(ValueCell(UnsafeCell::new([0; 16])));
        let ptr = NonNull::new(owner.0.get() as *mut u8).expect("non-null");
        store(ptr.as_ptr(), init);
        Cell {
            ptr,
            _owner: Owner::Heap(owner),
        }
    }
}

// ---------------------------------------------------------------------------
// Integer handles (i64, u64)
// ---------------------------------------------------------------------------

macro_rules! int_handle {
    ($name:ident, $t:ty, $std:ident) => {
        #[doc = concat!("Lock-free atomic `", stringify!($t), "`. Arithmetic wraps on overflow.")]
        #[derive(Clone)]
        pub struct $name {
            cell: Cell,
        }

        impl $name {
            /// New in-process (heap) variable.
            pub fn new(v: $t) -> Self {
                $name {
                    // SAFETY: fresh, aligned, exclusively owned cell.
                    cell: Cell::heap(v as u128, |p, v| unsafe {
                        sa::$std::from_ptr(p as *mut $t).store(v as $t, sa::Ordering::Relaxed)
                    }),
                }
            }

            pub(crate) fn from_cell(ptr: NonNull<u8>, owner: Owner) -> Self {
                $name {
                    cell: Cell { ptr, _owner: owner },
                }
            }

            #[inline]
            fn a(&self) -> &sa::$std {
                // SAFETY: 16-byte aligned cell viewed only as this type, kept alive by owner.
                unsafe { sa::$std::from_ptr(self.cell.ptr.as_ptr() as *mut $t) }
            }

            pub fn load(&self, o: Ordering) -> Result<$t> {
                Ok(self.a().load(o.for_load()?))
            }
            pub fn store(&self, v: $t, o: Ordering) -> Result<()> {
                self.a().store(v, o.for_store()?);
                Ok(())
            }
            pub fn swap(&self, v: $t, o: Ordering) -> $t {
                self.a().swap(v, o.std())
            }
            /// Strong CAS. `Ok(previous)` on success, `Err(actual)` on failure.
            pub fn compare_exchange(
                &self,
                current: $t,
                new: $t,
                success: Ordering,
                failure: Ordering,
            ) -> Result<std::result::Result<$t, $t>> {
                let (s, f) = Ordering::for_cas(success, failure)?;
                Ok(self.a().compare_exchange(current, new, s, f))
            }
            /// Weak CAS: may fail spuriously; use in retry loops.
            pub fn compare_exchange_weak(
                &self,
                current: $t,
                new: $t,
                success: Ordering,
                failure: Ordering,
            ) -> Result<std::result::Result<$t, $t>> {
                let (s, f) = Ordering::for_cas(success, failure)?;
                Ok(self.a().compare_exchange_weak(current, new, s, f))
            }
            pub fn fetch_add(&self, v: $t, o: Ordering) -> $t {
                self.a().fetch_add(v, o.std())
            }
            pub fn fetch_sub(&self, v: $t, o: Ordering) -> $t {
                self.a().fetch_sub(v, o.std())
            }
            pub fn fetch_and(&self, v: $t, o: Ordering) -> $t {
                self.a().fetch_and(v, o.std())
            }
            pub fn fetch_or(&self, v: $t, o: Ordering) -> $t {
                self.a().fetch_or(v, o.std())
            }
            pub fn fetch_xor(&self, v: $t, o: Ordering) -> $t {
                self.a().fetch_xor(v, o.std())
            }
            pub fn fetch_max(&self, v: $t, o: Ordering) -> $t {
                self.a().fetch_max(v, o.std())
            }
            pub fn fetch_min(&self, v: $t, o: Ordering) -> $t {
                self.a().fetch_min(v, o.std())
            }
            /// New value, derived from the single fetch_add's old value (wrapping).
            /// Same formula as `algo::add_and_get_i64`, which loom model-checks.
            pub fn add_and_get(&self, v: $t, o: Ordering) -> $t {
                self.fetch_add(v, o).wrapping_add(v)
            }
            /// New value, derived from the single fetch_sub's old value (wrapping).
            pub fn sub_and_get(&self, v: $t, o: Ordering) -> $t {
                self.fetch_sub(v, o).wrapping_sub(v)
            }
            pub fn increment_and_get(&self, o: Ordering) -> $t {
                self.add_and_get(1, o)
            }
            pub fn decrement_and_get(&self, o: Ordering) -> $t {
                self.sub_and_get(1, o)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}({})", stringify!($name), self.a().load(sa::Ordering::SeqCst))
            }
        }
    };
}

int_handle!(AtomicI64, i64, AtomicI64);
int_handle!(AtomicU64, u64, AtomicU64);

// ---------------------------------------------------------------------------
// Bool
// ---------------------------------------------------------------------------

/// Lock-free atomic `bool`.
#[derive(Clone)]
pub struct AtomicBool {
    cell: Cell,
}

impl AtomicBool {
    pub fn new(v: bool) -> Self {
        AtomicBool {
            // SAFETY: fresh, aligned, exclusively owned cell.
            cell: Cell::heap(v as u128, |p, v| unsafe {
                sa::AtomicBool::from_ptr(p as *mut bool).store(v != 0, sa::Ordering::Relaxed)
            }),
        }
    }
    pub(crate) fn from_cell(ptr: NonNull<u8>, owner: Owner) -> Self {
        AtomicBool {
            cell: Cell { ptr, _owner: owner },
        }
    }
    #[inline]
    fn a(&self) -> &sa::AtomicBool {
        // SAFETY: see int_handle.
        unsafe { sa::AtomicBool::from_ptr(self.cell.ptr.as_ptr() as *mut bool) }
    }
    pub fn load(&self, o: Ordering) -> Result<bool> {
        Ok(self.a().load(o.for_load()?))
    }
    pub fn store(&self, v: bool, o: Ordering) -> Result<()> {
        self.a().store(v, o.for_store()?);
        Ok(())
    }
    pub fn swap(&self, v: bool, o: Ordering) -> bool {
        self.a().swap(v, o.std())
    }
    pub fn compare_exchange(
        &self,
        current: bool,
        new: bool,
        success: Ordering,
        failure: Ordering,
    ) -> Result<std::result::Result<bool, bool>> {
        let (s, f) = Ordering::for_cas(success, failure)?;
        Ok(self.a().compare_exchange(current, new, s, f))
    }
    pub fn compare_exchange_weak(
        &self,
        current: bool,
        new: bool,
        success: Ordering,
        failure: Ordering,
    ) -> Result<std::result::Result<bool, bool>> {
        let (s, f) = Ordering::for_cas(success, failure)?;
        Ok(self.a().compare_exchange_weak(current, new, s, f))
    }
    pub fn fetch_and(&self, v: bool, o: Ordering) -> bool {
        self.a().fetch_and(v, o.std())
    }
    pub fn fetch_or(&self, v: bool, o: Ordering) -> bool {
        self.a().fetch_or(v, o.std())
    }
    pub fn fetch_xor(&self, v: bool, o: Ordering) -> bool {
        self.a().fetch_xor(v, o.std())
    }
    pub fn fetch_nand(&self, v: bool, o: Ordering) -> bool {
        self.a().fetch_nand(v, o.std())
    }
}

impl fmt::Debug for AtomicBool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "AtomicBool({})", self.a().load(sa::Ordering::SeqCst))
    }
}

// ---------------------------------------------------------------------------
// f64 (stored as its u64 bit pattern)
// ---------------------------------------------------------------------------

/// Lock-free atomic `f64`. CAS compares raw bits (+0.0 != -0.0; a NaN matches
/// only the identical bit pattern). fetch_add/sub are CAS loops (not wait-free).
#[derive(Clone)]
pub struct AtomicF64 {
    cell: Cell,
}

impl AtomicF64 {
    pub fn new(v: f64) -> Self {
        AtomicF64 {
            // SAFETY: fresh, aligned, exclusively owned cell.
            cell: Cell::heap(v.to_bits() as u128, |p, v| unsafe {
                sa::AtomicU64::from_ptr(p as *mut u64).store(v as u64, sa::Ordering::Relaxed)
            }),
        }
    }
    pub(crate) fn from_cell(ptr: NonNull<u8>, owner: Owner) -> Self {
        AtomicF64 {
            cell: Cell { ptr, _owner: owner },
        }
    }
    #[inline]
    fn a(&self) -> &sa::AtomicU64 {
        // SAFETY: see int_handle.
        unsafe { sa::AtomicU64::from_ptr(self.cell.ptr.as_ptr() as *mut u64) }
    }
    pub fn load(&self, o: Ordering) -> Result<f64> {
        Ok(f64::from_bits(self.a().load(o.for_load()?)))
    }
    pub fn load_bits(&self, o: Ordering) -> Result<u64> {
        Ok(self.a().load(o.for_load()?))
    }
    pub fn store(&self, v: f64, o: Ordering) -> Result<()> {
        self.store_bits(v.to_bits(), o)
    }
    pub fn store_bits(&self, bits: u64, o: Ordering) -> Result<()> {
        self.a().store(bits, o.for_store()?);
        Ok(())
    }
    pub fn swap(&self, v: f64, o: Ordering) -> f64 {
        f64::from_bits(self.a().swap(v.to_bits(), o.std()))
    }
    /// Bitwise CAS on f64 values.
    pub fn compare_exchange(
        &self,
        current: f64,
        new: f64,
        success: Ordering,
        failure: Ordering,
    ) -> Result<std::result::Result<f64, f64>> {
        Ok(self
            .compare_exchange_bits(current.to_bits(), new.to_bits(), success, failure)?
            .map(f64::from_bits)
            .map_err(f64::from_bits))
    }
    pub fn compare_exchange_bits(
        &self,
        current: u64,
        new: u64,
        success: Ordering,
        failure: Ordering,
    ) -> Result<std::result::Result<u64, u64>> {
        let (s, f) = Ordering::for_cas(success, failure)?;
        Ok(self.a().compare_exchange(current, new, s, f))
    }
    /// IEEE add via CAS loop; returns the old value.
    pub fn fetch_add(&self, v: f64, o: Ordering) -> f64 {
        algo::f64_fetch_update(self.a(), o.std(), |x| x + v)
    }
    /// IEEE subtract via CAS loop; returns the old value.
    pub fn fetch_sub(&self, v: f64, o: Ordering) -> f64 {
        algo::f64_fetch_update(self.a(), o.std(), |x| x - v)
    }
    pub fn add_and_get(&self, v: f64, o: Ordering) -> f64 {
        self.fetch_add(v, o) + v
    }
}

impl fmt::Debug for AtomicF64 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "AtomicF64({:?})", f64::from_bits(self.a().load(sa::Ordering::SeqCst)))
    }
}

// ---------------------------------------------------------------------------
// u128 (CMPXCHG16B)
// ---------------------------------------------------------------------------

/// Lock-free atomic `u128` (CMPXCHG16B). Intended for ABA-safe tagged values,
/// e.g. a `(version: u64, value: u64)` pair packed as `(version << 64) | value`.
#[derive(Clone)]
pub struct AtomicU128 {
    cell: Cell,
}

impl AtomicU128 {
    pub fn new(v: u128) -> Self {
        AtomicU128 {
            // SAFETY: fresh, aligned, exclusively owned cell.
            cell: Cell::heap(v, |p, v| unsafe {
                portable_atomic::AtomicU128::from_ptr(p as *mut u128)
                    .store(v, sa::Ordering::Relaxed)
            }),
        }
    }
    pub(crate) fn from_cell(ptr: NonNull<u8>, owner: Owner) -> Self {
        AtomicU128 {
            cell: Cell { ptr, _owner: owner },
        }
    }
    #[inline]
    fn a(&self) -> &portable_atomic::AtomicU128 {
        // SAFETY: see int_handle; the cell is 16-byte aligned.
        unsafe { portable_atomic::AtomicU128::from_ptr(self.cell.ptr.as_ptr() as *mut u128) }
    }
    pub fn load(&self, o: Ordering) -> Result<u128> {
        Ok(self.a().load(o.for_load()?))
    }
    pub fn store(&self, v: u128, o: Ordering) -> Result<()> {
        self.a().store(v, o.for_store()?);
        Ok(())
    }
    pub fn swap(&self, v: u128, o: Ordering) -> u128 {
        self.a().swap(v, o.std())
    }
    pub fn compare_exchange(
        &self,
        current: u128,
        new: u128,
        success: Ordering,
        failure: Ordering,
    ) -> Result<std::result::Result<u128, u128>> {
        let (s, f) = Ordering::for_cas(success, failure)?;
        Ok(self.a().compare_exchange(current, new, s, f))
    }
    pub fn compare_exchange_weak(
        &self,
        current: u128,
        new: u128,
        success: Ordering,
        failure: Ordering,
    ) -> Result<std::result::Result<u128, u128>> {
        let (s, f) = Ordering::for_cas(success, failure)?;
        Ok(self.a().compare_exchange_weak(current, new, s, f))
    }
}

impl fmt::Debug for AtomicU128 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "AtomicU128({})", self.a().load(sa::Ordering::SeqCst))
    }
}

// ---------------------------------------------------------------------------
// Type-erased handle
// ---------------------------------------------------------------------------

/// A handle of any supported type (used by the daemon and dynamic callers).
#[derive(Clone, Debug)]
pub enum AnyAtomic {
    I64(AtomicI64),
    U64(AtomicU64),
    Bool(AtomicBool),
    F64(AtomicF64),
    U128(AtomicU128),
}

impl AnyAtomic {
    pub(crate) fn from_slot(ty: ValueType, ptr: NonNull<u8>, owner: Owner) -> Self {
        match ty {
            ValueType::I64 => Self::I64(AtomicI64::from_cell(ptr, owner)),
            ValueType::U64 => Self::U64(AtomicU64::from_cell(ptr, owner)),
            ValueType::Bool => Self::Bool(AtomicBool::from_cell(ptr, owner)),
            ValueType::F64 => Self::F64(AtomicF64::from_cell(ptr, owner)),
            ValueType::U128 => Self::U128(AtomicU128::from_cell(ptr, owner)),
        }
    }

    pub fn value_type(&self) -> ValueType {
        match self {
            Self::I64(_) => ValueType::I64,
            Self::U64(_) => ValueType::U64,
            Self::Bool(_) => ValueType::Bool,
            Self::F64(_) => ValueType::F64,
            Self::U128(_) => ValueType::U128,
        }
    }

    pub fn load(&self, o: Ordering) -> Result<Value> {
        Ok(match self {
            Self::I64(h) => Value::I64(h.load(o)?),
            Self::U64(h) => Value::U64(h.load(o)?),
            Self::Bool(h) => Value::Bool(h.load(o)?),
            Self::F64(h) => Value::F64(h.load(o)?),
            Self::U128(h) => Value::U128(h.load(o)?),
        })
    }
}
