//! Best-effort request deduplication for mutations.
//!
//! Protocol:
//! 1. `reserve` atomically (under the table mutex) inserts IN_PROGRESS for an
//!    unseen request_id and hands back a `CompletionGuard`. Only the holder of
//!    the guard executes the operation.
//! 2. Concurrent duplicates find IN_PROGRESS and wait on the same channel;
//!    completed duplicates get the stored result. Reusing an id with a different
//!    payload (fingerprint) is IDEMPOTENCY_CONFLICT and executes nothing.
//! 3. The guard is created at reservation time and moved into the executing
//!    task. If that task is dropped (even before its first poll) or unwinds
//!    without recording a result, the guard's Drop records OUTCOME_UNKNOWN and
//!    wakes every waiter. An uncertain operation is never re-executed.
//!
//! Scope, deliberately narrow: dedup holds only within ONE daemon instance while
//! the entry is retained. Restart, TTL expiry, size eviction of completed
//! entries, or a retry sent to another daemon can reapply the operation.
//! panic=abort, SIGKILL or a crash run no guards; they fall under "restart".

use crate::error::{ApiError, ErrorCode};
use crate::service::OpResult;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::watch;

pub type Outcome = Result<OpResult, ApiError>;

enum State {
    InProgress(watch::Receiver<Option<Outcome>>),
    Completed { outcome: Outcome, at: Instant },
}

struct Entry {
    fingerprint: u64,
    state: State,
}

pub struct IdemTable {
    entries: Mutex<HashMap<String, Entry>>,
    capacity: usize,
    ttl: Duration,
}

pub enum Reservation {
    /// This caller must execute the op and complete the guard.
    Execute(CompletionGuard),
    /// Another request with this id is executing; wait for its result.
    Wait(watch::Receiver<Option<Outcome>>),
    /// Already completed (and retained): replay the stored result.
    Done(Outcome),
}

impl IdemTable {
    pub fn new(capacity: usize, ttl: Duration) -> Arc<Self> {
        Arc::new(IdemTable {
            entries: Mutex::new(HashMap::new()),
            capacity: capacity.max(1),
            ttl,
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Entry>> {
        self.entries.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn reserve(self: &Arc<Self>, id: &str, fingerprint: u64) -> Result<Reservation, ApiError> {
        let now = Instant::now();
        let mut map = self.lock();

        if let Some(e) = map.get(id) {
            let expired = matches!(e.state, State::Completed { at, .. } if now.duration_since(at) > self.ttl);
            if !expired {
                if e.fingerprint != fingerprint {
                    return Err(ApiError::new(
                        ErrorCode::IdempotencyConflict,
                        format!("request_id '{id}' was already used with a different operation or payload"),
                    ));
                }
                return Ok(match &e.state {
                    State::InProgress(rx) => Reservation::Wait(rx.clone()),
                    State::Completed { outcome, .. } => Reservation::Done(outcome.clone()),
                });
            }
            map.remove(id);
        }

        if map.len() >= self.capacity {
            // Drop expired completed entries, then the oldest completed one.
            map.retain(|_, e| !matches!(e.state, State::Completed { at, .. } if now.duration_since(at) > self.ttl));
            if map.len() >= self.capacity {
                let oldest = map
                    .iter()
                    .filter_map(|(k, e)| match e.state {
                        State::Completed { at, .. } => Some((at, k.clone())),
                        State::InProgress(_) => None,
                    })
                    .min();
                match oldest {
                    Some((_, k)) => {
                        map.remove(&k);
                    }
                    None => {
                        return Err(ApiError::new(
                            ErrorCode::ResourceExhausted,
                            "idempotency table is full of in-progress requests; retry later",
                        ))
                    }
                }
            }
        }

        let (tx, rx) = watch::channel(None);
        map.insert(
            id.to_string(),
            Entry {
                fingerprint,
                state: State::InProgress(rx),
            },
        );
        Ok(Reservation::Execute(CompletionGuard {
            table: self.clone(),
            id: id.to_string(),
            tx: Some(tx),
        }))
    }

    fn record(&self, id: &str, outcome: Outcome) {
        if let Some(e) = self.lock().get_mut(id) {
            e.state = State::Completed {
                outcome,
                at: Instant::now(),
            };
        }
    }

    pub fn len(&self) -> usize {
        self.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Test hook: ages every completed entry past the TTL.
    pub fn expire_all_for_test(&self) {
        let past = Instant::now() - self.ttl - Duration::from_secs(1);
        for e in self.lock().values_mut() {
            if let State::Completed { at, .. } = &mut e.state {
                *at = past;
            }
        }
    }
}

/// Owned by whoever executes a reserved request. Completing it (or dropping it
/// without completing) records the outcome and wakes all waiters.
pub struct CompletionGuard {
    table: Arc<IdemTable>,
    id: String,
    tx: Option<watch::Sender<Option<Outcome>>>,
}

impl CompletionGuard {
    pub fn subscribe(&self) -> watch::Receiver<Option<Outcome>> {
        self.tx.as_ref().expect("live guard").subscribe()
    }

    pub fn complete(mut self, outcome: Outcome) {
        self.finish(outcome);
    }

    fn finish(&mut self, outcome: Outcome) {
        if let Some(tx) = self.tx.take() {
            // Record first so a new reservation never sees IN_PROGRESS after
            // waiters have been released.
            self.table.record(&self.id, outcome.clone());
            let _ = tx.send(Some(outcome));
        }
    }
}

impl Drop for CompletionGuard {
    fn drop(&mut self) {
        self.finish(Err(ApiError::outcome_unknown(
            "the executing task ended without recording a result; the operation may or may not have been applied",
        )));
    }
}

/// Waits for the outcome of an in-progress request.
pub async fn wait(mut rx: watch::Receiver<Option<Outcome>>) -> Outcome {
    loop {
        if let Some(o) = rx.borrow_and_update().clone() {
            return o;
        }
        if rx.changed().await.is_err() {
            // Sender gone. The guard always sends before dropping, so this only
            // happens if we raced with that final send; check once more.
            return rx.borrow().clone().unwrap_or_else(|| {
                Err(ApiError::outcome_unknown("executor vanished without a result"))
            });
        }
    }
}
