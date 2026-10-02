//! Idempotency / failure-contract tests at the service layer.

use atomvar_core::{Arena, Ordering, Value, ValueType};
use atomvard::error::ErrorCode;
use atomvard::idem::{self, IdemTable, Reservation};
use atomvard::service::{FetchOp, OpKind, OpRequest, OpResult, Operand, Service};
use std::sync::atomic::Ordering as O;
use std::sync::Arc;
use std::time::Duration;

struct TempArena(String);
impl TempArena {
    fn new(tag: &str) -> Self {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        TempArena(format!("test-d-{tag}-{}-{}", std::process::id(), N.fetch_add(1, O::Relaxed)))
    }
}
impl Drop for TempArena {
    fn drop(&mut self) {
        let _ = atomvar_core::destroy_arena(&self.0);
    }
}

fn svc() -> Arc<Service> {
    Service::new(1000, Duration::from_secs(600))
}

fn add(arena: &str, delta: i64, id: Option<&str>) -> OpRequest {
    OpRequest {
        arena: arena.into(),
        name: "c".into(),
        value_type: None,
        kind: OpKind::Fetch {
            op: FetchOp::Add,
            operand: Operand::Typed(Value::I64(delta)),
            ordering: Ordering::SeqCst,
        },
        request_id: id.map(str::to_string),
    }
}

fn counter(arena: &str) -> atomvar_core::AtomicI64 {
    Arena::open(arena, None).unwrap().i64("c", 0).unwrap()
}

fn setup(tag: &str) -> (TempArena, Arc<Service>, atomvar_core::AtomicI64) {
    let t = TempArena::new(tag);
    let c = Arena::open(&t.0, Some(16)).unwrap().i64("c", 0).unwrap();
    (t, svc(), c)
}

