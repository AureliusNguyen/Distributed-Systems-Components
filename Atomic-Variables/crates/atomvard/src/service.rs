//! Transport-independent handler layer shared by gRPC, HTTP/JSON and MCP.
//!
//! Each value operation performs exactly one hardware atomic operation (or a
//! lock-free CAS loop for f64 add/sub) on the same shared-memory word that
//! native users see, so it is atomic with respect to them too.

use crate::error::{ApiError, ErrorCode};
use crate::idem::{self, IdemTable, Outcome, Reservation};
use crate::wire;
use atomvar_core::{AnyAtomic, Arena, Ordering, Value, ValueType, VarInfo};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering as O};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const MAX_REQUEST_ID_LEN: usize = 200;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FetchOp {
    Add,
    Sub,
    And,
    Or,
    Xor,
    Max,
    Min,
    Nand,
}

impl FetchOp {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.trim_start_matches("fetch_") {
            "add" => Self::Add,
            "sub" => Self::Sub,
            "and" => Self::And,
            "or" => Self::Or,
            "xor" => Self::Xor,
            "max" => Self::Max,
            "min" => Self::Min,
            "nand" => Self::Nand,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Add => "fetch_add",
            Self::Sub => "fetch_sub",
            Self::And => "fetch_and",
            Self::Or => "fetch_or",
            Self::Xor => "fetch_xor",
            Self::Max => "fetch_max",
            Self::Min => "fetch_min",
            Self::Nand => "fetch_nand",
        }
    }
}

/// A value as received: already typed (gRPC) or raw JSON (HTTP, MCP), decoded
/// once the variable's type is known.
#[derive(Clone, Debug)]
pub enum Operand {
    Typed(Value),
    Json(serde_json::Value),
}

impl Operand {
    fn resolve(&self, ty: ValueType, field: &str) -> Result<Value, ApiError> {
        match self {
            Operand::Typed(v) if v.value_type() == ty => Ok(*v),
            Operand::Typed(v) => Err(ApiError::new(
                ErrorCode::TypeMismatch,
                format!("field '{field}' is {} but the variable is {ty}", v.value_type()),
            )),
            Operand::Json(j) => wire::decode(j, ty, field),
        }
    }
}

#[derive(Clone, Debug)]
pub enum OpKind {
    /// Opens or creates. With `Operand::Json` the request must name the type.
    Create {
        init: Option<Operand>,
        capacity: Option<u32>,
    },
    Get {
        ordering: Ordering,
    },
    Set {
        value: Operand,
        ordering: Ordering,
    },
    Swap {
        value: Operand,
        ordering: Ordering,
    },
    CompareExchange {
        expected: Operand,
        desired: Operand,
        success: Ordering,
        failure: Option<Ordering>,
    },
    Fetch {
        op: FetchOp,
        operand: Operand,
        ordering: Ordering,
    },
}

#[derive(Clone, Debug)]
pub struct OpRequest {
    pub arena: String,
    pub name: String,
    /// Optional for everything except JSON creates; if given it must match.
    pub value_type: Option<ValueType>,
    pub kind: OpKind,
    pub request_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum OpResult {
    Value { value: Value },
    Stored,
    Swapped { previous: Value },
    Cas { exchanged: bool, previous: Value },
    Fetched { previous: Value, current: Value },
}

impl OpResult {
    /// JSON body shared by HTTP and MCP.
    pub fn to_json(&self) -> serde_json::Value {
        use serde_json::json;
        match self {
            OpResult::Value { value } => {
                json!({"type": value.value_type().as_str(), "value": wire::encode(value)})
            }
            OpResult::Stored => json!({"ok": true}),
            OpResult::Swapped { previous } => json!({"previous": wire::encode(previous)}),
            OpResult::Cas { exchanged, previous } => {
                json!({"exchanged": exchanged, "previous": wire::encode(previous)})
            }
            OpResult::Fetched { previous, current } => {
                json!({"previous": wire::encode(previous), "current": wire::encode(current)})
            }
        }
    }
}

pub fn vars_to_json(vars: &[VarInfo]) -> serde_json::Value {
    serde_json::json!({
        "variables": vars.iter().map(|v| serde_json::json!({
            "name": v.name, "type": v.value_type.as_str(), "slot": v.slot
        })).collect::<Vec<_>>()
    })
}

/// Fully decoded mutation/read, ready to execute.
#[derive(Clone, Debug, Hash)]
enum TypedOp {
    Get(Ordering),
    Set(Value, Ordering),
    Swap(Value, Ordering),
    Cas(Value, Value, Ordering, Ordering),
    Fetch(FetchOp, Value, Ordering),
}

struct Prepared {
    arena: String,
    name: String,
    value_type: ValueType,
    handle: AnyAtomic,
    op: TypedOp,
}

impl Prepared {
    fn fingerprint(&self) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (&self.arena, &self.name, self.value_type, &self.op).hash(&mut h);
        h.finish()
    }
}

