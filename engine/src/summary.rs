//! The summary of a meeting, written from its transcript and the notes typed
//! during the call by a small language model on this computer: what was
//! decided, who does what, what is left open.
//!
//! It can also be written by Claude Code (`claude -p`) or Codex (`codex
//! exec`) when they are installed and chosen: better and quicker, but the
//! transcript and the notes go to Anthropic or OpenAI through your account.
//!
//! The model runs in llama.cpp's own server, in a process of its own: it
//! leaves the GPU as soon as the summary is written, and llama.cpp's copy of
//! ggml does not meet whisper's. The server and the model are downloaded the
//! first time, each checked against the SHA-256 it must have.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use crate::transcribe::{Abort, CANCELLED, Event, Events};

/// llama.cpp's own build for Linux with Vulkan, which runs on NVIDIA, AMD
/// and Intel GPUs alike, and on the CPU without one.
const LLAMA: &str = "llama-b11582";
const LLAMA_URL: &str = "https://github.com/ggml-org/llama.cpp/releases/download/b11582/llama-b11582-bin-ubuntu-vulkan-x64.tar.gz";
const LLAMA_SHA256: &str = "44fccf3bf7654d02011ed652621b06fce8ff5499b32911eb78c9ea9078feb1c0";
const LLAMA_BYTES: u64 = 30_338_258;

/// Qwen3.5 4B (Apache 2.0), quantized to 4 bits: of the models that fit a
/// 4 GB GPU with an hour of meeting to read, the one whose summaries of two
/// French meetings held up, in about 2 minutes each.
const MODEL: &str = "Qwen3.5-4B-Q4_K_M.gguf";
const MODEL_URL: &str = "https://huggingface.co/unsloth/Qwen3.5-4B-GGUF/resolve/e87f176479d0855a907a41277aca2f8ee7a09523/Qwen3.5-4B-Q4_K_M.gguf";
const MODEL_SHA256: &str = "00fe7986ff5f6b463e62455821146049db6f9313603938a70800d1fb69ef11a4";
const MODEL_BYTES: u64 = 2_740_937_888;
/// What the download weighs, for the button that starts it.
pub const DOWNLOAD_MB: u64 = (LLAMA_BYTES + MODEL_BYTES) / 1_000_000;

/// The summary, next to the transcript.
pub const SUMMARY: &str = "summary.md";
/// Who wrote it: `local`, `claude` or `codex`.
const WRITTEN_BY: &str = ".summary-by";

/// Who writes the summary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Writer {
    /// The model on this computer.
    Local,
    /// Claude Code, `claude -p`.
    Claude,
    /// OpenAI's Codex, `codex exec`.
    Codex,
}

impl Writer {
    pub const ALL: [Writer; 3] = [Writer::Local, Writer::Claude, Writer::Codex];

    pub fn key(self) -> &'static str {
        match self {
            Writer::Local => "local",
            Writer::Claude => "claude",
            Writer::Codex => "codex",
        }
    }

    pub fn from_key(key: &str) -> Option<Writer> {
        Writer::ALL.into_iter().find(|w| w.key() == key)
    }

    /// Whether it can write here: the commands have to be installed.
    pub fn available(self) -> bool {
        match self {
            Writer::Local => true,
            Writer::Claude => glib::find_program_in_path("claude").is_some(),
            Writer::Codex => glib::find_program_in_path("codex").is_some(),
        }
    }

    /// Whether it writes without a download first.
    pub fn ready(self) -> bool {
        match self {
            Writer::Local => ready(),
            _ => self.available(),
        }
    }

    /// `summary_by` in the config, when it can write here; else this computer.
    pub fn configured() -> Writer {
        crate::models::config_value("summary_by")
            .and_then(|key| Writer::from_key(&key))
            .filter(|w| w.available())
            .unwrap_or(Writer::Local)
    }

    /// Keeps it as the one that writes the summaries from now on.
    pub fn choose(self) -> std::io::Result<()> {
        crate::models::set_config("summary_by", self.key())
    }
}

/// Who wrote the summary of the meeting in `dir`.
pub fn written_by(dir: &Path) -> Option<Writer> {
    std::fs::read_to_string(dir.join(WRITTEN_BY))
        .ok()
        .and_then(|key| Writer::from_key(key.trim()))
}

/// Turns the summaries after each meeting on or off.
pub fn set_enabled(on: bool) -> std::io::Result<()> {
    crate::models::set_config("summary", if on { "true" } else { "false" })
}

/// The longest the model reads at once, in tokens: an hour and a half of
/// meeting. A longer one is summarized in parts first.
const CONTEXT: usize = 32_768;
/// Room left for the instructions and the summary itself.
const ROOM: usize = 4_096;

