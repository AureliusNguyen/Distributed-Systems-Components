# Distributed-Systems toolkit

Reusable, language-agnostic building blocks for distributed systems. Each
component is usable from any language and callable by AI agents, so larger
projects can combine them.

## Components

| Component | Status | What it is |
|---|---|---|
| [Atomic-Variables](Atomic-Variables/) | v1 | Lock-free atomic variables (i64/u64/bool/f64/u128), in-process or in named shared memory, plus a daemon over HTTP/JSON, gRPC and MCP |

## Conventions every component follows

1. **Rust core crate** (`<name>-core`): the logic, with no I/O. Concurrency code
   is written against small traits so the same algorithm can be model-checked
   with loom.
2. **C ABI crate** (`<name>-ffi`): `libX.so` / `libX.a` plus a generated header.
   Opaque handles, status codes, a thread-local last-error message, no panics
   across the boundary, and only operations exposed (never raw pointers). This
   is how Go, Java, C#, Node, Ruby and friends use the component.
3. **Python bindings** (`<name>-py`): PyO3 + maturin with type stubs, and
   free-threaded CPython declared supported.
4. **Service API** (`proto/<name>.proto`): the single source of truth. A daemon
   serves it three ways from one handler layer:
   - gRPC (tonic);
   - HTTP/JSON (axum), with an `openapi.yaml` mirror;
   - MCP (rmcp, stdio + streamable HTTP) for AI agents, with tool descriptions
     that state the failure contract.
5. **A written guarantees section**: what is atomic, what locks, what happens
   on crash, on fork and on a lost network response, and what is explicitly
   NOT promised. Every claim is backed by a failure test (crash injection,
   concurrency, idempotency races), not only happy-path totals.
6. **Network mutation contract**: an unknown outcome is never auto-retried.
   Optional `request_id` dedup has a documented, narrow scope.
7. **Security default**: daemons bind loopback. A non-loopback bind requires a
   bearer token.

## Development notes

- On WSL, build with `CARGO_TARGET_DIR` on the Linux filesystem (for example
  `~/.cache/<component>-target`); building under `/mnt/c` is very slow.
- Each component has its own Cargo workspace and README with build and test
  commands.
