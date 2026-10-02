"""Shared helpers for the benchmark suite (see benches/suite/README.md)."""

import json
import os
import platform
import resource
import socket
import statistics
import subprocess
import sys
import time
import uuid

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))  # Atomic-Variables/
TARGET = os.path.join(
    os.environ.get("CARGO_TARGET_DIR", os.path.expanduser("~/.cache/atomvar-target")), "release"
)
PROTO_DIR = os.path.join(ROOT, "proto")


def percentile(sorted_vals, p):
    """Nearest-rank percentile of an already sorted list."""
    if not sorted_vals:
        return float("nan")
    k = max(0, min(len(sorted_vals) - 1, int(round(p / 100.0 * len(sorted_vals) + 0.5)) - 1))
    return sorted_vals[k]


def latency_summary(samples_ns):
    s = sorted(samples_ns)
    return {
        "n": len(s),
        "mean_ns": statistics.fmean(s),
        "p50_ns": percentile(s, 50),
        "p95_ns": percentile(s, 95),
        "p99_ns": percentile(s, 99),
        "max_ns": s[-1],
    }


def cpu_self_s():
    r = resource.getrusage(resource.RUSAGE_SELF)
    return r.ru_utime + r.ru_stime


def proc_cpu_s(pid):
    """user+sys CPU seconds of another process (from /proc, 1/CLK_TCK resolution)."""
    with open(f"/proc/{pid}/stat") as f:
        fields = f.read().rsplit(")", 1)[1].split()
    ticks = int(fields[11]) + int(fields[12])  # utime, stime (fields 14, 15)
    return ticks / os.sysconf("SC_CLK_TCK")


def timer_overhead_ns(n=200_000):
    """Median cost of an empty perf_counter_ns() pair: included in every
    per-op latency sample, so it is reported alongside them."""
    pc = time.perf_counter_ns
    out = []
    for _ in range(n):
        a = pc()
        out.append(pc() - a)
    return statistics.median(out)


def machine_info():
    model = "unknown"
    try:
        with open("/proc/cpuinfo") as f:
            for line in f:
                if line.startswith("model name"):
                    model = line.split(":", 1)[1].strip()
                    break
    except OSError:
        pass
    return {
        "host": socket.gethostname(),
        "cpu_model": model,
        "logical_cpus": os.cpu_count(),
        "kernel": platform.release(),
        "python": sys.version.split()[0],
        "gil_enabled": getattr(sys, "_is_gil_enabled", lambda: True)(),
        "date": time.strftime("%Y-%m-%d %H:%M:%S"),
    }


def unique(tag):
    return f"{tag}-{os.getpid()}-{uuid.uuid4().hex[:6]}"


def remove_arena(name):
    try:
        os.remove(f"/dev/shm/atomvar-{name}")
    except FileNotFoundError:
        pass


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def wait_port(port, timeout=15):
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=0.2):
                return
        except OSError:
            time.sleep(0.05)
    raise RuntimeError(f"port {port} did not open")


def gen_grpc_stubs(out_dir):
    """Generates atomvar_pb2(_grpc).py from proto/atomvar.proto into out_dir."""
    os.makedirs(out_dir, exist_ok=True)
    subprocess.run(
        [sys.executable, "-m", "grpc_tools.protoc", f"-I{PROTO_DIR}", f"--python_out={out_dir}",
         f"--grpc_python_out={out_dir}", os.path.join(PROTO_DIR, "atomvar.proto")],
        check=True,
    )
    if out_dir not in sys.path:
        sys.path.insert(0, out_dir)


def save_json(path, obj):
    with open(path, "w") as f:
        json.dump(obj, f, indent=2)