fn dir() -> PathBuf {
    crate::transcribe::models_dir().join("llama")
}

fn server_path() -> PathBuf {
    dir().join(LLAMA).join("llama-server")
}

fn model_path() -> PathBuf {
    crate::transcribe::models_dir().join(MODEL)
}

/// Whether summaries are written at all: `summary = false` in the config
/// turns them off.
pub fn enabled() -> bool {
    crate::models::config_value("summary").as_deref() != Some("false")
}

/// Whether the server and the model are here: then a summary needs no
/// download and is written after every transcript.
pub fn ready() -> bool {
    server_path().exists() && model_path().exists()
}

/// Downloads the server and the model when they are missing.
fn ensure(events: &Events, abort: &Abort) -> Result<(), String> {
    if !server_path().exists() {
        let archive = dir().join(format!("{LLAMA}.tar.gz"));
        crate::transcribe::download(
            LLAMA_URL,
            &archive,
            "Downloading llama.cpp",
            LLAMA_BYTES,
            LLAMA_SHA256,
            events,
            abort,
        )?;
        let status = Command::new("tar")
            .arg("-xzf")
            .arg(&archive)
            .arg("-C")
            .arg(dir())
            .status()
            .map_err(|e| format!("could not run tar: {e}"))?;
        let _ = std::fs::remove_file(&archive);
        if !status.success() || !server_path().exists() {
            return Err(format!("could not unpack {LLAMA_URL}"));
        }
    }
    if !model_path().exists() {
        crate::transcribe::download(
            MODEL_URL,
            &model_path(),
            "Downloading the summary model",
            MODEL_BYTES,
            MODEL_SHA256,
            events,
            abort,
        )?;
    }
    Ok(())
}

/// Writes `summary.md` in the meeting folder `dir`, from its transcript and
/// its notes, by `writer`. Blocking: run it off the main thread.
pub fn summarize(dir: &Path, writer: Writer, events: &Events, abort: &Abort) -> Result<(), String> {
    let markdown = std::fs::read_to_string(dir.join("transcript.md"))
        .map_err(|e| format!("no transcript in {}: {e}", dir.display()))?;
    let lines = lines_of(&markdown);
    if lines.is_empty() {
        return Err("nobody was heard in this meeting".into());
    }
    let notes = crate::session::notes(dir).unwrap_or_default();
    let french = language_of(&markdown) == "French";
    let summary = match writer {
        Writer::Local => {
            ensure(events, abort)?;
            emit(events, Event::Stage("Writing the summary".into()));
            let server = Server::start(abort)?;
            write(&server, &lines, &notes, french, events, abort)?
        }
        // An hour of meeting is far within what they read at once.
        _ => {
            emit(events, Event::Stage("Writing the summary".into()));
            emit(events, Event::Progress(0.1));
            ask_command(
                writer,
                &prompt(&lines.join("\n"), &notes, french, false),
                abort,
            )?
        }
    };
    let summary = format!("{}\n", summary.trim());
    std::fs::write(dir.join(SUMMARY), summary)
        .and_then(|()| std::fs::write(dir.join(WRITTEN_BY), writer.key()))
        .map_err(|e| format!("could not write the summary: {e}"))
}

/// The answer of Claude Code or Codex to `messages`, with no tools and in an
/// empty folder: they only read what they are given. Cancelling stops them.
fn ask_command(
    writer: Writer,
    messages: &[(&str, String)],
    abort: &Abort,
) -> Result<String, String> {
    let system = format!("{} Answer with the minutes only.", messages[0].1);
    let user = &messages[1].1;
    let work = std::env::temp_dir().join(format!(
        "{}-summary-{}",
        crate::APP_NAME,
        std::process::id()
    ));
    std::fs::create_dir_all(&work).map_err(|e| format!("{}: {e}", work.display()))?;
    let answer = work.join("answer.md");
    let (program, args, input): (&str, Vec<String>, String) = match writer {
        Writer::Claude => (
            "claude",
            [
                "-p",
                "--tools",
                "",
                "--strict-mcp-config",
                "--no-session-persistence",
                "--output-format",
                "text",
                "--system-prompt",
                &system,
            ]
            .map(str::to_owned)
            .to_vec(),
            user.clone(),
        ),
        // Codex has no system prompt of its own: the instructions go first.
        Writer::Codex => (
            "codex",
            [
                "exec",
                "--sandbox",
                "read-only",
                "--skip-git-repo-check",
                "--color",
                "never",
                "--output-last-message",
                &answer.to_string_lossy(),
            ]
            .map(str::to_owned)
            .to_vec(),
            format!("{system}\n\n{user}"),
        ),
        Writer::Local => unreachable!("the model on this computer has its own server"),
    };
    let result = run_command(program, &args, &input, &work, abort).and_then(|out| match writer {
        Writer::Codex => {
            std::fs::read_to_string(&answer).map_err(|e| format!("codex gave no summary: {e}"))
        }
        _ => Ok(out),
    });
    let _ = std::fs::remove_dir_all(&work);
    let text = result?;
    if text.trim().is_empty() {
        return Err(format!("{program} gave an empty summary"));
    }
    Ok(text)
}

