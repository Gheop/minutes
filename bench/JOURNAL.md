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

## Second round: the instance at rest, the time after Stop, the preview (2026-09-27)

Targets, in the user's words: what is too slow or too heavy today, all three of
- the instance started at login, waiting all day: resident memory of `minutes --background` after 20 s, 10 starts on a private session bus (`bench/idle.py`);
- the time after Stop: as in the first round (`bench/perf.py`, first 5 minutes of ES2004a, 10 alternating runs);
- the preview during a call: median and 90th percentile delay of its lines, replaying the call fixture in real time, 10 alternating runs (`bench/preview.py`).

Browsers and Teams closed for every series.

### Profile of the instance at rest

237.5 MB resident (±0.3), of which 127.7 MB anonymous. One mapping holds 101.5 MB of it: the `.bss` of `libcublasLt.so.13`, right after its data segment, written when the library loads. With the CUDA libraries' code, CUDA is about 180 MB of the 237, in a process that transcribes nothing until a call is recorded. During the first audit this read as 8 MB: that count only looked at the libraries' file pages.

| # | Hypothesis | Files | Result | Δ main metric | Δ RSS | Verdict |
|---|---|---|---|---|---|---|
| 5 | The Vulkan backend links only `libvulkan`, which loads the driver when a device is opened; built with `--features vulkan`, the idle instance should lose the CUDA libraries' 180 MB. The risk is whisper running slower | build | At rest 237.5 → 57.7 MB (±0.1). By default ggml puts the Intel Arc first and whisper ran there: 192 s for 5 minutes, the RTX idle. Forced onto the RTX (`GGML_VK_VISIBLE_DEVICES=1`), a first run took 52.8 s, which made Vulkan look slower; it was the driver compiling the shaders once. Over 10 alternating runs: 42.3 s ± 2.4 % against 44.4 s ± 2.6 % for CUDA, faster in every pair; whisper 26.0 against 27.5 s, speakers 15.9 against 16.7 s, peak RSS 614 against 904 MB | at rest −76 %, after Stop −4.7 % | peak −32 % | Kept, with #6 |
| 6 | whisper.cpp counts integrated and dedicated GPUs alike, in ggml's order; picking the index of the first dedicated one should put whisper on the RTX without an environment variable | `transcribe.rs`, `engine/Cargo.toml` (`raw-api`) | The RTX is used (100 %), `GGML_VK_VISIBLE_DEVICES` still picks another | — | — | Kept |
| 7 | With Vulkan, moving whisper into a worker process started only to transcribe should take the idle instance further down | `engine/`, `worker/` (branch `perf-worker`) | At rest 57.9 → 48.9 MB (±0.3); the app binary 74 → 4.5 MB, pages that were never loaded anyway. It adds a process, a JSON protocol and a start per transcript for 9 MB | at rest −16 % | — | Not merged: not worth the extra process |

Quality, Vulkan against CUDA, same model (`large-v3-turbo`): on the bench's 8 cases, 5 score a little higher, 3 the same, and ES2004a's call (first 5 minutes) loses on the side (96.7 → 93.0 %) with a fourth speaker. On whole meetings the differences go both ways: ES2004a import 96.7 → 94.1 % right person, ES2004a call 94.9 → 97.1 % right side and 92.4 → 93.5 % right person, IS1009a import 89.4 → 89.1 %, IS1009a call 34.7 → 33.0 % side and 73.1 → 75.0 % person. The speaker error is identical everywhere (the speakers are found on the CPU either way). Both builds fall into whisper's repetition loops, at different places: tiny numeric differences take the decoding down different paths. No bias either way: the same quality.

The preview, 10 alternating runs each: 43 lines every time for both; median delay 2.7 s (CUDA) and 2.5 s (Vulkan), 90th percentile 4.8 and 4.6 s, worst line 6.5 and 7.5 s, drafts 1.0 s at the 90th percentile for both. The differences are within the half-second tick: the same.

### The preview

