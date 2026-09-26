#!/usr/bin/env python3
"""Time the two-track transcription of a real meeting, for a before and after.

    bench/perf.py BIN [BIN ...] [--runs 10] [--minutes 5] [--cpus 0-11] [--json FILE]

Every binary transcribes the AMI ES2004a call (your headset as the mic, the
other three people as the computer audio), the same input `bench/run.py --ami`
uses. The binaries take turns, run after run, so a warming laptop or a
background job weighs on all of them alike. For each: the median wall time,
its spread, the median of every stage the app reports on stderr, and the
largest resident set size.

On a CPU with performance and efficiency cores, pin the runs to one kind
with --cpus (0-11 are the performance cores of a Core Ultra 7 155H): a run
the scheduler spreads over the slow cores differs from one it does not by
more than most changes are worth.

Needs the AMI files in bench/.cache, which `bench/run.py --ami` downloads.
"""

import argparse
import json
import re
import statistics
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
CACHE = HERE / ".cache" / "ES2004a"


def inputs(minutes):
    part = f"-{minutes}m" if minutes else ""
    mic, computer = CACHE / f"headset-0{part}.wav", CACHE / f"computer{part}.wav"
    if not mic.exists() or not computer.exists():
        sys.exit(f"missing {mic.name} or {computer.name}: run bench/run.py --ami --ami-minutes {minutes} first")
    return mic, computer


def stages(stderr, wall):
    """Seconds spent in each stage, from the "[ 12.3s] Stage" lines; the last
    one runs until the process ends."""
    marks = [(float(t), name) for t, name in re.findall(r"^\[\s*([\d.]+)s\] ([A-Za-z][^\d%\n]*?)\s*$", stderr, re.M)]
    out = {}
    for (t, name), (next_t, _) in zip(marks, marks[1:] + [(wall, None)]):
        out[name] = out.get(name, 0.0) + next_t - t
    return out


def run_once(binary, mic, computer, cpus=None):
    pin = ["taskset", "-c", cpus] if cpus else []
    start = time.perf_counter()
    proc = subprocess.run(
        [*pin, "/usr/bin/time", "-f", "RSS %M", binary, "transcribe", str(mic), str(computer), "--language", "en"],
        capture_output=True, text=True,
    )
    wall = time.perf_counter() - start
    if proc.returncode != 0:
        sys.exit(f"{binary} failed:\n{proc.stderr[-2000:]}")
    rss = int(re.findall(r"RSS (\d+)", proc.stderr)[-1]) / 1024
    return wall, rss, stages(proc.stderr, wall), proc.stdout


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("binaries", nargs="+")
    parser.add_argument("--runs", type=int, default=10)
    parser.add_argument("--minutes", type=int, default=5)
    parser.add_argument("--cpus", help="taskset CPU list to pin every run to")
    parser.add_argument("--json")
    args = parser.parse_args()
    mic, computer = inputs(args.minutes)

    # One warm-up each: the model files into the page cache, the GPU awake.
    for binary in args.binaries:
        run_once(binary, mic, computer, args.cpus)
    results = {b: {"wall": [], "rss": [], "stages": [], "transcript": None} for b in args.binaries}
    for i in range(args.runs):
        for binary in args.binaries:
            wall, rss, st, transcript = run_once(binary, mic, computer, args.cpus)
            r = results[binary]
            r["wall"].append(wall)
            r["rss"].append(rss)
            r["stages"].append(st)
            r["transcript"] = transcript
            print(f"run {i + 1}/{args.runs} {Path(binary).name}: {wall:.1f}s", file=sys.stderr, flush=True)

    summary = {}
    for binary, r in results.items():
        walls = r["wall"]
        names = sorted({n for st in r["stages"] for n in st})
        summary[binary] = {
            "median_s": statistics.median(walls),
            "stdev_s": statistics.stdev(walls) if len(walls) > 1 else 0.0,
            "min_s": min(walls),
            "max_s": max(walls),
            "rss_max_mb": max(r["rss"]),
            "stages_median_s": {n: statistics.median(st.get(n, 0.0) for st in r["stages"]) for n in names},
            "runs": walls,
        }
        s = summary[binary]
        print(f"{binary}\n  wall {s['median_s']:.1f}s median, ±{s['stdev_s']:.1f}s ({100 * s['stdev_s'] / s['median_s']:.1f}%), "
              f"{s['min_s']:.1f}–{s['max_s']:.1f}s, RSS max {s['rss_max_mb']:.0f} MB")
        for n, t in s["stages_median_s"].items():
            print(f"    {n:20} {t:6.1f}s")
    if args.json:
        Path(args.json).write_text(json.dumps({
            "input": f"AMI ES2004a call, first {args.minutes} min" if args.minutes else "AMI ES2004a call, whole",
            "runs": args.runs,
            "cpus": args.cpus,
            "results": summary,
        }, indent=1))


if __name__ == "__main__":
    main()