/// Runs `program` in `dir` with `input` on its standard input; its output.
fn run_command(
    program: &str,
    args: &[String],
    input: &str,
    dir: &Path,
    abort: &Abort,
) -> Result<String, String> {
    use std::io::Write;
    let mut child = Command::new(program)
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not run {program}: {e}"))?;
    let mut stdin = child.stdin.take().expect("piped stdin");
    let input = input.to_owned();
    let feeding = std::thread::spawn(move || stdin.write_all(input.as_bytes()));
    let mut stdout = child.stdout.take().expect("piped stdout");
    let reading = std::thread::spawn(move || {
        let mut text = String::new();
        stdout.read_to_string(&mut text).map(|_| text)
    });
    let mut stderr = child.stderr.take().expect("piped stderr");
    let errors = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stderr.read_to_string(&mut text);
        text
    });
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            break status;
        }
        let late = started.elapsed() > Duration::from_secs(15 * 60);
        if abort.load(Ordering::Relaxed) || late {
            let _ = child.kill();
            let _ = child.wait();
            return Err(if late {
                format!("{program} did not answer in 15 minutes")
            } else {
                CANCELLED.into()
            });
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    let _ = feeding.join();
    let out = reading
        .join()
        .map_err(|_| "reading the summary panicked".to_owned())?
        .map_err(|e| e.to_string())?;
    if !status.success() {
        let errors = errors.join().unwrap_or_default();
        let last = errors
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("");
        return Err(format!("{program} failed ({status}): {last}"));
    }
    Ok(out)
}

fn emit(events: &Events, event: Event) {
    let _ = events.send_blocking(event);
}

/// The transcript's lines as the model reads them: `[12:34] Name: text`.
fn lines_of(markdown: &str) -> Vec<String> {
    markdown
        .lines()
        .filter_map(crate::session::parse_segment)
        .map(|(time, speaker, text)| format!("[{time}] {speaker}: {text}"))
        .collect()
}

/// The language the transcript header names, in English.
fn language_of(markdown: &str) -> &str {
    markdown
        .lines()
        .find_map(|l| l.strip_prefix("- **Language:** "))
        .map_or("", str::trim)
}

/// Tokens a text takes, about: Qwen's tokenizer gives 3.6 to 3.9 characters
/// a token on French meetings; 3 leaves a margin.
fn tokens(text: &str) -> usize {
    text.chars().count() / 3
}

/// The summary of `lines`: in one go when they fit the model's context,
/// else from the key points of each part.
fn write(
    server: &Server,
    lines: &[String],
    notes: &str,
    french: bool,
    events: &Events,
    abort: &Abort,
) -> Result<String, String> {
    let room = CONTEXT - ROOM - tokens(notes);
    let parts = split(lines, room);
    if let [part] = parts.as_slice() {
        emit(events, Event::Progress(0.1));
        return server.ask(&prompt(&part.join("\n"), notes, french, false), abort);
    }
    let mut points = Vec::new();
    for (i, part) in parts.iter().enumerate() {
        emit(events, Event::Progress(i as f64 / (parts.len() + 1) as f64));
        points.push(server.ask(&prompt(&part.join("\n"), notes, french, true), abort)?);
    }
    emit(
        events,
        Event::Progress(parts.len() as f64 / (parts.len() + 1) as f64),
    );
    server.ask(&prompt(&points.join("\n\n"), notes, french, false), abort)
}

/// `lines` in parts of at most `room` tokens, whole lines each.
fn split(lines: &[String], room: usize) -> Vec<Vec<String>> {
    let mut parts: Vec<Vec<String>> = vec![Vec::new()];
    let mut size = 0;
    for line in lines {
        let n = tokens(line) + 1;
        if size + n > room && !parts.last().is_some_and(Vec::is_empty) {
            parts.push(Vec::new());
            size = 0;
        }
        parts
            .last_mut()
            .expect("one part at least")
            .push(line.clone());
        size += n;
    }
    parts
}

