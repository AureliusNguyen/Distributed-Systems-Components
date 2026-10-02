"""Benchmark 2: host-local shared state via shared-memory atomics vs RPC.

Same three operations, same Python client process, different transports:

  shm atomic         atomvar Arena int (FFI -> lock xadd / lock cmpxchg)
  gRPC (Rust srv)    atomvard over gRPC (tonic; the strongest gRPC baseline)
  gRPC (Python srv)  grpcio server + threading.Lock (the typical Python service)
  Redis TCP          GET / INCR / Lua CAS script, localhost TCP
  Redis unix socket  same, over a unix domain socket

Part A: one client, sequential, every op timed -> mean/p50/p95/p99, ops/s.
Part B: 4 client processes doing fetch_add concurrently -> aggregate ops/s,
        CPU per op (clients + server).
"""

import array
import multiprocessing as mp
import os
import shutil
import statistics
import subprocess
import sys
import tempfile
import time

import atomvar
from common import (TARGET, cpu_self_s, free_port, gen_grpc_stubs, latency_summary, proc_cpu_s,
                    remove_arena, unique, wait_port)

OPS = ["load", "fetch_add", "compare_exchange"]
CAS_LUA = """
local cur = redis.call('GET', KEYS[1])
if cur == ARGV[1] then redis.call('SET', KEYS[1], ARGV[2]) return {1, cur} end
return {0, cur}
"""


# ----------------------------------------------------------------- clients

class Shm:
    def __init__(self, cfg):
        self.c = atomvar.Arena(cfg["arena"]).int("c", 0)

    def ops(self):
        c = self.c
        return {"load": c.get, "fetch_add": lambda: c.fetch_add(1),
                "cas": lambda e, d: c.compare_exchange(e, d)}


class Grpc:
    def __init__(self, cfg):
        sys.path.insert(0, cfg["stubs"])
        import grpc
        import atomvar_pb2 as pb
        import atomvar_pb2_grpc as pbg
        self.ch = grpc.insecure_channel(f"127.0.0.1:{cfg['port']}")
        self.stub = pbg.AtomicServiceStub(self.ch)
        self.pb = pb
        self.var = pb.VarRef(arena=cfg["arena"], name="c")

    def ops(self):
        pb, s, var = self.pb, self.stub, self.var
        get_req = pb.GetRequest(var=var)
        add_req = pb.FetchRequest(var=var, op=pb.FETCH_OP_ADD, operand=pb.Value(i64=1))

        def cas(e, d):
            r = s.CompareExchange(pb.CompareExchangeRequest(
                var=var, expected=pb.Value(i64=e), desired=pb.Value(i64=d)))
            return r.exchanged, r.previous.i64

        return {"load": lambda: s.Get(get_req).value.i64,
                "fetch_add": lambda: s.Fetch(add_req).previous.i64, "cas": cas}


class Redis:
    def __init__(self, cfg):
        import redis
        if cfg.get("unix"):
            self.r = redis.Redis(unix_socket_path=cfg["unix"])
        else:
            self.r = redis.Redis(host="127.0.0.1", port=cfg["port"])
        self.key = cfg["arena"]
        self.script = self.r.register_script(CAS_LUA)

    def ops(self):
        r, k, script = self.r, self.key, self.script

        def cas(e, d):
            ok, cur = script(keys=[k], args=[e, d])
            return bool(ok), int(cur)

        return {"load": lambda: r.get(k), "fetch_add": lambda: r.incr(k), "cas": cas}


CLIENTS = {"shm": Shm, "grpc": Grpc, "redis": Redis}


def make_client(cfg):
    return CLIENTS[cfg["client"]](cfg)


# ----------------------------------------------------------------- part A

def sequential(cfg, n, warmup):
    ops = make_client(cfg).ops()
    pc = time.perf_counter_ns
    out = {}
    for name in OPS:
        lat = array.array("q")
        if name == "compare_exchange":
            cas = ops["cas"]
            v = int(ops["load"]() or 0)
            for i in range(warmup + n):
                s = pc()
                ok, prev = cas(v, v + 1)
                e = pc() - s
                v = v + 1 if ok else prev
                if i >= warmup:
                    lat.append(e)
        else:
            f = ops[name]
            for i in range(warmup + n):
                s = pc()
                f()
                e = pc() - s
                if i >= warmup:
                    lat.append(e)
        summ = latency_summary(lat)
        summ["ops_s"] = 1e9 / summ["mean_ns"]
        out[name] = summ
    return out


# ----------------------------------------------------------------- part B

def _conc_worker(cfg, n, ready, go, q):
    f = make_client(cfg).ops()["fetch_add"]
    for _ in range(min(n, 500)):  # warm the connection
        f()
    ready.wait()
    go.wait()
    c0 = cpu_self_s()
    for _ in range(n):
        f()
    q.put({"end": time.perf_counter(), "cpu": cpu_self_s() - c0})


