"""Runs the whole evaluation and writes benches/results/<host>-<timestamp>/.

    python benches/suite/run_all.py            # full run
    python benches/suite/run_all.py --quick    # smoke run (small counts)

1. Atomic vs mutex          (processes; is the primitive faster?)
2. Shared memory vs RPC     (is the architecture worth it for host-local state?)
3. Contention scaling       (1, 2, 4, 8, 16 processes; part of 1)
4. Cross-language           (Python + Go + C++ + Rust on one counter)

Requires: atomvar installed in this Python, grpcio + grpcio-tools + redis
(pip), redis-server, Go with cgo, g++, and cargo.
"""

import argparse
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import bench_crosslang  # noqa: E402
import bench_rpc  # noqa: E402
import bench_sync  # noqa: E402
from common import ROOT, machine_info, save_json, timer_overhead_ns  # noqa: E402
from report import render  # noqa: E402


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--quick", action="store_true")
    ap.add_argument("--only", choices=["sync", "rpc", "crosslang"], action="append")
    args = ap.parse_args()
    q = args.quick
    only = set(args.only or ["sync", "rpc", "crosslang"])

    info = machine_info()
    info["timer_overhead_ns"] = timer_overhead_ns()
    print(f"machine: {info}")
    res = {"machine": info, "quick": q}
    t0 = time.time()
    if "sync" in only:
        print("[1+3] atomic vs mutex, contention scaling")
        res["sync"] = bench_sync.run(
            procs_list=(1, 2, 4) if q else (1, 2, 4, 8, 16),
            n=20_000 if q else 100_000, n_lat=5_000 if q else 20_000, repeats=1 if q else 3)
    if "rpc" in only:
        print("[2] shared memory vs RPC")
        res["rpc"] = bench_rpc.run(
            n_shm=20_000 if q else 200_000, n_rpc=1_000 if q else 10_000,
            conc_n_shm=20_000 if q else 200_000, conc_n_rpc=500 if q else 5_000)
    if "crosslang" in only:
        print("[4] cross-language")
        res["crosslang"] = bench_crosslang.run(n=100_000 if q else 1_000_000, n_grpc=2_000 if q else 20_000)
    res["runtime_s"] = time.time() - t0

    stamp = time.strftime("%Y%m%d-%H%M%S")
    out_dir = os.path.join(ROOT, "benches", "results", f"{info['host']}-{stamp}{'-quick' if q else ''}")
    os.makedirs(out_dir, exist_ok=True)
    save_json(os.path.join(out_dir, "results.json"), res)
    with open(os.path.join(out_dir, "report.md"), "w") as f:
        f.write(render(res))
    print(f"\nwrote {out_dir}/report.md and results.json ({res['runtime_s']:.0f}s)")


if __name__ == "__main__":
    main()
