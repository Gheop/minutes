//! Chapters: an optional enrichment made by the default agent after the
//! transcript is done. Without an agent nothing here runs and the meeting is
//! simply without chapters.
//!
//! They are kept in the meeting's `.meeting-recorder` file, and also written into
//! `transcript.md` as a `## Chapters` list, so a copied transcript carries them
//! along.

use serde_json::Value;

use crate::agent::{self, Agent};

/// Shorter meetings do not need chapters.
pub const MIN_DURATION_MS: i64 = 3 * 60 * 1000;
const MAX_CHAPTERS: usize = 20;
const MAX_TITLE_CHARS: usize = 80;

#[derive(Clone, Debug, PartialEq)]
pub struct Chapter {
    pub start_ms: i64,
    pub title: String,
}

/// One turn of the transcript, as the agent gets to see it.
pub struct Line<'a> {
    pub start_ms: i64,
    pub speaker: &'a str,
    pub text: &'a str,
}

const PROMPT: &str = "You get the transcript of a meeting, one line per turn, as \
[time] Speaker: text. Divide it into chapters for someone who wants to jump straight \
to the part they need.

Rules:
- One chapter for every 5 to 10 minutes or so, following the topics: at least 2, at most 12.
- The first chapter starts at the first line.
- Every chapter starts at the time of an existing line, copied exactly.
- Titles are 2 to 6 words, specific to what is discussed, in the language of the \
transcript, without numbering or quotes.

Reply with only a JSON array and nothing else, like:
[{\"start\": \"00:00\", \"title\": \"Opening and agenda\"}]";

/// Asks `agent` for chapters of `lines`. Blocking: run it off the main thread.
pub fn generate(agent: &Agent, lines: &[Line]) -> Result<Vec<Chapter>, String> {
    let text: String = lines
        .iter()
        .map(|l| format!("[{}] {}: {}\n", clock(l.start_ms), l.speaker, l.text))
        .collect();
    let answer = agent::run(agent, PROMPT, &text)?;
    let starts: Vec<i64> = lines.iter().map(|l| l.start_ms).collect();
    let chapters = parse(&answer, &starts);
    if chapters.is_empty() {
        return Err("the agent's answer had no usable chapters".into());
    }
    Ok(chapters)
}

/// Reads the agent's JSON, wherever it sits in the answer, and makes it safe:
/// every start on an existing line, sorted, one chapter per line, the first
/// at the start, titles short and on one line.
fn parse(answer: &str, starts: &[i64]) -> Vec<Chapter> {
    let (Some(open), Some(close)) = (answer.find('['), answer.rfind(']')) else {
        return Vec::new();
    };
    let Ok(Value::Array(items)) = serde_json::from_str::<Value>(&answer[open..=close.max(open)])
    else {
        return Vec::new();
    };
    let Some(&first) = starts.first() else {
        return Vec::new();
    };

    let mut chapters: Vec<Chapter> = items
        .iter()
        .filter_map(|item| {
            let start = parse_clock(item["start"].as_str()?)?;
            let title: String = item["title"]
                .as_str()?
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .trim_matches(|c: char| c == '"' || c == '\'' || c == '*' || c == '#')
                .chars()
                .take(MAX_TITLE_CHARS)
                .collect();
            if title.is_empty() {
                return None;
            }
            // Snap to the line that starts closest to the given time.
            let snapped = *starts.iter().min_by_key(|s| (*s - start).abs())?;
            Some(Chapter {
                start_ms: snapped,
                title,
            })
        })
        .collect();
    chapters.sort_by_key(|c| c.start_ms);
    chapters.dedup_by_key(|c| c.start_ms);
    chapters.truncate(MAX_CHAPTERS);
    if let Some(opening) = chapters.first_mut() {
        opening.start_ms = first;
    }
    chapters
}

/// Puts a `## Chapters` list before `## Transcript`, replacing an older one.
pub fn apply_to_markdown(markdown: &str, chapters: &[Chapter]) -> String {
    let without = remove_section(markdown);
    if chapters.is_empty() {
        return without;
    }
    let mut section = String::from("## Chapters\n\n");
    for chapter in chapters {
        section += &format!("- [{}] {}\n", clock(chapter.start_ms), chapter.title);
    }
    section += "\n";
    match without.find("## Transcript") {
        Some(at) => format!("{}{section}{}", &without[..at], &without[at..]),
        None => format!("{without}\n{section}"),
    }
}

fn remove_section(markdown: &str) -> String {
    let Some(start) = markdown.find("## Chapters\n") else {
        return markdown.to_owned();
    };
    let rest = &markdown[start + "## Chapters\n".len()..];
    let end = rest
        .find("\n## ")
        .map(|i| start + "## Chapters\n".len() + i + 1)
        .unwrap_or(markdown.len());
    format!("{}{}", &markdown[..start], &markdown[end..])
}

pub fn clock(ms: i64) -> String {
    let secs = ms / 1000;
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

fn parse_clock(text: &str) -> Option<i64> {
    let mut total = 0i64;
    for part in text.trim().split(':') {
        total = total * 60 + part.trim().parse::<i64>().ok()?;
    }
    Some(total * 1000)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fenced_json_and_snaps_to_lines() {
        let starts = [2_000, 65_000, 190_000, 400_000];
        let answer = "Sure!\n```json\n[{\"start\": \"03:08\", \"title\": \" **Pricing** \"},\
                      {\"start\": \"00:00\", \"title\": \"Opening\"},\
                      {\"start\": \"03:10\", \"title\": \"Duplicate\"},\
                      {\"start\": \"xx\", \"title\": \"Broken\"}]\n```";
        let chapters = parse(answer, &starts);
        assert_eq!(
            chapters,
            vec![
                Chapter {
                    start_ms: 2_000,
                    title: "Opening".into()
                },
                Chapter {
                    start_ms: 190_000,
                    title: "Pricing".into()
                },
            ]
        );
    }

    #[test]
    fn garbage_gives_no_chapters() {
        assert!(parse("I cannot help with that.", &[0]).is_empty());
        assert!(parse("[not json]", &[0]).is_empty());
    }

    #[test]
    fn markdown_section_is_inserted_and_replaced() {
        let md = "# T\n\n- **Date:** x\n\n## Transcript\n\n**[00:00] You:** Hi.\n";
        let one = apply_to_markdown(
            md,
            &[Chapter {
                start_ms: 0,
                title: "A".into(),
            }],
        );
        assert!(one.contains("## Chapters\n\n- [00:00] A\n\n## Transcript"));
        let two = apply_to_markdown(
            &one,
            &[Chapter {
                start_ms: 61_000,
                title: "B".into(),
            }],
        );
        assert!(two.contains("- [01:01] B") && !two.contains("- [00:00] A"));
        assert_eq!(apply_to_markdown(&two, &[]), md);
    }
}
