# Transcribing during the call: what was measured

Question: can the transcript be written while the call goes on, a few seconds behind, with the same quality as after the call? Answer so far: not with the same quality. Everything below is on the `live-sim` branch; `MINUTES_LIVE_SIM=<seconds>` replays a recording as it would be done live.

## How the replay works

Each decision only uses what has been heard so far plus 2 s ahead: the noise floor and the gain of each track are updated once a second, each stretch of speech gets the gain known when it ended, and whisper gets the stretches in batches (every stretch alone, or about 20 s of speech), with the text already written on that side as its context. The speakers are found the same way as after the call; the model already works in chunks.

## Results

Scores against the ground truth (`bench/run.py --ami`), words that differ between the live and the after-call transcript, and speaker labels:

| | After the call | Live, every stretch alone | Live, 20 s batches |
|---|---|---|---|
| Headset call (found / side / person) | 96.3 / 96.3 / 94.4 | 96.0 / 96.0 / 94.1 | 96.3 / 96.3 / 94.4 |
| Call through speakers | 96.3 / 96.3 / 94.4, no echo | 96.0, 2 echo lines, 5 voices for 4 | 96.0 / 96.0 / 94.1 |
| Two people at one mic | 96.0 | 94.9 | 95.4 |
| AMI ES2004a, first 5 min (side / person) | 96.7 / 95.7 | 98.2 / 95.0, 4 voices for 3 | 100.0 / 96.5, 4 voices for 3 |
| AMI IS1009a, first 5 min (side / person) | 34.7 / 73.1 | 62.8 / 61.5, 5 voices for 4 | 58.7 / 67.1, 5 voices for 4 |
| French hearing, 42 min: words that differ | | 2.2 % | 2.2 % |
| AMI ES2004a whole (17 min): words that differ | | 15.7 % | 15.7 % |

On clear speech the text is the same, within what whisper changes anyway when its input moves a little (beam search changed 1.9 % of words, a prompt 2.2 %). On a real spontaneous meeting it is not: the live transcript has 19 turns of yours where the after-call one has 40.

## Why

`regions_live_vs_offline` (an ignored test in `transcribe.rs`) compares the speech found on the mic, minute by minute. All of the loss is in the first 8 minutes; after that the two agree to the frame. The noise threshold is not the cause: it settles within 5 s. The gain is: after the call it comes from the 95th percentile of the level over the whole meeting, which depends on how much of the meeting you spent talking. At the start of ES2004a the project manager talks a lot, the level so far is high, the gain low, and her quieter stretches fall under the threshold. That number cannot be known during the call.

## Tried and dropped

**Levelling on the speech alone** (90th percentile of the frames above the noise floor), in both modes, so that both could compute it early: it made the after-call transcript worse (AMI ES2004a side 96.7 → 92.8 %, a voice too many) and the live one further from it (41.7 % of the words differ on the whole meeting). Reverted.

**Speech detection that does not depend on the gain**, in both modes: speech is found on the raw tracks above `max(4 × noise floor, -72 dBFS)`, and the echo test compares the mic and the computer audio each against its own speech level (the 90th percentile of its speech frames), both of which are known a few seconds into a call. `regions_live_vs_offline` then shows live and after-call detection agreeing to the frame on the threshold, and the speech lost on the ES2004a mic going from about 73 s to 25 s (0 s on IS1009a).

| | After the call, as it was | After the call, new detection | Live, new detection, 20 s batches |
|---|---|---|---|
| Headset call (found / side / person) | 96.3 / 96.3 / 94.4 | 96.0 / 96.0 / 94.1 | 95.0 / 95.0 / 93.1 |
| Call through speakers | 96.3 / 96.3 / 94.4 | 96.0 / 96.0 / 94.1 | 92.5 / 92.2 / 90.3, 5 voices for 4 |
| AMI ES2004a (side / person) | 96.7 / 95.7 | 97.8 / 96.8 | 99.5 / 98.4 |
| AMI IS1009a (side / person) | 34.7 / 73.1 | 34.4 / 73.8 | 46.6 / 75.7, 5 voices for 4 |
| ES2004a whole: words that differ from after the call | | | 11.1 % (was 15.7 %); your turns 31 and 31 |

Much closer, but still not the same: through speakers the live transcript is 3.8 points worse with a voice that is not there. Reverted; the after-call detection stays as it was, and is what a live mode has to match.

**Hybrid** (`MINUTES_LIVE_SIM=hybrid:20`): stretches transcribed during the call; at Stop, speech found again over the whole call as after the call, the stretches that came out exactly the same keep their live text, the others are transcribed again, and the speakers are found over the whole call.

| | After the call | Hybrid |
|---|---|---|
| Stop to transcript, ES2004a first 5 min | 43 s | 35 s (20 of 28 stretches kept) |
| Stop to transcript, ES2004a whole | 234 s | 178 s (47 of 141 stretches kept) |
| Bench (5 min cases) | as above | same or better, one voice too many on IS1009a |
| ES2004a whole: words | 2821 | 2443, 60 lines for 162: a fault in the simulation, not found |

The wait shrinks by a fifth to a quarter. Two things bound it whatever the fault: a stretch whose edges moved by a single frame live must be transcribed again, and two thirds of them did; and finding the speakers over the whole call at Stop takes about 70 s for 17 minutes on its own. Transcribing during the call with the same quality as after it, a few seconds behind, is not reached with this engine.

## What is left

- **Hybrid**: transcribe live for a draft to read during the call; when it ends, find the speech again over the whole call (under a second), transcribe only the stretches that came out differently, and find the speakers over the whole call. Same result as today by construction; the wait after the call shrinks without going away.
- **Speech detection that does not depend on the gain**, in both modes, measured on the bench before anything else.
