//! The words of a finished transcript worth asking about: names, tools and
//! jargon whisper got wrong, so Minutes learns them meeting after meeting.
//!
//! Two signs, measured on two real meetings (2 h 47 and 1 h 02, French):
//! a word the dictionary does not know, which whisper was unsure of, or that
//! comes in several close spellings ("Zabix", "Zabitz"). Right technical terms
//! the dictionary does not know either (Grafana, Kubernetes) are left out:
//! whisper wrote them sure of itself (0.94 and 0.97 at the median), where
//! its mistakes came out below 0.2.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::process::{Command, Stdio};

use crate::transcribe::{Heard, Segment};

/// Asked about after one meeting at most: more would not be read.
pub const MAX: usize = 10;
/// A lone unknown word is asked about when whisper was less sure than this.
const UNSURE: f32 = 0.5;
/// Spellings of one word are asked about unless whisper was this sure.
const SPELLINGS_UNSURE: f32 = 0.9;

/// A word to check, with what is needed to ask about it.
#[derive(Debug, Clone, PartialEq)]
pub struct Doubt {
    /// The spellings the transcript has, the most frequent first.
    pub spellings: Vec<String>,
    /// How many times, all spellings together.
    pub count: usize,
    /// Where it is first heard, in milliseconds of the recording.
    pub at_ms: i64,
    /// The sentence it is first said in, cut to a few words around it.
    pub line: String,
    /// How sure whisper was of it, the median over where it was heard;
    /// 1 when it came from a fix.
    pub sure: f32,
}

/// A word as it is compared: without an elided article ("l'", "d'") and
/// the punctuation around it.
fn bare(word: &str) -> &str {
    let word = word.trim_matches(|c: char| !c.is_alphanumeric());
    let lower = word.to_lowercase();
    for article in [
        "l'", "d'", "j'", "n'", "m'", "s'", "c'", "t'", "qu'", "l’", "d’", "qu’",
    ] {
        if lower.starts_with(article) && word.len() > article.len() {
            return &word[article.len()..];
        }
    }
    word
}

/// The sentence of `text` that has `word`, and no more than a dozen words
/// on each side of it: transcript lines are whole paragraphs.
fn excerpt(text: &str, word: &str) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    let Some(at) = words
        .iter()
        .position(|w| bare(w).eq_ignore_ascii_case(word))
    else {
        return text.to_owned();
    };
    let ends = |w: &&str| w.ends_with(['.', '!', '?', '…']);
    let start = words[..at]
        .iter()
        .rposition(ends)
        .map_or(0, |i| i + 1)
        .max(at.saturating_sub(12));
    let end = words[at..]
        .iter()
        .position(ends)
        .map_or(words.len(), |i| at + i + 1)
        .min(at + 13);
    let mut out = words[start..end].join(" ");
    if start > 0 && !words[start - 1].ends_with(['.', '!', '?', '…']) {
        out.insert_str(0, "… ");
    }
    if end < words.len() && !words[end - 1].ends_with(['.', '!', '?', '…']) {
        out.push_str(" …");
    }
    out
}

/// Worth asking about at all: words of three letters or more, not numbers
/// nor what follows one ("2ème").
fn is_word(word: &str) -> bool {
    word.chars().count() >= 3 && word.chars().next().is_some_and(char::is_alphabetic)
}

fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    for (i, x) in a.chars().enumerate() {
        let mut current = vec![i + 1];
        for (j, y) in b.iter().enumerate() {
            current.push(
                (previous[j + 1] + 1)
                    .min(current[j] + 1)
                    .min(previous[j] + usize::from(x != *y)),
            );
        }
        previous = current;
    }
    previous[b.len()]
}

/// Two spellings of the same word: one or two letters apart, for words long
/// enough that this is not another word.
fn close(a: &str, b: &str) -> bool {
    let shorter = a.chars().count().min(b.chars().count());
    shorter >= 4 && distance(a, b) <= if shorter >= 5 { 2 } else { 1 }
}