fn val(c: &atomvar_core::AtomicI64) -> i64 {
    c.load(Ordering::SeqCst).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lost_response_retry_with_same_id_does_not_double_apply() {
    let (t, s, c) = setup("lost");
    let first = s.execute(add(&t.0, 5, Some("r1"))).await.unwrap();
    // response "lost"; client retries with the same id
    let retry = s.execute(add(&t.0, 5, Some("r1"))).await.unwrap();
    assert_eq!(first, retry);
    assert_eq!(val(&c), 5);
    // without an id, a retry applies twice (documented behavior)
    s.execute(add(&t.0, 5, None)).await.unwrap();
    s.execute(add(&t.0, 5, None)).await.unwrap();
    assert_eq!(val(&c), 15);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn simultaneous_duplicates_apply_once_and_share_result() {
    let (t, s, c) = setup("dups");
    s.hooks.delay_ms.store(300, O::SeqCst);
    let before = s.hooks.executions.load(O::SeqCst);
    let tasks: Vec<_> = (0..50)
        .map(|_| {
            let (s, a) = (s.clone(), t.0.clone());
            tokio::spawn(async move { s.execute(add(&a, 1, Some("same"))).await })
        })
        .collect();
    let mut results = Vec::new();
    for task in tasks {
        results.push(task.await.unwrap().unwrap());
    }
    assert!(results.iter().all(|r| *r == results[0]), "all 50 got the identical result");
    assert_eq!(val(&c), 1);
    assert_eq!(s.hooks.executions.load(O::SeqCst) - before, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn conflicting_payload_is_rejected_and_not_executed() {
    let (t, s, c) = setup("conflict");
    s.hooks.delay_ms.store(300, O::SeqCst);
    let first = {
        let (s, a) = (s.clone(), t.0.clone());
        tokio::spawn(async move { s.execute(add(&a, 1, Some("k"))).await })
    };
    tokio::time::sleep(Duration::from_millis(50)).await; // first has reserved
    let conflicting: Vec<_> = (0..10)
        .map(|_| {
            let (s, a) = (s.clone(), t.0.clone());
            tokio::spawn(async move { s.execute(add(&a, 2, Some("k"))).await })
        })
        .collect();
    for task in conflicting {
        let e = task.await.unwrap().unwrap_err();
        assert_eq!(e.code, ErrorCode::IdempotencyConflict);
    }
    first.await.unwrap().unwrap();
    assert_eq!(val(&c), 1, "only the first reserver's op ran");
    // also after completion
    let e = s.execute(add(&t.0, 2, Some("k"))).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::IdempotencyConflict);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn retry_after_ttl_expiry_reapplies() {
    let (t, s, c) = setup("ttl");
    s.execute(add(&t.0, 1, Some("x"))).await.unwrap();
    s.idem.expire_all_for_test();
    s.execute(add(&t.0, 1, Some("x"))).await.unwrap();
    assert_eq!(val(&c), 2, "documented limit: expired entries do not dedup");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn client_disconnect_does_not_cancel_execution() {
    let (t, s, c) = setup("cancel");
    s.hooks.delay_ms.store(300, O::SeqCst);
    let before = s.hooks.executions.load(O::SeqCst);
    let handler = {
        let (s, a) = (s.clone(), t.0.clone());
        tokio::spawn(async move { s.execute(add(&a, 7, Some("dc"))).await })
    };
    tokio::time::sleep(Duration::from_millis(50)).await;
    let waiter = {
        let (s, a) = (s.clone(), t.0.clone());
        tokio::spawn(async move { s.execute(add(&a, 7, Some("dc"))).await })
    };
    tokio::time::sleep(Duration::from_millis(20)).await;
    handler.abort(); // the client went away: its handler future is dropped
    assert!(handler.await.unwrap_err().is_cancelled());

    let waited = waiter.await.unwrap().unwrap();
    assert_eq!(val(&c), 7, "op still applied exactly once");
    let retry = s.execute(add(&t.0, 7, Some("dc"))).await.unwrap();
    assert_eq!(retry, waited);
    assert_eq!(val(&c), 7);
    assert_eq!(s.hooks.executions.load(O::SeqCst) - before, 1);
}

#[test]
fn task_dropped_before_first_poll_wakes_waiters_with_outcome_unknown() {
    let table = IdemTable::new(10, Duration::from_secs(600));
    let guard = match table.reserve("p", 1).unwrap() {
        Reservation::Execute(g) => g,
        _ => panic!("expected to reserve"),
    };
    let waiter_rx = match table.reserve("p", 1).unwrap() {
        Reservation::Wait(rx) => rx,
        _ => panic!("expected to wait"),
    };

    // Spawn the executing task on a runtime that is never driven, then shut the
    // runtime down: the task is dropped without ever being polled.
    let rt1 = tokio::runtime::Builder::new_current_thread().build().unwrap();
    rt1.spawn(async move {
        let _guard = guard;
        unreachable!("never polled");
    });
    drop(rt1);

    let rt2 = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
    let out = rt2.block_on(async {
        tokio::time::timeout(Duration::from_secs(5), idem::wait(waiter_rx))
            .await
            .expect("waiter must not hang")
    });
    assert_eq!(out.unwrap_err().code, ErrorCode::OutcomeUnknown);
    // later retries see the recorded outcome, never a re-execution
    match table.reserve("p", 1).unwrap() {
        Reservation::Done(Err(e)) => assert_eq!(e.code, ErrorCode::OutcomeUnknown),
        _ => panic!("expected recorded OutcomeUnknown"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn executor_panic_yields_outcome_unknown_and_no_reexecution() {
    let (t, s, c) = setup("panic");
    s.hooks.delay_ms.store(200, O::SeqCst);
    s.hooks.panic_after_apply.store(true, O::SeqCst);
    let before = s.hooks.executions.load(O::SeqCst);
    let a = {
        let (s, ar) = (s.clone(), t.0.clone());
        tokio::spawn(async move { s.execute(add(&ar, 3, Some("boom"))).await })
    };
    tokio::time::sleep(Duration::from_millis(50)).await;
    let b = {
        let (s, ar) = (s.clone(), t.0.clone());
        tokio::spawn(async move { s.execute(add(&ar, 3, Some("boom"))).await })
    };
    for task in [a, b] {
        let r = tokio::time::timeout(Duration::from_secs(5), task).await.expect("no indefinite wait");
        assert_eq!(r.unwrap().unwrap_err().code, ErrorCode::OutcomeUnknown);
    }
    let retry = s.execute(add(&t.0, 3, Some("boom"))).await.unwrap_err();
    assert_eq!(retry.code, ErrorCode::OutcomeUnknown);
    assert_eq!(s.hooks.executions.load(O::SeqCst) - before, 1, "never re-executed");
    assert_eq!(val(&c), 3, "the op was applied once before the panic");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn full_table_of_in_progress_rejects_new_reservations() {
    let t = TempArena::new("full");
    Arena::open(&t.0, Some(16)).unwrap().i64("c", 0).unwrap();
    let s = Service::new(2, Duration::from_secs(600));
    s.hooks.delay_ms.store(400, O::SeqCst);
    let busy: Vec<_> = ["a", "b"]
        .into_iter()
        .map(|id| {
            let (s, ar) = (s.clone(), t.0.clone());
            tokio::spawn(async move { s.execute(add(&ar, 1, Some(id))).await })
        })
        .collect();
    tokio::time::sleep(Duration::from_millis(50)).await;
    let e = s.execute(add(&t.0, 100, Some("c"))).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::ResourceExhausted);
    for b in busy {
        b.await.unwrap().unwrap();
    }
    assert_eq!(val(&counter(&t.0)), 2, "the rejected op never executed");
    // once entries complete they can be evicted to admit new ids
    s.hooks.delay_ms.store(0, O::SeqCst);
    s.execute(add(&t.0, 1, Some("d"))).await.unwrap();
    assert_eq!(val(&counter(&t.0)), 3);
}

fn cas_req(arena: &str, name: &str, e: Value, d: Value) -> OpRequest {
    OpRequest {
        arena: arena.into(),
        name: name.into(),
        value_type: None,
        kind: OpKind::CompareExchange {
            expected: Operand::Typed(e),
            desired: Operand::Typed(d),
            success: Ordering::SeqCst,
            failure: None,
        },
        request_id: None,
    }
}

fn exchanged(r: &OpResult) -> bool {
    matches!(r, OpResult::Cas { exchanged: true, .. })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn aba_plain_cas_retry_is_not_safe_but_documented_patterns_are() {
    let t = TempArena::new("aba");
    let arena = Arena::open(&t.0, Some(16)).unwrap();
    let s = svc();
    const A: u64 = 10;
    const B: u64 = 20;
    const X: u64 = 99;

    // Plain CAS: A -> X, others move it X -> B -> A, retry succeeds AGAIN.
    let v = arena.u64("plain", A).unwrap();
    assert!(exchanged(&s.execute(cas_req(&t.0, "plain", Value::U64(A), Value::U64(X))).await.unwrap()));
    v.store(B, Ordering::SeqCst).unwrap();
    v.store(A, Ordering::SeqCst).unwrap();
    assert!(
        exchanged(&s.execute(cas_req(&t.0, "plain", Value::U64(A), Value::U64(X))).await.unwrap()),
        "ABA: the retried CAS was applied twice"
    );

    // (a) Irreversible claim 0 -> token: a retry fails and shows our own token.
    arena.u64("claim", 0).unwrap();
    let token = 0xC0FFEE;
    assert!(exchanged(&s.execute(cas_req(&t.0, "claim", Value::U64(0), Value::U64(token))).await.unwrap()));
    match s.execute(cas_req(&t.0, "claim", Value::U64(0), Value::U64(token))).await.unwrap() {
        OpResult::Cas { exchanged: false, previous } => assert_eq!(previous, Value::U64(token)),
        other => panic!("{other:?}"),
    }

    // (b) Versioned u128 (version << 64 | value): values never repeat.
    let pack = |ver: u64, val: u64| ((ver as u128) << 64) | val as u128;
    let w = arena.u128("versioned", pack(1, A)).unwrap();
    assert!(exchanged(&s.execute(cas_req(&t.0, "versioned", Value::U128(pack(1, A)), Value::U128(pack(2, X)))).await.unwrap()));
    w.store(pack(3, B), Ordering::SeqCst).unwrap();
    w.store(pack(4, A), Ordering::SeqCst).unwrap(); // value A again, new version
    assert!(
        !exchanged(&s.execute(cas_req(&t.0, "versioned", Value::U128(pack(1, A)), Value::U128(pack(2, X)))).await.unwrap()),
        "stale retry rejected"
    );
    assert_eq!(arena.lookup("versioned").unwrap().unwrap().value_type, ValueType::U128);
}
