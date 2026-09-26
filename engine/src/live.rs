//! The preview written while a call goes on: every few seconds, the audio
//! recorded so far is read from the staging files, the stretches of speech
//! that have ended are transcribed in batches, and their lines are sent out.
//!
//! It is a preview. Speech is found on what has been heard so far, voices on
//! one side are not told apart, and each batch is transcribed on its own; the
//! transcript made after the call is the one to keep. Each line has its times,
//! so the app can name the speaker from another source (the meeting app).

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::transcribe::{
    Abort, Downsampler, Region, Segment, WHISPER_RATE, load_preview_whisper, preview_pass, preview_regions,
    raw_to_mono,
};

/// Speech waits for this much more of it before whisper gets a batch.
const BATCH: usize = WHISPER_RATE * 8;
/// Unless the oldest stretch waiting ended this long ago.
const MAX_WAIT: usize = WHISPER_RATE * 10;
/// A stretch counts as ended when this much has been heard after it.
const SETTLED: usize = WHISPER_RATE * 2;

/// The preview of one recording, running in its own thread.
pub struct Preview {
    stop: Abort,
    thread: Option<JoinHandle<Vec<Segment>>>,
}

impl Preview {
    /// Starts previewing the recording in `staging`. Lines go to `lines` as
    /// they are written. Without a model for it (see `models::preview`) the
    /// preview does nothing.
    pub fn start(staging: &Path, language: &str, lines: async_channel::Sender<Vec<Segment>>) -> Preview {
        let stop: Abort = Arc::new(AtomicBool::new(false));
        let thread = crate::models::preview().map(|model| {
            let (staging, language, stop) = (staging.to_path_buf(), language.to_owned(), stop.clone());
            std::thread::spawn(move || run(&model, &staging, &language, &lines, &stop))
        });
        Preview { stop, thread }
    }

    /// Stops the preview, freeing the model (and the GPU memory the final
    /// transcript needs), and returns every line written. Waits for the batch
    /// being transcribed, a few seconds at most.
    pub fn finish(mut self) -> Vec<Segment> {
        self.stop.store(true, Ordering::Relaxed);
        self.thread.take().and_then(|t| t.join().ok()).unwrap_or_default()
    }
}

impl Drop for Preview {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Follows one raw file as it grows.
struct Follow {
    path: PathBuf,
    read: u64,
    /// Bytes of a stereo frame not complete yet.
    carry: Vec<u8>,
    down: Downsampler,
    samples: Vec<f32>,
}

impl Follow {
    fn new(path: PathBuf) -> Self {
        Self { path, read: 0, carry: Vec::new(), down: Downsampler::new(), samples: Vec::new() }
    }

