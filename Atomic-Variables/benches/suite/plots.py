"""Renders the README charts from a results.json into Atomic-Variables/images/.

    python benches/suite/plots.py benches/results/<run>/results.json
"""

import json
import os
import sys

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt  # noqa: E402

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
OUT = os.path.join(ROOT, "images")

# Validated categorical slots 1-2 (light surface) and chart ink.
BLUE, ORANGE = "#2a78d6", "#eb6834"
SURFACE, INK, INK2, MUTED, GRID, AXIS = "#fcfcfb", "#0b0b0b", "#52514e", "#898781", "#e1e0d9", "#c3c2b7"
BASELINE_GRAY = "#a8a7a0"

plt.rcParams.update({
    "figure.facecolor": SURFACE, "axes.facecolor": SURFACE, "savefig.facecolor": SURFACE,
    "font.size": 11, "axes.edgecolor": AXIS, "axes.labelcolor": INK2,
    "xtick.color": MUTED, "ytick.color": MUTED, "axes.spines.top": False, "axes.spines.right": False,
})


def note(fig, text):
    fig.text(0.01, 0.01, text, color=MUTED, fontsize=8.5, ha="left", va="bottom")


def title(ax, main, sub):
    ax.set_title(main, loc="left", color=INK, fontsize=14, fontweight="bold", pad=26)
    ax.text(0, 1.03, sub, transform=ax.transAxes, color=INK2, fontsize=10.5, va="bottom")


def scaling(res, machine):
    rows = res["sync"]
    procs = sorted({r["procs"] for r in rows})
    series = [("atomic", "atomvar", BLUE), ("mp.Value+lock", "multiprocessing.Value + lock", ORANGE)]
    fig, ax = plt.subplots(figsize=(8.4, 4.6))
    xs = range(len(procs))
    for kind, label, color in series:
        ys = [next(r["throughput_ops_s"] for r in rows if r["kind"] == kind and r["procs"] == p) / 1e6
              for p in procs]
        ax.plot(xs, ys, color=color, linewidth=2, marker="o", markersize=8,
                markeredgecolor=SURFACE, markeredgewidth=2, label=label, zorder=3)
        ax.annotate(f"{label}  {ys[-1]:.1f}M", (xs[-1], ys[-1]), xytext=(10, 0), textcoords="offset points",
                    va="center", color=INK2, fontsize=10)
    ax.set_xticks(list(xs), [str(p) for p in procs])
    ax.set_xlabel("processes incrementing one shared counter")
    ax.set_ylabel("million increments / second")
    ax.set_ylim(bottom=0)
    ax.set_xlim(-0.3, len(procs) - 1 + 2.4)
    ax.grid(axis="y", color=GRID, linewidth=1)
    ax.set_axisbelow(True)
    ax.legend(frameon=False, loc="upper left", bbox_to_anchor=(0, 0.98), labelcolor=INK2)
    title(ax, "Shared counter across processes", "Higher is better. Every run counted exactly.")
    note(fig, f"{machine}. Throughput flattens past 2 processes because the machine has 2 vCPUs.")
    fig.tight_layout(rect=(0, 0.04, 1, 1))
    fig.savefig(os.path.join(OUT, "throughput_vs_lock.png"), dpi=160)
    plt.close(fig)


def latency(res, machine):
    seq = res["rpc"]["sequential"]
    order = [("shm atomic", "atomvar (shared memory)"), ("Redis unix socket", "Redis (unix socket)"),
             ("Redis TCP", "Redis (TCP)"), ("gRPC (Rust server)", "gRPC (Rust server)"),
             ("gRPC (Python server)", "gRPC (Python server)")]
    labels = [lab for _, lab in order]
    vals = [seq[k]["fetch_add"]["p50_ns"] for k, _ in order]
    colors = [BLUE] + [BASELINE_GRAY] * (len(order) - 1)
    fig, ax = plt.subplots(figsize=(8.4, 4.2))
    y = list(range(len(order)))[::-1]
    ax.barh(y, vals, color=colors, height=0.56, zorder=3)
    ax.set_xscale("log")
    ax.set_yticks(y, labels, color=INK2)
    ax.tick_params(axis="y", length=0)

    def fmt(v):
        return f"{v:.0f} ns" if v < 1000 else f"{v / 1000:,.0f} microsec"

    base = vals[0]
    for yi, v in zip(y, vals):
        txt = fmt(v) if v == base else f"{fmt(v)}  ({v / base:,.0f}x slower)"
        ax.text(v * 1.15, yi, txt, va="center", color=INK2, fontsize=10)
    ticks = [100, 10_000, 1_000_000]
    ax.set_xticks(ticks, ["100 ns", "10 microsec", "1 ms"])
    ax.minorticks_off()
    ax.set_xlim(left=min(vals) * 0.6, right=max(vals) * 400)
    ax.grid(axis="x", color=GRID, linewidth=1)
    ax.set_axisbelow(True)
    ax.spines["left"].set_visible(False)
    title(ax, "One atomic increment: shared memory vs the network",
          "Median time per fetch_add from a Python client (log scale, lower is better).")
    note(fig, f"{machine}. Network numbers depend on the OS network stack (WSL2 loopback is slow).")
    fig.tight_layout(rect=(0, 0.04, 1, 1))
    fig.savefig(os.path.join(OUT, "latency_vs_network.png"), dpi=160)
    plt.close(fig)


def main():
    res = json.load(open(sys.argv[1]))
    m = res["machine"]
    machine = f"{m['cpu_model'].replace('12th Gen Intel(R) Core(TM) ', '')}, {m['logical_cpus']} vCPUs, Python {m['python']}"
    os.makedirs(OUT, exist_ok=True)
    scaling(res, machine)
    latency(res, machine)
    print(f"wrote {OUT}/throughput_vs_lock.png and latency_vs_network.png")


if __name__ == "__main__":
    main()
