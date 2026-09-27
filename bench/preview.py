#!/usr/bin/env python3
"""How far behind the call the preview is, for a before and after.

    bench/preview.py TESTBIN [TESTBIN ...] [--runs N]

TESTBIN is the engine's test binary of each build (`cargo test --release
--no-run` prints it). Each run plays the call fixture in real time through
`preview_follows_a_recording`; the builds take turns. For each: the median
over the runs of the median and 90th percentile delay of the lines, and of
the drafts, and the number of lines.
"""

import argparse
import os
import re
import statistics
import subprocess
from pathlib import Path

HERE = Path(__file__).resolve().parent


def run(binary):
    fixture = HERE / "fixtures" / "call"
    env = dict(os.environ, MINUTES_PREVIEW_SPEED="1",
               MINUTES_PREVIEW_MIC=str(fixture / "mic.ogg"), MINUTES_PREVIEW_COMPUTER=str(fixture / "computer.ogg"))
    out = subprocess.run([binary, "--ignored", "--nocapture", "--exact", "live::tests::preview_follows_a_recording"],
                         env=env, capture_output=True, text=True).stdout
    lines = re.search(r"(\d+) lines in all", out)
    behind = re.search(r"behind the call: median ([\d.]+) s, 90th percentile ([\d.]+) s, most ([\d.]+) s", out)
    drafts = re.search(r"drafts behind the call: median ([\d.]+) s, 90th percentile ([\d.]+) s", out)
    if not (lines and behind and drafts):
        raise RuntimeError(f"no result from {binary}:\n{out[-2000:]}")
    return {"lines": int(lines[1]), "median": float(behind[1]), "p90": float(behind[2]), "most": float(behind[3]),
            "draft median": float(drafts[1]), "draft p90": float(drafts[2])}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("bins", nargs="+")
    parser.add_argument("--runs", type=int, default=10)
    args = parser.parse_args()
    results = {b: [] for b in args.bins}
    for i in range(args.runs):
        for binary in args.bins:
            result = run(binary)
            results[binary].append(result)
            print(f"run {i + 1}/{args.runs} {Path(binary).name}: {result}", flush=True)
    for binary, runs in results.items():
        print(binary)
        for key in runs[0]:
            values = [r[key] for r in runs]
            print(f"  {key:13} median {statistics.median(values):5.1f}  stdev {statistics.pstdev(values):4.2f}  "
                  f"min {min(values):5.1f}  max {max(values):5.1f}")


if __name__ == "__main__":
    main()
