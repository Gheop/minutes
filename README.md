# Minutes

A meeting recorder and transcriber for GNOME. It records your microphone and what your computer plays as two tracks, and when the meeting ends you get a transcript with who said what, made on your own machine.

No bot joins the call and no audio leaves your computer. It works with Teams, Meet, Zoom or anything else that plays sound, because it listens to your devices, not to the meeting service.

> **Status: early.** The engine records, transcribes and tells speakers apart, and is tested. The GNOME app records, transcribes and shows the transcript; a Shell extension shows it in the top bar. Renaming speakers, playback, preferences and call detection are still to come.

<p align="center"><img src="docs/screenshots/ready.webp" alt="Minutes ready to record: the meeting name, the language, the level of your microphone and of the computer audio, and the Record button" width="480">&nbsp;<img src="docs/screenshots/transcript.webp" alt="A transcript in Minutes: each paragraph with its speaker and time, the meetings listed on the left" width="480"></p>

## Where it comes from

Minutes starts from [omarchy-meeting-recorder](https://github.com/jankeesvw/omarchy-meeting-recorder) by Jankees van Woezik, a recorder built for Omarchy and Hyprland. Its engine is kept: two-track capture through PipeWire, echo removal, local transcription with [whisper.cpp](https://github.com/ggml-org/whisper.cpp), speaker diarization with NVIDIA's [Nemotron 3 Diarization](https://huggingface.co/nvidia/Nemotron-3-Diarization), crash recovery and the benchmark suite. The git history is kept too, so fixes made there can be brought here with `git cherry-pick`.

What Minutes changes:

- **A native GNOME app** in GTK 4 and libadwaita, following the GNOME Human Interface Guidelines, translated with gettext (English and French first).
- **A GNOME Shell extension**: recording state in the top bar, controls, and a notification when a transcript is ready.
- **Call detection**: when an app opens the microphone and plays sound for a while, Minutes offers to record. It asks; it does not start on its own, because the other people in the call have to know they are recorded.
- **Real names for the other side**, taken from the meeting app where it exposes them (Teams first), instead of "Remote 1" and "Remote 2".
- **GPU transcription on NVIDIA cards** through CUDA, and a glossary for names and jargon whisper gets wrong.

## Build and run

```bash
cargo run --release -p minutes                                  # CPU
CUDAARCHS=86 cargo run --release -p minutes --features cuda     # NVIDIA GPU; set your card's compute capability
```

It needs PipeWire with `parec` and `pacat`, `ffmpeg` with libopus, GTK 4 and libadwaita 1.6 or newer, gettext, and Rust and CMake to build. The CUDA build needs the CUDA toolkit with `nvcc` on the `PATH`, recent enough for your GCC.

`minutes <meeting folder>` opens a meeting. `minutes transcribe <mic> <computer>` and `minutes transcribe-file <audio>` print a transcript as Markdown without opening a window.

## GNOME Shell extension

`extension/` shows in the top bar while Minutes records, is paused or writes a transcript: a red dot and the time, with a menu to pause, stop or open Minutes. It stays hidden the rest of the time. It reads Minutes' state over D-Bus (the `status` action of the app), so it needs nothing but Minutes running.

```bash
extension/build.sh --install                  # then log out and in
gnome-extensions enable minutes@gheop.github
```

## Layout

- `engine/`: recording, transcription, speakers and meeting folders, without GTK, so it runs and is tested without a display. `engine/src/session.rs` is one meeting from Start to transcript.
- `app/`: the GNOME app (GTK 4, libadwaita, translations in `app/po/`).
- `legacy/`: the interface Minutes was forked from, kept until the new one does everything it did. `bench/run.py` still runs its binary.
- `extension/`: the GNOME Shell extension; `status.js` is what it shows for each state, apart from the Shell so it can be tested.
- `bench/`: transcript quality (`run.py`, with thresholds) and timing (`perf.py`); `PERF.md` has the numbers.

## Testing

```bash
cargo test --workspace --release        # the window test needs a display, and is skipped without one
gjs -m extension/tests/status.test.js   # what the top bar shows
extension/tests/shell-smoke.sh          # loads the extension into a GNOME Shell with no screen
bench/run.py --ami --check              # transcript and speaker quality against the thresholds
```

## Configuration

`~/.config/omarchy-meeting-recorder/config.toml` (the path moves when the app is renamed):

```toml
model = "large-v3-turbo"
prompt = "Budget review with Maya Okafor and Tom Lindqvist: the CAPEX, the SLA, Kubernetes."
fix = "Okafur => Okafor"
```

`prompt` tells whisper the names and words to expect; it only reads it for the first half minute or so, so a word it keeps getting wrong later needs a `fix` line, which replaces it in the finished transcript.

## License

MIT, like the project it comes from. See [LICENSE](LICENSE).

## Changelog

### v0.1.0 — A GNOME app on the engine of omarchy-meeting-recorder (2026-09-26)

- New GNOME interface in GTK 4 and libadwaita: the meetings on the left; getting ready, recording, writing the transcript and reading it on the right
- In English and French
- A GNOME Shell extension shows the recording in the top bar, with pause and stop, and a notification says when the transcript is ready
- GPU transcription on NVIDIA cards with the `cuda` feature
- `prompt` and `fix` in the config for names and words whisper gets wrong
- Speakers are found only where someone speaks: a call is transcribed 15 to 30 % faster, with the same quality
- The whisper model loads while the speakers are found
- No more "unknown language" warning when nothing was said
