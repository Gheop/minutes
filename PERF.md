# Performance

What Minutes costs on a laptop with a Core Ultra 7 155H (22 threads) and an RTX 2050 (4 GB): the time from Stop until the transcript is ready, for a two-track call (your mic and the computer audio), the memory of the instance waiting at login, and the delay of the preview during a call.

## Second round: the GPU backend (2026-09-27)

Built with Vulkan instead of CUDA, on the same laptop, browsers and Teams closed:

| | CUDA | Vulkan | Change |
|---|---|---|---|
| Instance waiting at login, resident memory (10 starts) | 237.5 MB ± 0.3 | 57.7 MB ± 0.1 | −76 % |
| First 5 minutes of ES2004a after Stop (10 alternating runs) | 44.4 s ± 2.6 % | 42.3 s ± 2.4 % | −4.7 % |
| Same, largest RSS | 904 MB | 614 MB | −32 % |
| Preview, delay of its lines, median / 90th percentile (10 runs each) | 2.7 / 4.8 s | 2.5 / 4.6 s | same (tick is 0.5 s) |

The CUDA libraries are loaded with the program, and `libcublasLt` writes over 100 MB as it loads, so the instance started at login held about 180 MB for nothing all day. Vulkan links only the loader, which opens the driver when whisper starts. Transcripts are as good: on whole meetings the scores move by 2 to 3 points both ways between the two builds, the way whisper's decoding does with any numeric difference (`bench/JOURNAL.md` has every case).

Two things to know: whisper.cpp would take the first GPU ggml lists, which on this laptop is the Intel Arc (192 s instead of 42 s), so Minutes picks the first dedicated one; and the first transcript after a new build takes about 10 s longer, while the driver compiles the shaders.

The preview then waited less behind slow passes: its half-second tick now includes whisper's time instead of coming on top of it. Over 10 alternating runs its lines were 4.7 s behind the call at the 90th percentile instead of 5.0 s, and 5.5 s at worst instead of 6.2 s; the median stays at 2.8 s. The replay it is measured on now appends a tenth of a second at a time, as the recorder does; whole seconds had added half a second of delay the app does not have.

Not merged: running whisper in a worker process, so the app itself holds no GPU library (branch `perf-worker`). On top of Vulkan it saves 9 MB more at rest, not worth a second process and a protocol between them.

## First round: finding speakers where someone speaks


AMI ES2004a as a call (a real four-person meeting; one headset is your mic, the other three are the computer audio), median of 10 alternating runs, browsers and Teams closed:

| | Before | After | Change |
|---|---|---|---|
| First 5 minutes: wall time | 60.6 s ± 0.4 % | 43.2 s ± 0.4 % | −29 % |
| Finding speakers | 30.5 s | 15.7 s | −49 % |
| Loading whisper | 3.1 s | off the critical path | |
| Transcribing | 26.7 s | 27.2 s | +2 % |
| Largest RSS | 903 MB | 916 MB | +1.4 % |
| Whole meeting (17 min 29 s), 3 runs: wall time | 274.8 s ± 0.5 % | 233.9 s ± 0.1 % | −15 % |
| Whole meeting: finding speakers / transcribing | 109.6 s / 161.1 s | 70.8 s / 162.4 s | −35 % / +1 % |

The whole meeting gains less than its first 5 minutes: whisper weighs more there, and the three people on the computer side talk nearly all the time, so there is less silence to skip. How much a real call gains depends on how much of it is silence on each side.

Transcript quality did not move: every case of `bench/run.py --ami --check` scores the same to the tenth of a percent, and on AMI IS1009a, a meeting never used while tuning, the call scores went up (right side 32.3 → 34.7 %, right person 69.4 → 73.1 %).

## What paid

1. **Finding speakers only where someone speaks** (−26 %). Each side was diarized over its whole length. On the mic, everything but your own voice had been zeroed and still went through the model, several cores busy for every second of it. Now each side's speech regions are glued together, as they already were for whisper, and the turns found are moved back to the real timeline.
2. **Loading whisper while the speakers are found** (−4 %). Reading and uploading the 1.6 GB model barely uses the cores diarization needs. It is loaded ahead only when it is already on disk, so a first download keeps its progress bar.

## What did not pay

1. **Diarizing both sides at the same time.** Alone, each side already keeps about 6 cores busy (606–631 % CPU); together they split the same compute and each takes twice as long. The stage is bound by CPU throughput.
2. **Transcribing one side while the other side's speakers are found.** The sides go longest first, because that side's language counts for both, so the side left to diarize is the mic, which takes 3–4 s now, and whisper's own CPU threads compete for them. No change beyond noise, 32 MB more memory.
3. **Pinning the runs to the performance cores** (for measuring, not in the app). With other apps open, six cores for whisper, ONNX Runtime and everything else made runs slower and no steadier. What made measurements steady (±0.3 % instead of ±15 %) was closing the browsers and Teams.

Ruled out before trying: whisper's flash attention, which whisper.cpp does not combine with the DTW word times the app needs to split speakers; beam search, measured earlier on a French hearing: 30 % slower for 2 % of words changed, not for the better.

## Where the time goes now

For 5 minutes of call: whisper 27 s (63 %), finding speakers 16 s (36 %); for the whole 17-minute meeting, whisper 162 s (69 %) and finding speakers 71 s (30 %). Both are model inference; there is little left to skip.

- **Finding speakers on the GPU.** Nemotron runs on the CPU through ONNX Runtime. Its CUDA execution provider would move it to the GPU, where whisper has only 2.2 GB of 4 GB in use; the models run one after the other, so both fit. It needs cuDNN, a heavy dependency to ask of every user, so it would have to stay optional.
- **A smaller or quantized whisper model.** `large-v3-turbo` in q5 or q8 loads faster and needs less VRAM; whether French transcripts stay as good has to be measured, not assumed.

Both stages grow in a straight line with the length of the call: finding speakers took 6.1 s per minute of audio on 5 minutes and 6.3 s on the whole 17. A first profile of the whole meeting, taken with browsers open, had taken 8 min 15 s: that was the busy machine, not the laptop heating up.

## Reproduce

```bash
cargo build --release --features vulkan                # or cuda, with CUDAARCHS=86 for an RTX 30 series
bench/run.py --ami                                     # downloads AMI ES2004a into bench/.cache once
cp target/release/minutes bench/bin/after
bench/perf.py bench/bin/before bench/bin/after --runs 10            # first 5 minutes
bench/perf.py bench/bin/before bench/bin/after --runs 3 --minutes 0 # the whole meeting
bench/run.py --ami --check --bin bench/bin/after                    # quality
bench/run.py --ami --ami-meeting IS1009a --case ami-IS1009a-call --bin bench/bin/after
bench/idle.py bench/bin/before bench/bin/after                      # memory of the instance at rest
bench/preview.py TESTBIN_BEFORE TESTBIN_AFTER                       # preview delay; TESTBIN from cargo test --release --no-run
```

Close the browsers and anything else busy first: with them open the spread is ±15 % and nothing under that can be told apart. `bench/perf.py` alternates the binaries run after run, so slow drift weighs on all of them alike.

`bench/JOURNAL.md` has every attempt, the failed ones included. `bench/baseline.json` has the numbers before these changes.

## Watch

- `bench/run.py --ami --check` on every pull request that touches the engine: it fails when a case drops under `bench/thresholds.json`.
- `bench/perf.py` against the last release, CPU build on the CI runners (they have no GPU), 5 runs of the first 5 minutes: warn when the median grows by more than 10 %. On shared runners the spread is wider than here, so a smaller threshold would raise false alarms; the GPU numbers are checked here before a release.
