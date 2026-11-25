"""Split a bench.py CSV into the daemon's policy states and report each.

    python tools/session_report.py samples.csv %LOCALAPPDATA%\\wallSet\\daemon.log

State boundaries come from the daemon's own log ("X -> Y (reason)") rather
than from a human following a script, so imprecise timing during a session
does not contaminate the numbers. Samples within SETTLE seconds after a
boundary are discarded (log timestamps have 1 s resolution, and a renderer
needs a moment to start or to quiesce).

CPU per segment is computed from cumulative CPU time across the segment
(precise), never from per-sample percentages (quantised to ~1.56% by the
Windows 15.6 ms CPU-time tick).
"""

from __future__ import annotations

import csv
import re
import statistics
import sys

SETTLE_S = 5.0
TRANSITION = re.compile(r"^\[(\d+)\] (.+?): (\w+) -> (\w+) \((.+)\)$")


def load_transitions(log_path: str) -> list[tuple[float, str, str]]:
    out = []
    for line in open(log_path, encoding="utf-8", errors="replace"):
        m = TRANSITION.match(line.strip())
        if m:
            out.append((float(m.group(1)), m.group(4), m.group(5)))
    return out


def segments(transitions, t_end):
    """[(start, end, tier, reason)] covering the session."""
    segs = []
    for i, (t, tier, reason) in enumerate(transitions):
        end = transitions[i + 1][0] if i + 1 < len(transitions) else t_end
        segs.append((t, end, tier, reason))
    return segs


def summarise(rows):
    if len(rows) < 2:
        return None
    rows = sorted(rows, key=lambda r: r["epoch"])
    span = rows[-1]["epoch"] - rows[0]["epoch"]
    cpu = (rows[-1]["cpu_time_s"] - rows[0]["cpu_time_s"]) / span * 100 if span > 0 else 0.0
    priv = [r["private_mb"] for r in rows]
    return {
        "secs": span,
        "cpu": cpu,
        "priv_med": statistics.median(priv),
        "priv_max": max(priv),
        "ws_med": statistics.median(r["working_set_mb"] for r in rows),
        "gpu_med": statistics.median(r["gpu_pct_total"] for r in rows),
        "dec_med": statistics.median(r["gpu_pct_videodecode"] for r in rows),
    }


def main() -> int:
    csv_path, log_path = sys.argv[1], sys.argv[2]
    rows = []
    for r in csv.DictReader(open(csv_path)):
        for k in ("epoch", "cpu_time_s", "working_set_mb", "private_mb", "gpu_pct_total", "gpu_pct_videodecode"):
            r[k] = float(r[k])
        rows.append(r)
    if not rows:
        print("no samples")
        return 1
    trans = load_transitions(log_path)
    t_end = max(r["epoch"] for r in rows)
    print("| State (from daemon log) | Measured for | Process | CPU (% one core) | Private MB (median / max) | Working set MB | GPU % | VideoDecode % |")
    print("|---|---|---|---|---|---|---|---|")
    for start, end, tier, reason in segments(trans, t_end):
        lo, hi = start + SETTLE_S, end - 1.0
        if hi - lo < 3:
            continue
        window = [r for r in rows if lo <= r["epoch"] <= hi]
        by_proc: dict[str, list] = {}
        for r in window:
            by_proc.setdefault(f'{r["name"]} ({r["pid"]})', []).append(r)
        label = f"**{tier}**: {reason}"
        if not any("render" in k for k in by_proc):
            print(f"| {label} | {hi - lo:.0f} s | wallset-render | not running | **0** | **0** | — | — |")
        for name, rs in sorted(by_proc.items()):
            s = summarise(rs)
            if s:
                print(
                    f"| {label} | {s['secs']:.0f} s | {name} | {s['cpu']:.2f}% | "
                    f"{s['priv_med']:.1f} / {s['priv_max']:.1f} | {s['ws_med']:.1f} | {s['gpu_med']:.1f} | {s['dec_med']:.1f} |"
                )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
