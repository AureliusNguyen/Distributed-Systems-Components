"""Benchmark 4: one AtomicI64 shared by Python, Go, C++ and Rust processes.

Each language's worker does N fetch_add(1) on the same arena variable; the
final value must be exactly 4 * N. The same increments are then pushed through
atomvard's gRPC API by 4 Rust (tonic) client processes for comparison.
"""

import json
import os
import shutil
import subprocess
import sys
import tempfile
import time

import atomvar
from common import HERE, ROOT, TARGET, free_port, remove_arena, unique, wait_port

XL = os.path.join(HERE, "crosslang")
INCLUDE = os.path.join(ROOT, "crates", "atomvar-ffi", "include")


def build(log=print):
    """Builds the C++, Go and Rust workers; returns {lang: argv prefix}."""
    out = tempfile.mkdtemp(prefix="atomvar-xl-")
    env = dict(os.environ, CARGO_TARGET_DIR=os.path.dirname(TARGET))
    subprocess.run(["cargo", "build", "--release", "-q", "-p", "atomvar-ffi", "-p", "atomvar-core",
                    "--example", "xl_incr"], cwd=ROOT, env=env, check=True)
    subprocess.run(["cargo", "build", "--release", "-q", "-p", "atomvard", "--example", "grpc_incr",
                    "--bin", "atomvard"], cwd=ROOT, env=env, check=True)
    cpp = os.path.join(out, "worker_cpp")
    subprocess.run(["g++", "-O2", "-std=c++17", "-I", INCLUDE, os.path.join(XL, "worker.cpp"),
                    "-L", TARGET, f"-Wl,-rpath,{TARGET}", "-latomvar", "-o", cpp], check=True)
    workers = {
        "Python": [sys.executable, os.path.join(XL, "worker.py")],
        "C++": [cpp],
        "Rust": [os.path.join(TARGET, "examples", "xl_incr")],
    }
    if shutil.which("go"):
        gobin = os.path.join(out, "worker_go")
        genv = dict(os.environ, CGO_ENABLED="1", CGO_CFLAGS=f"-I{INCLUDE}",
                    CGO_LDFLAGS=f"-L{TARGET} -Wl,-rpath,{TARGET}")
        subprocess.run(["go", "build", "-o", gobin, "."], cwd=os.path.join(XL, "go"), env=genv, check=True)
        workers["Go"] = [gobin]
    else:
        log("  crosslang: Go not found: SKIPPING the Go worker")
    log(f"  crosslang: built workers for {', '.join(workers)}")
    return workers


def _race(cmds, go_file):
    """Starts all workers, waits for 'ready' from each, releases them together."""
    ps = [subprocess.Popen(c, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True) for c in cmds]
    for p in ps:
        line = p.stdout.readline().strip()
        if line != "ready":
            raise RuntimeError(f"worker failed to start: {line!r} {p.stderr.read()}")
    t0 = time.perf_counter()
    open(go_file, "w").close()
    outs = []
    for p in ps:
        out, err = p.communicate()
        if p.returncode != 0:
            raise RuntimeError(err)
        outs.append(json.loads(out.strip().splitlines()[-1]))
    wall = time.perf_counter() - t0  # includes process exit: conservative
    os.remove(go_file)
    return wall, outs


def run(n=1_000_000, n_grpc=20_000, log=print):
    workers = build(log)
    arena = unique("xl")
    atomvar.Arena(arena, 16).int("counter", 0)
    go_file = os.path.join(tempfile.gettempdir(), f"{arena}.go")
    res = {}
    try:
        # shared memory, 4 languages at once
        k = len(workers)
        cmds = [w + [arena, "counter", str(n), go_file] for w in workers.values()]
        wall, outs = _race(cmds, go_file)
        final = atomvar.Arena(arena).open_int("counter").get()
        res["shm"] = {
            "languages_count": k,
            "per_process_ops": n,
            "expected": k * n,
            "final": final,
            "exact": final == k * n,
            "wall_s": wall,
            "aggregate_ops_s": k * n / wall,
            "languages": {o["lang"]: {"elapsed_s": o["elapsed_ns"] / 1e9,
                                      "ns_per_op": o["elapsed_ns"] / o["ops"]} for o in outs},
        }
        log(f"  crosslang shm: {k} languages x {n:,} -> final {final:,} "
            f"({'EXACT' if final == k * n else 'WRONG'}), wall {wall:.2f}s, "
            f"{k * n / wall / 1e6:.2f} M ops/s")
        for lang, d in res["shm"]["languages"].items():
            log(f"    {lang:<7} {d['ns_per_op']:7.1f} ns/op over its own run")

        # same increments through gRPC (4 tonic clients -> atomvard)
        port = free_port()
        srv = subprocess.Popen([os.path.join(TARGET, "atomvard"), "serve", "--no-http", "--grpc",
                                f"127.0.0.1:{port}"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            wait_port(port)
            g_arena = unique("xlg")
            atomvar.Arena(g_arena, 16).int("counter", 0)
            gbin = os.path.join(TARGET, "examples", "grpc_incr")
            cmds = [[gbin, str(port), g_arena, "counter", str(n_grpc), go_file] for _ in range(4)]
            wall_g, _ = _race(cmds, go_file)
            final_g = atomvar.Arena(g_arena).open_int("counter").get()
            remove_arena(g_arena)
        finally:
            srv.kill()
            srv.wait()
        res["grpc"] = {
            "per_process_ops": n_grpc,
            "expected": 4 * n_grpc,
            "final": final_g,
            "exact": final_g == 4 * n_grpc,
            "wall_s": wall_g,
            "aggregate_ops_s": 4 * n_grpc / wall_g,
        }
        res["speedup_shm_vs_grpc"] = res["shm"]["aggregate_ops_s"] / res["grpc"]["aggregate_ops_s"]
        log(f"  crosslang gRPC: 4 clients x {n_grpc:,} -> {res['grpc']['aggregate_ops_s']:,.0f} ops/s "
            f"({'EXACT' if res['grpc']['exact'] else 'WRONG'}); shm is "
            f"{res['speedup_shm_vs_grpc']:,.0f}x the throughput")
    finally:
        remove_arena(arena)
    return res
