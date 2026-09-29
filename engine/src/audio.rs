//! Audio capture through `parec`: one process per source, kept running for the
//! whole life of the app so the meters work before and after a recording too.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{BufWriter, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

pub const RATE: u32 = 48_000;
pub const CHANNELS: u32 = 2;
/// 20 ms of s16le audio.
const CHUNK_BYTES: usize = (RATE / 50 * 2 * CHANNELS) as usize;
/// Bytes of one second of raw audio.
const BYTES_PER_SEC: u64 = RATE as u64 * 2 * CHANNELS as u64;
/// How far behind the clock a track may fall before silence fills the gap:
/// parec delivers in small bursts, which is not a gap.
const GAP_TOLERANCE: u64 = BYTES_PER_SEC / 2;
/// While recording, a parec that has sent nothing for this long is restarted.
const STALL: Duration = Duration::from_secs(5);
/// And not more often than this, for a device that really sends nothing.
const RESTART_EVERY: Duration = Duration::from_secs(10);
/// Three seconds of 20 ms peaks.
pub const HISTORY: usize = 150;
const FLOOR_DB: f64 = -60.0;

struct Inner {
    levels: VecDeque<f32>,
    file: Option<BufWriter<File>>,
    /// While paused the meters keep running but nothing is written.
    paused: bool,
    /// Set by `stop`: parec is killed and not started again.
    stopped: bool,
    /// While muted, silence is written in place of the sound, so the track
    /// keeps its length and stays in step with the other one.
    muted: bool,
    /// The parec running now, to kill on `stop`.
    pid: Option<u32>,
    /// While recording, how much time the track should hold.
    clock: Option<TrackClock>,
    /// When sound last arrived from the device.
    last_data: Instant,
    /// When parec was last restarted for sending nothing, and whether it was
    /// during this recording.
    restarted_at: Option<Instant>,
    restarted: bool,
}

/// The time a recording has run, pauses left out, and what the track holds.
struct TrackClock {
    since: Instant,
    paused_since: Option<Instant>,
    paused: Duration,
    written: u64,
}

impl TrackClock {
    fn new() -> Self {
        Self {
            since: Instant::now(),
            paused_since: None,
            paused: Duration::ZERO,
            written: 0,
        }
    }

    /// Bytes the track should hold by now.
    fn expected(&self) -> u64 {
        let paused = self.paused + self.paused_since.map_or(Duration::ZERO, |p| p.elapsed());
        let running = self.since.elapsed().saturating_sub(paused);
        (running.as_secs_f64() * BYTES_PER_SEC as f64) as u64
    }
}

/// The silence to write so a track holding `written` bytes catches up with
/// `expected`: none within `GAP_TOLERANCE`, else the whole gap, in whole frames.
fn silence_to_add(expected: u64, written: u64) -> u64 {
    let frame = 2 * u64::from(CHANNELS);
    if expected <= written + GAP_TOLERANCE {
        return 0;
    }
    (expected - written) / frame * frame
}

/// Writes `bytes` of silence to `file`.
fn write_silence(file: &mut BufWriter<File>, bytes: u64) {
    let zeros = [0u8; 4096];
    let mut left = bytes;
    while left > 0 {
        let n = left.min(zeros.len() as u64) as usize;
        if file.write_all(&zeros[..n]).is_err() {
            return;
        }
        left -= n as u64;
    }
}

#[derive(Clone)]
pub struct Source {
    inner: Arc<Mutex<Inner>>,
}

