"""Top-level worker functions for multiprocessing tests (must be importable by
spawn/forkserver children)."""

import atomvar


def incr(arena_name: str, var: str, n: int) -> int:
    c = atomvar.Arena(arena_name).int(var, 0)
    for _ in range(n):
        c.fetch_add(1)
    return n


def create_many(arena_name: str, names: list) -> None:
    arena = atomvar.Arena(arena_name)
    for name in names:
        arena.int(name, 0).fetch_add(1)
