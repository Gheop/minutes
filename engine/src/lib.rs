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

/// The name in paths (config, models, cache) and messages. It stays the one
/// of the project Minutes comes from until the rename moves those paths.
pub const APP_NAME: &str = "omarchy-meeting-recorder";