/// What the model is asked: the summary of `text` (the transcript, or the
/// key points of its parts), or with `points` only the key points of a part.
fn prompt(text: &str, notes: &str, french: bool, points: bool) -> Vec<(&'static str, String)> {
    let (system, intro, notes_intro, ask) = if french {
        (
            "Tu rédiges le compte rendu d'une réunion à partir de sa transcription automatique \
             (elle contient des erreurs de reconnaissance). Écris en français, en Markdown, sans \
             inventer : uniquement ce qui est dit dans la transcription.",
            "Transcription :",
            "Notes prises pendant la réunion par la personne qui l'a enregistrée (ce qui compte \
             pour elle, à développer en priorité) :",
            if points {
                "Ceci est une partie de la réunion. Liste ses points clés, en puces : décisions, \
                 actions (qui, quoi, échéance si elle est dite), questions ouvertes et sujets, \
                 chacun avec l'horodatage [mm:ss] où il est dit."
            } else {
                "Rédige le compte rendu avec ces sections :\n\
                 ## Résumé\n3 à 5 phrases.\n\
                 ## Décisions\nUne puce par décision prise.\n\
                 ## Actions\nUne puce par chose à faire : qui, quoi, échéance si elle est dite.\n\
                 ## Questions ouvertes\nCe qui reste à trancher.\n\
                 ## Sujets abordés\nUne puce par sujet, avec l'horodatage [mm:ss] où il commence."
            },
        )
    } else {
        (
            "You write the minutes of a meeting from its automatic transcript (it has speech \
             recognition errors). Write in the transcript's language, in Markdown, without \
             making anything up: only what is said in the transcript.",
            "Transcript:",
            "Notes taken during the meeting by the person who recorded it (what matters to them, \
             to cover first):",
            if points {
                "This is one part of the meeting. List its key points as bullets: decisions, \
                 action items (who, what, due date if one is said), open questions and topics, \
                 each with the [mm:ss] time it is said at."
            } else {
                "Write the minutes with these sections:\n\
                 ## Summary\n3 to 5 sentences.\n\
                 ## Decisions\nOne bullet per decision made.\n\
                 ## Action Items\nOne bullet per thing to do: who, what, due date if one is said.\n\
                 ## Open Questions\nWhat is left to settle.\n\
                 ## Topics\nOne bullet per topic, with the [mm:ss] time it starts at."
            },
        )
    };
    let mut user = format!("{intro}\n\n{text}\n\n");
    if !notes.trim().is_empty() {
        user.push_str(&format!("{notes_intro}\n\n{}\n\n", notes.trim()));
    }
    user.push_str(ask);
    vec![("system", system.to_owned()), ("user", user)]
}

/// llama.cpp's server with the summary model, on a port of its own and with a
/// key, stopped when dropped.
struct Server {
    child: Child,
    url: String,
    key: String,
}

