//! Names and words whisper keeps getting wrong.
//!
//! Before: `prompt = "…"` in the config file, and the names of the people in
//! the call (`set_names`), tell whisper what to expect. After: each
//! `fix = "Sasse => Sas"` line in the config file replaces the words on
//! the left with the words on the right in the finished transcript. The match
//! ignores case and only takes whole words, so "Sasse" does not touch
//! "Sassenage".

use std::sync::Mutex;

use crate::transcribe::Segment;

/// The people in the call being transcribed, from the meeting app.
static NAMES: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// The names of the people in the call to transcribe next.
pub fn set_names(names: &[String]) {
    *NAMES.lock().unwrap_or_else(|e| e.into_inner()) = names.to_vec();
}

/// What whisper is told to expect: the `prompt` of the config file, then the
/// names of the people in the call.
pub fn prompt() -> Option<String> {
    let names = NAMES.lock().unwrap_or_else(|e| e.into_inner()).join(", ");
    let parts: Vec<String> = crate::models::config_value("prompt")
        .into_iter()
        .chain((!names.is_empty()).then(|| format!("{names}.")))
        .collect();
    (!parts.is_empty()).then(|| parts.join(" "))
}

pub fn load() -> Vec<(String, String)> {
    crate::models::config_values("fix")
        .into_iter()
        .filter_map(|fix| {
            let (wrong, right) = fix.split_once("=>")?;
            let (wrong, right) = (wrong.trim(), right.trim());
            (!wrong.is_empty()).then(|| (wrong.to_owned(), right.to_owned()))
        })
        .collect()
}

pub fn apply(mut segments: Vec<Segment>) -> Vec<Segment> {
    let fixes = load();
    for segment in &mut segments {
        for (wrong, right) in &fixes {
            segment.text = replace_words(&segment.text, wrong, right);
        }
    }
    segments
}

/// `wrong` made `right` in the text of a transcript's lines, leaving the
/// title, the times and the names alone.
pub fn fix_markdown(markdown: &str, wrong: &str, right: &str) -> String {
    let mut out: Vec<String> = markdown
        .lines()
        .map(|line| match line.split_once(":** ") {
            Some((head, text)) if line.starts_with("**[") => {
                format!("{head}:** {}", replace_words(text, wrong, right))
            }
            _ => line.to_owned(),
        })
        .collect();
    if markdown.ends_with('\n') {
        out.push(String::new());
    }
    out.join("\n")
}

fn replace_words(text: &str, wrong: &str, right: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let pattern: Vec<char> = wrong.chars().collect();
    let same = |a: char, b: char| a.to_lowercase().eq(b.to_lowercase());
    let is_word = |c: Option<&char>| c.is_some_and(|c| c.is_alphanumeric());
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let end = i + pattern.len();
        let found = end <= chars.len()
            && chars[i..end]
                .iter()
                .zip(&pattern)
                .all(|(&a, &b)| same(a, b))
            && !is_word(i.checked_sub(1).and_then(|j| chars.get(j)))
            && !is_word(chars.get(end));
        if found {
            out.push_str(right);
            i = end;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{fix_markdown, replace_words};

    #[test]
    fn a_fix_in_a_transcript_touches_only_what_was_said() {
        let markdown = "# Zabix review\n\n**[00:04] Zabix Team:** On regarde Zabix.\n\n";
        assert_eq!(
            fix_markdown(markdown, "Zabix", "Zabbix"),
            "# Zabix review\n\n**[00:04] Zabix Team:** On regarde Zabbix.\n\n"
        );
    }

    #[test]
    fn whole_words_only_and_any_case() {
        assert_eq!(
            replace_words("Eva Sasse et Sassenage", "sasse", "Sas"),
            "Eva Sas et Sassenage"
        );
        assert_eq!(
            replace_words(
                "mesdames Claire-Marie Boy et",
                "Claire-Marie Boy",
                "Claire Marais-Beuil"
            ),
            "mesdames Claire Marais-Beuil et"
        );
        assert_eq!(
            replace_words("le paquet FIT455.", "fit455", "Fit for 55"),
            "le paquet Fit for 55."
        );
    }
}
