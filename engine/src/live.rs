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
    Abort, Downsampler, FRAME, Levels, Region, Segment, WHISPER_RATE, load_preview_whisper,
    preview_pass, preview_regions, raw_to_mono,
};

/// A stretch counts as ended when this much has been heard after it.
const SETTLED: usize = WHISPER_RATE * 4 / 5;
/// Someone talking on without a pause: a stretch still going is cut once it
/// is this long, at its quietest moment, rather than waited for.
const LONG: usize = WHISPER_RATE * 5;
/// How much before where the transcription is up to the stretches are looked
/// for again, so one going on there is found as it would be on the whole
/// recording.
const MARGIN: usize = WHISPER_RATE * 2;

/// What the preview sends out as it goes.
#[derive(Debug, Clone)]
pub enum Update {
    /// Lines of stretches that have ended: they stay.
    Lines(Vec<Segment>),
    /// The stretch someone is still in, as far as it has been heard: shown
    /// until the next draft on that side replaces it; empty text clears it.
    Draft {
        speaker: &'static str,
        start_ms: i64,
        heard_ms: i64,
        text: String,
    },
}

/// The preview of one recording, running in its own thread.
pub struct Preview {
    stop: Abort,
    thread: Option<JoinHandle<Vec<Segment>>>,
}

impl Preview {
    /// Starts previewing the recording in `staging`; what it writes goes to
    /// `updates`. Without a model for it (see `models::preview`) the preview
    /// does nothing.
    pub fn start(
        staging: &Path,
        language: &str,
        updates: async_channel::Sender<Update>,
    ) -> Preview {
        let stop: Abort = Arc::new(AtomicBool::new(false));
        let thread = crate::models::preview().map(|model| {
            let (staging, language, stop) =
                (staging.to_path_buf(), language.to_owned(), stop.clone());
            std::thread::spawn(move || run(&model, &staging, &language, &updates, &stop))
        });
        Preview { stop, thread }
    }

