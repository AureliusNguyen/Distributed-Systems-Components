# atomvar: design, guarantees and reference

Real hardware atomic variables for any language, the same idea as Java's
`AtomicLong` / `AtomicBoolean`: a read-modify-write such as "add 1" happens as
one indivisible operation, so concurrent writers never lose updates.

Python has no atomics, so this fills that gap, and it goes further:

- **In-process**: share an `AtomicInt` between threads (works on free-threaded
  CPython 3.14t, where there is no GIL at all).
- **Cross-process**: a named variable in a shared-memory *arena*. Any process
  that opens the same arena and name (Python, C, Rust, anything with a C FFI)
  operates on the same memory word.
- **Over the network / for agents**: the `atomvard` daemon exposes the same
  variables over HTTP/JSON, gRPC and MCP.

```
                +--------------------- one shared-memory word ---------------------+
 Python  ------>|                                                                  |
 C / Go / ...-->|  /dev/shm/atomvar-<arena>   slot "counter": LOCK XADD / CMPXCHG  |
 Rust    ------>|                                                                  |
 atomvard ----->|  (HTTP/JSON, gRPC, MCP clients go through the daemon)            |
                +------------------------------------------------------------------+
```

## Guarantees (and what is NOT promised)

- **Value operations are atomic and lock-free.** They are hardware atomics with
  no fallback locks on supported platforms. They are **not** promised to be
  wait-free or single-instruction: f64 add, `fetch_max/min`, and (on x86)
  value-returning `fetch_and/or/xor` are CAS loops.
- **Registry operations** (open/create a name) and **arena bootstrap** take an
  OS lock (`flock`) with crash recovery. These are cold paths, not lock-free,
  and they can block while another process holds that arena's lock. The Python
  binding releases the GIL around them. The daemon runs them on Tokio's
  blocking pool, one worker at a time per arena; that worker keeps the arena's
  turn until its work really ends, even if the client disconnects. Requests
  admitted under the limits (1 being served + up to 256 queued per arena, up to
  64 arenas blocked at once) wait for as long as the lock is held; requests
  beyond them get `RESOURCE_EXHAUSTED` at once. A stuck arena therefore cannot
  starve other arenas or health checks. Lookups and value operations never take
  the lock.
- **Only its own states are recovered**: a segment is initialized only if it is
  empty, or if it is exactly what a creator that died before publishing leaves
  behind: a valid arena size, zero signature, header fields that are zero or
  what this layout writes, and all-zero slots. Anything else (another
  signature, an impossible size, stray data) is rejected with
  `LAYOUT_MISMATCH` and left untouched.
- **Default ordering is SeqCst** (Java `AtomicLong` semantics). Other orderings
  are opt-in.
- **Not durable**: state lives in `/dev/shm`. It survives process and daemon
  crashes, not a reboot.
- **Not consensus**: this is a single-host store with remote access. CAS gives
  "exactly one winner" for a claim within this store. There are no leases,
  fencing or failover, so it is not leader election.
- **Network calls**: a timeout or lost response means the outcome is
  **unknown**. Clients never auto-retry mutations (see "Retries" below).

### Supported platform (v1)

- Linux x86_64 (including WSL2) with CMPXCHG16B, i.e. x86-64-v2 or newer (every
  x86_64 CPU since roughly 2008). The build enables `+cmpxchg16b` globally
  (`.cargo/config.toml`) and refuses to compile without it, so u128 atomics
  never fall back to a lock. Running on a CPU without the instruction is outside
  the contract: `atomvar_init()` / `cpu_supported()` is only a best-effort
  diagnostic, and graceful rejection is not guaranteed.
- Fixed-capacity arenas. No deletion and no slot reuse in v1: a slot's type is
  fixed for the arena's lifetime, and handles keep the mapping alive.

### Process model

Start worker processes **fresh**: `spawn`, `forkserver`, `posix_spawn`,
fork+exec, or just separately launched programs. A child made by plain
`fork()` (without exec) from a process that already used atomvar gets
`ForkedProcess` / `ForkedProcessError` from arena and registry calls. It is
refused, not "recovered", because locks inherited across a multithreaded fork
cannot be recovered safely.

- Python 3.14 already defaults to `forkserver` on Linux.
- On Python 3.12/3.13 call `multiprocessing.set_start_method("spawn")` (or
  `"forkserver"`).
- Known hazard: if a process forks while one of its threads holds an arena's
  lock, the child's inherited descriptor keeps that lock held until the child
  exits. Until then, new opens and name creations for THAT arena block (other
  arenas and value operations are unaffected). Using spawn/forkserver avoids it.

### Trust model