/// The words of `segments` to ask about. `unknown` tells which of the words
/// given the dictionary does not know; `known` holds, in lower case, those
/// already settled (the config's vocabulary and fixes, the words set aside,
/// the names of the people in the call).
pub fn doubts(
    segments: &[Segment],
    heard: &[Heard],
    unknown: impl Fn(&[String]) -> HashSet<String>,
    known: &HashSet<String>,
) -> Vec<Doubt> {
    // Each word of the final text (fixes applied), by lower case: its
    // spellings with their counts, and where it is first said.
    struct Seen {
        spellings: Vec<(String, usize)>,
        at_ms: i64,
        line: String,
    }
    let mut seen: HashMap<String, Seen> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    for segment in segments {
        for word in segment
            .text
            .split_whitespace()
            .map(bare)
            .filter(|w| is_word(w))
        {
            let key = word.to_lowercase();
            if known.contains(&key) {
                continue;
            }
            let entry = seen.entry(key.clone()).or_insert_with(|| {
                order.push(key.clone());
                Seen {
                    spellings: Vec::new(),
                    at_ms: segment.start_ms,
                    line: segment.text.clone(),
                }
            });
            match entry.spellings.iter_mut().find(|(s, _)| s == word) {
                Some((_, count)) => *count += 1,
                None => entry.spellings.push((word.to_owned(), 1)),
            }
        }
    }
    let words: Vec<String> = order
        .iter()
        .flat_map(|k| seen[k].spellings.iter().map(|(s, _)| s.clone()))
        .collect();
    let unknown: HashSet<String> = unknown(&words)
        .into_iter()
        .map(|w| w.to_lowercase())
        .collect();

    let mut sure: HashMap<String, Vec<f32>> = HashMap::new();
    for word in heard {
        let key = bare(&word.text).to_lowercase();
        if seen.contains_key(&key) {
            sure.entry(key).or_default().push(word.sure);
        }
    }
    let median = |keys: &[&String]| -> f32 {
        let mut all: Vec<f32> = keys
            .iter()
            .flat_map(|k| sure.get(*k).into_iter().flatten().copied())
            .collect();
        if all.is_empty() {
            return 1.0;
        }
        all.sort_by(f32::total_cmp);
        all[all.len() / 2]
    };

    // Close spellings of unknown words together, the most frequent first.
    let count = |k: &String| seen[k].spellings.iter().map(|(_, c)| c).sum::<usize>();
    let mut candidates: Vec<&String> = order.iter().filter(|k| unknown.contains(*k)).collect();
    candidates.sort_by_key(|k| std::cmp::Reverse(count(k)));
    let mut grouped: HashSet<&String> = HashSet::new();
    let mut ranked: Vec<(u8, f32, Doubt)> = Vec::new();
    for key in &candidates {
        if grouped.contains(key) {
            continue;
        }
        let group: Vec<&String> = candidates
            .iter()
            .filter(|k| !grouped.contains(*k) && (*k == key || close(k, key)))
            .copied()
            .collect();
        grouped.extend(group.iter().copied());
        let certainty = median(&group);
        let rank = match (group.len() > 1, certainty) {
            (true, s) if s < SPELLINGS_UNSURE => 0,
            (false, s) if s < UNSURE => 1,
            _ => continue,
        };
        let mut spellings: Vec<(String, usize)> = group
            .iter()
            .flat_map(|k| seen[*k].spellings.iter().cloned())
            .collect();
        spellings.sort_by_key(|(_, c)| std::cmp::Reverse(*c));
        let first = group
            .iter()
            .min_by_key(|k| seen[**k].at_ms)
            .expect("a group has a word");
        // Where whisper heard it, to play it; else where its line starts.
        let heard_at = heard
            .iter()
            .filter(|w| bare(&w.text).eq_ignore_ascii_case(first))
            .map(|w| w.at_ms)
            .find(|&at| at >= seen[*first].at_ms);
        let first_spelling = &seen[*first].spellings[0].0;
        ranked.push((
            rank,
            certainty,
            Doubt {
                count: spellings.iter().map(|(_, c)| c).sum(),
                spellings: spellings.into_iter().map(|(s, _)| s).collect(),
                at_ms: heard_at.unwrap_or(seen[*first].at_ms),
                line: excerpt(&seen[*first].line, first_spelling),
                sure: certainty,
            },
        ));
    }
    // Spellings first, the most said first; then the words whisper was the
    // least sure of.
    ranked.sort_by(|a, b| {
        a.0.cmp(&b.0).then_with(|| match a.0 {
            0 => b.2.count.cmp(&a.2.count),
            _ => a.1.total_cmp(&b.1),
        })
    });
    ranked
        .into_iter()
        .take(MAX)
        .map(|(_, _, doubt)| doubt)
        .collect()
}