    fn catch_up(&mut self) {
        let Ok(mut file) = File::open(&self.path) else {
            return;
        };
        let mut new = Vec::new();
        if file.seek(SeekFrom::Start(self.read)).is_err() || file.read_to_end(&mut new).is_err() {
            return;
        }
        self.read += new.len() as u64;
        let mut bytes = std::mem::take(&mut self.carry);
        bytes.extend_from_slice(&new);
        let whole = bytes.len() / 4 * 4;
        self.carry = bytes[whole..].to_vec();
        let mono = raw_to_mono(&bytes[..whole]);
        self.samples.extend(self.down.push(&mono));
    }
}

fn run(
    model: &Path,
    staging: &Path,
    language: &str,
    lines: &async_channel::Sender<Vec<Segment>>,
    stop: &Abort,
) -> Vec<Segment> {
    let Ok(context) = load_preview_whisper(model) else {
        return Vec::new();
    };
    let (mic_raw, system_raw) = crate::session::raw_tracks(staging);
    let (mut mic, mut computer) = (Follow::new(mic_raw), Follow::new(system_raw));
    // Per side: how far its stretches are transcribed, and what it said.
    let mut done = [0usize; 2];
    let mut earlier = [String::new(), String::new()];
    let mut written = Vec::new();
    while !stop.load(Ordering::Relaxed) {
        for _ in 0..20 {
            if stop.load(Ordering::Relaxed) {
                return written;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        mic.catch_up();
        computer.catch_up();
        let heard = mic.samples.len().min(computer.samples.len());
        if heard < SETTLED {
            continue;
        }
        let (mic_track, mic_regions, computer_track, computer_regions) =
            preview_regions(&mic.samples[..heard], &computer.samples[..heard]);
        for (side, (track, regions, label)) in [
            (&mic_track, &mic_regions, crate::meeting::DEFAULT_YOU),
            (&computer_track, &computer_regions, crate::meeting::DEFAULT_REMOTE),
        ]
        .into_iter()
        .enumerate()
        {
            let ready = ready_batch(regions, done[side], heard);
            if ready.is_empty() {
                continue;
            }
            match preview_pass(&context, track, &ready, label, language, &earlier[side], stop) {
                Ok(new) => {
                    done[side] = ready.last().map_or(done[side], |r| r.end);
                    for line in &new {
                        earlier[side].push(' ');
                        earlier[side].push_str(&line.text);
                    }
                    if !new.is_empty() {
                        let _ = lines.send_blocking(new.clone());
                        written.extend(new);
                    }
                }
                Err(_) => return written,
            }
        }
    }
    written
}

/// The stretches after `done` that have ended, when there is enough of them
/// to be worth a batch or the oldest has waited long enough.
fn ready_batch(regions: &[Region], done: usize, heard: usize) -> Vec<Region> {
    let ended: Vec<Region> = regions
        .iter()
        .copied()
        .filter(|r| r.start >= done && r.end + SETTLED <= heard)
        .collect();
    let speech: usize = ended.iter().map(|r| r.end - r.start).sum();
    match ended.first() {
        Some(first) if speech >= BATCH || heard - first.end >= MAX_WAIT => ended,
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn region(start_secs: f64, end_secs: f64) -> Region {
        let at = |s: f64| (s * WHISPER_RATE as f64) as usize;
        Region { start: at(start_secs), onset: at(start_secs), end: at(end_secs) }
    }

    /// Records a call as the app would, ten times faster, with the preview
    /// running, and prints the lines as they come:
    /// `MINUTES_PREVIEW_MIC=mic.ogg MINUTES_PREVIEW_COMPUTER=computer.ogg cargo test --release -- --ignored --nocapture preview_follows_a_recording`
    #[test]
    #[ignore]
    fn preview_follows_a_recording() {
        let (Ok(mic), Ok(computer)) = (std::env::var("MINUTES_PREVIEW_MIC"), std::env::var("MINUTES_PREVIEW_COMPUTER")) else {
            return;
        };
        let raw = |path: &str| {
            let out = std::process::Command::new("ffmpeg")
                .args(["-v", "error", "-i", path, "-ac", "2", "-ar", "48000", "-f", "s16le", "-"])
                .output()
                .unwrap();
            out.stdout
        };
        let (mic, computer) = (raw(&mic), raw(&computer));
        let staging = std::env::temp_dir().join(format!("minutes-preview-{}", std::process::id()));
        std::fs::create_dir_all(&staging).unwrap();
        let (mic_path, computer_path) = crate::session::raw_tracks(&staging);
        let (tx, rx) = async_channel::unbounded();
        let started = std::time::Instant::now();
        let preview = Preview::start(&staging, "en", tx);
        // A second of audio every 100 ms.
        let second = 48_000 * 4;
        let mut written = 0;
        while written < mic.len().max(computer.len()) {
            written = (written + second).min(mic.len().max(computer.len()));
            std::fs::write(&mic_path, &mic[..written.min(mic.len())]).unwrap();
            std::fs::write(&computer_path, &computer[..written.min(computer.len())]).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(100));
            while let Ok(lines) = rx.try_recv() {
                for line in lines {
                    println!(
                        "[{:>5.1}s, audio at {:>3}s] {} {:>3}s: {}",
                        started.elapsed().as_secs_f64(),
                        written / second,
                        line.speaker,
                        line.start_ms / 1000,
                        line.text
                    );
                }
            }
        }
        // Let the last stretches settle and be written, as the call goes on.
        std::thread::sleep(std::time::Duration::from_secs(8));
        let lines = preview.finish();
        println!("{} lines in all", lines.len());
        let _ = std::fs::remove_dir_all(&staging);
        assert!(!lines.is_empty());
    }

    #[test]
    fn a_batch_waits_for_enough_speech_or_for_time() {
        let regions = [region(0.0, 3.0), region(4.0, 7.0), region(8.0, 30.0)];
        let secs = |s: usize| s * WHISPER_RATE;
        // Two short stretches have ended, 6 s of speech: wait.
        assert!(ready_batch(&regions, 0, secs(10)).is_empty());
        // The long one has ended too: 28 s, go.
        assert_eq!(ready_batch(&regions, 0, secs(33)).len(), 3);
        // After the first two, nothing ended waits.
        assert!(ready_batch(&regions, secs(7), secs(20)).is_empty());
        // A short stretch alone goes once it has waited long enough.
        assert_eq!(ready_batch(&regions[..1], 0, secs(14)).len(), 1);
    }

    #[test]
    fn a_growing_raw_file_is_read_to_the_end_once() {
        let dir = std::env::temp_dir().join(format!("minutes-follow-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mic.raw");
        // One second of 48 kHz stereo, written in two uneven pieces.
        let frame = |i: i16| [i.to_le_bytes(), i.to_le_bytes()].concat();
        let bytes: Vec<u8> = (0..48_000).flat_map(|i| frame((i % 100) as i16)).collect();
        std::fs::write(&path, &bytes[..70_001]).unwrap();
        let mut follow = Follow::new(path.clone());
        follow.catch_up();
        std::fs::write(&path, &bytes).unwrap();
        follow.catch_up();
        follow.catch_up();
        assert_eq!(follow.read, bytes.len() as u64);
        assert!(follow.carry.is_empty());
        // 16 kHz, less the last few samples the filter still waits on.
        assert!((15_960..=16_000).contains(&follow.samples.len()), "{}", follow.samples.len());
        let _ = std::fs::remove_dir_all(dir);
    }
}
