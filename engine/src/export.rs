//! Turns the two raw tracks into the audio file(s) the user asked for.

use std::fs::OpenOptions;
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::audio::{CHANNELS, RATE};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Mono,
    Stereo,
    Separate,
}

impl Format {
    pub const ALL: [Format; 3] = [Format::Mono, Format::Stereo, Format::Separate];

    pub fn key(self) -> &'static str {
        match self {
            Format::Mono => "mono",
            Format::Stereo => "stereo",
            Format::Separate => "separate",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Format::Mono => "Mono",
            Format::Stereo => "Stereo (mic left, computer right)",
            Format::Separate => "Separate files",
        }
    }

    /// For the one-line summary on the done page.
    pub fn short_label(self) -> &'static str {
        match self {
            Format::Mono => "Mono",
            Format::Stereo => "Stereo",
            Format::Separate => "Separate files",
        }
    }

    pub fn from_key(key: &str) -> Format {
        Format::ALL
            .into_iter()
            .find(|f| f.key() == key)
            .unwrap_or(Format::Mono)
    }
}

/// Appends silence so both tracks are equally long and stay aligned at the end.
fn pad_to_same_length(paths: [&Path; 2]) -> std::io::Result<()> {
    let sizes = [
        std::fs::metadata(paths[0])?.len(),
        std::fs::metadata(paths[1])?.len(),
    ];
    let length = sizes[0].max(sizes[1]);
    for (path, size) in paths.into_iter().zip(sizes) {
        if size < length {
            let mut file = OpenOptions::new().append(true).open(path)?;
            file.write_all(&vec![0u8; (length - size) as usize])?;
        }
    }
    Ok(())
}

/// Speech level both tracks are brought to: the 95th percentile of 50 ms RMS
/// frames, about -18 dBFS. Loud enough to listen to, with room for peaks.
const TARGET_LEVEL: f64 = 0.125;
/// Below this (about -55 dBFS) a track is silence or hiss and is left alone,
/// so the noise floor is never pumped up to speech level.
const SILENT_LEVEL: f64 = 0.0018;
const MAX_BOOST_DB: f64 = 24.0;
const MAX_CUT_DB: f64 = -12.0;

/// The fixed gain in dB that brings the speech in a raw track to `TARGET_LEVEL`.
/// One gain for the whole track keeps its dynamics; the limiter after it
/// catches the peaks.
pub fn speech_gain_db(raw: &Path) -> f64 {
    let Ok(file) = std::fs::File::open(raw) else {
        return 0.0;
    };
    let mut reader = BufReader::new(file);
    let frame_bytes = (RATE / 20 * CHANNELS * 2) as usize; // 50 ms
    let mut buf = vec![0u8; frame_bytes];
    let mut levels = Vec::new();
    while reader.read_exact(&mut buf).is_ok() {
        let sum: f64 = buf
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| f64::from(i16::from_le_bytes(*b)) / 32768.0)
            .map(|v| v * v)
            .sum();
        levels.push((sum / (frame_bytes / 2) as f64).sqrt());
    }
    if levels.is_empty() {
        return 0.0;
    }
    levels.sort_by(f64::total_cmp);
    let p95 = levels[(levels.len() - 1) * 95 / 100];
    if p95 < SILENT_LEVEL {
        return 0.0;
    }
    (20.0 * (TARGET_LEVEL / p95).log10()).clamp(MAX_CUT_DB, MAX_BOOST_DB)
}

/// Encodes `mic_raw` and `system_raw` into `out`. True when every file was written.
/// Each track is levelled on its own first, so a quiet side is as easy to hear
/// as a loud one.
pub fn export_audio(mic_raw: &Path, system_raw: &Path, out: &Path, format: Format) -> bool {
    for path in [mic_raw, system_raw] {
        if OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .is_err()
        {
            return false;
        }
    }
    if pad_to_same_length([mic_raw, system_raw]).is_err() {
        return false;
    }
    if std::fs::metadata(mic_raw).map(|m| m.len()).unwrap_or(0) == 0 {
        return true; // nothing was recorded
    }

    let rate = RATE.to_string();
    let channels = CHANNELS.to_string();
    let input = |path: &Path| -> Vec<String> {
        ["-f", "s16le", "-ar", &rate, "-ac", &channels, "-i"]
            .iter()
            .map(|s| s.to_string())
            .chain([path.to_string_lossy().into_owned()])
            .collect()
    };
    let to_mono = "pan=mono|c0=0.5*c0+0.5*c1";
    let target = |name: &str| out.join(name).to_string_lossy().into_owned();
    let mic_gain = format!("volume={:.1}dB", speech_gain_db(mic_raw));
    let system_gain = format!("volume={:.1}dB", speech_gain_db(system_raw));
    let limit = "alimiter=limit=0.9:level=disabled";

    let jobs: Vec<Vec<String>> = match format {
        Format::Separate => vec![
            [
                input(mic_raw),
                vec!["-af".into(), format!("{to_mono},{mic_gain},{limit}")],
                strings(&["-c:a", "libopus", "-b:a", "64k"]),
                vec![target("mic.ogg")],
            ]
            .concat(),
            [
                input(system_raw),
                vec!["-af".into(), format!("{system_gain},{limit}")],
                strings(&["-c:a", "libopus", "-b:a", "96k"]),
                vec![target("computer.ogg")],
            ]
            .concat(),
        ],
        Format::Stereo => vec![
            [
                input(mic_raw),
                input(system_raw),
                vec![
                    "-filter_complex".into(),
                    format!(
                        "[0]{to_mono},{mic_gain}[l];[1]{to_mono},{system_gain}[r];\
                         [l][r]join=inputs=2:channel_layout=stereo,{limit}"
                    ),
                ],
                strings(&["-c:a", "libopus", "-b:a", "96k"]),
                vec![target("audio.ogg")],
            ]
            .concat(),
        ],
        Format::Mono => vec![
            [
                input(mic_raw),
                input(system_raw),
                vec![
                    "-filter_complex".into(),
                    format!(
                        "[0]{to_mono},{mic_gain}[a];[1]{to_mono},{system_gain}[b];\
                         [a][b]amix=inputs=2:normalize=0,{limit}"
                    ),
                ],
                strings(&["-c:a", "libopus", "-b:a", "64k"]),
                vec![target("audio.ogg")],
            ]
            .concat(),
        ],
    };

    jobs.iter().all(|args| {
        Command::new("ffmpeg")
            .args(["-y", "-loglevel", "error"])
            .args(args)
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    })
}

