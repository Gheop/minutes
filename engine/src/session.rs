//! One meeting from Start to transcript, without the interface: where the raw
//! audio waits while recording, the note that lets a crashed recording be
//! saved later, the meeting folder, and naming the speakers once the
//! transcript is in. The app drives these steps and shows them; everything
//! here runs and is tested without a display.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::export::{Format, export_audio, export_tracks};
use crate::meeting::{self, Manifest};

/// Bytes per second of a raw track: 48 kHz, stereo, 16 bits.
const RAW_BYTES_PER_SEC: u64 = 48_000 * 2 * 2;
const NOTE: &str = "recording.json";

/// Where recordings wait while they are made, one folder each.
pub fn staging_root() -> PathBuf {
    glib::user_cache_dir().join(crate::APP_NAME)
}

/// Where meeting folders go.
pub fn meetings_root() -> PathBuf {
    glib::home_dir().join("Documents/Meetings")
}

/// Creates `dir` inside `root`, both readable by you alone: meetings hold the
/// voices of people who did not choose where they are kept.
pub fn private_dir(root: &Path, dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    for path in [root, dir] {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// The raw files of a recording in its staging folder: (mic, computer audio).
pub fn raw_tracks(staging: &Path) -> (PathBuf, PathBuf) {
    (staging.join("mic.raw"), staging.join("system.raw"))
}

/// What is known about a recording while it is made, kept next to its raw
/// audio so a recording cut short by a crash can still be saved.
#[derive(Debug, Clone, PartialEq)]
pub struct Note {
    pub title: String,
    pub started_at: i64,
    pub format: Format,
    pub language: String,
    /// The people the meeting app showed in the call: whisper is told to
    /// expect their names.
    pub names: Vec<String>,
}

impl Note {
    pub fn write(&self, staging: &Path) -> std::io::Result<()> {
        let note = serde_json::json!({
            "title": self.title,
            "started_at": self.started_at,
            "format": self.format.key(),
            "language": self.language,
            "names": self.names,
        });
        std::fs::write(staging.join(NOTE), note.to_string())
    }

    pub fn read(staging: &Path) -> Option<Note> {
        let text = std::fs::read_to_string(staging.join(NOTE)).ok()?;
        let value: serde_json::Value = serde_json::from_str(&text).ok()?;
        Some(Note {
            title: value["title"]
                .as_str()
                .filter(|t| !t.is_empty())?
                .to_owned(),
            started_at: value["started_at"].as_i64()?,
            format: Format::from_key(value["format"].as_str().unwrap_or("mono")),
            language: value["language"].as_str().unwrap_or("auto").to_owned(),
            names: value["names"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|n| n.as_str().map(str::to_owned))
                .collect(),
        })
    }
}

/// Recorded seconds in a staging folder, from the size of the longer raw track.
pub fn raw_duration(staging: &Path) -> i64 {
    let (mic, system) = raw_tracks(staging);
    let bytes = [mic, system]
        .iter()
        .map(|path| std::fs::metadata(path).map_or(0, |m| m.len()))
        .max()
        .unwrap_or(0);
    (bytes / RAW_BYTES_PER_SEC) as i64
}

/// Staging folders under `root` left behind with some audio in them, oldest first.
pub fn unfinished(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        // Either track counts: a machine without a microphone still records
        // the computer audio.
        .filter(|dir| raw_duration(dir) > 0)
        .collect();
    found.sort();
    found
}

/// The folder for a meeting: `<YYYYMMDDHHMM> <title>` under `root`, so the
/// folders sort by date.
pub fn meeting_dir(root: &Path, started_at: i64, title: &str) -> PathBuf {
    let stamp = glib::DateTime::from_unix_local(started_at)
        .and_then(|t| t.format("%Y%m%d%H%M"))
        .map(|s| s.to_string())
        .unwrap_or_default();
    root.join(format!("{stamp} {}", meeting::safe_name(title)))
}

/// Writes the audio of a finished recording into its meeting folder, in the
/// format asked for, plus the two tracks kept to transcribe again.
/// Returns whether each of the two went well: (audio, tracks).
pub fn save_audio(staging: &Path, out: &Path, format: Format) -> (bool, bool) {
    let _ = private_dir(out.parent().unwrap_or(out), out);
    let (mic, system) = raw_tracks(staging);
    (
        export_audio(&mic, &system, out, format),
        export_tracks(&mic, &system, out),
    )
}

/// Everything after Stop for the recording in `staging`: its audio kept in
/// the meeting folder `out`, then its transcript, with the two sides called
/// `speakers`. The raw files go once the audio and the tracks are kept, even
/// if the transcript fails: it can be made again from the tracks. Blocking:
/// run it off the main thread; `events` tells how far it is.
pub fn write_up(
    staging: &Path,
    out: &Path,
    note: &Note,
    speakers: [String; 2],
    events: &crate::transcribe::Events,
    abort: &crate::transcribe::Abort,
) -> Result<(), String> {
    crate::glossary::set_names(&note.names);
    let saved = save_audio(staging, out, note.format);
    let mut manifest = Manifest {
        title: note.title.clone(),
        started_at: note.started_at,
        duration_secs: raw_duration(staging),
        format: note.format,
        language: note.language.clone(),
        speakers: speakers.to_vec(),
        labels: Vec::new(),
        imported: None,
        speaker_count: None,
        model: None,
    };
    // Written before the transcript, so the folder opens as a meeting even
    // if the transcript never comes.
    if let Err(e) = meeting::write(out, &manifest) {
        crate::warn(format!("could not write {}: {e}", out.display()));
    }
    let result = transcribe_into(staging, out, &mut manifest, &note.language, events, abort);
    if saved == (true, true) {
        let _ = std::fs::remove_dir_all(staging);
    }
    result
}

/// `minutes write-up <recording folder> [--into folder] [--model name]`:
/// what the app does after Stop, for a staging folder. Prints the meeting
/// folder it made, in the meetings folder or in `--into`.
pub fn cli(args: &[String]) -> glib::ExitCode {
    let mut staging = None;
    let mut root = meetings_root();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--into" => match iter.next() {
                Some(dir) => root = PathBuf::from(dir),
                None => return crate::transcribe::usage(),
            },
            "--model" | "-m" => match iter.next() {
                Some(name) => crate::models::set_override(name),
                None => return crate::transcribe::usage(),
            },
            _ if staging.is_none() => staging = Some(PathBuf::from(arg)),
            _ => return crate::transcribe::usage(),
        }
    }
    let Some(staging) = staging else {
        return crate::transcribe::usage();
    };
    crate::transcribe::run_cli(|events, abort| {
        let note =
            Note::read(&staging).ok_or_else(|| format!("no recording in {}", staging.display()))?;
        let out = meeting_dir(&root, note.started_at, &note.title);
        let speakers = [meeting::DEFAULT_YOU.into(), meeting::DEFAULT_REMOTE.into()];
        write_up(&staging, &out, &note, speakers, events, abort)?;
        Ok(format!("{}\n", out.display()))
    })
}

