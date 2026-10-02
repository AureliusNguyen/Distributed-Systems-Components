import math
import multiprocessing as mp
import os
import struct
import sys
import threading
import time
import uuid

import pytest

import atomvar
from atomvar import (
    Arena,
    AtomicBool,
    AtomicFloat,
    AtomicInt,
    AtomicU128,
    AtomicUInt,
    Ordering,
)

sys.path.insert(0, os.path.dirname(__file__))
import mp_workers  # noqa: E402

I64_MAX = 2**63 - 1
I64_MIN = -(2**63)
U64_MAX = 2**64 - 1


@pytest.fixture
def arena_name():
    name = f"pytest-{os.getpid()}-{uuid.uuid4().hex[:8]}"
    yield name
    try:
        os.remove(f"/dev/shm/atomvar-{name}")
    except FileNotFoundError:
        pass


# --------------------------------------------------------------------------
# Value semantics
# --------------------------------------------------------------------------


def test_int_ops_and_wrapping():
    c = AtomicInt(I64_MAX)
    assert c.fetch_add(1) == I64_MAX
    assert c.get() == I64_MIN  # wraps like Java
    assert c.decrement_and_get() == I64_MAX
    c.set(10)
    assert c.get_and_set(20) == 10
    assert c.compare_and_set(20, 30) is True
    assert c.compare_and_set(20, 40) is False
    assert c.compare_exchange(30, 31) == (True, 30)
    assert c.compare_exchange(30, 32) == (False, 31)
    assert c.add_and_get(9) == 40
    assert c.get_and_increment() == 40
    assert c.value == 41
    assert c.fetch_max(100) == 41 and c.fetch_min(-1) == 100 and c.value == -1
    with pytest.raises(OverflowError):
        c.set(I64_MAX + 1)


def test_uint_wraps():
    u = AtomicUInt(U64_MAX)
    assert u.add_and_get(1) == 0
    assert u.sub_and_get(1) == U64_MAX
    with pytest.raises(OverflowError):
        AtomicUInt(-1)


def test_orderings_validated():
    c = AtomicInt()
    with pytest.raises(atomvar.InvalidOrderingError):
        c.get(Ordering.RELEASE)
    with pytest.raises(atomvar.InvalidOrderingError):
        c.set(1, Ordering.ACQUIRE)
    with pytest.raises(atomvar.InvalidOrderingError):
        c.compare_exchange(0, 1, Ordering.RELAXED, Ordering.SEQ_CST)
    # failure defaults to the strongest valid ordering for the success ordering
    assert c.compare_exchange(0, 1, Ordering.RELEASE) == (True, 0)
    assert c.fetch_add(1, Ordering.RELAXED) == 1
    assert c.get(Ordering.ACQUIRE) == 2


def test_bool_float_u128():
    b = AtomicBool()
    assert b.fetch_or(True) is False
    assert bool(b) is True
    assert b.compare_and_set(True, False)

    f = AtomicFloat(0.0)
    assert not f.compare_and_set(-0.0, 1.0)  # bitwise: -0.0 != +0.0
    assert f.compare_and_set(0.0, -0.0)
    assert f.get_bits() == struct.unpack("<Q", struct.pack("<d", -0.0))[0]
    f.set_bits(0x7FF8000000000001)
    assert math.isnan(f.get())
    assert not f.compare_and_set_bits(0x7FF8000000000002, 0)
    assert f.compare_and_set_bits(0x7FF8000000000001, 0)
    assert f.fetch_add(1.5) == 0.0 and f.add_and_get(1.0) == 2.5

    w = AtomicU128(2**128 - 1)
    assert w.get() == 2**128 - 1
    tagged = (7 << 64) | 42
    assert w.compare_and_set(2**128 - 1, tagged)
    assert w.compare_exchange(2**128 - 1, 0) == (False, tagged)


def test_threads_exact_total():
    c = AtomicInt(0)
    n_threads, per = 16, 100_000

    def work():
        for _ in range(per):
            c.fetch_add(1)

    ts = [threading.Thread(target=work) for _ in range(n_threads)]
    for t in ts:
        t.start()
    for t in ts:
        t.join()
    assert c.get() == n_threads * per


