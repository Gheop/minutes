# Minutes

A meeting recorder and transcriber for GNOME. It records your microphone and what your computer plays as two tracks, and when the meeting ends you get a transcript with who said what, made on your own machine.

No bot joins the call and no audio leaves your computer. It works with Teams, Meet, Zoom or anything else that plays sound, because it listens to your devices, not to the meeting service.

> **Status: early.** The recording and transcription engine works and is tested. The GNOME app, the Shell extension and the call detection described below are being written; today the program still carries the interface it was forked from.

## Where it comes from

Minutes starts from [omarchy-meeting-recorder](https://github.com/jankeesvw/omarchy-meeting-recorder) by Jankees van Woezik, a recorder built for Omarchy and Hyprland. Its engine is kept: two-track capture through PipeWire, echo removal, local transcription with [whisper.cpp](https://github.com/ggml-org/whisper.cpp), speaker diarization with NVIDIA's [Nemotron 3 Diarization](https://huggingface.co/nvidia/Nemotron-3-Diarization), crash recovery and the benchmark suite. The git history is kept too, so fixes made there can be brought here with `git cherry-pick`.

What Minutes changes:

- **A native GNOME app** in GTK 4 and libadwaita, following the GNOME Human Interface Guidelines, translated with gettext (English and French first).
- **A GNOME Shell extension**: recording state in the top bar, controls, and a notification when a transcript is ready.
- **Call detection**: when an app opens the microphone and plays sound for a while, Minutes offers to record. It asks; it does not start on its own, because the other people in the call have to know they are recorded.
- **Real names for the other side**, taken from the meeting app where it exposes them (Teams first), instead of "Remote 1" and "Remote 2".
- **GPU transcription on NVIDIA cards** through CUDA, and a glossary for names and jargon whisper gets wrong.

## Build

For now, the same as the engine it comes from:

```bash
cargo build --release                           # CPU
CUDAARCHS=86 cargo build --release --features cuda   # NVIDIA GPU; set your card's compute capability
```

It needs PipeWire with `parec` and `pacat`, `ffmpeg` with libopus, GTK 4 and libadwaita 1.6 or newer, and Rust and CMake to build. The CUDA build needs the CUDA toolkit with `nvcc` on the `PATH`.

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