/// Transcribes the two tracks in `tracks` (a staging folder or a meeting
/// folder's kept tracks) into `out/transcript.md`, names the speakers and
/// writes the manifest. Blocking: run it off the main thread; `events` tells
/// how far it is.
pub fn transcribe_into(
    tracks: &Path,
    out: &Path,
    manifest: &mut Manifest,
    language: &str,
    events: &crate::transcribe::Events,
    abort: &crate::transcribe::Abort,
) -> Result<(), String> {
    let (mic_path, computer_path) = if tracks.join("mic.raw").exists() {
        raw_tracks(tracks)
    } else {
        crate::export::tracks(tracks)
    };
    let mic = crate::transcribe::load_track(&mic_path)?;
    let computer = crate::transcribe::load_track(&computer_path)?;
    let transcript = crate::transcribe::transcribe(&mic, &computer, language, events, abort)?;
    let date = glib::DateTime::from_unix_local(manifest.started_at)
        .and_then(|t| t.format("%Y-%m-%d %H:%M"))
        .map(|s| s.to_string())
        .unwrap_or_default();
    let markdown = crate::transcribe::to_markdown(&manifest.title, &date, &transcript);
    let markdown = name_speakers(manifest, &markdown);
    manifest.language = language.to_owned();
    manifest.model = Some(crate::models::configured());
    meeting::write(out, manifest).map_err(|e| format!("could not write the meeting: {e}"))?;
    std::fs::write(out.join("transcript.md"), markdown)
        .map_err(|e| format!("could not write the transcript: {e}"))
}

