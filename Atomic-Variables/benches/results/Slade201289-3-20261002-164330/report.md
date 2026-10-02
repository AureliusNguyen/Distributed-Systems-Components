# atomvar evaluation report

- Machine: AMD Ryzen 9 7950X 16-Core Processor, 32 logical CPUs, kernel 6.8.0-111-generic
- Python 3.10.12 (GIL enabled: True), run 2026-10-02 16:42:59
- perf_counter_ns timer overhead (included in every per-op latency sample): ~30 ns
- Throughput = total ops / wall time (clock started before parked workers are released; each worker records its own finish). Latency = per-op samples. CPU = user+sys of all participating processes (clients and, for RPC, the server) / ops.
- With more workers than logical CPUs, workers time-share cores: those rows measure oversubscription, not parallel speedup.

## 1. Atomic vs mutex across processes (+ 3. contention scaling)

N spawned processes increment one shared counter.

| variant | procs | M ops/s | mean ns | p50 ns | p95 ns | p99 ns | CPU ns/op | result |
|---|---|---|---|---|---|---|---|---|
| atomic | 1 | 24.03 | 59 | 60 | 70 | 70 | 41 | exact |
| atomic | 2 | 23.43 | 69 | 70 | 80 | 90 | 82 | exact |
| atomic | 4 | 40.99 | 143 | 130 | 340 | 420 | 96 | exact |
| atomic | 8 | 40.70 | 147 | 130 | 340 | 430 | 184 | exact |
| atomic | 16 | 38.73 | 379 | 380 | 650 | 690 | 375 | exact |
| mp.Value+lock | 1 | 2.84 | 383 | 380 | 400 | 430 | 351 | exact |
| mp.Value+lock | 2 | 1.30 | 1,352 | 1,110 | 2,920 | 5,930 | 1,323 | exact |
| mp.Value+lock | 4 | 1.62 | 2,516 | 1,330 | 7,410 | 12,330 | 1,940 | exact |
| mp.Value+lock | 8 | 1.35 | 6,409 | 3,630 | 18,870 | 29,280 | 3,861 | exact |
| mp.Value+lock | 16 | 1.16 | 13,893 | 4,710 | 47,819 | 75,540 | 4,700 | exact |
| shm+mp.Lock | 1 | 5.02 | 227 | 220 | 250 | 260 | 199 | exact |
| shm+mp.Lock | 2 | 2.17 | 324 | 250 | 750 | 1,110 | 920 | exact |
| shm+mp.Lock | 4 | 2.38 | 2,225 | 1,260 | 7,190 | 12,080 | 1,390 | exact |
| shm+mp.Lock | 8 | 1.84 | 4,333 | 2,510 | 12,980 | 20,370 | 3,405 | exact |
| shm+mp.Lock | 16 | 1.54 | 10,514 | 7,060 | 29,430 | 45,140 | 8,722 | exact |

Atomic vs `multiprocessing.Value(lock=True)`:

| procs | throughput | p50 latency | p99 latency | CPU per op |
|---|---|---|---|---|
| 1 | 8.5x higher | 6.3x lower | 6.1x lower | 8.5x less |
| 2 | 18.1x higher | 15.9x lower | 65.9x lower | 16.2x less |
| 4 | 25.3x higher | 10.2x lower | 29.4x lower | 20.3x less |
| 8 | 30.3x higher | 27.9x lower | 68.1x lower | 20.9x less |
| 16 | 33.4x higher | 12.4x lower | 109.5x lower | 12.5x less |

## 2. Host-local state: shared-memory atomic vs RPC

One Python client, sequential ops, each op timed.

| transport | op | p50 us | p95 us | p99 us | ops/s |
|---|---|---|---|---|---|
| shm atomic | load | 0.05 | 0.06 | 0.08 | 21,525,731 |
| shm atomic | fetch_add | 0.07 | 0.12 | 0.13 | 12,994,724 |
| shm atomic | compare_exchange | 0.10 | 0.12 | 0.18 | 9,609,072 |
| gRPC (Rust server) | load | 88.87 | 107.87 | 129.19 | 11,017 |
| gRPC (Rust server) | fetch_add | 84.68 | 109.29 | 124.28 | 11,594 |
| gRPC (Rust server) | compare_exchange | 90.31 | 110.86 | 124.35 | 10,972 |
| gRPC (Python server) | load | 144.37 | 184.19 | 211.98 | 6,760 |
| gRPC (Python server) | fetch_add | 148.19 | 182.69 | 197.48 | 6,618 |
| gRPC (Python server) | compare_exchange | 159.90 | 199.00 | 220.79 | 6,153 |
| Redis TCP | load | 29.10 | 37.11 | 47.82 | 32,648 |
| Redis TCP | fetch_add | 28.33 | 32.52 | 36.56 | 34,200 |
| Redis TCP | compare_exchange | 36.23 | 49.99 | 64.18 | 24,758 |
| Redis unix socket | load | 21.29 | 24.15 | 29.65 | 45,697 |
| Redis unix socket | fetch_add | 20.98 | 23.27 | 26.97 | 46,235 |
| Redis unix socket | compare_exchange | 27.40 | 31.15 | 36.60 | 35,881 |

Slowdown vs shared memory (p50 / p99):

| transport | load | fetch_add | compare_exchange |
|---|---|---|---|
| gRPC (Rust server) | 1,777x / 1,615x | 1,210x / 956x | 903x / 691x |
| gRPC (Python server) | 2,887x / 2,650x | 2,117x / 1,519x | 1,599x / 1,227x |
| Redis TCP | 582x / 598x | 405x / 281x | 362x / 357x |
| Redis unix socket | 426x / 371x | 300x / 207x | 274x / 203x |

4 client processes doing fetch_add concurrently:

| transport | ops/s | CPU ns/op (clients+server) | shm advantage |
|---|---|---|---|
| shm atomic | 24,690,467 | 162 | baseline |
| gRPC (Rust server) | 32,961 | 125,119 | 749x throughput, 774x less CPU |
| gRPC (Python server) | 11,645 | 266,565 | 2,120x throughput, 1,648x less CPU |
| Redis TCP | 122,887 | 32,607 | 201x throughput, 202x less CPU |
| Redis unix socket | 153,048 | 23,226 | 161x throughput, 144x less CPU |

## 4. Cross-language: one counter, several runtimes

Python, C++, Rust, Go processes each did 1,000,000 `fetch_add(1)` on the same arena variable at the same time.

- Final value: **4,000,000** (expected 4,000,000): **exact**
- Aggregate: 50.93 M ops/s, wall 0.08 s (includes process exit)

| language | ns/op (over its own run) |
|---|---|
| Python | 76.8 |
| C++ | 40.7 |
| Rust | 39.5 |
| Go | 72.7 |

Same increments via atomvard gRPC (4 Rust tonic clients x 20,000): 71,728 ops/s (exact). Shared memory delivered **710x** the throughput.