/// Integer fetch op plus the resulting value, derived from the returned old
/// value (never a second load).
macro_rules! int_fetch {
    ($h:expr, $op:expr, $v:expr, $o:expr) => {{
        let (h, v, o) = ($h, $v, $o);
        match $op {
            FetchOp::Add => { let p = h.fetch_add(v, o); (p, p.wrapping_add(v)) }
            FetchOp::Sub => { let p = h.fetch_sub(v, o); (p, p.wrapping_sub(v)) }
            FetchOp::And => { let p = h.fetch_and(v, o); (p, p & v) }
            FetchOp::Or => { let p = h.fetch_or(v, o); (p, p | v) }
            FetchOp::Xor => { let p = h.fetch_xor(v, o); (p, p ^ v) }
            FetchOp::Max => { let p = h.fetch_max(v, o); (p, p.max(v)) }
            FetchOp::Min => { let p = h.fetch_min(v, o); (p, p.min(v)) }
            FetchOp::Nand => unreachable!("validated in prepare"),
        }
    }};
}

/// Test hooks (inert by default): execution delay, injected panic after the
/// atomic op is applied, and a counter of executed mutations.
#[derive(Default)]
pub struct TestHooks {
    pub delay_ms: AtomicU64,
    pub panic_after_apply: AtomicBool,
    pub executions: AtomicU64,
}

/// Default bound on arenas that may be inside blocking work at the same time.
pub const DEFAULT_MAX_BLOCKING_ARENAS: usize = 64;
/// Default bound on requests queued behind the one being served for an arena.
pub const DEFAULT_MAX_WAITERS_PER_ARENA: usize = 256;

/// Serializes one arena's potentially blocking work (bootstrap, name creation;
/// both are serialized by the arena's flock anyway). Waiters queue here
/// asynchronously. The gate's turn is owned by the BLOCKING WORKER, not by the
/// request, so a stuck arena holds at most one blocking thread even when its
/// clients disconnect and retry.
#[derive(Default)]
struct Gate {
    turn: Arc<tokio::sync::Mutex<()>>,
    waiters: std::sync::atomic::AtomicUsize,
}

type GateTable = Arc<Mutex<HashMap<String, Arc<Gate>>>>;

/// A reference to one arena's gate, held by each waiting request and by the
/// running worker. Every clone AND every release of a lease's reference happens
/// under the table mutex, so "only the table still references it" is an exact
/// test and the entry is removed when the last lease goes away: the table stays
/// bounded by in-flight operations.
struct Lease {
    table: GateTable,
    name: String,
    gate: Option<Arc<Gate>>,
}

impl Lease {
    fn acquire(table: &GateTable, name: &str) -> Lease {
        let gate = table
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(name.to_string())
            .or_default()
            .clone();
        Lease { table: table.clone(), name: name.to_string(), gate: Some(gate) }
    }

    fn gate(&self) -> &Gate {
        self.gate.as_deref().expect("live lease")
    }

    fn duplicate(&self) -> Lease {
        let _g = self.table.lock().unwrap_or_else(|e| e.into_inner());
        Lease { table: self.table.clone(), name: self.name.clone(), gate: self.gate.clone() }
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        let mut map = self.table.lock().unwrap_or_else(|e| e.into_inner());
        let Some(ours) = self.gate.take() else { return };
        let is_entry = map.get(&self.name).is_some_and(|g| Arc::ptr_eq(g, &ours));
        drop(ours); // release our reference while still holding the table lock
        if is_entry && map.get(&self.name).is_some_and(|g| Arc::strong_count(g) == 1) {
            map.remove(&self.name);
        }
    }
}