/// Splits `**[01:23] You:** text` into its time, speaker and text.
pub fn parse_segment(line: &str) -> Option<(&str, &str, &str)> {
    let rest = line.strip_prefix("**[")?;
    let (time, rest) = rest.split_once("] ")?;
    let (speaker, text) = rest.split_once(":** ")?;
    Some((time, speaker, text.trim()))
}

/// The speaker labels in transcript Markdown, in order of first appearance.
pub fn speakers_in(markdown: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for line in markdown.lines() {
        if let Some((_, speaker, _)) = parse_segment(line)
            && !found.iter().any(|f| f == speaker)
        {
            found.push(speaker.to_owned());
        }
    }
    found
}

/// Gives the speakers of a fresh transcript this meeting's names, and records
/// in `manifest` which label each name belongs to. The transcription labels
/// speakers You, Remote, You 1, Remote 2, ... for a recording and Speaker 1,
/// Speaker 2, ... for an imported file. Names already given to a label are
/// kept; a new label gets a default. Returns the transcript with the names in.
pub fn name_speakers(manifest: &mut Manifest, markdown: &str) -> String {
    if manifest.imported.is_some() {
        // An import finds its own number of speakers: keep the names already
        // given and number the rest.
        let found = speakers_in(markdown);
        let mut names = manifest.speakers.clone();
        names.resize_with(found.len(), String::new);
        for (i, name) in names.iter_mut().enumerate() {
            if name.is_empty() {
                *name = format!("Speaker {}", i + 1);
            }
        }
        manifest.speakers = names;
    } else {
        let mut labels: Vec<String> = speakers_in(markdown)
            .into_iter()
            .filter(|l| meeting::side_of(l).is_some())
            .collect();
        // Your side first, then by number.
        labels.sort_by_key(|l| {
            let (side, n) = meeting::side_of(l).unwrap_or(("", 0));
            (side != meeting::DEFAULT_YOU, n)
        });
        if !labels.is_empty() {
            let known: HashMap<String, String> = manifest
                .default_labels()
                .into_iter()
                .zip(manifest.speakers.iter().cloned())
                .collect();
            let names = labels
                .iter()
                .map(|label| {
                    if let Some(name) = known.get(label) {
                        return name.clone();
                    }
                    match meeting::side_of(label) {
                        // The first voice on the mic is you.
                        Some((side, 1)) => {
                            known.get(side).cloned().unwrap_or_else(|| label.clone())
                        }
                        Some((side, n)) if side == meeting::DEFAULT_YOU => format!("Room {n}"),
                        _ => label.clone(),
                    }
                })
                .collect();
            manifest.labels = labels;
            manifest.speakers = names;
        }
    }
    let renames: Vec<(String, String)> = manifest
        .default_labels()
        .into_iter()
        .zip(manifest.speakers.iter().cloned())
        .filter(|(label, name)| label != name)
        .collect();
    meeting::relabel_all(markdown, &renames)
}

