"""Renders results.json into a Markdown report with the headline ratios."""


def _f(x, nd=0):
    return f"{x:,.{nd}f}"


def headline(res):
    """Key comparisons, computed (not hand-copied) from the measurements."""
    h = {}
    sync = res.get("sync")
    if sync:
        by = {(r["kind"], r["procs"]): r for r in sync}
        procs = sorted({r["procs"] for r in sync})
        h["sync"] = []
        for p in procs:
            a, m = by.get(("atomic", p)), by.get(("mp.Value+lock", p))
            if a and m:
                h["sync"].append({
                    "procs": p,
                    "throughput_x": a["throughput_ops_s"] / m["throughput_ops_s"],
                    "p50_x": m["p50_ns"] / a["p50_ns"],
                    "p99_x": m["p99_ns"] / a["p99_ns"],
                    "cpu_x": m["cpu_ns_per_op"] / a["cpu_ns_per_op"],
                })
    rpc = res.get("rpc")
    if rpc:
        shm = rpc["sequential"]["shm atomic"]
        h["rpc"] = {}
        for name, d in rpc["sequential"].items():
            if name == "shm atomic":
                continue
            h["rpc"][name] = {op: {"p50_x": d[op]["p50_ns"] / shm[op]["p50_ns"],
                                   "p99_x": d[op]["p99_ns"] / shm[op]["p99_ns"]} for op in d}
        c_shm = rpc["concurrent"]["shm atomic"]
        h["rpc_concurrent"] = {name: {"throughput_x": c_shm["ops_s"] / d["ops_s"],
                                      "cpu_x": d["cpu_ns_per_op"] / c_shm["cpu_ns_per_op"]}
                               for name, d in rpc["concurrent"].items() if name != "shm atomic"}
    return h


def render(res):
    m = res["machine"]
    out = [
        "# atomvar evaluation report",
        "",
        f"- Machine: {m['cpu_model']}, {m['logical_cpus']} logical CPUs, kernel {m['kernel']}",
        f"- Python {m['python']} (GIL enabled: {m['gil_enabled']}), run {m['date']}"
        + (" (QUICK run: small counts, not for publication)" if res.get("quick") else ""),
        f"- perf_counter_ns timer overhead (included in every per-op latency sample): "
        f"~{_f(m['timer_overhead_ns'])} ns",
        "- Throughput = total ops / wall time (clock started before parked workers are released; "
        "each worker records its own finish). Latency = per-op samples. CPU = user+sys of all "
        "participating processes (clients and, for RPC, the server) / ops.",
        "- With more workers than logical CPUs, workers time-share cores: those rows measure "
        "oversubscription, not parallel speedup.",
        "",
    ]
    h = headline(res)

    if res.get("sync"):
        out += ["## 1. Atomic vs mutex across processes (+ 3. contention scaling)", "",
                "N spawned processes increment one shared counter.", "",
                "| variant | procs | M ops/s | mean ns | p50 ns | p95 ns | p99 ns | CPU ns/op | result |",
                "|---|---|---|---|---|---|---|---|---|"]
        for r in res["sync"]:
            out.append(f"| {r['kind']} | {r['procs']} | {r['throughput_ops_s']/1e6:.2f} | {_f(r['mean_ns'])} | "
                       f"{_f(r['p50_ns'])} | {_f(r['p95_ns'])} | {_f(r['p99_ns'])} | {_f(r['cpu_ns_per_op'])} | "
                       f"{'exact' if r['correct'] else 'WRONG'} |")
        out += ["", "Atomic vs `multiprocessing.Value(lock=True)`:", "",
                "| procs | throughput | p50 latency | p99 latency | CPU per op |", "|---|---|---|---|---|"]
        for s in h["sync"]:
            out.append(f"| {s['procs']} | {s['throughput_x']:.1f}x higher | {s['p50_x']:.1f}x lower | "
                       f"{s['p99_x']:.1f}x lower | {s['cpu_x']:.1f}x less |")
        out.append("")

    if res.get("rpc"):
        rpc = res["rpc"]
        out += ["## 2. Host-local state: shared-memory atomic vs RPC", "",
                "One Python client, sequential ops, each op timed.", "",
                "| transport | op | p50 us | p95 us | p99 us | ops/s |", "|---|---|---|---|---|---|"]
        for name, d in rpc["sequential"].items():
            for op, s in d.items():
                out.append(f"| {name} | {op} | {s['p50_ns']/1e3:.2f} | {s['p95_ns']/1e3:.2f} | "
                           f"{s['p99_ns']/1e3:.2f} | {_f(s['ops_s'])} |")
        out += ["", "Slowdown vs shared memory (p50 / p99):", "",
                "| transport | load | fetch_add | compare_exchange |", "|---|---|---|---|"]
        for name, d in h["rpc"].items():
            cells = [f"{d[op]['p50_x']:,.0f}x / {d[op]['p99_x']:,.0f}x" for op in ("load", "fetch_add",
                                                                                     "compare_exchange")]
            out.append(f"| {name} | " + " | ".join(cells) + " |")
        out += ["", "4 client processes doing fetch_add concurrently:", "",
                "| transport | ops/s | CPU ns/op (clients+server) | shm advantage |", "|---|---|---|---|"]
        for name, d in rpc["concurrent"].items():
            adv = ("baseline" if name == "shm atomic" else
                   f"{h['rpc_concurrent'][name]['throughput_x']:,.0f}x throughput, "
                   f"{h['rpc_concurrent'][name]['cpu_x']:,.0f}x less CPU")
            out.append(f"| {name} | {_f(d['ops_s'])} | {_f(d['cpu_ns_per_op'])} | {adv} |")
        out.append("")

    if res.get("crosslang"):
        x = res["crosslang"]
        s, g = x["shm"], x["grpc"]
        out += ["## 4. Cross-language: one counter, several runtimes", "",
                f"{', '.join(s['languages'])} processes each did {_f(s['per_process_ops'])} "
                f"`fetch_add(1)` on the same arena variable at the same time.", "",
                f"- Final value: **{_f(s['final'])}** (expected {_f(s['expected'])}): "
                f"**{'exact' if s['exact'] else 'WRONG'}**",
                f"- Aggregate: {s['aggregate_ops_s']/1e6:.2f} M ops/s, wall {s['wall_s']:.2f} s "
                "(includes process exit)", "",
                "| language | ns/op (over its own run) |", "|---|---|"]
        for lang, d in s["languages"].items():
            out.append(f"| {lang} | {d['ns_per_op']:.1f} |")
        out += ["", f"Same increments via atomvard gRPC (4 Rust tonic clients x {_f(g['per_process_ops'])}): "
                f"{_f(g['aggregate_ops_s'])} ops/s ({'exact' if g['exact'] else 'WRONG'}). "
                f"Shared memory delivered **{x['speedup_shm_vs_grpc']:,.0f}x** the throughput.", ""]
    return "\n".join(out) + "\n"