impl Source {
    /// Starts capturing `device`, a PulseAudio source name such as `@DEFAULT_MONITOR@`.
    pub fn spawn(device: &'static str) -> Self {
        let inner = Arc::new(Mutex::new(Inner {
            levels: VecDeque::from(vec![0.0; HISTORY]),
            file: None,
            paused: false,
            stopped: false,
            muted: false,
            pid: None,
            clock: None,
            last_data: Instant::now(),
            restarted_at: None,
            restarted: false,
        }));
        let shared = inner.clone();
        thread::spawn(move || {
            while !shared.lock().unwrap().stopped {
                capture(device, &shared);
                // parec exits when the device goes away; try again.
                thread::sleep(Duration::from_secs(1));
            }
        });
        // A parec can also stall with its device still there: seen with a
        // headset connected in the middle of a call, the stream PipeWire
        // moved to it sending nothing more. While recording, one silent for
        // STALL is stopped, so the loop above starts it again; the clock
        // fills the gap with silence.
        let watched = inner.clone();
        thread::spawn(move || {
            loop {
                thread::sleep(Duration::from_secs(1));
                let mut inner = watched.lock().unwrap();
                if inner.stopped {
                    return;
                }
                let due = inner
                    .restarted_at
                    .is_none_or(|at| at.elapsed() >= RESTART_EVERY);
                if inner.clock.is_some() && inner.last_data.elapsed() >= STALL && due {
                    let Some(pid) = inner.pid.take() else {
                        continue;
                    };
                    // SIGKILL: a stalled process may never get to a SIGTERM.
                    // SAFETY: kill only sends a signal; a pid that has gone is harmless.
                    unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
                    inner.restarted_at = Some(Instant::now());
                    if !inner.restarted {
                        inner.restarted = true;
                        crate::warn(format!(
                            "{device} sent nothing for {} s while recording: listening again",
                            STALL.as_secs()
                        ));
                    }
                }
            }
        });
        Source { inner }
    }

    /// Stops listening for good: the microphone is released, and GNOME no
    /// longer shows it in use. Recording through this source ends too.
    pub fn stop(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.stopped = true;
        inner.finish_track();
        if let Some(pid) = inner.pid.take() {
            // SAFETY: kill only sends a signal; a pid that has gone is harmless.
            unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
        }
    }

    /// Tees the raw stream (s16le, RATE, CHANNELS) into `path` from now on.
    pub fn start_recording(&self, path: &Path) -> std::io::Result<()> {
        let file = BufWriter::new(File::create(path)?);
        let mut inner = self.inner.lock().unwrap();
        inner.file = Some(file);
        inner.paused = false;
        inner.clock = Some(TrackClock::new());
        inner.restarted = false;
        Ok(())
    }

    /// Records silence instead of this source, for as long as it is muted.
    pub fn set_muted(&self, muted: bool) {
        self.inner.lock().unwrap().muted = muted;
    }

    pub fn is_muted(&self) -> bool {
        self.inner.lock().unwrap().muted
    }

    pub fn set_paused(&self, paused: bool) {
        let mut inner = self.inner.lock().unwrap();
        inner.paused = paused;
        if let Some(clock) = inner.clock.as_mut() {
            match (paused, clock.paused_since) {
                (true, None) => clock.paused_since = Some(Instant::now()),
                (false, Some(since)) => {
                    clock.paused += since.elapsed();
                    clock.paused_since = None;
                }
                _ => {}
            }
        }
    }

    /// Ends the recording; a track that fell behind (a device that stopped
    /// sending) is filled with silence up to now, so both sides end together.
    pub fn stop_recording(&self) {
        self.inner.lock().unwrap().finish_track();
    }

    /// How long the device has sent nothing: a headset asleep or taken off.
    pub fn silent_for(&self) -> Duration {
        self.inner.lock().unwrap().last_data.elapsed()
    }

    pub fn levels(&self) -> Vec<f32> {
        self.inner.lock().unwrap().levels.iter().copied().collect()
    }

    /// The loudest of the last `n` peaks, so a short burst is not missed by a slower reader.
    pub fn recent_peak(&self, n: usize) -> f32 {
        let inner = self.inner.lock().unwrap();
        inner
            .levels
            .iter()
            .rev()
            .take(n)
            .copied()
            .fold(0.0, f32::max)
    }
}

impl Inner {
    fn finish_track(&mut self) {
        if let (Some(file), Some(clock)) = (self.file.as_mut(), self.clock.as_ref()) {
            write_silence(file, silence_to_add(clock.expected(), clock.written));
        }
        if let Some(mut file) = self.file.take() {
            let _ = file.flush();
        }
        self.clock = None;
    }
}