/// Names the voices of the other side after who Teams showed speaking while
/// they spoke (see `teams::Speaking`): a voice takes the name lit during most
/// of its lines, the one voice with the most votes first, each name once, and
/// never `me`. Voices already named from Teams at the time of the call, or
/// with too little said to tell, keep their name. Returns the renamed
/// transcript; `manifest` gets the new names.
pub fn name_from_teams(
    manifest: &mut Manifest,
    markdown: &str,
    speaking: &crate::teams::Speaking,
    me: Option<&str>,
) -> String {
    let labels = manifest.default_labels();
    let remote: Vec<usize> = (0..manifest.speakers.len())
        .filter(|&i| {
            labels
                .get(i)
                .and_then(|l| meeting::side_of(l))
                .is_some_and(|(side, _)| side == meeting::DEFAULT_REMOTE)
        })
        .collect();
    // Each line runs until the next one starts, 30 s at most.
    let lines: Vec<(i64, String)> = markdown
        .lines()
        .filter_map(parse_segment)
        .filter_map(|(time, speaker, _)| Some((clock_ms(time)?, speaker.to_owned())))
        .collect();
    let mut votes: Vec<(usize, String, usize)> = Vec::new();
    for &i in &remote {
        let name = &manifest.speakers[i];
        let mut tally: Vec<(String, usize)> = Vec::new();
        let mut said = 0;
        for (n, (start, speaker)) in lines.iter().enumerate() {
            if speaker != name {
                continue;
            }
            let end = lines
                .get(n + 1)
                .map_or(start + 30_000, |next| next.0.min(start + 30_000));
            let Some(heard) = crate::teams::speaker_between(speaking, *start, end) else {
                continue;
            };
            said += 1;
            match tally.iter_mut().find(|(h, _)| *h == heard) {
                Some((_, count)) => *count += 1,
                None => tally.push((heard, 1)),
            }
        }
        // Named when the same person is lit for most of its lines, three at least.
        if let Some((heard, count)) = tally.into_iter().max_by_key(|(_, c)| *c)
            && count >= 3
            && count * 2 > said
            && Some(heard.as_str()) != me
        {
            votes.push((i, heard, count));
        }
    }
    votes.sort_by_key(|(_, _, count)| std::cmp::Reverse(*count));
    let mut renames = Vec::new();
    for (i, heard, _) in votes {
        let taken = manifest.speakers.contains(&heard);
        if !taken {
            renames.push((manifest.speakers[i].clone(), heard.clone()));
            manifest.speakers[i] = heard;
        }
    }
    meeting::relabel_all(markdown, &renames)
}

