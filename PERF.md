# Performance

What a call costs once you press Stop: the time until the transcript is ready, for a two-track call (your mic and the computer audio), with the CUDA build on a laptop with a Core Ultra 7 155H (22 threads) and an RTX 2050 (4 GB).

## Result

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
cargo build --release --features cuda                  # CUDAARCHS=86 for an RTX 30 series
bench/run.py --ami                                     # downloads AMI ES2004a into bench/.cache once
cp target/release/minutes bench/bin/after
bench/perf.py bench/bin/before bench/bin/after --runs 10            # first 5 minutes
bench/perf.py bench/bin/before bench/bin/after --runs 3 --minutes 0 # the whole meeting
bench/run.py --ami --check --bin bench/bin/after                    # quality
bench/run.py --ami --ami-meeting IS1009a --case ami-IS1009a-call --bin bench/bin/after
```

Close the browsers and anything else busy first: with them open the spread is ±15 % and nothing under that can be told apart. `bench/perf.py` alternates the binaries run after run, so slow drift weighs on all of them alike.

`bench/JOURNAL.md` has every attempt, the failed ones included. `bench/baseline.json` has the numbers before these changes.

## Watch

- `bench/run.py --ami --check` on every pull request that touches the engine: it fails when a case drops under `bench/thresholds.json`.
- `bench/perf.py` against the last release, CPU build on the CI runners (they have no GPU), 5 runs of the first 5 minutes: warn when the median grows by more than 10 %. On shared runners the spread is wider than here, so a smaller threshold would raise false alarms; the GPU numbers are checked here before a release.
