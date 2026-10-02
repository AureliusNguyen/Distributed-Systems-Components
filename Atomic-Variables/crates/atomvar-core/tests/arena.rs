//! In-process registry behavior: shared-handle concurrency, collisions, full
//! arenas, type and layout mismatches.

mod common;

use atomvar_core::*;
use common::{colliding_names, TempArena};
use std::collections::HashSet;
use std::sync::{Arc, Barrier};

const SC: Ordering = Ordering::SeqCst;

fn assert_no_duplicates(arena: &Arena) -> Vec<VarInfo> {
    let vars = arena.list().unwrap();
    let names: HashSet<_> = vars.iter().map(|v| v.name.clone()).collect();
    let slots: HashSet<_> = vars.iter().map(|v| v.slot).collect();
    assert_eq!(names.len(), vars.len(), "duplicate names: {vars:?}");
    assert_eq!(slots.len(), vars.len(), "overlapping slots: {vars:?}");
    vars
}

#[test]
fn concurrent_creation_through_one_shared_handle() {
    // 32 threads share ONE Arena (one fd, one open file description), so flock
    // alone would not exclude them; the process mutex must.
    let t = TempArena::new("shared-handle");
    let cap = 64;
    let arena = Arena::open(&t.0, Some(cap)).unwrap();
    let mut names: Vec<String> = colliding_names(cap, 8);
    names.extend(["same", "other", "x", "y"].map(String::from));
    let names = Arc::new(names);
    let barrier = Arc::new(Barrier::new(32));

    let threads: Vec<_> = (0..32)
        .map(|_| {
            let arena = arena.clone();
            let names = names.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                for n in names.iter() {
                    arena.i64(n, 0).unwrap().fetch_add(1, SC);
                }
            })
        })
        .collect();
    threads.into_iter().for_each(|t| t.join().unwrap());

    let vars = assert_no_duplicates(&arena);
    assert_eq!(vars.len(), names.len());
    for n in names.iter() {
        assert_eq!(arena.open_i64(n).unwrap().load(SC).unwrap(), 32, "{n}");
    }
}

#[test]
fn hash_collisions_resolve_to_distinct_slots() {
    let t = TempArena::new("collide");
    let cap = 32;
    let arena = Arena::open(&t.0, Some(cap)).unwrap();
    let names = colliding_names(cap, 10);
    for (i, n) in names.iter().enumerate() {
        arena.u64(n, i as u64).unwrap();
    }
    for (i, n) in names.iter().enumerate() {
        assert_eq!(arena.open_u64(n).unwrap().load(SC).unwrap(), i as u64);
        assert_eq!(arena.lookup(n).unwrap().unwrap().value_type, ValueType::U64);
    }
    assert_eq!(assert_no_duplicates(&arena).len(), 10);
    assert!(arena.lookup("collide-not-there").unwrap().is_none());
}

#[test]
fn full_arena_is_reported_and_existing_vars_still_work() {
    let t = TempArena::new("full");
    let arena = Arena::open(&t.0, Some(4)).unwrap();
    for i in 0..4 {
        arena.i64(&format!("v{i}"), i).unwrap();
    }
    match arena.i64("v4", 0) {
        Err(Error::ArenaFull { capacity: 4, .. }) => {}
        other => panic!("expected ArenaFull, got {other:?}"),
    }
    // open-existing on a full table still works
    for i in 0..4 {
        let v = arena.i64(&format!("v{i}"), 100).unwrap();
        assert_eq!(v.increment_and_get(SC), i + 1);
    }
    assert!(matches!(arena.open_i64("missing"), Err(Error::NotFound { .. })));
}

#[test]
fn type_mismatch_is_an_error() {
    let t = TempArena::new("types");
    let arena = Arena::open(&t.0, Some(8)).unwrap();
    arena.i64("x", 1).unwrap();
    match arena.u64("x", 1) {
        Err(Error::TypeMismatch { existing: ValueType::I64, requested: ValueType::U64, .. }) => {}
        other => panic!("{other:?}"),
    }
    assert!(matches!(arena.open_f64("x"), Err(Error::TypeMismatch { .. })));
}

#[test]
fn capacity_mismatch_on_reopen_is_an_error() {
    let t = TempArena::new("layout");
    let a = Arena::open(&t.0, Some(8)).unwrap();
    assert!(matches!(Arena::open(&t.0, Some(16)), Err(Error::LayoutMismatch(_))));
    // None adopts the existing capacity
    assert_eq!(Arena::open(&t.0, None).unwrap().capacity(), 8);
    drop(a);
}

#[test]
fn invalid_names_are_rejected() {
    assert!(matches!(Arena::open("", None), Err(Error::InvalidName(_))));
    assert!(matches!(Arena::open("../etc", None), Err(Error::InvalidName(_))));
    assert!(matches!(Arena::open(".hidden", None), Err(Error::InvalidName(_))));
    assert!(matches!(Arena::open("a/b", None), Err(Error::InvalidName(_))));
    let t = TempArena::new("names");
    let arena = Arena::open(&t.0, Some(8)).unwrap();
    assert!(matches!(arena.i64("", 0), Err(Error::InvalidName(_))));
    assert!(matches!(arena.i64(&"x".repeat(65), 0), Err(Error::InvalidName(_))));
    assert!(arena.i64(&"x".repeat(64), 0).is_ok());
    assert!(arena.i64("unicode-\u{00e9}", 0).is_ok());
    assert!(matches!(Arena::open("x", Some(0)), Err(Error::InvalidArgument(_))));
}

#[test]
fn list_reports_published_vars() {
    let t = TempArena::new("list");
    let arena = Arena::open(&t.0, Some(8)).unwrap();
    assert!(arena.is_empty().unwrap());
    arena.i64("a", 0).unwrap();
    arena.bool("b", false).unwrap();
    arena.u128("c", 0).unwrap();
    let mut got: Vec<_> = arena
        .list()
        .unwrap()
        .into_iter()
        .map(|v| (v.name, v.value_type))
        .collect();
    got.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(
        got,
        vec![
            ("a".into(), ValueType::I64),
            ("b".into(), ValueType::Bool),
            ("c".into(), ValueType::U128)
        ]
    );
}