/// The dictionary of a whisper language code, when hunspell and it are there.
fn dictionary(language: &str) -> Option<&'static str> {
    let name = match language {
        "fr" => "fr_FR",
        "en" => "en_US",
        "de" => "de_DE",
        "nl" => "nl_NL",
        "es" => "es_ES",
        "it" => "it_IT",
        "pt" => "pt_PT",
        _ => return None,
    };
    std::path::Path::new(&format!("/usr/share/hunspell/{name}.dic"))
        .exists()
        .then_some(name)
}

/// The words the dictionary of `language` does not know, from hunspell. With
/// no dictionary, none: then only close spellings are asked about.
pub fn unknown_words(language: &str) -> impl Fn(&[String]) -> HashSet<String> {
    let dictionary = dictionary(language);
    move |words: &[String]| {
        let Some(dictionary) = dictionary else {
            return HashSet::new();
        };
        let Ok(mut child) = Command::new("hunspell")
            .args(["-d", dictionary, "-l"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        else {
            return HashSet::new();
        };
        if let Some(mut stdin) = child.stdin.take() {
            use std::io::Write;
            let _ = stdin.write_all(words.join("\n").as_bytes());
        }
        child
            .wait_with_output()
            .map(|out| {
                String::from_utf8_lossy(&out.stdout)
                    .lines()
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// Where a meeting folder keeps its words to check.
const FILE: &str = "review.json";

/// Keeps `doubts` in the meeting folder `dir`; none leaves no file.
pub fn save(dir: &Path, doubts: &[Doubt]) -> std::io::Result<()> {
    let path = dir.join(FILE);
    if doubts.is_empty() {
        return match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    let list: Vec<serde_json::Value> = doubts
        .iter()
        .map(|d| {
            serde_json::json!({
                "spellings": d.spellings,
                "count": d.count,
                "at_ms": d.at_ms,
                "line": d.line,
                "sure": d.sure,
            })
        })
        .collect();
    std::fs::write(
        path,
        serde_json::to_string_pretty(&list).unwrap_or_default() + "\n",
    )
}

/// The words a meeting folder still has to check.
pub fn load(dir: &Path) -> Vec<Doubt> {
    let Ok(text) = std::fs::read_to_string(dir.join(FILE)) else {
        return Vec::new();
    };
    let list: Vec<serde_json::Value> = serde_json::from_str(&text).unwrap_or_default();
    list.iter()
        .filter_map(|d| {
            Some(Doubt {
                spellings: d["spellings"]
                    .as_array()?
                    .iter()
                    .filter_map(|s| s.as_str().map(str::to_owned))
                    .collect(),
                count: d["count"].as_u64().unwrap_or(1) as usize,
                at_ms: d["at_ms"].as_i64().unwrap_or(0),
                line: d["line"].as_str().unwrap_or("").to_owned(),
                sure: d["sure"].as_f64().unwrap_or(1.0) as f32,
            })
        })
        .filter(|d| !d.spellings.is_empty())
        .collect()
}

/// Takes a word, settled, off the folder's list.
pub fn settle(dir: &Path, word: &str) -> std::io::Result<()> {
    let left: Vec<Doubt> = load(dir)
        .into_iter()
        .filter(|d| d.spellings[0] != word)
        .collect();
    save(dir, &left)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(start_ms: i64, text: &str) -> Segment {
        Segment {
            start_ms,
            end_ms: start_ms + 1000,
            speaker: "Remote".into(),
            text: text.into(),
        }
    }

    fn heard(text: &str, sure: f32) -> Heard {
        Heard {
            text: text.into(),
            at_ms: 0,
            sure,
        }
    }

    /// The words not in a small dictionary of this test.
    fn not_in(dictionary: &[&str]) -> impl Fn(&[String]) -> HashSet<String> {
        let dictionary: HashSet<String> = dictionary.iter().map(|w| w.to_lowercase()).collect();
        move |words: &[String]| {
            words
                .iter()
                .filter(|w| !dictionary.contains(&w.to_lowercase()))
                .cloned()
                .collect()
        }
    }

    #[test]
    fn close_spellings_and_unsure_unknown_words_are_asked_about() {
        let segments = [
            line(0, "On regarde Zabix et Grafana."),
            line(4000, "Zabix remonte les alertes."),
            line(8000, "Zabitz aussi, dans l'Grafana."),
            line(12000, "Le promédié est prêt."),
            line(16000, "Encore une fois Grafana, avec Kubernetes."),
        ];
        let heard = [
            heard("Zabix", 0.4),
            heard("Zabix", 0.6),
            heard("Zabitz", 0.3),
            heard("Grafana", 0.94),
            heard("Grafana", 0.97),
            heard("promédié", 0.17),
            heard("Kubernetes", 0.97),
        ];
        let dictionary = [
            "on", "regarde", "et", "remonte", "les", "alertes", "aussi", "dans", "le", "est",
            "prêt", "encore", "une", "fois", "avec",
        ];
        let doubts = doubts(&segments, &heard, not_in(&dictionary), &HashSet::new());
        let words: Vec<&str> = doubts.iter().map(|d| d.spellings[0].as_str()).collect();
        // Spellings first; Grafana and Kubernetes, heard sure, are not asked.
        assert_eq!(words, ["Zabix", "promédié"]);
        assert_eq!(doubts[0].spellings, ["Zabix", "Zabitz"]);
        assert_eq!(doubts[0].count, 3);
        assert_eq!(doubts[0].at_ms, 0);
        assert_eq!(doubts[1].line, "Le promédié est prêt.");
    }

    #[test]
    fn the_line_shown_is_the_sentence_with_the_word() {
        let paragraph = "Bonjour à tous. On parle ensuite de Tellari, l'outil que l'équipe a choisi pour les tableaux de bord de la plateforme et des alertes aussi. Voilà.";
        assert_eq!(
            excerpt(paragraph, "Tellari"),
            "On parle ensuite de Tellari, l'outil que l'équipe a choisi pour les tableaux de bord de la …"
        );
        assert_eq!(
            excerpt("Le promédié est prêt.", "promédié"),
            "Le promédié est prêt."
        );
    }

    #[test]
    fn what_is_known_is_not_asked_again() {
        let segments = [line(0, "Le promédié et Quorbix.")];
        let heard = [heard("promédié", 0.1), heard("Quorbix", 0.2)];
        let known: HashSet<String> = ["quorbix".to_owned()].into();
        let doubts = doubts(&segments, &heard, not_in(&["le", "et"]), &known);
        assert_eq!(doubts.len(), 1);
        assert_eq!(doubts[0].spellings, ["promédié"]);
    }

    #[test]
    fn no_more_than_ten() {
        // Twenty words far from each other: "aaaaaa", "bbbbbb"...
        let text: Vec<String> = (b'a'..b'u')
            .map(|c| (c as char).to_string().repeat(6))
            .collect();
        let segments = [line(0, &text.join(" "))];
        let heard: Vec<Heard> = text.iter().map(|w| heard(w, 0.1)).collect();
        assert_eq!(
            doubts(&segments, &heard, not_in(&[]), &HashSet::new()).len(),
            MAX
        );
    }

    #[test]
    fn the_list_is_kept_with_the_meeting_and_shrinks_as_words_are_settled() {
        let dir = std::env::temp_dir().join(format!("minutes-review-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let doubt = |word: &str| Doubt {
            spellings: vec![word.into(), format!("{word}s")],
            count: 3,
            at_ms: 1200,
            line: format!("Le {word} ici."),
            sure: 0.25,
        };
        save(&dir, &[doubt("Zabix"), doubt("Tellari")]).unwrap();
        assert_eq!(load(&dir), [doubt("Zabix"), doubt("Tellari")]);
        settle(&dir, "Zabix").unwrap();
        assert_eq!(load(&dir), [doubt("Tellari")]);
        settle(&dir, "Tellari").unwrap();
        assert!(!dir.join(FILE).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn articles_and_numbers_are_left_out() {
        assert_eq!(bare("l'ADNAT"), "ADNAT");
        assert_eq!(bare("d'OpenSearch,"), "OpenSearch");
        assert!(!is_word("2ème"));
        assert!(is_word("ème"));
        assert!(close("tellari", "tellary"));
        assert!(!close("chat", "chien"));
    }
}