/// The hidden directory in a meeting folder that keeps both tracks, so the
/// meeting can be transcribed again with You and Remote apart, whatever format
/// the user picked.
pub const TRACKS_DIR: &str = ".tracks";

pub fn tracks(meeting_dir: &Path) -> (PathBuf, PathBuf) {
    let dir = meeting_dir.join(TRACKS_DIR);
    (dir.join("mic.ogg"), dir.join("computer.ogg"))
}

/// Writes both raw tracks as mono Opus into `meeting_dir/.tracks`.
pub fn export_tracks(mic_raw: &Path, system_raw: &Path, meeting_dir: &Path) -> bool {
    let (mic, computer) = tracks(meeting_dir);
    if std::fs::create_dir_all(meeting_dir.join(TRACKS_DIR)).is_err() {
        return false;
    }
    let (rate, channels) = (RATE.to_string(), CHANNELS.to_string());
    [(mic_raw, mic), (system_raw, computer)]
        .iter()
        .all(|(raw, target)| {
            Command::new("ffmpeg")
                .args([
                    "-y",
                    "-loglevel",
                    "error",
                    "-f",
                    "s16le",
                    "-ar",
                    &rate,
                    "-ac",
                    &channels,
                    "-i",
                ])
                .arg(raw)
                .args(["-ac", "1", "-c:a", "libopus", "-b:a", "48k"])
                .arg(target)
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        })
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A raw track (s16le, RATE, CHANNELS) of a 440 Hz tone at `amplitude`.
    fn tone(path: &Path, amplitude: f64, secs: u32) {
        let mut bytes = Vec::new();
        for i in 0..RATE * secs {
            let v = (amplitude
                * (i as f64 * 440.0 * std::f64::consts::TAU / RATE as f64).sin()
                * 32767.0) as i16;
            for _ in 0..CHANNELS {
                bytes.extend_from_slice(&v.to_le_bytes());
            }
        }
        std::fs::write(path, bytes).unwrap();
    }

    fn mean_db(path: &Path) -> f64 {
        let out = Command::new("ffmpeg")
            .args(["-hide_banner", "-i"])
            .arg(path)
            .args(["-af", "volumedetect", "-f", "null", "-"])
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stderr);
        let at = text.find("mean_volume: ").unwrap() + "mean_volume: ".len();
        text[at..]
            .split_whitespace()
            .next()
            .unwrap()
            .parse()
            .unwrap()
    }

    #[test]
    fn quiet_and_loud_tracks_end_up_at_the_same_level() {
        let dir = std::env::temp_dir().join(format!("omr-export-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (mic, system) = (dir.join("mic.raw"), dir.join("system.raw"));
        tone(&mic, 0.5, 3); // loud
        tone(&system, 0.01, 3); // 34 dB quieter
        assert!(export_audio(&mic, &system, &dir, Format::Separate));
        let (loud, quiet) = (
            mean_db(&dir.join("mic.ogg")),
            mean_db(&dir.join("computer.ogg")),
        );
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(
            (loud - quiet).abs() < 1.5,
            "mic {loud} dB, computer {quiet} dB"
        );
    }

    #[test]
    fn silence_is_not_boosted() {
        let dir = std::env::temp_dir().join(format!("omr-silence-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let raw = dir.join("hiss.raw");
        tone(&raw, 0.0005, 2); // about -66 dBFS
        let gain = speech_gain_db(&raw);
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(gain, 0.0);
    }
}