fn capture(device: &str, shared: &Mutex<Inner>) {
    let Ok(mut child) = Command::new("parec")
        .args([
            "--raw",
            "--format=s16le",
            &format!("--rate={RATE}"),
            &format!("--channels={CHANNELS}"),
            "--latency-msec=20",
            "-d",
            device,
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        crate::warn(format!(
            "could not start parec for {device}: nothing is recorded from it"
        ));
        return;
    };
    {
        let mut inner = shared.lock().unwrap();
        if inner.stopped {
            let _ = child.kill();
            let _ = child.wait();
            return;
        }
        inner.pid = Some(child.id());
    }
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut buf = vec![0u8; CHUNK_BYTES];
    // So a crash loses at most a second: flush every second, and push it to
    // the disk itself every half minute in case the machine goes down too.
    let mut chunks: u64 = 0;
    let mut write_failed = false;
    while stdout.read_exact(&mut buf).is_ok() {
        chunks += 1;
        if shared.lock().unwrap().muted {
            buf.fill(0);
        }
        let peak = buf
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| i16::from_le_bytes([b[0], b[1]]).unsigned_abs())
            .max()
            .unwrap_or(0) as f32
            / 32768.0;
        let mut inner = shared.lock().unwrap();
        inner.last_data = Instant::now();
        inner.levels.pop_front();
        inner.levels.push_back(peak);
        let Inner {
            paused,
            file,
            clock,
            ..
        } = &mut *inner;
        if !*paused && let Some(file) = file.as_mut() {
            // The device may have sent nothing for a while (a headset asleep):
            // the silence it missed goes in first, so the track keeps time.
            if let Some(clock) = clock.as_mut() {
                let before = clock.expected().saturating_sub(buf.len() as u64);
                let gap = silence_to_add(before, clock.written);
                write_silence(file, gap);
                clock.written += gap + buf.len() as u64;
            }
            if let Err(e) = file.write_all(&buf)
                && !write_failed
            {
                // Once: a full disk would say it every 100 ms.
                crate::warn(format!("could not write what {device} records: {e}"));
                write_failed = true;
            }
            if chunks.is_multiple_of(50) {
                let _ = file.flush();
            }
            if chunks.is_multiple_of(1500) {
                let _ = file.get_ref().sync_data();
            }
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Maps a linear peak to 0..1 on a -60 dB..0 dB scale.
pub fn to_meter(peak: f32) -> f64 {
    if peak <= 0.0 {
        return 0.0;
    }
    (1.0 - 20.0 * f64::from(peak).log10() / FLOOR_DB).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_track_that_fell_behind_is_filled_to_the_clock() {
        // Within the tolerance: parec's small bursts are no gap.
        assert_eq!(silence_to_add(BYTES_PER_SEC, BYTES_PER_SEC - 1_000), 0);
        assert_eq!(silence_to_add(BYTES_PER_SEC, BYTES_PER_SEC + 5_000), 0);
        // Five seconds without sound: all of them, in whole frames.
        let gap = silence_to_add(10 * BYTES_PER_SEC + 3, 5 * BYTES_PER_SEC);
        assert_eq!(gap, 5 * BYTES_PER_SEC);
        assert_eq!(gap % 4, 0);
    }

    #[test]
    fn pauses_do_not_count_as_time_to_fill() {
        let mut clock = TrackClock::new();
        clock.since -= Duration::from_secs(10);
        clock.paused = Duration::from_secs(4);
        let expected = clock.expected() as f64 / BYTES_PER_SEC as f64;
        assert!((5.9..6.1).contains(&expected), "{expected} s");
        clock.paused_since = Some(Instant::now() - Duration::from_secs(2));
        let expected = clock.expected() as f64 / BYTES_PER_SEC as f64;
        assert!((3.9..4.1).contains(&expected), "{expected} s");
    }

    #[test]
    fn silence_is_written_in_full() {
        let path = std::env::temp_dir().join(format!("minutes-silence-{}", std::process::id()));
        let mut file = BufWriter::new(File::create(&path).unwrap());
        write_silence(&mut file, 10_004);
        file.flush().unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(bytes.len(), 10_004);
        assert!(bytes.iter().all(|&b| b == 0));
    }
}