def test_forced_interleaving_lost_update_control():
    """Deterministic: both threads read before either writes."""
    barrier = threading.Barrier(2)

    plain = {"v": 0}

    def plain_incr():
        seen = plain["v"]  # read
        barrier.wait()  # both have read
        plain["v"] = seen + 1  # write back

    ts = [threading.Thread(target=plain_incr) for _ in range(2)]
    for t in ts:
        t.start()
    for t in ts:
        t.join()
    assert plain["v"] == 1  # one update lost, every time

    barrier.reset()
    atomic = AtomicInt(0)

    def atomic_incr():
        barrier.wait()  # same forced interleaving point
        atomic.fetch_add(1)  # read-modify-write is one atomic op

    ts = [threading.Thread(target=atomic_incr) for _ in range(2)]
    for t in ts:
        t.start()
    for t in ts:
        t.join()
    assert atomic.get() == 2


# --------------------------------------------------------------------------
# Arenas
# --------------------------------------------------------------------------


def test_arena_basics_and_errors(arena_name):
    a = Arena(arena_name, 4)
    assert a.capacity == 4 and len(a) == 0
    x = a.int("x", 5)
    assert a.int("x", 999).get() == 5  # init only on creation
    assert a.open_int("x").increment_and_get() == 6
    assert x.get() == 6
    assert a.lookup("x") == "i64" and a.lookup("nope") is None
    with pytest.raises(atomvar.TypeMismatchError):
        a.uint("x")
    with pytest.raises(atomvar.NotFoundError):
        a.open_float("missing")
    a.bool("b"), a.float("f"), a.u128("w")
    with pytest.raises(atomvar.ArenaFullError):
        a.int("overflow")
    assert sorted(a.list()) == [("b", "bool"), ("f", "f64"), ("w", "u128"), ("x", "i64")]
    with pytest.raises(atomvar.LayoutMismatchError):
        Arena(arena_name, 8)
    assert Arena(arena_name).capacity == 4
    with pytest.raises(atomvar.InvalidNameError):
        Arena("bad/name")
    with pytest.raises(atomvar.InvalidNameError):
        a.int("x" * 65)
    assert issubclass(atomvar.ArenaFullError, atomvar.AtomvarError)


@pytest.mark.parametrize("method", ["spawn", "forkserver"])
def test_multiprocessing_exact_total(arena_name, method):
    Arena(arena_name, 64).int("counter", 0)
    ctx = mp.get_context(method)
    workers, per = 8, 100_000
    with ctx.Pool(workers) as pool:
        results = [
            pool.apply_async(mp_workers.incr, (arena_name, "counter", per))
            for _ in range(workers)
        ]
        for r in results:
            r.get(timeout=120)
    assert Arena(arena_name).open_int("counter").get() == workers * per


@pytest.mark.parametrize("method", ["spawn", "forkserver"])
def test_multiprocessing_concurrent_creation_no_duplicates(arena_name, method):
    Arena(arena_name, 64)
    ctx = mp.get_context(method)
    names = [f"v{i}" for i in range(20)]
    workers = 8
    with ctx.Pool(workers) as pool:
        rs = [pool.apply_async(mp_workers.create_many, (arena_name, names)) for _ in range(workers)]
        for r in rs:
            r.get(timeout=120)
    listed = Arena(arena_name).list()
    assert sorted(n for n, _ in listed) == sorted(names)  # each exactly once
    for n in names:
        assert Arena(arena_name).open_int(n).get() == workers


def test_plain_fork_child_gets_forked_process_error(arena_name):
    arena = Arena(arena_name, 8)
    arena.int("pre", 0)
    pid = os.fork()
    if pid == 0:  # child: must fail fast, never hang
        code = 0
        try:
            Arena(arena_name)
            code |= 1
        except atomvar.ForkedProcessError:
            pass
        try:
            arena.int("new")
            code |= 2
        except atomvar.ForkedProcessError:
            pass
        os._exit(code)
    deadline = time.monotonic() + 10
    while True:
        done, status = os.waitpid(pid, os.WNOHANG)
        if done == pid:
            break
        if time.monotonic() > deadline:
            os.kill(pid, 9)
            os.waitpid(pid, 0)
            pytest.fail("forked child hung")
        time.sleep(0.01)
    assert os.WIFEXITED(status) and os.WEXITSTATUS(status) == 0


