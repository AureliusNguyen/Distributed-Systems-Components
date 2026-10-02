"""Normal Python shared counters vs atomvar: speed AND correctness.

    python benches/python_compare.py [--quick]

Every variant increments one shared counter `ops` times in total, split across
N workers (threads or processes).

Timing: every worker is started and parked first; the parent records the start
time, THEN releases them; each worker records its own finish time. Wall time =
latest finish - start, so it excludes thread/process startup and shutdown.

Reported per cell:
- ns/incr (aggregate) = wall time / total increments. This is the group's
  throughput, not the latency of one call (with 1 worker the two coincide).
- the MEDIAN wall time over the repeats, and the WORST run's lost updates
  (expected - final). A variant that loses updates is fast but wrong.

Results hold for the implementations tested, on this workload and machine.

Thread variants (one process):
  plain int            box[0] += 1              no protection (read-modify-write race)
  threading.Lock       with lock: box[0] += 1
  AtomicInt            c.fetch_add(1)            in-process atomic
  Arena int            c.fetch_add(1)            shared-memory atomic

Process variants (spawned workers):
  mp.Value no lock     v.value += 1              shared memory, no protection
  mp.Value + lock      with v.get_lock(): ...    shared memory + semaphore
  Manager().Value      v.value += 1 via proxy    server process (also racy)
  Arena int            c.fetch_add(1)            shared-memory atomic
"""

import argparse
import multiprocessing as mp
import os
import statistics
import sys
import threading
import time
import uuid

import atomvar

# ---------------------------------------------------------------- threads


def run_threads(kind, workers, ops, arena_name):
    per = ops // workers
    box = [0]
    lock = threading.Lock()
    atomic = atomvar.AtomicInt(0)
    shm = atomvar.Arena(arena_name).int(f"t-{uuid.uuid4().hex[:8]}", 0)

    def plain(n):
        for _ in range(n):
            box[0] += 1

    def locked(n):
        for _ in range(n):
            with lock:
                box[0] += 1

    def atom(n):
        f = atomic.fetch_add
        for _ in range(n):
            f(1)

    def arena(n):
        f = shm.fetch_add
        for _ in range(n):
            f(1)

    fn, read = {
        "plain int": (plain, lambda: box[0]),
        "threading.Lock": (locked, lambda: box[0]),
        "AtomicInt": (atom, atomic.get),
        "Arena int": (arena, shm.get),
    }[kind]
    ready = threading.Barrier(workers + 1)
    go = threading.Event()
    ends = [0.0] * workers

    def body(i):
        ready.wait()
        go.wait()
        fn(per)
        ends[i] = time.perf_counter()

    ts = [threading.Thread(target=body, args=(i,)) for i in range(workers)]
    for t in ts:
        t.start()
    ready.wait()  # all parked
    t0 = time.perf_counter()  # start BEFORE release
    go.set()
    for t in ts:
        t.join()
    return max(ends) - t0, read(), per * workers


# ---------------------------------------------------------------- processes
# Workers are top-level functions so spawn can import them.


def _proc_worker(kind, n, shared, ready, go, ends, i, arena_name, var):
    # time.perf_counter is CLOCK_MONOTONIC on Linux: comparable across processes.
    if kind == "Arena int":
        f = atomvar.Arena(arena_name).int(var, 0).fetch_add
        ready.wait()
        go.wait()
        for _ in range(n):
            f(1)
    elif kind == "mp.Value no lock":
        ready.wait()
        go.wait()
        for _ in range(n):
            shared.value += 1
    elif kind == "mp.Value + lock":
        lock = shared.get_lock()
        ready.wait()
        go.wait()
        for _ in range(n):
            with lock:
                shared.value += 1
    elif kind == "Manager().Value":
        ready.wait()
        go.wait()
        for _ in range(n):
            shared.value += 1
    ends[i] = time.perf_counter()


def run_processes(kind, workers, ops, arena_name, ctx, manager):
    per = ops // workers
    var = f"p-{uuid.uuid4().hex[:8]}"
    shared = None
    if kind == "mp.Value no lock":
        shared = ctx.Value("q", 0, lock=False)
    elif kind == "mp.Value + lock":
        shared = ctx.Value("q", 0, lock=True)
    elif kind == "Manager().Value":
        shared = manager.Value("q", 0)
    else:
        atomvar.Arena(arena_name).int(var, 0)
    ready = ctx.Barrier(workers + 1)
    go = ctx.Event()
    ends = ctx.Array("d", workers, lock=False)
    ps = [
        ctx.Process(target=_proc_worker, args=(kind, per, shared, ready, go, ends, i, arena_name, var))
        for i in range(workers)
    ]
    for p in ps:
        p.start()
    ready.wait()  # every worker is started and parked
    t0 = time.perf_counter()  # start BEFORE release
    go.set()
    for p in ps:
        p.join()
    wall = max(ends) - t0  # excludes process shutdown
    if kind == "Arena int":
        final = atomvar.Arena(arena_name).open_int(var).get()
    else:
        final = shared.value
    return wall, final, per * workers


# ---------------------------------------------------------------- driver


def measure(run, repeats):
    walls, lost = [], []
    for _ in range(repeats):
        wall, final, expected = run()
        walls.append(wall)
        lost.append(expected - final)
    wall = statistics.median(walls)
    return wall, expected, max(lost)


def row(label, workers, wall, expected, lost):
    ns = wall * 1e9 / expected
    pct = 100.0 * lost / expected
    verdict = "observed exact" if lost == 0 else f"LOST {lost:,} ({pct:.1f}%)"
    print(f"| {label:<18} | {workers:>7} | {expected:>9,} | {ns:>9.0f} | {expected / wall / 1e6:>8.2f} | {verdict:<22} |")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--quick", action="store_true", help="fewer ops/repeats")
    args = ap.parse_args()
    ops = 200_000 if args.quick else 1_000_000
    repeats = 3 if args.quick else 5
    worker_counts = [1, 2, 4]

    gil = getattr(sys, "_is_gil_enabled", lambda: True)()
    print(f"Python {sys.version.split()[0]} | GIL enabled: {gil} | CPUs: {os.cpu_count()} | "
          f"median of {repeats} runs\n"
          "* aggregate: wall time / total increments (throughput, not per-call latency)\n"
          "  'observed exact' = no loss seen in these runs; not a synchronization guarantee\n")
    arena_name = f"bench-cmp-{os.getpid()}"
    atomvar.Arena(arena_name, 4096)
    header = ("| variant            | workers |       ops | ns/incr * | M incr/s | lost (worst run)       |\n"
              "|--------------------|---------|-----------|-----------|----------|------------------------|")
    try:
        print("Threads (one process)\n")
        print(header)
        for kind in ["plain int", "threading.Lock", "AtomicInt", "Arena int"]:
            for w in worker_counts:
                row(kind, w, *measure(lambda: run_threads(kind, w, ops, arena_name), repeats))

        print("\nProcesses (spawn)\n")
        print(header)
        ctx = mp.get_context("spawn")
        with ctx.Manager() as manager:
            for kind in ["mp.Value no lock", "mp.Value + lock", "Manager().Value", "Arena int"]:
                # The Manager proxy does a round trip per access: use fewer ops.
                n = ops // 50 if kind == "Manager().Value" else ops
                for w in worker_counts:
                    row(kind, w, *measure(
                        lambda: run_processes(kind, w, n, arena_name, ctx, manager), repeats))
    finally:
        try:
            os.remove(f"/dev/shm/atomvar-{arena_name}")
        except FileNotFoundError:
            pass


if __name__ == "__main__":
    main()
