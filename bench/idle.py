#!/usr/bin/env python3
"""What Minutes costs while it waits in the background all day.

    bench/idle.py [BIN ...] [--runs N] [--wait S]

Starts `BIN --background` on a private session bus with an empty home, as the
session starts it at login, waits, and reads its memory from /proc. With
several binaries, the runs alternate between them.
"""

import argparse
import os
import signal
import statistics
import subprocess
import tempfile
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent


def memory(pid):
    fields = {}
    for line in Path(f"/proc/{pid}/smaps_rollup").read_text().splitlines()[1:]:
        key, value = line.split(":", 1)
        fields[key] = int(value.split()[0]) / 1024
    return fields


def child_pid(parent, name):
    # dbus-run-session starts the bus and then the app; the app is the child
    # whose command is the binary.
    for _ in range(100):
        out = subprocess.run(["pgrep", "-P", str(parent)], capture_output=True, text=True).stdout.split()
        for pid in out:
            try:
                if Path(f"/proc/{pid}/exe").resolve().name == name:
                    return int(pid)
            except OSError:
                pass
        time.sleep(0.1)
    raise RuntimeError("the app did not start")


def leftovers(home):
    """Stops what the run left behind with its home, in case it left its group."""
    for proc in Path("/proc").iterdir():
        try:
            if f"HOME={home}".encode() in (proc / "environ").read_bytes().split(b"\0"):
                os.kill(int(proc.name), signal.SIGTERM)
        except (OSError, ValueError):
            pass


def run(binary, wait):
    with tempfile.TemporaryDirectory() as home:
        env = dict(os.environ, HOME=home, XDG_CACHE_HOME=f"{home}/.cache", XDG_CONFIG_HOME=f"{home}/.config")
        # In a process group of its own: the app starts services on its bus
        # (portals, gvfsd) that outlive it and would pile up run after run.
        session = subprocess.Popen(["dbus-run-session", "--", binary, "--background"], env=env,
                                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                                   start_new_session=True)
        pid = None
        try:
            pid = child_pid(session.pid, Path(binary).resolve().name)
            time.sleep(wait)
            return memory(pid)
        finally:
            os.killpg(session.pid, signal.SIGTERM)
            session.wait(timeout=10)
            leftovers(home)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("bins", nargs="*", default=[str(HERE.parent / "target/release/minutes")])
    parser.add_argument("--runs", type=int, default=10)
    parser.add_argument("--wait", type=float, default=30)
    args = parser.parse_args()
    results = {b: [] for b in args.bins}
    for _ in range(args.runs):
        for binary in args.bins:
            results[binary].append(run(binary, args.wait))
    for binary, runs in results.items():
        print(binary)
        for key in ("Rss", "Pss", "Anonymous", "Private_Clean", "Private_Dirty"):
            values = [r[key] for r in runs]
            print(f"  {key:14} median {statistics.median(values):7.1f} MB  "
                  f"stdev {statistics.pstdev(values):5.1f}  min {min(values):7.1f}  max {max(values):7.1f}")


if __name__ == "__main__":
    main()