impl Server {
    fn start(abort: &Abort) -> Result<Server, String> {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .and_then(|l| l.local_addr())
            .map_err(|e| format!("no free port for the summary: {e}"))?
            .port();
        let key = random_key()?;
        let log = std::fs::File::create(dir().join("server.log")).ok();
        let mut command = Command::new(server_path());
        command
            .arg("-m")
            .arg(model_path())
            .args(["-c", &CONTEXT.to_string(), "-np", "1", "-fa", "on"])
            .args(["--cache-type-k", "q8_0", "--cache-type-v", "q8_0"])
            .args(["--host", "127.0.0.1", "--port", &port.to_string()])
            .args(["--api-key", &key, "--no-webui"])
            .env("LD_LIBRARY_PATH", dir().join(LLAMA))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log.map_or_else(Stdio::null, Stdio::from));
        // All of the model on the GPU whisper uses: left to itself, llama.cpp
        // kept some layers on the CPU and wrote ten times slower.
        if let Some(device) = crate::transcribe::vulkan_device() {
            command.args(["--device", &format!("Vulkan{device}"), "-ngl", "99"]);
        }
        let child = command
            .spawn()
            .map_err(|e| format!("could not start llama.cpp: {e}"))?;
        let mut server = Server {
            child,
            url: format!("http://127.0.0.1:{port}"),
            key,
        };
        server.wait_ready(abort)?;
        Ok(server)
    }

    fn wait_ready(&mut self, abort: &Abort) -> Result<(), String> {
        let started = Instant::now();
        loop {
            if abort.load(Ordering::Relaxed) {
                return Err(CANCELLED.into());
            }
            if let Ok(Some(status)) = self.child.try_wait() {
                return Err(format!(
                    "llama.cpp stopped ({status}); see {}",
                    dir().join("server.log").display()
                ));
            }
            if ureq::get(&format!("{}/health", self.url)).call().is_ok() {
                return Ok(());
            }
            if started.elapsed() > Duration::from_secs(180) {
                return Err("llama.cpp did not start in 3 minutes".into());
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }

    /// The model's answer to `messages`. Cancelling stops the server, which
    /// ends the request.
    fn ask(&self, messages: &[(&str, String)], abort: &Abort) -> Result<String, String> {
        let body = serde_json::json!({
            "messages": messages
                .iter()
                .map(|(role, content)| serde_json::json!({"role": role, "content": content}))
                .collect::<Vec<_>>(),
            "temperature": 0.3,
            "max_tokens": 2_000,
            // Qwen3.5 thinks aloud first unless told not to: minutes more.
            "chat_template_kwargs": {"enable_thinking": false},
        });
        let (url, key) = (
            format!("{}/v1/chat/completions", self.url),
            self.key.clone(),
        );
        let request = std::thread::spawn(move || -> Result<String, String> {
            let mut response = ureq::post(&url)
                .header("Authorization", &format!("Bearer {key}"))
                .header("Content-Type", "application/json")
                .config()
                .timeout_global(Some(Duration::from_secs(3_600)))
                .build()
                .send(body.to_string())
                .map_err(|e| format!("llama.cpp did not answer: {e}"))?;
            let mut text = String::new();
            response
                .body_mut()
                .as_reader()
                .read_to_string(&mut text)
                .map_err(|e| e.to_string())?;
            let reply: serde_json::Value =
                serde_json::from_str(&text).map_err(|e| e.to_string())?;
            reply["choices"][0]["message"]["content"]
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("llama.cpp gave no summary: {text}"))
        });
        while !request.is_finished() {
            if abort.load(Ordering::Relaxed) {
                // Dropping the server stops it, and the request with it.
                return Err(CANCELLED.into());
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        request
            .join()
            .map_err(|_| "the summary request panicked".to_owned())?
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A key only Minutes knows, so no other program on this computer uses the
/// server meanwhile.
fn random_key() -> Result<String, String> {
    let mut bytes = [0u8; 16];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .map_err(|e| format!("no random key for the summary: {e}"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// `minutes summarize <meeting folder> [--by local|claude|codex]`: writes its
/// `summary.md` and prints it.
pub fn cli(args: &[String]) -> glib::ExitCode {
    let (dir, writer) = match args {
        [dir] => (dir, Writer::configured()),
        [dir, by, key] if by == "--by" => match Writer::from_key(key) {
            Some(writer) => (dir, writer),
            None => return crate::transcribe::usage(),
        },
        _ => return crate::transcribe::usage(),
    };
    let dir = PathBuf::from(dir);
    crate::transcribe::run_cli(|events, abort| {
        summarize(&dir, writer, events, abort)?;
        std::fs::read_to_string(dir.join(SUMMARY)).map_err(|e| e.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_transcript_is_read_as_lines_with_their_time_and_speaker() {
        let markdown = "# Point\n\n- **Language:** French\n\n## Transcript\n\n\
                        **[00:05] Alice:** Bonjour.\n\n**[01:10] Remote 2:** On commence ?\n";
        assert_eq!(
            lines_of(markdown),
            ["[00:05] Alice: Bonjour.", "[01:10] Remote 2: On commence ?"]
        );
        assert_eq!(language_of(markdown), "French");
    }

    #[test]
    fn a_long_meeting_is_cut_between_lines() {
        let lines: Vec<String> = (0..10)
            .map(|i| format!("[00:0{i}] A: {}", "mot ".repeat(30)))
            .collect();
        let parts = split(&lines, 100);
        assert!(parts.len() > 1);
        assert_eq!(parts.concat(), lines);
        assert!(
            parts
                .iter()
                .all(|p| p.iter().map(|l| tokens(l) + 1).sum::<usize>() <= 100)
        );
        // One line longer than the room still goes, alone.
        let long = vec!["x".repeat(600)];
        assert_eq!(split(&long, 100), vec![long.clone()]);
    }

    #[test]
    fn the_notes_go_to_the_model_when_there_are_some() {
        let with = prompt("[00:01] A: oui", "budget ?", true, false);
        assert!(with[1].1.contains("budget ?"));
        assert!(with[1].1.contains("## Décisions"));
        let without = prompt("[00:01] A: yes", "  ", false, false);
        assert!(!without[1].1.contains("Notes taken"));
        assert!(without[1].1.contains("## Action Items"));
    }
}