An arena lives at `/dev/shm/atomvar-<name>` with mode 0600. Anyone who can open
that file can read and change every variable in it, so the arena name is the
trust boundary. The library refuses symlinks and segments that are not regular
files owned by the current user.

## Quick start

### Python

```bash
cd crates/atomvar-py && maturin develop --release   # inside a virtualenv
```

```python
from atomvar import AtomicInt, Arena, Ordering

hits = AtomicInt(0)                      # in-process, shared by threads
hits.increment_and_get()
hits.compare_and_set(1, 10)              # Java-style CAS -> bool
hits.compare_exchange(10, 11)            # -> (exchanged, previous)

jobs = Arena("jobs").int("done", 0)      # shared memory; same name = same variable
jobs.fetch_add(1)                        # in every process that opens it
jobs.fetch_add(1, Ordering.RELAXED)      # explicit ordering (experts only)
```

Types: `AtomicInt` (i64), `AtomicUInt` (u64), `AtomicBool`, `AtomicFloat`
(f64), `AtomicU128`. Arena methods: `int/uint/bool/float/u128(name, init)`
(open or create; `init` is used only on creation), `open_*` (must exist),
`lookup`, `list`. Errors are subclasses of `atomvar.AtomvarError`.

### Rust

```rust
use atomvar_core::{Arena, AtomicI64, Ordering};

let local = AtomicI64::new(0);
local.fetch_add(1, Ordering::SeqCst);

let shared = Arena::open("jobs", None)?.i64("done", 0)?;
shared.add_and_get(1, Ordering::SeqCst);
```

### C (and anything with a C FFI: Go cgo, Java Panama, C#, Node, Ruby, ...)

```c
#include "atomvar.h"   /* crates/atomvar-ffi/include/atomvar.h, link -latomvar */

atomvar_arena *arena;  atomvar_i64 *done;  int64_t now;
atomvar_arena_open("jobs", 0, &arena);
atomvar_arena_i64(arena, "done", 0, &done);
atomvar_i64_add_and_get(done, 1, ATOMVAR_SEQ_CST, &now);
```

Every function returns `ATOMVAR_OK` (0) or an error code; `atomvar_last_error()`
gives the message. Only operations are exposed, never raw pointers. See
`../examples/c_example.c`.

### HTTP/JSON, gRPC, MCP (daemon)

```bash
atomvard serve                         # HTTP :7878 (MCP at /mcp), gRPC :7879, loopback only
curl -X POST localhost:7878/v1/arenas/jobs/vars/done/create    -d '{"type":"i64"}' -H 'content-type: application/json'
curl -X POST localhost:7878/v1/arenas/jobs/vars/done/fetch_add -d '{"value":1,"request_id":"job-42"}' -H 'content-type: application/json'
# -> {"previous":"0","current":"1"}
curl localhost:7878/v1/arenas/jobs/vars/done
```

- HTTP API: `../openapi.yaml`. gRPC API: `../proto/atomvar.proto`.
- MCP for Claude Code: `claude mcp add atomvar -- atomvard mcp` (stdio), or
  point an MCP client at `http://127.0.0.1:7878/mcp`. Tools: `atomic_create`,
  `atomic_get`, `atomic_set`, `atomic_add`, `atomic_compare_and_set`,
  `atomic_swap`, `atomic_list` (all SeqCst).
- Binding a non-loopback address requires `--token` (or `ATOMVAR_TOKEN`);
  clients then send `Authorization: Bearer <token>`.
- Admin: `atomvard arena inspect <name>`; `atomvard arena destroy <name> --yes`.
  Only destroy an arena nothing is using: processes that still have it mapped
  keep the orphaned copy while new openers get a fresh one (split brain).

## Types and operations

| Type | Operations |
|---|---|
| i64, u64 | load, store, swap, compare_exchange (strong/weak), fetch_add/sub/and/or/xor/max/min, add_and_get, sub_and_get, increment/decrement_and_get |
| bool | load, store, swap, compare_exchange, fetch_and/or/xor/nand |
| f64 | load, store, swap, compare_exchange (bitwise), fetch_add/sub (CAS loop), exact-bits load/store/CAS |
| u128 | load, store, swap, compare_exchange (CMPXCHG16B) |

