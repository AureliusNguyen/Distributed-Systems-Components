//! Loom model checks. Run with:
//!   RUSTFLAGS="--cfg loom -C target-feature=+cmpxchg16b" \
//!     cargo test --release -p atomvar-core --test loom
//!
//! Loom only explores operations on its own types, so these tests drive the
//! exact generic algorithms from `atomvar_core::algo` over loom-backed storage.
//! mmap/flock are not modeled; tests/process.rs covers them with real processes.
#![cfg(loom)]

use atomvar_core::algo::{self, Bits64, FetchAddI64, Inserted, Probe, SlotTable, READY};
use loom::cell::UnsafeCell;
use loom::sync::atomic::{AtomicI64, AtomicU32, AtomicU64};
use loom::sync::{Arc, Mutex};
use loom::thread;
use std::sync::atomic::Ordering as O;

struct LoomBits(AtomicU64);
impl Bits64 for LoomBits {
    fn load_bits(&self, o: O) -> u64 {
        self.0.load(o)
    }
    fn cas_weak_bits(&self, c: u64, n: u64, s: O, f: O) -> Result<u64, u64> {
        self.0.compare_exchange_weak(c, n, s, f)
    }
}

struct LoomI64(AtomicI64);
impl FetchAddI64 for LoomI64 {
    fn fetch_add_i64(&self, v: i64, o: O) -> i64 {
        self.0.fetch_add(v, o)
    }
}

#[test]
fn loom_f64_fetch_add_no_lost_updates() {
    loom::model(|| {
        let a = Arc::new(LoomBits(AtomicU64::new(0f64.to_bits())));
        let hs: Vec<_> = (0..2)
            .map(|_| {
                let a = a.clone();
                thread::spawn(move || algo::f64_fetch_update(&*a, O::SeqCst, |x| x + 1.0))
            })
            .collect();
        let mut olds: Vec<f64> = hs.into_iter().map(|h| h.join().unwrap()).collect();
        olds.sort_by(f64::total_cmp);
        assert_eq!(olds, vec![0.0, 1.0]);
        assert_eq!(f64::from_bits(a.0.load(O::SeqCst)), 2.0);
    });
}

#[test]
fn loom_add_and_get_results_are_distinct() {
    loom::model(|| {
        let a = Arc::new(LoomI64(AtomicI64::new(i64::MAX - 1)));
        let hs: Vec<_> = (0..2)
            .map(|_| {
                let a = a.clone();
                thread::spawn(move || algo::add_and_get_i64(&*a, 1, O::Relaxed))
            })
            .collect();
        let mut r: Vec<i64> = hs.into_iter().map(|h| h.join().unwrap()).collect();
        r.sort();
        // wraps: MAX-1 -> MAX -> MIN
        assert_eq!(r, vec![i64::MIN, i64::MAX]);
    });
}

/// Heap-backed slot table: states are loom atomics, payload lives in loom
/// UnsafeCells so any unsynchronized payload access is reported as a race.
struct Table {
    states: Vec<AtomicU32>,
    payload: Vec<UnsafeCell<(u8, Vec<u8>, u128)>>,
}

impl Table {
    fn new(cap: usize) -> Self {
        Table {
            states: (0..cap).map(|_| AtomicU32::new(0)).collect(),
            payload: (0..cap).map(|_| UnsafeCell::new((0, Vec::new(), 0))).collect(),
        }
    }
}

impl SlotTable for Table {
    fn capacity(&self) -> usize {
        self.states.len()
    }
    fn load_state(&self, i: usize, o: O) -> u32 {
        self.states[i].load(o)
    }
    fn store_state(&self, i: usize, v: u32, o: O) {
        self.states[i].store(v, o)
    }
    unsafe fn name_eq(&self, i: usize, name: &[u8]) -> bool {
        self.payload[i].with(|p| (*p).1 == name)
    }
    unsafe fn type_tag(&self, i: usize) -> u8 {
        self.payload[i].with(|p| (*p).0)
    }
    unsafe fn write_payload(&self, i: usize, tag: u8, name: &[u8], init: u128) {
        self.payload[i].with_mut(|p| *p = (tag, name.to_vec(), init));
    }
}

