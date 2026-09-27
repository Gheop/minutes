#!/usr/bin/env python3
"""What the app does after Stop, end to end, on a recorded call.

    bench/write_up.py [--bin PATH] [--model NAME] [--fixture DIR]

Builds a staging folder as a recording leaves it (the two raw tracks and its
note) from a fixture, runs `minutes write-up` on it, and checks the meeting
folder: the audio, the kept tracks, the manifest, a transcript with both
sides in it, readable by its owner alone, and the raw files gone.
"""

import argparse
import json
import stat
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent


def raw(source, target):
    # As parec writes it: 48 kHz, stereo, 16 bits.
    subprocess.run(["ffmpeg", "-v", "error", "-y", "-i", str(source), "-ac", "2", "-ar", "48000",
                    "-f", "s16le", str(target)], check=True)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--bin", default=str(HERE.parent / "target/release/minutes"))
    parser.add_argument("--model")
    parser.add_argument("--fixture", default=str(HERE / "fixtures/call"))
    args = parser.parse_args()

    failures = []

    def check(ok, what):
        print(f"{'ok  ' if ok else 'FAIL'} {what}")
        if not ok:
            failures.append(what)

    with tempfile.TemporaryDirectory() as tmp:
        staging, meetings = Path(tmp, "staging"), Path(tmp, "Meetings")
        staging.mkdir()
        raw(Path(args.fixture, "mic.ogg"), staging / "mic.raw")
        raw(Path(args.fixture, "computer.ogg"), staging / "system.raw")
        (staging / "recording.json").write_text(json.dumps(
            {"title": "Weekly sync", "started_at": 1790000000, "format": "mono", "language": "en"}))

        command = [args.bin, "write-up", str(staging), "--into", str(meetings)]
        if args.model:
            command += ["--model", args.model]
        out = subprocess.run(command, capture_output=True, text=True)
        check(out.returncode == 0, f"write-up exits cleanly ({out.stderr.strip().splitlines()[-1:]})")
        if out.returncode != 0:
            return 1
        folder = Path(out.stdout.strip())
        check(folder.parent == meetings and folder.name.endswith(" Weekly sync"),
              f"the meeting folder is named after the meeting ({folder.name})")

        files = {p.name for p in folder.iterdir()}
        check("transcript.md" in files, "the transcript is there")
        kept = {p.name for p in (folder / ".tracks").glob("*")}
        check({"mic.ogg", "computer.ogg"} <= kept, "both tracks are kept to transcribe again")
        check(any(p.suffix in (".ogg", ".opus", ".mp3", ".m4a", ".flac", ".wav") and p.stem not in ("mic", "computer")
                  for p in folder.iterdir()), "the audio of the meeting is there")
        manifests = [p for p in folder.iterdir() if p.suffix == ".meeting-recorder"]
        check(len(manifests) == 1, "one manifest")
        if manifests:
            manifest = json.loads(manifests[0].read_text())
            check(manifest["title"] == "Weekly sync" and manifest["duration_secs"] > 60,
                  f"the manifest has the title and the length ({manifest['duration_secs']} s)")
            check(bool(manifest.get("model")), "the manifest says which model wrote the transcript")

        transcript = (folder / "transcript.md").read_text() if "transcript.md" in files else ""
        lines = [l for l in transcript.splitlines() if l.startswith("**[")]
        check(any("You" in l.split("**")[1] for l in lines), "your side is in the transcript")
        check(any("Remote" in l.split("**")[1] for l in lines), "the other side is in the transcript")
        check(len(lines) >= 10, f"the transcript has its lines ({len(lines)})")

        for path in (meetings, folder):
            check(stat.S_IMODE(path.stat().st_mode) == 0o700, f"{path.name} is readable by its owner alone")
        check(not staging.exists(), "the raw files are gone once the audio is kept")

    print(f"{len(failures)} failed" if failures else "all checks pass")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
