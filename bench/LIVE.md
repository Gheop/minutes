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

## What is left

- **Hybrid**: transcribe live for a draft to read during the call; when it ends, find the speech again over the whole call (under a second), transcribe only the stretches that came out differently, and find the speakers over the whole call. Same result as today by construction; the wait after the call shrinks without going away.
- **Speech detection that does not depend on the gain**, in both modes, measured on the bench before anything else.
