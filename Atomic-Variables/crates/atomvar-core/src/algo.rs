//! Generic algorithms written against small traits, so the exact same code runs
//! on shared memory and under the loom model checker (see tests/loom.rs).

use std::sync::atomic::Ordering as O;

/// Strongest valid failure ordering for a given success ordering. Also the
/// ordering used for the initial load of a CAS loop.
pub fn failure_for(o: O) -> O {
    match o {
        O::Relaxed | O::Release => O::Relaxed,
        O::Acquire | O::AcqRel => O::Acquire,
        _ => O::SeqCst,
    }
}

// ---------------------------------------------------------------------------
// f64 read-modify-write via a CAS loop on the u64 bit pattern
// ---------------------------------------------------------------------------

pub trait Bits64 {
    fn load_bits(&self, o: O) -> u64;
    fn cas_weak_bits(&self, current: u64, new: u64, success: O, failure: O)
        -> Result<u64, u64>;
}

impl Bits64 for std::sync::atomic::AtomicU64 {
    fn load_bits(&self, o: O) -> u64 {
        self.load(o)
    }
    fn cas_weak_bits(&self, c: u64, n: u64, s: O, f: O) -> Result<u64, u64> {
        self.compare_exchange_weak(c, n, s, f)
    }
}

/// Atomically replaces the value with `f(old)` and returns `old`. Lock-free, not
/// wait-free: retries while other writers win the race.
pub fn f64_fetch_update<A: Bits64 + ?Sized>(a: &A, o: O, f: impl Fn(f64) -> f64) -> f64 {
    let fail = failure_for(o);
    let mut cur = a.load_bits(fail);
    loop {
        let new = f(f64::from_bits(cur)).to_bits();
        match a.cas_weak_bits(cur, new, o, fail) {
            Ok(_) => return f64::from_bits(cur),
            Err(actual) => cur = actual,
        }
    }
}

// ---------------------------------------------------------------------------
// Derived results: always computed from the RMW's returned old value
// ---------------------------------------------------------------------------

pub trait FetchAddI64 {
    fn fetch_add_i64(&self, v: i64, o: O) -> i64;
}

impl FetchAddI64 for std::sync::atomic::AtomicI64 {
    fn fetch_add_i64(&self, v: i64, o: O) -> i64 {
        self.fetch_add(v, o)
    }
}

/// New value after the add, derived from the single atomic RMW (never a second
/// load). Wraps on overflow, like the hardware and Java.
pub fn add_and_get_i64<A: FetchAddI64 + ?Sized>(a: &A, delta: i64, o: O) -> i64 {
    a.fetch_add_i64(delta, o).wrapping_add(delta)
}

// ---------------------------------------------------------------------------
// Name registry: lock-free lookup, insertion serialized by the caller's locks
// ---------------------------------------------------------------------------

pub const EMPTY: u32 = 0;
pub const READY: u32 = 1;

/// FNV-1a 64-bit. Part of the on-disk contract (slot placement).
pub const fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut i = 0;
    while i < bytes.len() {
        h ^= bytes[i] as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
        i += 1;
    }
    h
}

/// Storage for the slot table. `state` is atomic; the payload (type tag, name,
/// initial value) is written only while the slot is unpublished and the caller
/// holds the insert locks, and read only after observing READY with Acquire.
pub trait SlotTable {
    fn capacity(&self) -> usize;
    fn load_state(&self, i: usize, o: O) -> u32;
    fn store_state(&self, i: usize, v: u32, o: O);
    /// # Safety
    /// Slot `i` was observed READY with an Acquire load, or the caller holds the
    /// insert locks.
    unsafe fn name_eq(&self, i: usize, name: &[u8]) -> bool;
    /// # Safety
    /// Same as `name_eq`.
    unsafe fn type_tag(&self, i: usize) -> u8;
    /// Overwrites only the payload fields; never touches `state`. The value must
    /// be initialized with an atomic store of the width implied by `type_tag`.
    ///
    /// # Safety
    /// The caller holds the insert locks and slot `i` is not READY.
    unsafe fn write_payload(&self, i: usize, type_tag: u8, name: &[u8], init: u128);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Probe {
    Found { slot: usize, type_tag: u8 },
    Vacant { slot: usize },
    Full,
}

/// Lock-free lookup. Linear probing from the name's home slot, stopping at the
/// first non-READY slot. Correct because slots are never freed: if a name is
/// present, it sits before the first EMPTY slot of its probe sequence.
pub fn probe<T: SlotTable + ?Sized>(t: &T, name: &[u8]) -> Probe {
    let cap = t.capacity();
    let start = (fnv1a(name) % cap as u64) as usize;
    for k in 0..cap {
        let i = (start + k) % cap;
        if t.load_state(i, O::Acquire) != READY {
            return Probe::Vacant { slot: i };
        }
        // SAFETY: READY observed with Acquire just above.
        if unsafe { t.name_eq(i, name) } {
            return Probe::Found {
                slot: i,
                type_tag: unsafe { t.type_tag(i) },
            };
        }
    }
    Probe::Full
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Inserted {
    Existing { slot: usize, type_tag: u8 },
    Created { slot: usize },
    Full,
}

/// Re-probes and, if the name is still missing, publishes it in the first
/// vacant slot. A vacant slot may hold debris from a crashed inserter; only its
/// payload is overwritten, and it is published with a Release store.
///
/// # Safety
/// The caller holds the insert locks (process-local mutex + cross-process lock).
pub unsafe fn insert_locked<T: SlotTable + ?Sized>(
    t: &T,
    name: &[u8],
    type_tag: u8,
    init: u128,
) -> Inserted {
    match probe(t, name) {
        Probe::Found { slot, type_tag } => Inserted::Existing { slot, type_tag },
        Probe::Full => Inserted::Full,
        Probe::Vacant { slot } => {
            t.write_payload(slot, type_tag, name, init);
            t.store_state(slot, READY, O::Release);
            Inserted::Created { slot }
        }
    }
}
