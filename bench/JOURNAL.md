# Performance journal

Target: the time from Stop to a finished transcript for a two-track call, CUDA build, on a Core Ultra 7 155H laptop with an RTX 2050 (4 GB). Main metric: median wall time of `transcribe mic computer` on the AMI ES2004a call (first 5 minutes), over 10 alternating runs (`bench/perf.py`). Guards: largest RSS, and the scores of `bench/run.py --ami --check`.

## Profile before any change

Whole ES2004a call (17 min 29 s), one run, CUDA build:

| Stage | Time | Share |
|---|---|---|
| Finding speakers (Nemotron on the CPU, mic side then computer side) | 298 s | 60 % |
| Loading the whisper model | 2 s | 0.4 % |
| Transcribing (whisper on the GPU) | 193 s | 39 % |

Total 8 min 15 s (with browsers open; the same run on a quiet machine takes 4 min 35 s), largest RSS 1.27 GB, largest VRAM 2.2 GB, GPU busy 23 % of the time on average: it waits while the speakers are found.

## Measurement

| # | Setup | Median | Spread | Verdict |
|---|---|---|---|---|
| A/A 1 | not pinned, Firefox, Chrome and Teams open | 71.3 s / 76.2 s | ±15 % / ±11 % | Too noisy to judge anything |
| A/A 2 | pinned to the performance cores (`--cpus 0-11`), same apps open | 82.7 s / 89.5 s | ±14 % / ±16 % | Worse: 6 cores for whisper, ONNX and the other apps. Pinning dropped |
| A/A 3 | not pinned, browsers and Teams closed | 60.7 s / 60.5 s | ±0.3 % / ±0.2 % | Good: every later comparison is made this way |

## Changes

| # | Hypothesis | Files | Result | Δ wall | Δ RSS | Verdict |
|---|---|---|---|---|---|---|
| 1 | The two sides are diarized one after the other with 8 ONNX threads each on a 22-thread CPU; running them side by side should take the stage from 30 s to about 16 s | `transcribe.rs`, `nemotron.rs` | No change: 30.4 s against 30.6 s. Alone, each side already keeps 6 cores busy (606–631 % CPU, 14–16 s); together they share the same compute and each takes twice as long. The stage is bound by CPU throughput, not by waiting | −0.3 % (noise) | +28 MB (two sessions) | Reverted |
| 2 | Each side is diarized over its whole length, silences included; on the mic everything but your own voice is zeroed, yet still goes through the model. Giving it only the speech regions, glued like for whisper, should cut the stage by the share of silence | `transcribe.rs` | Stage 30.5 s → 14.8 s. Same scores on every bench case and on ES2004a; on IS1009a, a meeting never used for tuning, the call scores go up (side 32.3 → 34.7 %, person 69.4 → 73.1 %) and the time down (59.8 → 48.0 s) | −26 % (60.6 → 44.9 s, ±0.3 %) | +0.8 % | Kept |
| 3 | The whisper model (1.6 GB) is loaded only once the speakers are found, though loading is mostly reading and uploading to the GPU; loading it meanwhile should take its 3.1 s off the critical path | `transcribe.rs` | Loading disappears from the path; finding speakers gets 0.9 s slower while the model is read (shared CPU and memory), so 1.8 s of the 3.1 s is won. Not done when the model still has to be downloaded, so the download keeps its progress bar. Same scores everywhere | −4.0 % (45.0 → 43.2 s, ±0.4 %) | +0.1 % | Kept |
| 4 | Whisper (GPU) waits for both sides' speakers (CPU); transcribing the first side while the second side's speakers are found should overlap the two | `transcribe.rs` | No change: 43.3 s either way. The sides go longest first (its language counts for both), so the side overlapped is the mic, whose speakers take 3–4 s, and whisper's own CPU threads compete for that time. The last two runs of both binaries were 10 s slower when wireplumber restarted; the first eight differ by 0.2 s | 0 % | +3.5 % | Reverted |

## Whole meeting

`bench/perf.py bench/bin/base bench/bin/h3 --runs 3 --minutes 0` (ES2004a, 17 min 29 s): 274.8 s ± 0.5 % before, 233.9 s ± 0.1 % after (−15 %). Finding speakers 109.6 → 70.8 s, transcribing 161.1 → 162.4 s, largest RSS 1247 → 1249 MB. Finding speakers costs the same per minute of audio on 5 and on 17 minutes (6.1 and 6.3 s): it grows in a straight line, the laptop does not slow down with heat.
