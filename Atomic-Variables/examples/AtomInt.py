import threading
import time

from atomvar import AtomicInt

THREADS = 4
PER_THREAD = 250_000

counter = AtomicInt(0)


def work():
    add = counter.fetch_add
    for i in range(PER_THREAD):
        add(1)  # atomic: never loses an update

start = time.time()
threads = [threading.Thread(target=work) for _ in range(THREADS)]
for t in threads:
    t.start()
for t in threads:
    t.join()
elapsed = time.time() - start

total = THREADS * PER_THREAD
print(f"AtomicInt: counter = {counter.get():,} (expected {total:,})")
print(f"Time: {elapsed:.3f}s  ->  {elapsed * 1e9 / total:.0f}ns per increment")
