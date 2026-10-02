"""Benchmark 1 + 3: atomic vs mutex across PROCESSES, and contention scaling.

N spawned processes all increment ONE shared counter.

  atomic         atomvar Arena int: c.fetch_add(1)              (lock xadd)
  mp.Value+lock  multiprocessing.Value('q', lock=True):  with v.get_lock(): v.value += 1
  shm+mp.Lock    multiprocessing.shared_memory + multiprocessing.Lock

Two runs per cell:
- throughput run: tight loop, no per-op timing. Clock starts before the parked
  workers are released; each worker records its own finish time.
- latency run: every op timed with perf_counter_ns (timer overhead reported
  separately), giving mean / p50 / p95 / p99 per op.
CPU: user+sys CPU seconds of the workers / total ops.
"""

import array
import multiprocessing as mp
import statistics
import time
from multiprocessing import shared_memory

import atomvar
from common import cpu_self_s, latency_summary, remove_arena, unique

KINDS = ["atomic", "mp.Value+lock", "shm+mp.Lock"]


def _worker(kind, mode, n, arena, shm_name, shared, lock, ready, go, out_q):
    pc = time.perf_counter_ns
    shm = None
    if kind == "atomic":
        add = atomvar.Arena(arena).int("c", 0).fetch_add
    elif kind == "mp.Value+lock":
        lk = shared.get_lock()
    else:
        shm = shared_memory.SharedMemory(name=shm_name)
        buf = shm.buf.cast("q")
    lat = array.array("q")
    ready.wait()
    go.wait()
    c0 = cpu_self_s()
    t0 = time.perf_counter()
    if mode == "throughput":
        if kind == "atomic":
            for _ in range(n):
                add(1)
        elif kind == "mp.Value+lock":
            for _ in range(n):
                with lk:
                    shared.value += 1
        else:
            for _ in range(n):
                with lock:
                    buf[0] += 1
    else:
        if kind == "atomic":
            for _ in range(n):
                s = pc()
                add(1)
                lat.append(pc() - s)
        elif kind == "mp.Value+lock":
            for _ in range(n):
                s = pc()
                with lk:
                    shared.value += 1
                lat.append(pc() - s)
        else:
            for _ in range(n):
                s = pc()
                with lock:
                    buf[0] += 1
                lat.append(pc() - s)
    t1 = time.perf_counter()
    out_q.put({"end": t1, "elapsed": t1 - t0, "cpu": cpu_self_s() - c0,
               "lat": lat.tobytes() if mode == "latency" else None})
    if shm is not None:
        del buf
        shm.close()


def run_cell(ctx, kind, procs, n, mode):
    arena = unique("bsync")
    shm = shared = lock = None
    shm_name = None
    if kind == "atomic":
        atomvar.Arena(arena, 16).int("c", 0)
    elif kind == "mp.Value+lock":
        shared = ctx.Value("q", 0, lock=True)
    else:
        shm = shared_memory.SharedMemory(create=True, size=8)
        shm.buf[:8] = bytes(8)
        shm_name = shm.name
        lock = ctx.Lock()
    ready, go, q = ctx.Barrier(procs + 1), ctx.Event(), ctx.Queue()
    ps = [ctx.Process(target=_worker, args=(kind, mode, n, arena, shm_name, shared, lock, ready, go, q))
          for _ in range(procs)]
    for p in ps:
        p.start()
    ready.wait()
    t0 = time.perf_counter()
    go.set()
    results = [q.get() for _ in ps]
    for p in ps:
        p.join()
    wall = max(r["end"] for r in results) - t0
    if kind == "atomic":
        final = atomvar.Arena(arena).open_int("c").get()
        remove_arena(arena)
    elif kind == "mp.Value+lock":
        final = shared.value
    else:
        final = shm.buf.cast("q")[0]
        shm.close()
        shm.unlink()
    lat = []
    if mode == "latency":
        for r in results:
            a = array.array("q")
            a.frombytes(r["lat"])
            lat.extend(a)
    return {
        "wall_s": wall,
        "cpu_s": sum(r["cpu"] for r in results),
        "ops": procs * n,
        "correct": final == procs * n,
        "lat": lat,
    }


def run(procs_list=(1, 2, 4, 8, 16), n=100_000, n_lat=20_000, repeats=3, log=print):
    ctx = mp.get_context("spawn")
    rows = []
    for kind in KINDS:
        for p in procs_list:
            tps, cpus, correct = [], [], True
            for _ in range(repeats):
                r = run_cell(ctx, kind, p, n, "throughput")
                tps.append(r["ops"] / r["wall_s"])
                cpus.append(r["cpu_s"] * 1e9 / r["ops"])
                correct &= r["correct"]
            lr = run_cell(ctx, kind, p, n_lat, "latency")
            correct &= lr["correct"]
            row = {
                "kind": kind,
                "procs": p,
                "ops_per_run": p * n,
                "throughput_ops_s": statistics.median(tps),
                "cpu_ns_per_op": statistics.median(cpus),
                "correct": correct,
                **{k: v for k, v in latency_summary(lr["lat"]).items()},
            }
            rows.append(row)
            log(f"  sync {kind:<14} procs={p:>2}  {row['throughput_ops_s']/1e6:7.2f} M ops/s  "
                f"p50={row['p50_ns']:>7.0f} p99={row['p99_ns']:>9.0f} ns  "
                f"cpu={row['cpu_ns_per_op']:>7.0f} ns/op  {'exact' if correct else 'WRONG'}")
    return rows