- Integers **wrap** on overflow (two's complement), like the hardware and Java.
- `add_and_get` is computed from the single `fetch_add`'s returned old value,
  never from a second load.
- f64 CAS compares **bits**: `0.0` and `-0.0` differ, and a NaN matches only the
  identical bit pattern. `fetch_add` follows IEEE (NaN and infinities propagate).

## Memory orderings in plain English

If you are not sure, do not pass an ordering: the default `SEQ_CST` behaves
like Java's `AtomicLong`, with all atomic operations appearing in one global
order.

- `RELAXED`: the operation itself is atomic, but it orders nothing else.
  Fine for pure counters/statistics that nobody uses to publish other data.
- `RELEASE` (writes) / `ACQUIRE` (reads): a pair. Everything written before a
  release-store is visible to a thread that acquire-loads that value. This is
  the "publish a flag after preparing data" pattern.
- `ACQ_REL`: both, for read-modify-write operations.
- Invalid combinations are rejected: a `RELEASE` load, an `ACQUIRE` store, or a
  CAS whose failure ordering is `RELEASE`/`ACQ_REL` or stronger than the success
  ordering.

## Retries over the network (important for agents)

1. A timeout or lost response means the outcome is **unknown**. Do not blindly
   retry a mutation.
2. Pass a unique `request_id` (HTTP body field or `Idempotency-Key` header; gRPC
   field; MCP argument). A retry with the same id returns the original result
   instead of applying the change twice. Concurrent duplicates wait for, and
   share, the first result. Reusing an id with a different payload is
   `IDEMPOTENCY_CONFLICT`.
3. The guarantee is narrow: dedup holds only within **one daemon instance while
   the entry is retained** (default 10 minutes). A restart, TTL expiry, size
   eviction of completed entries, or a retry sent to a different daemon can
   reapply the operation. Size the table (`--idem-capacity`, default 100,000)
   well above your maximum in-flight requests: when it is full, the oldest
   completed entry is evicted, and `RESOURCE_EXHAUSTED` is returned only when
   every entry is still in progress. `create` ignores `request_id`: it is
   open-or-create and idempotent by itself. If the executing task dies or panics, the recorded
   outcome is `OUTCOME_UNKNOWN` and the op is never re-executed automatically.
   (A process-level crash, SIGKILL or `panic=abort` loses the table, which is
   the "restart" case.)
4. **CAS is not automatically retry-safe either**: if the value goes
   A -> B -> A, a retried `compare_and_set(A, X)` succeeds again (ABA).
   Retry-safe patterns:
   - Irreversible claims: the variable only ever moves `0 -> unique_token` and
     is never reset. A retry either fails or shows your own token.
   - Values that never repeat: CAS on a version counter, or a u128 packed as
     `(version << 64) | value` where every write bumps the version. Finite
     versions eventually wrap, so this is safe only while wrap cannot happen
     within the window in which an old request might still be retried; a
     64-bit version cannot wrap at realistic rates.

## Benchmarks

See the main README for charts, and the full reports in `../benches/results/`
(`Slade201289-3-20261002-164330` is the native-Linux reference run). Reproduce
with `python benches/suite/run_all.py`; method in `../benches/suite/README.md`.

## Building and testing

```bash
export CARGO_TARGET_DIR=~/.cache/atomvar-target     # optional: faster than /mnt/c on WSL
cargo test --workspace --release                    # core, C ABI (compiles + runs a C program), daemon
RUSTFLAGS="--cfg loom -C target-feature=+cmpxchg16b" \
  cargo test --release -p atomvar-core --test loom  # loom model checks
cd crates/atomvar-py && maturin develop --release && cd ../..
python -m pytest tests/python                       # threads, spawn/forkserver, fork refusal
cargo run -p atomvar-ffi --bin atomvar-gen-header > crates/atomvar-ffi/include/atomvar.h
```

What the failure tests cover: concurrent same-name creation (threads sharing one
handle, and 16 processes), hash collisions, a full arena, type and layout
mismatches, a creator SIGKILLed before publishing the header, a recovering process SIGKILLed right
after resizing an unpublished segment, an inserter
SIGKILLed between writing a slot and publishing it, fork refusal while another
thread holds the registry locks, lost responses, 50 simultaneous duplicate
request ids, conflicting payloads, client disconnects, a task dropped before
its first poll, an executor panic, a full idempotency table, and the ABA
scenario.

## Layout

```
crates/atomvar-core   types, orderings, shared-memory arena (layout.rs is the on-disk contract)
crates/atomvar-ffi    C ABI -> libatomvar.so / .a + include/atomvar.h (generated, checked by a test)
crates/atomvar-py     Python bindings (PyO3 + maturin), type stubs included
crates/atomvard       daemon: HTTP/JSON + MCP (axum/rmcp), gRPC (tonic), admin CLI
proto/atomvar.proto   service API source of truth; openapi.yaml mirrors it
examples/ benches/ tests/python/
```

Other languages should use the C ABI rather than mapping `/dev/shm` segments
themselves: the ABI guarantees every access is atomic and follows the registry
protocol.
