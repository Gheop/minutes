//! Telling when a call is on, from the PipeWire graph: an app that both takes
//! the microphone and plays sound is in a call. That holds for Teams, Meet in a
//! browser, Zoom or anything else, without knowing any of them. A video (sound
//! out only) or a voice memo (microphone only) is not a call.
//!
//! Minutes only ever *offers* to record: the people in the call have to know.

use std::collections::{BTreeMap, HashMap};

/// How long an app has to look like a call before it counts: a notification
/// sound while dictating should not.
pub const START_AFTER_SECS: f64 = 10.0;
/// How long a call has to be gone before it counts as over: muting and
/// unmuting, or switching the headset, closes streams for a moment.
pub const END_AFTER_SECS: f64 = 15.0;

/// Programs that record without being a call; Minutes itself runs `parec`.
const RECORDERS: [&str; 5] = ["parec", "pacat", "pw-record", "pw-cat", "minutes"];

/// The apps in a call in a `pw-dump` graph: (key, name to show), sorted by key.
/// The key is the program; the name, what the app calls itself.
pub fn calls_in(dump: &serde_json::Value) -> Vec<(String, String)> {
    #[derive(Default)]
    struct App {
        name: String,
        capture: bool,
        playback: bool,
    }
    let mut apps: BTreeMap<String, App> = BTreeMap::new();
    for node in dump.as_array().into_iter().flatten() {
        if node["type"] != "PipeWire:Interface:Node" {
            continue;
        }
        let props = &node["info"]["props"];
        let class = props["media.class"].as_str().unwrap_or("");
        let name = props["application.name"].as_str().unwrap_or("");
        let binary = props["application.process.binary"].as_str().unwrap_or(name);
        if binary.is_empty() || RECORDERS.contains(&binary) || RECORDERS.contains(&name) {
            continue;
        }
        let app = apps.entry(binary.to_owned()).or_default();
        if app.name.is_empty() {
            app.name = if name.is_empty() {
                binary.to_owned()
            } else {
                name.to_owned()
            };
        }
        match class {
            // Capturing what a sink plays is recording the computer, not a microphone.
            "Stream/Input/Audio" if props["stream.capture.sink"] != true => app.capture = true,
            "Stream/Output/Audio" => app.playback = true,
            _ => {}
        }
    }
    apps.into_iter()
        .filter(|(_, app)| app.capture && app.playback)
        .map(|(key, app)| (key, app.name))
        .collect()
}

#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    /// A call has been on for `START_AFTER_SECS`: (key, name to show).
    Started(String, String),
    /// A call announced before has been gone for `END_AFTER_SECS`.
    Ended(String, String),
}

struct Seen {
    name: String,
    since: f64,
    announced: bool,
    gone_since: Option<f64>,
}

/// Turns the calls seen at each look into calls starting and ending.
#[derive(Default)]
pub struct Tracker {
    seen: HashMap<String, Seen>,
}

impl Tracker {
    /// `now` in seconds on any steady clock; `calls` as `calls_in` gives them.
    pub fn update(&mut self, now: f64, calls: &[(String, String)]) -> Vec<Change> {
        let mut changes = Vec::new();
        for (key, name) in calls {
            let seen = self.seen.entry(key.clone()).or_insert_with(|| Seen {
                name: name.clone(),
                since: now,
                announced: false,
                gone_since: None,
            });
            seen.gone_since = None;
            if !seen.announced && now - seen.since >= START_AFTER_SECS {
                seen.announced = true;
                changes.push(Change::Started(key.clone(), seen.name.clone()));
            }
        }
        self.seen.retain(|key, seen| {
            if calls.iter().any(|(k, _)| k == key) {
                return true;
            }
            if !seen.announced {
                return false;
            }
            let gone_since = *seen.gone_since.get_or_insert(now);
            if now - gone_since >= END_AFTER_SECS {
                changes.push(Change::Ended(key.clone(), seen.name.clone()));
                return false;
            }
            true
        });
        changes.sort_by_key(|c| match c {
            Change::Started(k, _) | Change::Ended(k, _) => k.clone(),
        });
        changes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> serde_json::Value {
        let path = format!(
            "{}/tests/fixtures/pipewire/{name}.json",
            env!("CARGO_MANIFEST_DIR")
        );
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
    }

    fn teams() -> Vec<(String, String)> {
        vec![("teams-for-linux".into(), "teams-for-linux".into())]
    }

    #[test]
    fn a_teams_call_is_a_call_and_minutes_is_not() {
        assert_eq!(calls_in(&fixture("teams-call")), teams());
    }

    #[test]
    fn minutes_recording_on_its_own_is_no_call() {
        assert!(calls_in(&fixture("minutes-only")).is_empty());
    }

    #[test]
    fn a_video_or_a_voice_memo_is_no_call() {
        assert!(calls_in(&fixture("video")).is_empty());
        assert!(calls_in(&fixture("voice-memo")).is_empty());
    }

    #[test]
    fn a_call_counts_after_ten_seconds_and_once() {
        let mut tracker = Tracker::default();
        assert!(tracker.update(0.0, &teams()).is_empty());
        assert!(tracker.update(9.0, &teams()).is_empty());
        assert_eq!(
            tracker.update(10.0, &teams()),
            [Change::Started(
                "teams-for-linux".into(),
                "teams-for-linux".into()
            )]
        );
        assert!(tracker.update(13.0, &teams()).is_empty());
    }

    #[test]
    fn a_short_blip_is_never_announced() {
        let mut tracker = Tracker::default();
        tracker.update(0.0, &teams());
        tracker.update(5.0, &teams());
        assert!(tracker.update(6.0, &[]).is_empty());
        // Coming back later starts the count again.
        assert!(tracker.update(20.0, &teams()).is_empty());
        assert_eq!(tracker.update(30.0, &teams()).len(), 1);
    }

    #[test]
    fn a_call_ends_after_fifteen_seconds_gone_not_on_a_mute() {
        let mut tracker = Tracker::default();
        tracker.update(0.0, &teams());
        tracker.update(10.0, &teams());
        assert!(tracker.update(20.0, &[]).is_empty(), "gone for a moment");
        assert!(
            tracker.update(30.0, &teams()).is_empty(),
            "back: still the same call"
        );
        assert!(tracker.update(40.0, &[]).is_empty());
        assert_eq!(
            tracker.update(55.0, &[]),
            [Change::Ended(
                "teams-for-linux".into(),
                "teams-for-linux".into()
            )]
        );
        assert!(tracker.update(70.0, &[]).is_empty());
    }
}