`bench/preview.py` replayed the call fixture in whole seconds, rewriting the files each time, while the recorder appends every 40 to 60 ms (its 8 KiB buffer holds two or three 20 ms pieces): the replay now appends tenths of a second. With it, 10 runs: 46 lines (44 to 51: where a long stretch is cut now depends on when audio arrives), delay 2.8 s median, 5.0 s at the 90th percentile, drafts 0.3 / 1.5 s. Timing each pass: whisper takes 0.26 s for a batch at the median, but one pass in ten retries at a higher temperature and takes 1.3 to 1.4 s; drafts, twice a second on each side, take 0.26 s at the median and up to 2.4 s (4.3 s for the very first, while the GPU warms up).

| # | Hypothesis | Files | Result | Δ p90 delay | Δ RSS | Verdict |
|---|---|---|---|---|---|---|
| 8 | The preview sleeps half a second after its work rather than until the next half second, so each line also waits for the passes before it; counting whisper's time in the tick should shorten the waits behind slow passes | `live.rs` | 10 alternating runs: median 2.8 s both; 90th percentile 5.0 → 4.7 s (±0.13 / 0.15), worst line 6.2 → 5.5 s, drafts at the 90th percentile 1.5 → 1.4 s; 46 and 45.5 lines. The same work, done sooner after a slow pass | −6 % (worst −11 %) | — | Kept |

### Third round: the speakers on the GPU (2026-09-28)

| # | Hypothesis | Files | Result | Δ after Stop | Δ at rest | Verdict |
|---|---|---|---|---|---|---|
| 10 | Nemotron runs on the CPU through ONNX Runtime, 6 cores for 6 s per minute of speech, while the GPU waits. ONNX Runtime's WebGPU provider runs on Vulkan, like whisper, with no cuDNN to install; its prebuilt library comes with the `ort` crate | `nemotron.rs`, `engine/Cargo.toml`, `app/build.rs`, `install.sh` | Diarizing 5 minutes: 26.5 s → 5.6 s, the same 60 turns to the hundredth of a second, 256 MB of VRAM, the RTX picked on its own. 10 alternating runs, first 5 minutes of ES2004a: 42.1 s ± 2.3 % → 27.8 s ± 0.4 %, finding speakers 15.6 → 2.6 s. The 12 bench cases, whole meetings included, score exactly the same; the whole ES2004a call 178 → 135 s. The Dawn library (12 MB on disk) is linked at start: +3.5 MB at rest (57.8 → 61.3) | −34 % | +6 % | Kept |

### Fourth round: a quantized whisper model (2026-09-28)

| # | Hypothesis | Files | Result | Δ after Stop | Δ peak RSS | Verdict |
|---|---|---|---|---|---|---|
| 9 | Whisper's decoding reads the whole model for each token, and the RTX 2050 has a narrow memory bus (64 bits); large-v3-turbo in 8 or 5 bits (874 / 574 MB instead of 1.6 GB) should decode faster, at a quality to be measured | `models.rs` | First 5 minutes of ES2004a, 10 alternating runs, speakers on the CPU: f16 41.7 s ± 2.8 %, q8_0 39.6 s ± 1.9 %, q5_0 36.5 s ± 2.1 % (whisper 25.9, 23.9, 21.0 s); peak RSS 614, 550, 525 MB. But on the whole ES2004a meeting q5_0 is slower: whisper 125.6 s against 114.5 s (median of 3, speakers on the GPU), and 207.6 against 178.0 s in the quality run, q8_0 314 s; likely more of whisper's retries on hard passages. On the 42-minute hearing of the Assemblée nationale in French, 3 alternating runs: f16 235.1 s, q5_0 221.3 s (−5.9 %). Quality: q5_0 as good as f16 on 12 of 14 English cases and in French (2.2 % of the words differ, nearly all commas); q8_0 lost a stretch once, whole ES2004a import 94.1 → 71.3 % right person | q5_0 −12.5 % (5 min), +10 % (17 min, English), −5.9 % (42 min, French) | −15 % | Offered as options; the default stays large-v3-turbo, which no meeting made slower |