/// Counts a request as queued on a gate (borrowing, not owning, the gate).
struct Waiting<'a>(&'a Gate);
impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        self.0.waiters.fetch_sub(1, O::SeqCst);
    }
}

pub struct Service {
    arenas: Mutex<HashMap<String, Arena>>,
    gates: GateTable,
    pub idem: Arc<IdemTable>,
    pub hooks: Arc<TestHooks>,
    /// Bounds the arenas concurrently inside blocking work (= blocking threads).
    blocking: Arc<tokio::sync::Semaphore>,
    max_blocking_arenas: usize,
    max_waiters_per_arena: usize,
}

impl Service {
    pub fn new(idem_capacity: usize, idem_ttl: Duration) -> Arc<Self> {
        Self::with_limits(idem_capacity, idem_ttl, DEFAULT_MAX_BLOCKING_ARENAS, DEFAULT_MAX_WAITERS_PER_ARENA)
    }

    pub fn with_limits(
        idem_capacity: usize,
        idem_ttl: Duration,
        max_blocking_arenas: usize,
        max_waiters_per_arena: usize,
    ) -> Arc<Self> {
        let max_blocking_arenas = max_blocking_arenas.max(1);
        Arc::new(Service {
            arenas: Mutex::new(HashMap::new()),
            gates: Arc::new(Mutex::new(HashMap::new())),
            idem: IdemTable::new(idem_capacity, idem_ttl),
            hooks: Arc::new(TestHooks::default()),
            blocking: Arc::new(tokio::sync::Semaphore::new(max_blocking_arenas)),
            max_blocking_arenas,
            max_waiters_per_arena: max_waiters_per_arena.max(1),
        })
    }

    /// Diagnostics: gate-table entries (bounded by active blocking operations).
    pub fn gate_entries(&self) -> usize {
        self.gates.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    /// Diagnostics: blocking workers currently running (each holds one arena's gate).
    pub fn blocking_in_use(&self) -> usize {
        self.max_blocking_arenas - self.blocking.available_permits()
    }

    /// Runs a possibly-blocking step for `arena` (it may wait on the arena's
    /// flock) on Tokio's blocking pool, never on an async worker:
    /// - requests for one arena queue asynchronously behind its gate; the
    ///   worker owns the gate's turn until its work really ends, so a stuck
    ///   arena ties up at most one blocking thread even if clients disconnect;
    /// - more than `max_waiters_per_arena` queued requests, or more than
    ///   `max_blocking_arenas` arenas in blocking work, fail fast with
    ///   RESOURCE_EXHAUSTED. Admitted requests wait as long as the lock is held.
    async fn run_blocking<T: Send + 'static>(
        self: &Arc<Self>,
        arena: &str,
        f: impl FnOnce(&Service) -> Result<T, ApiError> + Send + 'static,
    ) -> Result<T, ApiError> {
        // Validate first: invalid names must never create gate entries.
        atomvar_core::validate_arena_name(arena)?;
        let lease = Lease::acquire(&self.gates, arena);
        // Counts requests QUEUED behind the one currently being served.
        if lease.gate().waiters.fetch_add(1, O::SeqCst) >= self.max_waiters_per_arena {
            lease.gate().waiters.fetch_sub(1, O::SeqCst);
            return Err(ApiError::new(
                ErrorCode::ResourceExhausted,
                format!("too many requests are waiting on arena '{arena}' (is its lock held by a stuck or forked process?)"),
            ));
        }
        let waiting = Waiting(lease.gate());
        let turn = lease.gate().turn.clone().lock_owned().await;
        drop(waiting);
        let permit = self.blocking.clone().try_acquire_owned().map_err(|_| {
            ApiError::new(ErrorCode::ResourceExhausted, "too many arenas are blocked on their locks")
        })?;
        let worker_lease = lease.duplicate();
        let svc = self.clone();
        // Everything that must outlive a cancelled request moves into the worker.
        let handle = tokio::task::spawn_blocking(move || {
            let out = f(&svc);
            drop(turn); // let the next queued request in
            drop(permit);
            drop(worker_lease); // may reclaim the gate entry
            out
        });
        drop(lease);
        handle
            .await
            .map_err(|e| ApiError::new(ErrorCode::Internal, format!("blocking task failed: {e}")))?
    }