# --------------------------------------------------------------------------
# Blocking arena calls must release the GIL
# --------------------------------------------------------------------------


def _lock_in_child(path, hold_s):
    """Another PROCESS holds the arena's lock for hold_s seconds, then exits.
    Releasing from outside this interpreter keeps the test from deadlocking
    if the GIL is (wrongly) held by the blocked call."""
    import subprocess

    code = (
        "import fcntl, os, sys, time\n"
        "fd = os.open(sys.argv[1], os.O_RDWR | os.O_CREAT, 0o600)\n"
        "fcntl.flock(fd, fcntl.LOCK_EX)\n"
        "print('locked', flush=True)\n"
        "time.sleep(float(sys.argv[2]))\n"
    )
    child = subprocess.Popen([sys.executable, "-c", code, path, str(hold_s)], stdout=subprocess.PIPE)
    assert child.stdout.readline().strip() == b"locked"
    return child


def _assert_progress_while_blocked(blocked_call, path, hold_s=1.0):
    """blocked_call waits ~hold_s on the arena lock; an unrelated Python thread
    must keep running during that window (i.e. the GIL was released)."""
    child = _lock_in_child(path, hold_s)
    in_call = threading.Event()
    done = threading.Event()
    errors = []
    ticks = [0]

    def heartbeat():
        while not done.is_set():
            if in_call.is_set():
                ticks[0] += 1
            time.sleep(0.001)

    def blocked():
        in_call.set()
        try:
            blocked_call()
        except Exception as e:
            errors.append(e)
        finally:
            done.set()

    h = threading.Thread(target=heartbeat)
    h.start()
    t0 = time.monotonic()
    t = threading.Thread(target=blocked)
    t.start()
    t.join(15)
    elapsed = time.monotonic() - t0
    h.join(5)
    child.wait(5)
    assert done.is_set() and not errors, errors
    assert elapsed > hold_s / 2, f"call did not actually block ({elapsed:.2f}s)"
    assert ticks[0] > 50, f"unrelated thread made no progress while blocked ({ticks[0]} ticks): GIL held"


def test_arena_open_releases_gil_while_blocked(arena_name):
    _assert_progress_while_blocked(lambda: Arena(arena_name, 8), f"/dev/shm/atomvar-{arena_name}")
    assert Arena(arena_name).capacity == 8


def test_variable_creation_releases_gil_while_blocked(arena_name):
    arena = Arena(arena_name, 8)
    _assert_progress_while_blocked(lambda: arena.int("new", 3), f"/dev/shm/atomvar-{arena_name}")
    assert arena.open_int("new").get() == 3


# --------------------------------------------------------------------------
# OpenAPI schema accepts the documented inputs
# --------------------------------------------------------------------------


def test_openapi_value_schema_accepts_documented_inputs():
    yaml = pytest.importorskip("yaml")
    jsonschema = pytest.importorskip("jsonschema")
    root = os.path.join(os.path.dirname(__file__), "..", "..", "openapi.yaml")
    spec = yaml.safe_load(open(root))
    schema = dict(spec["components"]["schemas"]["OpBody"])
    schema["components"] = spec["components"]  # make "#/components/..." refs resolvable
    ok = [
        {"value": 1},
        {"value": "1"},
        {"value": 1.5},
        {"value": True},
        {"value": {"bits": "0x7ff8000000000001"}},
        {"type": "i64", "init": 5, "capacity": 16},
        {"expected": "10", "desired": 11, "success": "acq_rel", "failure": "acquire"},
    ]
    for body in ok:
        jsonschema.validate(body, schema)
    with pytest.raises(jsonschema.ValidationError):
        jsonschema.validate({"valu": 1}, schema)  # additionalProperties: false
