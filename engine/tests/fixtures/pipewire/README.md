# PipeWire graphs for `calls.rs`

Stream nodes from `pw-dump`, cut down to the properties the call detection reads.

- `minutes-only.json`: real, taken while Minutes listened to the microphone and the speakers (its two `parec` streams; the second captures the speakers, `stream.capture.sink: true`).
- `teams-call.json`, `video.json`, `voice-memo.json`: made by hand on the same model. Replace them with real dumps when you can: `pw-dump > dump.json` during a Teams call, a video in a browser and a voice memo, then keep only the stream nodes.