    fn cached_arena(&self, name: &str) -> Option<Arena> {
        self.arenas.lock().unwrap_or_else(|e| e.into_inner()).get(name).cloned()
    }

    /// Async arena lookup: cached arenas are returned inline (no blocking);
    /// opening one goes through `run_blocking`.
    async fn arena_async(self: &Arc<Self>, name: &str, create: bool, capacity: Option<u32>) -> Result<Arena, ApiError> {
        if let Some(a) = self.cached_arena(name) {
            if capacity.map_or(true, |c| c == a.capacity()) {
                return Ok(a);
            }
        }
        let owned = name.to_string();
        self.run_blocking(name, move |s| s.arena(&owned, create, capacity)).await
    }

    /// Keeps arenas mapped for the daemon's lifetime (the core cache is weak).
    /// The map mutex is never held across `Arena::open`, which can block on the
    /// arena's flock: a stuck arena must not stall requests for other arenas.
    fn arena(&self, name: &str, create: bool, capacity: Option<u32>) -> Result<Arena, ApiError> {
        let lock = || self.arenas.lock().unwrap_or_else(|e| e.into_inner());
        let check = |a: &Arena| match capacity {
            Some(c) if c != a.capacity() => Err(ApiError::new(
                ErrorCode::LayoutMismatch,
                format!("arena '{name}' has capacity {}, requested {c}", a.capacity()),
            )),
            _ => Ok(()),
        };
        if let Some(a) = lock().get(name).cloned() {
            check(&a)?;
            return Ok(a);
        }
        if !create && !atomvar_core::arena_exists(name)? {
            return Err(ApiError::new(ErrorCode::NotFound, format!("arena '{name}' not found")));
        }
        let opened = Arena::open(name, capacity)?;
        let a = lock().entry(name.to_string()).or_insert(opened).clone();
        check(&a)?;
        Ok(a)
    }

    pub async fn list(self: &Arc<Self>, arena: &str) -> Result<Vec<VarInfo>, ApiError> {
        // list() itself is a lock-free scan.
        Ok(self.arena_async(arena, false, None).await?.list()?)
    }

    fn create(&self, req: &OpRequest, init: &Option<Operand>, capacity: Option<u32>) -> Outcome {
        let ty = match (req.value_type, init) {
            (Some(t), _) => t,
            (None, Some(Operand::Typed(v))) => v.value_type(),
            (None, _) => return Err(ApiError::invalid("create needs a 'type' (i64, u64, bool, f64, u128)")),
        };
        let init = match init {
            Some(op) => op.resolve(ty, "init")?,
            None => ty.zero(),
        };
        let arena = self.arena(&req.arena, true, capacity)?;
        let handle = arena.var(&req.name, init)?;
        Ok(OpResult::Value {
            value: handle.load(Ordering::SeqCst)?,
        })
    }

    /// Lock-free: lookup and opening an existing variable only probe the table.
    fn prepare(&self, arena: &Arena, req: &OpRequest) -> Result<Prepared, ApiError> {
        let info = arena.lookup(&req.name)?.ok_or_else(|| {
            ApiError::new(
                ErrorCode::NotFound,
                format!("variable '{}' not found in arena '{}'", req.name, req.arena),
            )
        })?;
        let ty = info.value_type;
        if let Some(t) = req.value_type {
            if t != ty {
                return Err(ApiError::new(
                    ErrorCode::TypeMismatch,
                    format!("variable '{}' is {ty}, request says {t}", req.name),
                ));
            }
        }
        let op = match &req.kind {
            OpKind::Create { .. } => unreachable!("handled by create"),
            OpKind::Get { ordering } => TypedOp::Get(*ordering),
            OpKind::Set { value, ordering } => TypedOp::Set(value.resolve(ty, "value")?, *ordering),
            OpKind::Swap { value, ordering } => TypedOp::Swap(value.resolve(ty, "value")?, *ordering),
            OpKind::CompareExchange {
                expected,
                desired,
                success,
                failure,
            } => TypedOp::Cas(
                expected.resolve(ty, "expected")?,
                desired.resolve(ty, "desired")?,
                *success,
                failure.unwrap_or_else(|| success.default_failure()),
            ),
            OpKind::Fetch {
                op,
                operand,
                ordering,
            } => {
                let supported = match ty {
                    ValueType::I64 | ValueType::U64 => !matches!(op, FetchOp::Nand),
                    ValueType::Bool => matches!(op, FetchOp::And | FetchOp::Or | FetchOp::Xor | FetchOp::Nand),
                    ValueType::F64 => matches!(op, FetchOp::Add | FetchOp::Sub),
                    ValueType::U128 => false,
                };
                if !supported {
                    return Err(ApiError::invalid(format!(
                        "{} is not supported for {ty} (u128 supports only get/set/swap/compare_exchange)",
                        op.as_str()
                    )));
                }
                TypedOp::Fetch(*op, operand.resolve(ty, "value")?, *ordering)
            }
        };
        let handle = arena.open_var(&req.name, ty)?;
        Ok(Prepared {
            arena: req.arena.clone(),
            name: req.name.clone(),
            value_type: ty,
            handle,
            op,
        })
    }

