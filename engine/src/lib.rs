//! The engine of Minutes: records a meeting as two tracks (the mic and the
//! computer audio), transcribes each with whisper.cpp after the call, tells
//! the voices on each side apart, and writes the meeting folder. No GTK: it
//! runs and is tested without a display.

pub mod audio;
pub mod calls;
pub mod diarize;
pub mod export;
pub mod glossary;
pub mod live;
pub mod meeting;
pub mod models;
pub mod nemotron;
pub mod session;
pub mod teams;
pub mod transcribe;

use std::path::Path;

/// The name in paths (config, models, cache) and messages.
pub const APP_NAME: &str = "minutes";
/// The name those paths had before, from the project Minutes comes from.
const OLD_NAME: &str = "omarchy-meeting-recorder";

/// Moves the config, the models and the recordings waiting in the cache from
/// the paths they had under the old name. Called at start, before anything
/// reads them; does nothing once they have moved.
pub fn move_old_paths() {
    let config = models::config_file();
    let models = transcribe::models_dir();
    let dirs = [
        config.parent(),
        models.parent(),
        Some(&*session::staging_root()),
    ];
    for dir in dirs.into_iter().flatten() {
        let old = dir.with_file_name(OLD_NAME);
        if let Err(e) = move_into(&old, dir) {
            eprintln!("{APP_NAME}: could not move {}: {e}", old.display());
        }
    }
}

/// Moves what is in `old` to `new`, leaving alone what `new` already has.
fn move_into(old: &Path, new: &Path) -> std::io::Result<()> {
    if !old.is_dir() {
        return Ok(());
    }
    if !new.exists() {
        return std::fs::rename(old, new);
    }
    for entry in std::fs::read_dir(old)? {
        let entry = entry?;
        let target = new.join(entry.file_name());
        if !target.exists() {
            std::fs::rename(entry.path(), target)?;
        }
    }
    // Only goes when empty: anything left there is kept.
    let _ = std::fs::remove_dir(old);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_paths_move_without_overwriting() {
        let root = std::env::temp_dir().join(format!("minutes-move-{}", std::process::id()));
        let (old, new) = (root.join(OLD_NAME), root.join(APP_NAME));
        std::fs::create_dir_all(old.join("models")).unwrap();
        std::fs::write(old.join("models/a.bin"), "a").unwrap();
        // Moved whole when there is nothing yet under the new name.
        move_into(&old, &new).unwrap();
        assert!(!old.exists());
        assert_eq!(
            std::fs::read_to_string(new.join("models/a.bin")).unwrap(),
            "a"
        );
        // Otherwise one entry at a time, and what is there already stays.
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("config.toml"), "old").unwrap();
        std::fs::write(old.join("models"), "clash").unwrap();
        move_into(&old, &new).unwrap();
        assert_eq!(
            std::fs::read_to_string(new.join("config.toml")).unwrap(),
            "old"
        );
        assert!(new.join("models/a.bin").exists());
        assert!(old.join("models").exists(), "kept where it was");
        // Nothing to move: nothing happens.
        std::fs::remove_dir_all(&old).unwrap();
        move_into(&old, &new).unwrap();
        let _ = std::fs::remove_dir_all(&root);
    }
}