/// open-or-create exactly as Arena::resolve does it: lock-free probe, then
/// mutex (standing in for process mutex + flock) and insert_locked.
fn open_or_create(t: &Table, lock: &Mutex<()>, name: &[u8], tag: u8) -> (usize, u8) {
    if let Probe::Found { slot, type_tag } = algo::probe(t, name) {
        return (slot, type_tag);
    }
    let _g = lock.lock().unwrap();
    match unsafe { algo::insert_locked(t, name, tag, 0) } {
        Inserted::Existing { slot, type_tag } => (slot, type_tag),
        Inserted::Created { slot } => (slot, tag),
        Inserted::Full => panic!("full"),
    }
}

#[test]
fn loom_same_name_creation_yields_one_slot() {
    loom::model(|| {
        let t = Arc::new(Table::new(2));
        let lock = Arc::new(Mutex::new(()));
        let hs: Vec<_> = (0..2)
            .map(|_| {
                let (t, lock) = (t.clone(), lock.clone());
                thread::spawn(move || open_or_create(&t, &lock, b"x", 1))
            })
            .collect();
        let r: Vec<_> = hs.into_iter().map(|h| h.join().unwrap()).collect();
        assert_eq!(r[0], r[1]);
        let ready = (0..2).filter(|&i| t.states[i].load(O::SeqCst) == READY).count();
        assert_eq!(ready, 1);
    });
}

#[test]
fn loom_reader_sees_complete_payload_or_nothing() {
    // One creator, one lock-free reader: the reader either misses or sees the
    // full payload; loom flags a race if payload reads are not synchronized.
    loom::model(|| {
        let t = Arc::new(Table::new(2));
        let lock = Arc::new(Mutex::new(()));
        let writer = {
            let (t, lock) = (t.clone(), lock.clone());
            thread::spawn(move || open_or_create(&t, &lock, b"a", 3))
        };
        let reader = {
            let t = t.clone();
            thread::spawn(move || match algo::probe(&*t, b"a") {
                Probe::Found { slot, type_tag } => Some((slot, type_tag)),
                _ => None,
            })
        };
        let w = writer.join().unwrap();
        if let Some(r) = reader.join().unwrap() {
            assert_eq!(r, w);
        }
    });
}

/// Two distinct names with the same home slot in a 2-slot table, so the second
/// inserter must probe past the first.
fn colliding_pair(cap: u64) -> (Vec<u8>, Vec<u8>) {
    let home = |n: &[u8]| algo::fnv1a(n) % cap;
    let a = b"p".to_vec();
    let b = (0..)
        .map(|i| format!("q{i}").into_bytes())
        .find(|n| home(n) == home(&a))
        .unwrap();
    (a, b)
}

#[test]
fn loom_distinct_names_colliding() {
    let (a, b) = colliding_pair(2);
    assert_eq!(algo::fnv1a(&a) % 2, algo::fnv1a(&b) % 2, "precondition: same home slot");
    assert_ne!(a, b);
    loom::model(move || {
        let t = Arc::new(Table::new(2));
        let lock = Arc::new(Mutex::new(()));
        let hs: Vec<_> = [a.clone(), b.clone()]
            .into_iter()
            .map(|n| {
                let (t, lock) = (t.clone(), lock.clone());
                thread::spawn(move || open_or_create(&t, &lock, &n, 2).0)
            })
            .collect();
        let r: Vec<_> = hs.into_iter().map(|h| h.join().unwrap()).collect();
        assert_ne!(r[0], r[1]);
        // both are findable afterwards from their shared home slot
        assert!(matches!(algo::probe(&*t, &a), Probe::Found { .. }));
        assert!(matches!(algo::probe(&*t, &b), Probe::Found { .. }));
    });
}