    /// Runs the single atomic operation. Synchronous: it completes within one
    /// poll, so a handler future can never be cancelled midway through it.
    fn apply(&self, p: &Prepared) -> Outcome {
        use AnyAtomic as A;
        use Value as V;
        let is_mutation = !matches!(p.op, TypedOp::Get(_));
        let out = match (&p.handle, &p.op) {
            (h, TypedOp::Get(o)) => OpResult::Value { value: h.load(*o)? },

            (A::I64(h), TypedOp::Set(V::I64(v), o)) => h.store(*v, *o).map(|_| OpResult::Stored)?,
            (A::U64(h), TypedOp::Set(V::U64(v), o)) => h.store(*v, *o).map(|_| OpResult::Stored)?,
            (A::Bool(h), TypedOp::Set(V::Bool(v), o)) => h.store(*v, *o).map(|_| OpResult::Stored)?,
            (A::F64(h), TypedOp::Set(V::F64(v), o)) => h.store_bits(v.to_bits(), *o).map(|_| OpResult::Stored)?,
            (A::U128(h), TypedOp::Set(V::U128(v), o)) => h.store(*v, *o).map(|_| OpResult::Stored)?,

            (A::I64(h), TypedOp::Swap(V::I64(v), o)) => OpResult::Swapped { previous: V::I64(h.swap(*v, *o)) },
            (A::U64(h), TypedOp::Swap(V::U64(v), o)) => OpResult::Swapped { previous: V::U64(h.swap(*v, *o)) },
            (A::Bool(h), TypedOp::Swap(V::Bool(v), o)) => OpResult::Swapped { previous: V::Bool(h.swap(*v, *o)) },
            (A::F64(h), TypedOp::Swap(V::F64(v), o)) => OpResult::Swapped { previous: V::F64(h.swap(*v, *o)) },
            (A::U128(h), TypedOp::Swap(V::U128(v), o)) => OpResult::Swapped { previous: V::U128(h.swap(*v, *o)) },

            (A::I64(h), TypedOp::Cas(V::I64(e), V::I64(d), s, f)) => cas(h.compare_exchange(*e, *d, *s, *f)?, V::I64),
            (A::U64(h), TypedOp::Cas(V::U64(e), V::U64(d), s, f)) => cas(h.compare_exchange(*e, *d, *s, *f)?, V::U64),
            (A::Bool(h), TypedOp::Cas(V::Bool(e), V::Bool(d), s, f)) => cas(h.compare_exchange(*e, *d, *s, *f)?, V::Bool),
            (A::F64(h), TypedOp::Cas(V::F64(e), V::F64(d), s, f)) => {
                cas(h.compare_exchange_bits(e.to_bits(), d.to_bits(), *s, *f)?, |b| V::F64(f64::from_bits(b)))
            }
            (A::U128(h), TypedOp::Cas(V::U128(e), V::U128(d), s, f)) => cas(h.compare_exchange(*e, *d, *s, *f)?, V::U128),

            (A::I64(h), TypedOp::Fetch(op, V::I64(v), o)) => {
                let (prev, cur) = int_fetch!(h, *op, *v, *o);
                OpResult::Fetched { previous: V::I64(prev), current: V::I64(cur) }
            }
            (A::U64(h), TypedOp::Fetch(op, V::U64(v), o)) => {
                let (prev, cur) = int_fetch!(h, *op, *v, *o);
                OpResult::Fetched { previous: V::U64(prev), current: V::U64(cur) }
            }
            (A::Bool(h), TypedOp::Fetch(op, V::Bool(v), o)) => {
                let prev = match op {
                    FetchOp::And => h.fetch_and(*v, *o),
                    FetchOp::Or => h.fetch_or(*v, *o),
                    FetchOp::Xor => h.fetch_xor(*v, *o),
                    FetchOp::Nand => h.fetch_nand(*v, *o),
                    _ => unreachable!("validated in prepare"),
                };
                let cur = match op {
                    FetchOp::And => prev & v,
                    FetchOp::Or => prev | v,
                    FetchOp::Xor => prev ^ v,
                    _ => !(prev & v),
                };
                OpResult::Fetched { previous: V::Bool(prev), current: V::Bool(cur) }
            }
            (A::F64(h), TypedOp::Fetch(op, V::F64(v), o)) => {
                // current is recomputed from the returned old value exactly as the
                // CAS loop computed the stored value.
                let (prev, cur) = match op {
                    FetchOp::Add => {
                        let p = h.fetch_add(*v, *o);
                        (p, p + v)
                    }
                    _ => {
                        let p = h.fetch_sub(*v, *o);
                        (p, p - v)
                    }
                };
                OpResult::Fetched { previous: V::F64(prev), current: V::F64(cur) }
            }
            _ => return Err(ApiError::new(ErrorCode::Internal, "operand/handle type mismatch")),
        };
        if is_mutation {
            self.hooks.executions.fetch_add(1, O::SeqCst);
            if self.hooks.panic_after_apply.swap(false, O::SeqCst) {
                panic!("injected test panic after the atomic op was applied");
            }
        }
        Ok(out)
    }