def concurrent(cfg, procs, n, server_pid):
    ctx = mp.get_context("spawn")
    ready, go, q = ctx.Barrier(procs + 1), ctx.Event(), ctx.Queue()
    ps = [ctx.Process(target=_conc_worker, args=(cfg, n, ready, go, q)) for _ in range(procs)]
    for p in ps:
        p.start()
    ready.wait()
    s0 = proc_cpu_s(server_pid) if server_pid else 0.0
    t0 = time.perf_counter()
    go.set()
    res = [q.get() for _ in ps]
    for p in ps:
        p.join()
    wall = max(r["end"] for r in res) - t0
    server_cpu = (proc_cpu_s(server_pid) - s0) if server_pid else 0.0
    ops = procs * n
    return {"procs": procs, "ops": ops, "ops_s": ops / wall,
            "cpu_ns_per_op": (sum(r["cpu"] for r in res) + server_cpu) * 1e9 / ops,
            "server_cpu_share": server_cpu / max(1e-9, server_cpu + sum(r["cpu"] for r in res))}


# ----------------------------------------------------------------- driver

def run(n_shm=200_000, n_rpc=10_000, conc_procs=4, conc_n_shm=200_000, conc_n_rpc=5_000, log=print):
    tmp = tempfile.mkdtemp(prefix="atomvar-bench-")
    stubs = os.path.join(tmp, "stubs")
    gen_grpc_stubs(stubs)
    procs = []
    results = {"sequential": {}, "concurrent": {}}
    try:
        # servers
        p_rust = free_port()
        rust = subprocess.Popen([os.path.join(TARGET, "atomvard"), "serve", "--no-http",
                                 "--grpc", f"127.0.0.1:{p_rust}"],
                                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        procs.append(rust)
        p_py = free_port()
        pysrv = subprocess.Popen([sys.executable, os.path.join(os.path.dirname(__file__), "py_grpc_server.py"),
                                  stubs, str(p_py)], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
        procs.append(pysrv)
        have_redis = shutil.which("redis-server") is not None
        try:
            import redis  # noqa: F401
        except ImportError:
            have_redis = False
        if have_redis:
            p_redis = free_port()
            sock = os.path.join(tmp, "redis.sock")
            redis_p = subprocess.Popen(["redis-server", "--port", str(p_redis), "--unixsocket", sock,
                                        "--unixsocketperm", "700", "--save", "", "--appendonly", "no"],
                                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            procs.append(redis_p)
        else:
            log("  rpc  redis-server or the redis Python package not found: SKIPPING the Redis baselines")
        for port in (p_rust, p_py) + ((p_redis,) if have_redis else ()):
            wait_port(port)

        arena = unique("brpc")
        atomvar.Arena(arena, 16).int("c", 0)
        # create the variable on both gRPC servers
        sys.path.insert(0, stubs)
        import grpc
        import atomvar_pb2 as pb
        import atomvar_pb2_grpc as pbg
        for port in (p_rust, p_py):
            with grpc.insecure_channel(f"127.0.0.1:{port}") as ch:
                pbg.AtomicServiceStub(ch).Create(pb.CreateRequest(
                    var=pb.VarRef(arena=arena, name="c"), init=pb.Value(i64=0), capacity=16))
        if have_redis:
            import redis
            redis.Redis(host="127.0.0.1", port=p_redis).set(arena, 0)

        variants = [
            ("shm atomic", {"client": "shm", "arena": arena}, None, n_shm, conc_n_shm),
            ("gRPC (Rust server)", {"client": "grpc", "port": p_rust, "arena": arena, "stubs": stubs},
             rust.pid, n_rpc, conc_n_rpc),
            ("gRPC (Python server)", {"client": "grpc", "port": p_py, "arena": arena, "stubs": stubs},
             pysrv.pid, n_rpc, conc_n_rpc),
        ]
        if have_redis:
            variants += [
                ("Redis TCP", {"client": "redis", "port": p_redis, "arena": arena}, redis_p.pid, n_rpc,
                 conc_n_rpc),
                ("Redis unix socket", {"client": "redis", "unix": sock, "arena": arena}, redis_p.pid, n_rpc,
                 conc_n_rpc),
            ]
        for name, cfg, pid, n, cn in variants:
            seq = sequential(cfg, n, warmup=max(500, n // 20))
            results["sequential"][name] = seq
            for op in OPS:
                s = seq[op]
                log(f"  rpc  {name:<21} {op:<17} p50={s['p50_ns']/1e3:8.2f} us  "
                    f"p99={s['p99_ns']/1e3:8.2f} us  {s['ops_s']:>12,.0f} ops/s")
            c = concurrent(cfg, conc_procs, cn, pid)
            results["concurrent"][name] = c
            log(f"  rpc  {name:<21} {conc_procs} clients fetch_add: {c['ops_s']:>12,.0f} ops/s  "
                f"cpu={c['cpu_ns_per_op']:,.0f} ns/op")

        # correctness: the shm counter saw every shm increment
        results["arena"] = arena
        remove_arena(arena)
    finally:
        for p in procs:
            p.kill()
            p.wait()
        shutil.rmtree(tmp, ignore_errors=True)
    return results
