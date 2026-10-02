//! Value-operation semantics, on heap and arena backings.

mod common;

use atomvar_core::*;
use common::TempArena;
use std::sync::Arc;

const SC: Ordering = Ordering::SeqCst;

#[test]
fn invalid_orderings_are_rejected() {
    let a = AtomicI64::new(0);
    assert!(matches!(a.load(Ordering::Release), Err(Error::InvalidOrdering(_))));
    assert!(matches!(a.load(Ordering::AcqRel), Err(Error::InvalidOrdering(_))));
    assert!(matches!(a.store(1, Ordering::Acquire), Err(Error::InvalidOrdering(_))));
    assert!(matches!(a.store(1, Ordering::AcqRel), Err(Error::InvalidOrdering(_))));
    // failure may not be release/acq_rel
    assert!(a.compare_exchange(0, 1, SC, Ordering::Release).is_err());
    assert!(a.compare_exchange(0, 1, SC, Ordering::AcqRel).is_err());
    // failure may not be stronger than success's load half
    assert!(a.compare_exchange(0, 1, Ordering::Relaxed, Ordering::Acquire).is_err());
    assert!(a.compare_exchange(0, 1, Ordering::Release, Ordering::Acquire).is_err());
    assert!(a.compare_exchange(0, 1, Ordering::Acquire, SC).is_err());
    // valid pairs
    assert!(a.compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire).is_ok());
    assert!(a.compare_exchange(1, 2, Ordering::Release, Ordering::Relaxed).is_ok());
    assert!(a.compare_exchange(2, 3, SC, SC).is_ok());
    for o in [Ordering::Relaxed, Ordering::Acquire, SC] {
        assert!(a.load(o).is_ok());
    }
    for o in [Ordering::Relaxed, Ordering::Release, SC] {
        assert!(a.store(0, o).is_ok());
    }
    assert!(Ordering::from_u8(5).is_err());
    // default_failure always yields a valid pair
    for o in [Ordering::Relaxed, Ordering::Acquire, Ordering::Release, Ordering::AcqRel, SC] {
        assert!(AtomicI64::new(0).compare_exchange(0, 1, o, o.default_failure()).is_ok());
    }
}

#[test]
fn integer_overflow_wraps() {
    let a = AtomicI64::new(i64::MAX);
    assert_eq!(a.fetch_add(1, SC), i64::MAX);
    assert_eq!(a.load(SC).unwrap(), i64::MIN);
    assert_eq!(a.sub_and_get(1, SC), i64::MAX);

    let u = AtomicU64::new(u64::MAX);
    assert_eq!(u.add_and_get(1, SC), 0);
    assert_eq!(u.decrement_and_get(SC), u64::MAX);
    assert_eq!(u.add_and_get(2, SC), 1);
}

#[test]
fn add_and_get_uses_returned_old_value() {
    let a = AtomicI64::new(10);
    assert_eq!(a.add_and_get(5, SC), 15);
    assert_eq!(a.increment_and_get(SC), 16);
    assert_eq!(a.decrement_and_get(SC), 15);
    assert_eq!(a.add_and_get(-20, SC), -5);

    // Under contention every returned value is unique (each is derived from its
    // own RMW); a separate load could return duplicates.
    let a = Arc::new(AtomicI64::new(0));
    let per = 20_000;
    let threads: Vec<_> = (0..4)
        .map(|_| {
            let a = a.clone();
            std::thread::spawn(move || (0..per).map(|_| a.increment_and_get(SC)).collect::<Vec<_>>())
        })
        .collect();
    let mut all: Vec<i64> = threads.into_iter().flat_map(|t| t.join().unwrap()).collect();
    all.sort_unstable();
    assert_eq!(all, (1..=4 * per).collect::<Vec<i64>>());
}

#[test]
fn integer_rmw_ops() {
    let a = AtomicU64::new(0b1100);
    assert_eq!(a.fetch_and(0b1010, SC), 0b1100);
    assert_eq!(a.fetch_or(0b0001, SC), 0b1000);
    assert_eq!(a.fetch_xor(0b1111, SC), 0b1001);
    assert_eq!(a.load(SC).unwrap(), 0b0110);
    assert_eq!(a.fetch_max(100, SC), 6);
    assert_eq!(a.fetch_min(3, SC), 100);
    assert_eq!(a.swap(9, SC), 3);
    assert_eq!(a.compare_exchange(9, 10, SC, SC).unwrap(), Ok(9));
    assert_eq!(a.compare_exchange(9, 11, SC, SC).unwrap(), Err(10));

    let s = AtomicI64::new(-5);
    assert_eq!(s.fetch_max(-10, SC), -5);
    assert_eq!(s.fetch_min(-10, SC), -5);
    assert_eq!(s.load(SC).unwrap(), -10);
}

