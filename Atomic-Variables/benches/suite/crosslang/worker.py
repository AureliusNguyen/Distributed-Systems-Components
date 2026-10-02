"""Python worker for the cross-language benchmark.
    python worker.py <arena> <var> <n> <go-file>
"""

import os
import sys
import time

import atomvar

arena, var, n, go = sys.argv[1], sys.argv[2], int(sys.argv[3]), sys.argv[4]
add = atomvar.Arena(arena).int(var, 0).fetch_add
print("ready", flush=True)
while not os.path.exists(go):
    pass
t0 = time.perf_counter_ns()
for _ in range(n):
    add(1)
el = time.perf_counter_ns() - t0
print(f'{{"lang": "Python", "ops": {n}, "elapsed_ns": {el}}}', flush=True)