/// "01:23" or "1:01:23" as milliseconds.
fn clock_ms(time: &str) -> Option<i64> {
    time.split(':')
        .try_fold(0i64, |total, part| {
            Some(total * 60 + part.parse::<i64>().ok()?)
        })
        .map(|secs| secs * 1000)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_other_voices_take_the_names_teams_showed_speaking() {
        let mut manifest = recording(&["Ludovic", "Remote 1", "Remote 2"]);
        manifest.labels = vec!["You 1".into(), "Remote 1".into(), "Remote 2".into()];
        let markdown = "## Transcript\n\n\
            **[00:00] Remote 1:** Bonjour.\n\n\
            **[00:04] Remote 1:** On commence.\n\n\
            **[00:08] Ludovic:** Oui.\n\n\
            **[00:10] Remote 2:** Moi aussi.\n\n\
            **[00:14] Remote 1:** Bien.\n\n\
            **[00:18] Remote 2:** Voilà.\n\n";
        let names = |n: &[&str]| n.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let speaking: Vec<(i64, Vec<String>)> = (0..40)
            .map(|half| {
                let at = half * 500;
                let who = match at {
                    0..8_000 | 14_000..18_000 => names(&["Jeanne Martin"]),
                    8_000..10_000 => names(&["Ludovic"]),
                    _ => names(&["Paul Durand"]),
                };
                (at, who)
            })
            .collect();
        let named = name_from_teams(&mut manifest, markdown, &speaking, Some("Ludovic"));
        assert_eq!(manifest.speakers, ["Ludovic", "Jeanne Martin", "Remote 2"]);
        assert!(named.contains("**[00:00] Jeanne Martin:** Bonjour."));
        // Paul spoke twice: not enough to be sure.
        assert!(named.contains("**[00:10] Remote 2:** Moi aussi."));
        assert!(named.contains("**[00:08] Ludovic:** Oui."));
    }

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("minutes-session-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn recording(speakers: &[&str]) -> Manifest {
        Manifest {
            title: "Budget".into(),
            started_at: 0,
            duration_secs: 60,
            format: Format::Mono,
            language: "fr".into(),
            speakers: speakers.iter().map(|s| s.to_string()).collect(),
            labels: Vec::new(),
            imported: None,
            speaker_count: None,
            model: None,
        }
    }

    #[test]
    fn meeting_folders_are_yours_alone() {
        use std::os::unix::fs::PermissionsExt;
        let root = scratch("private");
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
        let dir = root.join("202609271000 Budget");
        private_dir(&root, &dir).unwrap();
        for path in [&root, &dir] {
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
    }

    #[test]
    fn a_note_reads_back_as_written() {
        let dir = scratch("note");
        let note = Note {
            title: "Point hebdo".into(),
            started_at: 1_790_000_000,
            format: Format::Stereo,
            language: "fr".into(),
            names: vec!["Jeanne Martin".into()],
        };
        note.write(&dir).unwrap();
        assert_eq!(Note::read(&dir), Some(note));
        std::fs::write(dir.join(NOTE), r#"{"title": "", "started_at": 1}"#).unwrap();
        assert_eq!(Note::read(&dir), None, "a note without a title is no note");
    }

    #[test]
    fn only_staging_folders_with_audio_are_unfinished() {
        let root = scratch("unfinished");
        let empty = root.join("100");
        let computer_only = root.join("200");
        std::fs::create_dir_all(&empty).unwrap();
        std::fs::create_dir_all(&computer_only).unwrap();
        std::fs::write(
            raw_tracks(&computer_only).1,
            vec![0u8; 3 * RAW_BYTES_PER_SEC as usize],
        )
        .unwrap();
        assert_eq!(unfinished(&root), vec![computer_only.clone()]);
        assert_eq!(raw_duration(&computer_only), 3);
        assert!(unfinished(&root.join("missing")).is_empty());
    }

    #[test]
    fn meeting_folders_sort_by_date_and_have_safe_names() {
        let dir = meeting_dir(Path::new("/m"), 1_790_000_000, "Point: dev/java");
        let name = dir.file_name().unwrap().to_str().unwrap();
        let (stamp, title) = name.split_once(' ').unwrap();
        assert_eq!(stamp.len(), 12);
        assert!(stamp.bytes().all(|b| b.is_ascii_digit()));
        assert_eq!(title, "Point- dev-java");
        assert_eq!(dir.parent(), Some(Path::new("/m")));
    }

    #[test]
    fn segments_and_speakers_come_out_of_the_markdown() {
        let md = "# Budget\n\n**[00:01] You:** Bonjour.\n\n**[00:04] Remote 2:** Salut.\n\n**[01:02:03] You:** Merci.\n";
        assert_eq!(
            parse_segment("**[00:04] Remote 2:** Salut."),
            Some(("00:04", "Remote 2", "Salut."))
        );
        assert_eq!(parse_segment("## Chapters"), None);
        assert_eq!(speakers_in(md), ["You", "Remote 2"]);
    }

    #[test]
    fn a_recording_keeps_your_name_and_numbers_the_other_side() {
        let mut manifest = recording(&["Sib", "Remote"]);
        let md = "**[00:01] You:** Bonjour.\n\n**[00:04] Remote 2:** Salut.\n\n**[00:09] Remote 1:** Oui.\n";
        let named = name_speakers(&mut manifest, md);
        assert_eq!(manifest.labels, ["You", "Remote 1", "Remote 2"]);
        // The first voice on a side takes that side's name; the others keep their number.
        assert_eq!(manifest.speakers, ["Sib", "Remote", "Remote 2"]);
        assert!(named.contains("**[00:01] Sib:** Bonjour."));
        assert!(named.contains("**[00:09] Remote:** Oui."));
        assert!(named.contains("**[00:04] Remote 2:** Salut."));
    }

    #[test]
    fn a_second_voice_at_your_mic_is_someone_in_the_room() {
        let mut manifest = recording(&["Sib", "Remote"]);
        let md = "**[00:01] You 1:** Bonjour.\n\n**[00:02] You 2:** Moi aussi.\n\n**[00:04] Remote:** Salut.\n";
        name_speakers(&mut manifest, md);
        assert_eq!(manifest.speakers, ["Sib", "Room 2", "Remote"]);
    }

    #[test]
    fn an_import_numbers_the_speakers_it_found() {
        let mut manifest = recording(&["Maya"]);
        manifest.imported = Some("call.mp3".into());
        let md = "**[00:01] Speaker 1:** Hi.\n\n**[00:03] Speaker 2:** Hello.\n";
        let named = name_speakers(&mut manifest, md);
        assert_eq!(manifest.speakers, ["Maya", "Speaker 2"]);
        assert!(named.contains("**[00:01] Maya:** Hi."));
    }
}