#[test]
fn bool_ops() {
    let b = AtomicBool::new(false);
    assert!(!b.fetch_or(true, SC));
    assert!(b.fetch_and(true, SC));
    assert!(b.fetch_xor(true, SC));
    assert!(!b.load(SC).unwrap());
    assert!(!b.fetch_nand(true, SC));
    assert!(b.load(SC).unwrap());
    assert_eq!(b.compare_exchange(true, false, SC, SC).unwrap(), Ok(true));
    assert_eq!(b.compare_exchange(true, false, SC, SC).unwrap(), Err(false));
}

#[test]
fn f64_cas_is_bitwise() {
    let f = AtomicF64::new(0.0);
    // -0.0 == 0.0 numerically, but CAS compares bits.
    assert!(f.compare_exchange(-0.0, 1.0, SC, SC).unwrap().is_err());
    assert!(f.compare_exchange(0.0, -0.0, SC, SC).unwrap().is_ok());
    assert_eq!(f.load_bits(SC).unwrap(), (-0.0f64).to_bits());

    // A NaN matches only the identical bit pattern.
    let nan_a = f64::from_bits(0x7ff8_0000_0000_0001);
    let nan_b = f64::from_bits(0x7ff8_0000_0000_0002);
    f.store(nan_a, SC).unwrap();
    assert!(f.compare_exchange(nan_b, 1.0, SC, SC).unwrap().is_err());
    assert!(f.compare_exchange(nan_a, 1.0, SC, SC).unwrap().is_ok());
    assert_eq!(f.load(SC).unwrap(), 1.0);

    // Exact bit round trip.
    f.store_bits(0xfff0_dead_beef_0001, SC).unwrap();
    assert_eq!(f.load_bits(SC).unwrap(), 0xfff0_dead_beef_0001);
}

#[test]
fn f64_fetch_add_ieee_and_contended() {
    let f = AtomicF64::new(1.5);
    assert_eq!(f.fetch_add(2.0, SC), 1.5);
    assert_eq!(f.fetch_sub(0.5, SC), 3.5);
    assert_eq!(f.add_and_get(1.0, SC), 4.0);
    f.store(f64::INFINITY, SC).unwrap();
    f.fetch_add(f64::NEG_INFINITY, SC);
    assert!(f.load(SC).unwrap().is_nan());

    // Integers below 2^53 are exact in f64, so the contended total is exact.
    let f = Arc::new(AtomicF64::new(0.0));
    let ts: Vec<_> = (0..4)
        .map(|_| {
            let f = f.clone();
            std::thread::spawn(move || {
                for _ in 0..20_000 {
                    f.fetch_add(1.0, SC);
                }
            })
        })
        .collect();
    ts.into_iter().for_each(|t| t.join().unwrap());
    assert_eq!(f.load(SC).unwrap(), 80_000.0);
}

#[test]
fn u128_ops() {
    let a = AtomicU128::new(u128::MAX);
    assert_eq!(a.load(SC).unwrap(), u128::MAX);
    assert_eq!(a.swap(1 << 100, SC), u128::MAX);
    let tagged = (7u128 << 64) | 42;
    assert_eq!(a.compare_exchange(1 << 100, tagged, SC, SC).unwrap(), Ok(1 << 100));
    assert_eq!(a.compare_exchange(1 << 100, 0, SC, SC).unwrap(), Err(tagged));
    assert!(cpu_supported());
}

#[test]
fn arena_backed_handles_share_storage() {
    let t = TempArena::new("ops");
    let arena = Arena::open(&t.0, Some(16)).unwrap();
    let a = arena.i64("n", 5).unwrap();
    let b = arena.open_i64("n").unwrap();
    assert_eq!(b.add_and_get(1, SC), 6);
    assert_eq!(a.load(SC).unwrap(), 6);
    // init is ignored when the variable already exists
    let c = arena.i64("n", 999).unwrap();
    assert_eq!(c.load(SC).unwrap(), 6);

    let f = arena.f64("f", -0.0).unwrap();
    assert_eq!(f.load_bits(SC).unwrap(), (-0.0f64).to_bits());
    let u = arena.u128("u", u128::MAX).unwrap();
    assert_eq!(u.load(SC).unwrap(), u128::MAX);
    let b = arena.bool("b", true).unwrap();
    assert!(b.load(SC).unwrap());
    let x = arena.u64("x", u64::MAX).unwrap();
    assert_eq!(x.load(SC).unwrap(), u64::MAX);

    // handles keep the mapping alive after the Arena handle is dropped
    drop(arena);
    assert_eq!(a.increment_and_get(SC), 7);
}
