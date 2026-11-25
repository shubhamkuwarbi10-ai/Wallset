"""Resource benchmark for wallpaper engines on Windows.

Samples every process whose executable name matches a pattern, once per
interval, and reports per-process and total:

* working set     (what Task Manager shows; easy to make look small)
* private bytes   (committed memory; the honest number)
* CPU             (% of ONE core, from process CPU-time deltas)
* GPU             (% utilisation summed over engines, from the same
                   "GPU Engine" performance counters Task Manager uses),
                   with the VideoDecode engine reported separately so you can
                   see whether hardware decoding is actually in use

Usage:
    python tools/bench.py --match "wallset" --seconds 60
    python tools/bench.py --match "lively" --seconds 60 --csv lively.csv

Methodology notes (put these next to any number you publish):

* Run each engine on the SAME file, SAME monitor, SAME desktop state.
* Measure each state separately: desktop visible, desktop covered by a
  maximised window, fullscreen game/video in front, idle with a still image.
* Discard the first 10 s (startup) with --warmup.
* Report median and p95, not just mean: a renderer that idles at 1% with
  periodic 30% spikes is not a "1%" renderer.

Standard library only, so it adds nothing to the machine being measured.
"""

from __future__ import annotations

import argparse
import csv
import ctypes
import ctypes.wintypes as wt
import re
import statistics
import sys
import time

if sys.platform != "win32":
    sys.exit("bench.py measures Windows processes; run it on Windows.")

k32 = ctypes.WinDLL("kernel32", use_last_error=True)
psapi = ctypes.WinDLL("psapi", use_last_error=True)
pdh = ctypes.WinDLL("pdh", use_last_error=True)

PROCESS_QUERY_LIMITED_INFORMATION = 0x1000


class PMC_EX(ctypes.Structure):
    _fields_ = [
        ("cb", wt.DWORD),
        ("PageFaultCount", wt.DWORD),
        ("PeakWorkingSetSize", ctypes.c_size_t),
        ("WorkingSetSize", ctypes.c_size_t),
        ("QuotaPeakPagedPoolUsage", ctypes.c_size_t),
        ("QuotaPagedPoolUsage", ctypes.c_size_t),
        ("QuotaPeakNonPagedPoolUsage", ctypes.c_size_t),
        ("QuotaNonPagedPoolUsage", ctypes.c_size_t),
        ("PagefileUsage", ctypes.c_size_t),
        ("PeakPagefileUsage", ctypes.c_size_t),
        ("PrivateUsage", ctypes.c_size_t),
    ]


