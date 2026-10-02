"""atomvar: lock-free atomic variables for Python.

In-process (shared between threads):

    from atomvar import AtomicInt
    hits = AtomicInt(0)
    hits.increment_and_get()

Cross-process (any process, any language, same name = same variable):

    from atomvar import Arena
    counter = Arena("jobs").int("counter", 0)
    counter.fetch_add(1)

Guarantees: value operations are atomic and lock-free (hardware atomics, no
fallback locks); they are not wait-free or single-instruction. The default
ordering is SEQ_CST (Java AtomicLong semantics). Arena/registry calls take an
OS lock. Start worker processes with "spawn" or "forkserver"; a plain fork()
child gets ForkedProcessError from arena calls.
"""

from ._atomvar import (
    DEFAULT_CAPACITY,
    Arena,
    ArenaFullError,
    AtomicBool,
    AtomicFloat,
    AtomicInt,
    AtomicU128,
    AtomicUInt,
    AtomvarError,
    ForkedProcessError,
    InvalidNameError,
    InvalidOrderingError,
    LayoutMismatchError,
    NotFoundError,
    Ordering,
    TypeMismatchError,
    UnsupportedCpuError,
    cpu_supported,
)

__all__ = [
    "DEFAULT_CAPACITY",
    "Arena",
    "ArenaFullError",
    "AtomicBool",
    "AtomicFloat",
    "AtomicInt",
    "AtomicU128",
    "AtomicUInt",
    "AtomvarError",
    "ForkedProcessError",
    "InvalidNameError",
    "InvalidOrderingError",
    "LayoutMismatchError",
    "NotFoundError",
    "Ordering",
    "TypeMismatchError",
    "UnsupportedCpuError",
    "cpu_supported",
]
