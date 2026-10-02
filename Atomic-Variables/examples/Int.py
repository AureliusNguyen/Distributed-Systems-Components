import threading
import time

THREADS = 4
PER_THREAD = 250_000

counter = 0
lock = threading.Lock()


def work():
    global counter
    for i in range(PER_THREAD):
        with lock:  # without the lock, updates can be lost
            counter += 1

start = time.time()
threads = [threading.Thread(target=work) for _ in range(THREADS)]
for t in threads:
    t.start()
for t in threads:
    t.join()
elapsed = time.time() - start

total = THREADS * PER_THREAD
print(f"int + threading.Lock: counter = {counter:,} (expected {total:,})")
print(f"Time: {elapsed:.3f}s  ->  {elapsed * 1e9 / total:.0f}ns per increment")
