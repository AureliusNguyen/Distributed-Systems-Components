# atomvar evaluation report

- Machine: 12th Gen Intel(R) Core(TM) i7-12700H, 2 logical CPUs, kernel 6.18.40.1-microsoft-standard-WSL2
- Python 3.12.3 (GIL enabled: True), run 2026-10-02 15:27:43
- perf_counter_ns timer overhead (included in every per-op latency sample): ~49 ns
- Throughput = total ops / wall time (clock started before parked workers are released; each worker records its own finish). Latency = per-op samples. CPU = user+sys of all participating processes (clients and, for RPC, the server) / ops.
- With more workers than logical CPUs, workers time-share cores: those rows measure oversubscription, not parallel speedup.

## 1. Atomic vs mutex across processes (+ 3. contention scaling)

N spawned processes increment one shared counter.

| variant | procs | M ops/s | mean ns | p50 ns | p95 ns | p99 ns | CPU ns/op | result |
|---|---|---|---|---|---|---|---|---|
| atomic | 1 | 8.73 | 87 | 84 | 102 | 118 | 109 | exact |
| atomic | 2 | 15.33 | 144 | 131 | 190 | 231 | 124 | exact |
| atomic | 4 | 16.89 | 320 | 121 | 344 | 421 | 89 | exact |
| atomic | 8 | 15.78 | 370 | 120 | 184 | 228 | 99 | exact |
| atomic | 16 | 10.64 | 1,161 | 139 | 334 | 400 | 72 | exact |
| mp.Value+lock | 1 | 1.78 | 1,215 | 1,245 | 1,432 | 2,613 | 512 | exact |
| mp.Value+lock | 2 | 0.55 | 5,598 | 1,048 | 38,684 | 74,436 | 1,926 | exact |
| mp.Value+lock | 4 | 0.78 | 4,182 | 933 | 2,483 | 42,566 | 2,203 | exact |
| mp.Value+lock | 8 | 0.65 | 4,959 | 892 | 1,435 | 7,544 | 2,815 | exact |
| mp.Value+lock | 16 | 0.44 | 9,841 | 909 | 1,553 | 24,654 | 4,287 | exact |
| shm+mp.Lock | 1 | 2.28 | 404 | 380 | 449 | 590 | 425 | exact |
| shm+mp.Lock | 2 | 0.40 | 1,198 | 464 | 1,537 | 34,573 | 2,805 | exact |
| shm+mp.Lock | 4 | 1.84 | 2,140 | 748 | 1,842 | 3,629 | 934 | exact |
| shm+mp.Lock | 8 | 2.66 | 3,894 | 751 | 1,065 | 3,749 | 667 | exact |
| shm+mp.Lock | 16 | 2.11 | 8,736 | 759 | 1,397 | 2,389 | 857 | exact |

Atomic vs `multiprocessing.Value(lock=True)`:

| procs | throughput | p50 latency | p99 latency | CPU per op |
|---|---|---|---|---|
| 1 | 4.9x higher | 14.8x lower | 22.1x lower | 4.7x less |
| 2 | 27.6x higher | 8.0x lower | 322.2x lower | 15.5x less |
| 4 | 21.6x higher | 7.7x lower | 101.1x lower | 24.8x less |
| 8 | 24.2x higher | 7.4x lower | 33.1x lower | 28.4x less |
| 16 | 24.4x higher | 6.5x lower | 61.6x lower | 59.2x less |

## 2. Host-local state: shared-memory atomic vs RPC

One Python client, sequential ops, each op timed.

| transport | op | p50 us | p95 us | p99 us | ops/s |
|---|---|---|---|---|---|
| shm atomic | load | 0.07 | 0.16 | 0.17 | 11,609,078 |
| shm atomic | fetch_add | 0.10 | 0.13 | 0.14 | 9,399,950 |
| shm atomic | compare_exchange | 0.16 | 0.19 | 0.26 | 6,081,638 |
| gRPC (Rust server) | load | 239.07 | 413.46 | 529.05 | 3,664 |
| gRPC (Rust server) | fetch_add | 238.39 | 400.11 | 492.15 | 3,715 |
| gRPC (Rust server) | compare_exchange | 247.10 | 427.00 | 555.25 | 3,543 |
| gRPC (Python server) | load | 534.20 | 742.99 | 897.61 | 1,782 |
| gRPC (Python server) | fetch_add | 540.03 | 780.57 | 1009.19 | 1,735 |
| gRPC (Python server) | compare_exchange | 570.81 | 896.15 | 1286.57 | 1,611 |
| Redis TCP | load | 86.82 | 135.65 | 196.28 | 10,706 |
| Redis TCP | fetch_add | 88.14 | 165.03 | 223.55 | 10,377 |
| Redis TCP | compare_exchange | 103.81 | 155.92 | 216.13 | 9,001 |
| Redis unix socket | load | 82.96 | 119.30 | 174.12 | 11,310 |
| Redis unix socket | fetch_add | 82.58 | 122.93 | 178.37 | 11,447 |
| Redis unix socket | compare_exchange | 101.05 | 182.74 | 245.63 | 8,731 |

Slowdown vs shared memory (p50 / p99):

| transport | load | fetch_add | compare_exchange |
|---|---|---|---|
| gRPC (Rust server) | 3,367x / 3,058x | 2,360x / 3,541x | 1,584x / 2,161x |
| gRPC (Python server) | 7,524x / 5,189x | 5,347x / 7,260x | 3,659x / 5,006x |
| Redis TCP | 1,223x / 1,135x | 873x / 1,608x | 665x / 841x |
| Redis unix socket | 1,168x / 1,006x | 818x / 1,283x | 648x / 956x |

4 client processes doing fetch_add concurrently:

| transport | ops/s | CPU ns/op (clients+server) | shm advantage |
|---|---|---|---|
| shm atomic | 18,171,020 | 88 | baseline |
| gRPC (Rust server) | 9,031 | 203,916 | 2,012x throughput, 2,322x less CPU |
| gRPC (Python server) | 2,810 | 524,436 | 6,466x throughput, 5,971x less CPU |
| Redis TCP | 35,151 | 49,189 | 517x throughput, 560x less CPU |
| Redis unix socket | 33,894 | 46,175 | 536x throughput, 526x less CPU |

## 4. Cross-language: one counter, four runtimes

Python, Go (cgo), C++ and Rust processes each did 1,000,000 `fetch_add(1)` on the same arena variable at the same time.

- Final value: **4,000,000** (expected 4,000,000): **exact**
- Aggregate: 28.50 M ops/s, wall 0.14 s (includes process exit)

| language | ns/op (over its own run) |
|---|---|
| Python | 129.2 |
| Go | 134.8 |
| C++ | 46.4 |
| Rust | 31.0 |

Same increments via atomvard gRPC (4 Rust tonic clients x 20,000): 21,964 ops/s (exact). Shared memory delivered **1,298x** the throughput.