    /// Executes one request. Mutations with a `request_id` go through the
    /// idempotency table and run in a detached task, so client disconnection
    /// cannot cancel them; everything else runs inline.
    pub async fn execute(self: &Arc<Self>, req: OpRequest) -> Outcome {
        if let OpKind::Create { init, capacity } = req.kind.clone() {
            // Bootstrap and name creation may block on flock.
            let arena = req.arena.clone();
            return self.run_blocking(&arena, move |s| s.create(&req, &init, capacity)).await;
        }
        let arena = self.arena_async(&req.arena, false, None).await?;
        let prepared = self.prepare(&arena, &req)?;
        let id = match req.request_id.as_deref().filter(|s| !s.is_empty()) {
            Some(id) if matches!(prepared.op, TypedOp::Get(_)) => {
                let _ = id; // reads need no dedup
                return self.apply(&prepared);
            }
            Some(id) => id,
            None => return self.apply(&prepared),
        };
        if id.len() > MAX_REQUEST_ID_LEN {
            return Err(ApiError::invalid(format!(
                "request_id longer than {MAX_REQUEST_ID_LEN} bytes"
            )));
        }
        match self.idem.reserve(id, prepared.fingerprint())? {
            Reservation::Done(o) => o,
            Reservation::Wait(rx) => idem::wait(rx).await,
            Reservation::Execute(guard) => {
                let rx = guard.subscribe();
                let svc = self.clone();
                // The guard moves into the task: if the task is dropped before or
                // during execution, its Drop records OUTCOME_UNKNOWN.
                tokio::spawn(async move {
                    let guard = guard;
                    let delay = svc.hooks.delay_ms.load(O::SeqCst);
                    if delay > 0 {
                        tokio::time::sleep(Duration::from_millis(delay)).await;
                    }
                    let out = catch_unwind(AssertUnwindSafe(|| svc.apply(&prepared))).unwrap_or_else(|_| {
                        Err(ApiError::outcome_unknown(
                            "executor panicked; the operation may or may not have been applied",
                        ))
                    });
                    guard.complete(out);
                });
                idem::wait(rx).await
            }
        }
    }
}

fn cas<T>(r: Result<T, T>, wrap: impl Fn(T) -> Value) -> OpResult {
    match r {
        Ok(p) => OpResult::Cas { exchanged: true, previous: wrap(p) },
        Err(p) => OpResult::Cas { exchanged: false, previous: wrap(p) },
    }
}
