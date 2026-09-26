//! Fixes for words whisper keeps getting wrong, most often names.
//!
//! Each `fix = "Sasse => Sas"` line in the config file replaces the words on
//! the left with the words on the right in the finished transcript. The match
//! ignores case and only takes whole words, so "Sasse" does not touch
//! "Sassenage".

use crate::transcribe::Segment;

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
            && chars[i..end].iter().zip(&pattern).all(|(&a, &b)| same(a, b))
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
    use super::replace_words;

    #[test]
    fn whole_words_only_and_any_case() {
        assert_eq!(
            replace_words("Eva Sasse et Sassenage", "sasse", "Sas"),
            "Eva Sas et Sassenage"
        );
        assert_eq!(
            replace_words("mesdames Claire-Marie Boy et", "Claire-Marie Boy", "Claire Marais-Beuil"),
            "mesdames Claire Marais-Beuil et"
        );
        assert_eq!(replace_words("le paquet FIT455.", "fit455", "Fit for 55"), "le paquet Fit for 55.");
    }
}
