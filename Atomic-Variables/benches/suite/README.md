# Evaluation suite

One command runs the four benchmark families and writes
`benches/results/<host>-<timestamp>/report.md` (+ `results.json`):

```bash
export CARGO_TARGET_DIR=~/.cache/atomvar-target
pip install grpcio grpcio-tools redis matplotlib   # plus atomvar itself (maturin develop)
python benches/suite/run_all.py                 # full run (~10 min on 2 vCPUs)
python benches/suite/run_all.py --quick         # smoke run
python benches/suite/run_all.py --only rpc      # one family
python benches/suite/plots.py benches/results/<run>/results.json   # charts -> images/
```

Needs `g++` and `cargo`. Optional: `redis-server` (Redis baselines) and Go with
cgo (Go worker). If either is missing, that part is skipped with a message.

| # | Question | What runs |
|---|---|---|
| 1 | Is the primitive faster than a mutex? | N spawned processes increment one counter: atomvar `fetch_add` vs `multiprocessing.Value(lock=True)` vs `shared_memory` + `multiprocessing.Lock` |
| 2 | Is shared memory worth it for host-local state? | `load` / `fetch_add` / `compare_exchange` via shared memory vs gRPC (Rust server, the strongest baseline), gRPC (Python server, the typical one), Redis over TCP and over a unix socket |
| 3 | What happens under contention? | Benchmark 1 at 1, 2, 4, 8, 16 processes |
| 4 | Does it work across runtimes? | Python, Go (cgo), C++ and Rust processes each do 1,000,000 increments on one variable (final must be exactly 4,000,000); the same work through gRPC for comparison |

Method:

- **Throughput:** workers are spawned and parked, the clock starts, then they
  are released. Each worker records its own finish time, so startup and
  shutdown are excluded. The cross-language wall time includes process exit,
  which is conservative.
- **Latency:** every op is timed with `perf_counter_ns`; the report shows the
  timer's own overhead, which is included in each sample.
- **CPU:** user+sys time of all participating processes (clients and, for RPC,
  the server) divided by operations.
- **Results** are medians of repeated runs, valid for the implementations
  tested, on the measured machine. With more workers than logical CPUs, rows
  measure time-sharing, not parallel speedup.
