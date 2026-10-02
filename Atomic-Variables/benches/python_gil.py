"""Per-op cost of atomvar from Python, and the GIL hold-vs-release decision.

    python benches/python_gil.py

Compares (single thread, then N threads):
  - AtomicInt.fetch_add            (GIL held: the shipped behavior)
  - AtomicInt._fetch_add_detached  (GIL released around the op)
  - threading.Lock + int
  - Arena-backed AtomicInt.fetch_add (shared memory)
"""

import os
import sys
import threading
import time

import atomvar

N = 1_000_000


def per_op_ns(fn, n=N):
    t0 = time.perf_counter_ns()
    fn(n)
    return (time.perf_counter_ns() - t0) / n


def threaded_per_op_ns(make_fn, threads, n=N):
    per = n // threads
    fns = [make_fn() for _ in range(threads)]
    ts = [threading.Thread(target=f, args=(per,)) for f in fns]
    t0 = time.perf_counter_ns()
    for t in ts:
        t.start()
    for t in ts:
        t.join()
    return (time.perf_counter_ns() - t0) / (per * threads)


def main():
    gil = getattr(sys, "_is_gil_enabled", lambda: True)()
    print(f"Python {sys.version.split()[0]}  GIL enabled: {gil}  CPUs: {os.cpu_count()}")

    a = atomvar.AtomicInt(0)
    lock = threading.Lock()
    box = [0]
    name = f"bench-{os.getpid()}"
    shm = atomvar.Arena(name, 16).int("c", 0)

    def held(n):
        f = a.fetch_add
        for _ in range(n):
            f(1)

    def detached(n):
        f = a._fetch_add_detached
        for _ in range(n):
            f(1)

    def locked(n):
        for _ in range(n):
            with lock:
                box[0] += 1

    def shm_held(n):
        f = shm.fetch_add
        for _ in range(n):
            f(1)

    def empty(n):
        for _ in range(n):
            pass

    rows = [
        ("empty loop (baseline)", empty),
        ("AtomicInt.fetch_add (GIL held)", held),
        ("AtomicInt fetch_add (GIL released)", detached),
        ("threading.Lock + int", locked),
        ("Arena AtomicInt.fetch_add (shm)", shm_held),
    ]
    threads = 4
    print(f"{'variant':40s} {'1 thread ns/op':>15s} {f'{threads} threads ns/op':>17s}")
    for label, fn in rows:
        one = per_op_ns(fn)
        many = threaded_per_op_ns(lambda fn=fn: fn, threads)
        print(f"{label:40s} {one:15.1f} {many:17.1f}")

    assert a.get() > 0 and shm.get() > 0
    os.remove(f"/dev/shm/atomvar-{name}")


if __name__ == "__main__":
    main()