def list_pids() -> list[int]:
    arr = (wt.DWORD * 8192)()
    needed = wt.DWORD()
    if not psapi.EnumProcesses(arr, ctypes.sizeof(arr), ctypes.byref(needed)):
        raise ctypes.WinError(ctypes.get_last_error())
    return [arr[i] for i in range(needed.value // ctypes.sizeof(wt.DWORD)) if arr[i]]


def proc_name(h) -> str:
    buf = ctypes.create_unicode_buffer(1024)
    size = wt.DWORD(len(buf))
    if k32.QueryFullProcessImageNameW(h, 0, buf, ctypes.byref(size)):
        return buf.value.rsplit("\\", 1)[-1]
    return ""


def sample_process(pid: int):
    """Returns (name, working_set, private_bytes, cpu_100ns) or None."""
    h = k32.OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, False, pid)
    if not h:
        return None
    try:
        name = proc_name(h)
        pmc = PMC_EX()
        pmc.cb = ctypes.sizeof(PMC_EX)
        if not psapi.GetProcessMemoryInfo(h, ctypes.byref(pmc), pmc.cb):
            return None
        c, e, kt, ut = (wt.FILETIME() for _ in range(4))
        if not k32.GetProcessTimes(h, ctypes.byref(c), ctypes.byref(e), ctypes.byref(kt), ctypes.byref(ut)):
            return None
        ft = lambda f: (f.dwHighDateTime << 32) | f.dwLowDateTime  # noqa: E731
        return name, pmc.WorkingSetSize, pmc.PrivateUsage, ft(kt) + ft(ut)
    finally:
        k32.CloseHandle(h)


# --- GPU via PDH -------------------------------------------------------------

PDH_FMT_DOUBLE = 0x00000200
PDH_MORE_DATA = 0x800007D2


class PDH_FMT_COUNTERVALUE(ctypes.Structure):
    _fields_ = [("CStatus", wt.DWORD), ("doubleValue", ctypes.c_double)]


class PDH_FMT_COUNTERVALUE_ITEM_W(ctypes.Structure):
    _fields_ = [("szName", wt.LPWSTR), ("FmtValue", PDH_FMT_COUNTERVALUE)]


class GpuSampler:
    """Per-PID GPU utilisation, the way Task Manager computes it."""

    PID_RE = re.compile(r"pid_(\d+)_.*engtype_([A-Za-z0-9]+)")

    def __init__(self) -> None:
        self.query = wt.HANDLE()
        self.counter = wt.HANDLE()
        self.ok = (
            pdh.PdhOpenQueryW(None, 0, ctypes.byref(self.query)) == 0
            and pdh.PdhAddEnglishCounterW(
                self.query, r"\GPU Engine(*)\Utilization Percentage", 0, ctypes.byref(self.counter)
            )
            == 0
        )
        if self.ok:
            pdh.PdhCollectQueryData(self.query)  # rate counters need a baseline

    def sample(self) -> dict[int, dict[str, float]]:
        """{pid: {engine_type: percent}}"""
        if not self.ok or pdh.PdhCollectQueryData(self.query) != 0:
            return {}
        size, count = wt.DWORD(0), wt.DWORD(0)
        rc = pdh.PdhGetFormattedCounterArrayW(self.counter, PDH_FMT_DOUBLE, ctypes.byref(size), ctypes.byref(count), None)
        if rc & 0xFFFFFFFF != PDH_MORE_DATA:
            return {}
        buf = (ctypes.c_byte * size.value)()
        items = ctypes.cast(buf, ctypes.POINTER(PDH_FMT_COUNTERVALUE_ITEM_W))
        if pdh.PdhGetFormattedCounterArrayW(self.counter, PDH_FMT_DOUBLE, ctypes.byref(size), ctypes.byref(count), items) != 0:
            return {}
        out: dict[int, dict[str, float]] = {}
        for i in range(count.value):
            m = self.PID_RE.search(items[i].szName or "")
            if not m:
                continue
            pid, eng = int(m.group(1)), m.group(2).lower()
            per = out.setdefault(pid, {})
            per[eng] = per.get(eng, 0.0) + items[i].FmtValue.doubleValue
        return out


# --- main ----------------------------------------------------------------------

def pct(values: list[float], p: float) -> float:
    if not values:
        return 0.0
    s = sorted(values)
    return s[min(len(s) - 1, int(round(p / 100 * (len(s) - 1))))]


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--match", required=True, help="regex on executable name, e.g. 'wallset' or 'lively'")
    ap.add_argument("--seconds", type=float, default=60)
    ap.add_argument("--interval", type=float, default=1.0)
    ap.add_argument("--warmup", type=float, default=0.0, help="seconds to discard at the start")
    ap.add_argument("--csv", help="also write every sample to this CSV")
    a = ap.parse_args()

    rx = re.compile(a.match, re.IGNORECASE)
    gpu = GpuSampler()
    if not gpu.ok:
        print("note: GPU counters unavailable; GPU columns will read 0", file=sys.stderr)

    prev: dict[int, tuple[int, float]] = {}  # pid -> (cpu_100ns, wall)
    # Whole-window CPU per pid: (first cpu, first wall, last cpu, last wall).
    # Windows advances process CPU time in 15.625 ms ticks, so a 1 s sample
    # is quantised to ~1.56% steps; only the whole-window delta is precise.
    span: dict[int, list[float]] = {}
    rows: list[dict] = []
    t0 = time.perf_counter()
    print(f"sampling processes matching /{a.match}/ for {a.seconds:.0f}s ...", file=sys.stderr)

    while (now := time.perf_counter()) - t0 < a.seconds:
        g = gpu.sample()
        for pid in list_pids():
            s = sample_process(pid)
            if not s or not rx.search(s[0]):
                continue
            name, ws, priv, cpu = s
            cpu_pct = None
            if pid in prev:
                dcpu, dwall = cpu - prev[pid][0], now - prev[pid][1]
                cpu_pct = max(0.0, dcpu / (dwall * 1e7) * 100) if dwall > 0 else 0.0
            prev[pid] = (cpu, now)
            eng = g.get(pid, {})
            if now - t0 >= a.warmup:
                sp = span.setdefault(pid, [cpu, now, cpu, now])
                sp[2], sp[3] = cpu, now
            if cpu_pct is not None and now - t0 >= a.warmup:
                rows.append({
                    # epoch: lets samples be aligned with other logs (e.g. the
                    # daemon's state transitions). cpu_time_s: cumulative, so
                    # any segment's CPU can be computed precisely afterwards.
                    "epoch": round(time.time(), 3), "cpu_time_s": cpu / 1e7,
                    "t": round(now - t0, 2), "pid": pid, "name": name,
                    "working_set_mb": ws / 1048576, "private_mb": priv / 1048576,
                    "cpu_pct_one_core": cpu_pct,
                    "gpu_pct_total": sum(eng.values()),
                    "gpu_pct_videodecode": eng.get("videodecode", 0.0),
                })
        time.sleep(max(0.0, a.interval - (time.perf_counter() - now)))

    if not rows:
        print("no matching processes were sampled", file=sys.stderr)
        return 1

    if a.csv:
        with open(a.csv, "w", newline="") as f:
            w = csv.DictWriter(f, fieldnames=list(rows[0]))
            w.writeheader()
            w.writerows(rows)

    # Totals per timestamp (all matching processes together), then summarise.
    by_t: dict[float, dict[str, float]] = {}
    for r in rows:
        agg = by_t.setdefault(r["t"], {k: 0.0 for k in ("working_set_mb", "private_mb", "cpu_pct_one_core", "gpu_pct_total", "gpu_pct_videodecode")})
        for k in agg:
            agg[k] += r[k]

    def line(label: str, series: list[float], unit: str) -> str:
        return f"| {label} | {statistics.median(series):.1f}{unit} | {pct(series, 95):.1f}{unit} | {max(series):.1f}{unit} |"

    names = sorted({r["name"] for r in rows})
    totals = list(by_t.values())
    print(f"\nProcesses: {', '.join(names)}   samples: {len(totals)}   interval: {a.interval}s\n")
    print("| Metric (all matching processes) | median | p95 | max |")
    print("|---|---|---|---|")
    print(line("Private bytes", [t["private_mb"] for t in totals], " MB"))
    print(line("Working set", [t["working_set_mb"] for t in totals], " MB"))
    whole = sum((sp[2] - sp[0]) / 1e7 for sp in span.values())
    wall = max((sp[3] - sp[1]) for sp in span.values()) if span else 0
    print(f"| CPU, whole window (precise) | {whole / wall * 100 if wall else 0:.2f}% | | |")
    print(line("CPU per 1 s sample (quantised ~1.56%; use for spikes only)", [t["cpu_pct_one_core"] for t in totals], "%"))
    print(line("GPU (all engines)", [t["gpu_pct_total"] for t in totals], "%"))
    print(line("GPU VideoDecode engine", [t["gpu_pct_videodecode"] for t in totals], "%"))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