    /// Stops the preview, freeing the model (and the GPU memory the final
    /// transcript needs), and returns every line written. Waits for the batch
    /// being transcribed, a few seconds at most.
    pub fn finish(mut self) -> Vec<Segment> {
        self.stop.store(true, Ordering::Relaxed);
        self.thread
            .take()
            .and_then(|t| t.join().ok())
            .unwrap_or_default()
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
    /// The recording from sample `base` on; what is before was let go of.
    samples: Vec<f32>,
    base: usize,
}

impl Follow {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            read: 0,
            carry: Vec::new(),
            down: Downsampler::new(),
            samples: Vec::new(),
            base: 0,
        }
    }

    /// Samples heard so far, including those let go of.
    fn heard(&self) -> usize {
        self.base + self.samples.len()
    }

    /// `samples[from..to]` on the recording's timeline.
    fn between(&self, from: usize, to: usize) -> &[f32] {
        &self.samples[from - self.base..to - self.base]
    }

    /// Lets go of the samples before `at`.
    fn forget_before(&mut self, at: usize) {
        if at > self.base {
            self.samples.drain(..at - self.base);
            self.base = at;
        }
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
    updates: &async_channel::Sender<Update>,
    stop: &Abort,
) -> Vec<Segment> {
    let context = match load_preview_whisper(model) {
        Ok(context) => context,
        Err(e) => {
            crate::warn(format!("no preview: {e}"));
            return Vec::new();
        }
    };
    let (mic_raw, system_raw) = crate::session::raw_tracks(staging);
    let (mut mic, mut computer) = (Follow::new(mic_raw), Follow::new(system_raw));
    // Per side: how far its stretches are transcribed, and what it said.
    let mut done = [0usize; 2];
    let mut earlier = [String::new(), String::new()];
    let mut written = Vec::new();
    let mut drafts = [String::new(), String::new()];
    let mut levels = [Levels::default(), Levels::default()];
    let mut tick = 0u64;
    while !stop.load(Ordering::Relaxed) {
        tick += 1;
        // Twice a second: whisper gets a stretch a second or so after it ends.
        for _ in 0..2 {
            if stop.load(Ordering::Relaxed) {
                return written;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        mic.catch_up();
        computer.catch_up();
        let heard = mic.heard().min(computer.heard());
        if heard < SETTLED {
            continue;
        }
        levels[0].update(&mic.samples, mic.base, heard);
        levels[1].update(&computer.samples, computer.base, heard);
        // Only the part not transcribed yet is looked at, and kept.
        let from = done[0].min(done[1]).saturating_sub(MARGIN) / FRAME * FRAME;
        mic.forget_before(from);
        computer.forget_before(from);
        let (mic_track, mic_regions, computer_track, computer_regions) = preview_regions(
            mic.between(from, heard),
            computer.between(from, heard),
            from,
            &levels,
        );
        for (side, (track, regions, label)) in [
            (&mic_track, &mic_regions, crate::meeting::DEFAULT_YOU),
            (
                &computer_track,
                &computer_regions,
                crate::meeting::DEFAULT_REMOTE,
            ),
        ]
        .into_iter()
        .enumerate()
        {
            let ready = ready_batch(regions, done[side], heard, track, from);
            if ready.is_empty() {
                // Nothing said on this side since: no need to look back there.
                if pending(regions, done[side]).is_empty() {
                    done[side] = done[side].max(heard.saturating_sub(WHISPER_RATE));
                }
                continue;
            }
            match preview_pass(
                &context,
                track,
                from,
                &ready,
                label,
                language,
                &earlier[side],
                stop,
            ) {
                Ok(new) => {
                    // Whisper sometimes gives back its prompt, the text just
                    // before, for a short stretch: a line said again word for
                    // word among the last few on its side is dropped.
                    let recent: Vec<String> = written
                        .iter()
                        .rev()
                        .filter(|l: &&Segment| l.speaker == label)
                        .take(5)
                        .map(|l| l.text.trim().to_lowercase())
                        .collect();
                    let new: Vec<Segment> = new
                        .into_iter()
                        .filter(|l| !recent.contains(&l.text.trim().to_lowercase()))
                        .filter(|l| !crate::transcribe::is_hallucination(&l.text))
                        .collect();
                    done[side] = ready.last().map_or(done[side], |r| r.end);
                    for line in &new {
                        earlier[side].push(' ');
                        earlier[side].push_str(&line.text);
                    }
                    if !new.is_empty() {
                        let _ = updates.send_blocking(Update::Lines(new.clone()));
                        written.extend(new);
                    }
                }
                Err(e) => {
                    if !stop.load(Ordering::Relaxed) {
                        crate::warn(format!("the preview stopped: {e}"));
                    }
                    return written;
                }
            }
        }
        // Once a second, the stretch still going on each side, as a draft.
        if tick.is_multiple_of(2) {
            for (side, (track, regions, label)) in [
                (&mic_track, &mic_regions, crate::meeting::DEFAULT_YOU),
                (
                    &computer_track,
                    &computer_regions,
                    crate::meeting::DEFAULT_REMOTE,
                ),
            ]
            .into_iter()
            .enumerate()
            {
                let (start, text) = match open_stretch(regions, done[side], heard) {
                    Some(open) if heard >= open.start + WHISPER_RATE => {
                        let piece = Region {
                            start: open.start,
                            onset: open.onset,
                            end: heard,
                        };
                        let text = preview_pass(
                            &context,
                            track,
                            from,
                            &[piece],
                            label,
                            language,
                            &earlier[side],
                            stop,
                        )
                        .map(|lines| {
                            lines
                                .iter()
                                .map(|l| l.text.trim())
                                .collect::<Vec<_>>()
                                .join(" ")
                        })
                        .unwrap_or_default();
                        (open.start, text)
                    }
                    _ => (heard, String::new()),
                };
                if text != drafts[side] {
                    let ms = |samples: usize| (samples * 1000 / WHISPER_RATE) as i64;
                    let _ = updates.send_blocking(Update::Draft {
                        speaker: label,
                        start_ms: ms(start),
                        heard_ms: ms(heard),
                        text: text.clone(),
                    });
                    drafts[side] = text;
                }
            }
        }
    }
    written
}

/// The stretches after `done` that have ended, and the part of a stretch
/// still going that is already long, cut at its quietest moment so someone
/// talking on does not hold the preview back. Each goes to whisper as soon as
/// it is there: the text heard before is its context.
fn ready_batch(
    regions: &[Region],
    done: usize,
    heard: usize,
    track: &[f32],
    from: usize,
) -> Vec<Region> {
    let mut ready: Vec<Region> = pending(regions, done)
        .into_iter()
        .filter(|r| r.end + SETTLED <= heard)
        .collect();
    if let Some(open) = open_stretch(regions, done, heard) {
        let settled = heard.saturating_sub(SETTLED);
        if settled >= open.start + LONG {
            let at = from + quietest(track, open.start + LONG / 2 - from, settled - from);
            ready.push(Region {
                start: open.start,
                onset: open.onset,
                end: at,
            });
        }
    }
    ready
}

/// What is left of each stretch past `done`.
fn pending(regions: &[Region], done: usize) -> Vec<Region> {
    regions
        .iter()
        .filter(|r| r.end > done)
        .map(|r| Region {
            start: r.start.max(done),
            onset: r.onset.max(done),
            end: r.end,
        })
        .collect()
}

/// The stretch someone is still in, past `done`.
fn open_stretch(regions: &[Region], done: usize, heard: usize) -> Option<Region> {
    pending(regions, done)
        .into_iter()
        .find(|r| r.end + SETTLED > heard)
}

/// The start of the quietest 30 ms of `track[from..to]`: a pause between words.
fn quietest(track: &[f32], from: usize, to: usize) -> usize {
    const FRAME: usize = WHISPER_RATE * 30 / 1000;
    let to = to.min(track.len());
    (from..to.saturating_sub(FRAME))
        .step_by(FRAME)
        .min_by(|&a, &b| {
            let energy = |i: usize| track[i..i + FRAME].iter().map(|x| x * x).sum::<f32>();
            energy(a).total_cmp(&energy(b))
        })
        .unwrap_or(to)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn region(start_secs: f64, end_secs: f64) -> Region {
        let at = |s: f64| (s * WHISPER_RATE as f64) as usize;
        Region {
            start: at(start_secs),
            onset: at(start_secs),
            end: at(end_secs),
        }
    }

    /// Records a call as the app would, ten times faster, with the preview
    /// running, and prints the lines as they come:
    /// `MINUTES_PREVIEW_MIC=mic.ogg MINUTES_PREVIEW_COMPUTER=computer.ogg cargo test --release -- --ignored --nocapture preview_follows_a_recording`
    #[test]
    #[ignore]
    fn preview_follows_a_recording() {
        let (Ok(mic), Ok(computer)) = (
            std::env::var("MINUTES_PREVIEW_MIC"),
            std::env::var("MINUTES_PREVIEW_COMPUTER"),
        ) else {
            return;
        };
        let raw = |path: &str| {
            let out = std::process::Command::new("ffmpeg")
                .args([
                    "-v", "error", "-i", path, "-ac", "2", "-ar", "48000", "-f", "s16le", "-",
                ])
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
        // A second of audio every 100 ms, or every second with MINUTES_PREVIEW_SPEED=1.
        let second = 48_000 * 4;
        let pace = std::env::var("MINUTES_PREVIEW_SPEED")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(10);
        let mut delays = Vec::new();
        let mut draft_ages = Vec::new();
        let mut written = 0;
        while written < mic.len().max(computer.len()) {
            written = (written + second).min(mic.len().max(computer.len()));
            std::fs::write(&mic_path, &mic[..written.min(mic.len())]).unwrap();
            std::fs::write(&computer_path, &computer[..written.min(computer.len())]).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(1000 / pace));
            while let Ok(update) = rx.try_recv() {
                let lines = match update {
                    Update::Lines(lines) => lines,
                    Update::Draft {
                        heard_ms,
                        text,
                        speaker,
                        ..
                    } => {
                        if !text.is_empty() {
                            // How much of the call the draft is behind.
                            draft_ages
                                .push(written as f64 / second as f64 - heard_ms as f64 / 1000.0);
                            println!("  draft {speaker}: {text}");
                        }
                        continue;
                    }
                };
                for line in lines {
                    // Seconds of audio written since the line ended, in the call's own time.
                    delays.push(written as f64 / second as f64 - line.end_ms as f64 / 1000.0);
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
        if !draft_ages.is_empty() {
            draft_ages.sort_by(f64::total_cmp);
            println!(
                "drafts behind the call: median {:.1} s, 90th percentile {:.1} s ({} drafts)",
                draft_ages[draft_ages.len() / 2],
                draft_ages[draft_ages.len() * 9 / 10],
                draft_ages.len()
            );
        }
        if !delays.is_empty() {
            delays.sort_by(f64::total_cmp);
            println!(
                "behind the call: median {:.1} s, 90th percentile {:.1} s, most {:.1} s",
                delays[delays.len() / 2],
                delays[delays.len() * 9 / 10],
                delays[delays.len() - 1]
            );
        }
        let _ = std::fs::remove_dir_all(&staging);
        assert!(!lines.is_empty());
    }

    #[test]
    fn a_stretch_goes_as_soon_as_it_has_ended() {
        let regions = [region(0.0, 1.0), region(2.0, 3.5), region(5.0, 8.0)];
        let secs = |s: f64| (s * WHISPER_RATE as f64) as usize;
        let track = vec![0.1; secs(40.0)];
        // The first has ended (0.8 s heard after it), the second not yet.
        assert_eq!(ready_batch(&regions, 0, secs(2.5), &track, 0).len(), 1);
        // Both short ones have ended.
        assert_eq!(ready_batch(&regions, 0, secs(4.4), &track, 0).len(), 2);
        // After them, the third is still going and short: nothing.
        assert!(ready_batch(&regions, secs(3.5), secs(8.5), &track, 0).is_empty());
        assert!(open_stretch(&regions, secs(3.5), secs(8.5)).is_some());
    }

    #[test]
    fn someone_talking_on_is_cut_at_a_pause() {
        let secs = |s: f64| (s * WHISPER_RATE as f64) as usize;
        // Talking from 0 s on, with a breath at 4 s.
        let mut track = vec![0.1f32; secs(40.0)];
        track[secs(4.0)..secs(4.1)]
            .iter_mut()
            .for_each(|x| *x = 0.0);
        let going = [region(0.0, 7.0)];
        // Not long enough yet.
        assert!(ready_batch(&going, 0, secs(5.0), &track, 0).is_empty());
        // Long enough: the part up to the breath goes.
        let cut = ready_batch(&going, 0, secs(7.5), &track, 0);
        assert_eq!(cut.len(), 1);
        assert!(
            (secs(3.95)..=secs(4.1)).contains(&cut[0].end),
            "cut at {}",
            cut[0].end
        );
        // The rest starts where the cut was.
        let rest = ready_batch(&[region(0.0, 12.0)], cut[0].end, secs(20.0), &track, 0);
        assert_eq!(rest[0].start, cut[0].end);
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
        assert!(
            (15_960..=16_000).contains(&follow.samples.len()),
            "{}",
            follow.samples.len()
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
