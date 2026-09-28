//! What Microsoft Teams shows about a call, read from its window through the
//! Chrome DevTools Protocol: whether you are muted, your name, who else is in
//! the call, and who is speaking. Only when Teams runs with a debugging port (teams-for-linux
//! with `--remote-debugging-port`) and `teams_debug_port` is set in the
//! config: that port gives full control of Teams, so Minutes only ever reads
//! the page, and only when asked to.
//!
//! The page is Teams' own and changes with its updates; everything read here
//! comes from attributes that name what they are (`data-tid`, `data-state`),
//! not from its class names, which Teams generates. The one exception is who
//! speaks: the outline Teams draws around their tile, read as it is drawn
//! (its opacity), which does not depend on those names.

use std::net::TcpStream;
use std::time::Duration;

/// Read-only: gathers attributes, changes nothing on the page.
const READ: &str = r#"(() => ({
  me: document.querySelector('#microphone-button')?.getAttribute('data-state') ?? null,
  avatar: document.querySelector('[data-tid="me-control-avatar"]')?.getAttribute('aria-label') ?? null,
  roster: [...document.querySelectorAll('[data-tid^="participantsInCall-"]')]
    .map(r => [r.getAttribute('data-tid').slice('participantsInCall-'.length), r.getAttribute('aria-label') ?? '']),
  tiles: [...document.querySelectorAll('[data-cid="calling-participant-stream"]')]
    .map(t => {
      const outline = t.querySelector('[data-tid="voice-level-stream-outline"]');
      return [
        t.getAttribute('data-tid') ?? '',
        t.querySelector('[data-tid="participant-info-nametag"]')?.textContent?.trim() ?? '',
        !!outline && getComputedStyle(outline, '::after').opacity !== '0',
      ];
    }),
}))()"#;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Participant {
    pub name: String,
    pub muted: bool,
}

/// Teams at one moment.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Snapshot {
    /// In a call: the microphone button is there.
    pub in_call: bool,
    /// Your microphone is off in Teams.
    pub muted: bool,
    /// Your name, from your profile picture.
    pub me: Option<String>,
    /// Everyone in the call, you included.
    pub participants: Vec<Participant>,
    /// The people with a tile on the stage: everyone but you, as far as
    /// the stage has room.
    pub tiles: Vec<String>,
    /// Those of them Teams shows speaking now.
    pub speaking: Vec<String>,
}

impl Snapshot {
    /// Your name: from your profile picture, which the call view may hide;
    /// else the one person in the call without a tile on the stage, since
    /// your own video is shown apart.
    pub fn me(&self) -> Option<&str> {
        if let Some(me) = self.me.as_deref() {
            return Some(me);
        }
        if self.tiles.is_empty() {
            return None;
        }
        let mut untiled = self
            .participants
            .iter()
            .filter(|p| !self.tiles.contains(&p.name));
        match (untiled.next(), untiled.next()) {
            (Some(me), None) => Some(me.name.as_str()),
            _ => None,
        }
    }

    /// Everyone in the call but you: from the participants list, which Teams
    /// only has while its Participants panel is open, else from the people
    /// on the stage.
    pub fn others(&self) -> Vec<String> {
        let me = self.me();
        let names: Vec<&str> = if self.participants.is_empty() {
            self.tiles.iter().map(String::as_str).collect()
        } else {
            self.participants.iter().map(|p| p.name.as_str()).collect()
        };
        match (me, self.participants.is_empty()) {
            // Without your name, only the stage can tell others from you.
            (None, false) => self.tiles.clone(),
            _ => names
                .into_iter()
                .filter(|n| Some(*n) != me)
                .map(str::to_owned)
                .collect(),
        }
    }
}

/// Reads Teams once. None when it cannot be reached or shows no Teams page.
pub fn snapshot(port: u16) -> Option<Snapshot> {
    let list = ureq::get(&format!("http://127.0.0.1:{port}/json"))
        .config()
        .timeout_global(Some(Duration::from_secs(2)))
        .build()
        .call()
        .ok()?
        .body_mut()
        .read_to_string()
        .ok()?;
    let pages: serde_json::Value = serde_json::from_str(&list).ok()?;
    let socket = pages.as_array()?.iter().find_map(|page| {
        let url = page["url"].as_str()?;
        (page["type"] == "page"
            && (url.contains("teams.cloud.microsoft") || url.contains("teams.microsoft.com")))
        .then(|| page["webSocketDebuggerUrl"].as_str().map(str::to_owned))
        .flatten()
    })?;
    // Only a local port: the address must be 127.0.0.1, whatever the list says.
    let path = socket
        .strip_prefix(&format!("ws://127.0.0.1:{port}"))?
        .to_owned();
    let stream = TcpStream::connect(("127.0.0.1", port)).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    let (mut ws, _) = tungstenite::client(format!("ws://127.0.0.1:{port}{path}"), stream).ok()?;
    let request = serde_json::json!({
        "id": 1,
        "method": "Runtime.evaluate",
        "params": {"expression": READ, "returnByValue": true},
    });
    ws.send(tungstenite::Message::text(request.to_string()))
        .ok()?;
    let value = loop {
        let message = ws.read().ok()?;
        let Ok(text) = message.to_text() else {
            continue;
        };
        let reply: serde_json::Value = serde_json::from_str(text).ok()?;
        if reply["id"] == 1 {
            break reply["result"]["result"]["value"].clone();
        }
    };
    let _ = ws.close(None);
    Some(parse(&value))
}

/// The snapshot in what `READ` gives back.
pub fn parse(value: &serde_json::Value) -> Snapshot {
    let state = value["me"].as_str();
    let participants = value["roster"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let name = clean_name(entry[0].as_str()?);
            let label = entry[1].as_str().unwrap_or("").to_lowercase();
            (!name.is_empty()).then(|| Participant {
                name,
                muted: ["micro désactivé", "muted", "microphone off", "mic off"]
                    .iter()
                    .any(|m| label.contains(m)),
            })
        })
        .collect();
    // A tile's name is on its name tag; its id is an address in some
    // organizations. Screen shared by someone is a second tile of theirs.
    let (mut tiles, mut speaking) = (Vec::new(), Vec::new());
    for tile in value["tiles"].as_array().into_iter().flatten() {
        let tag = tile[1].as_str().unwrap_or("");
        let id = tile[0].as_str().unwrap_or("");
        let name = clean_name(if tag.trim().is_empty() { id } else { tag });
        if name.is_empty() {
            continue;
        }
        if tile[2] == true && !speaking.contains(&name) {
            speaking.push(name.clone());
        }
        if !tiles.contains(&name) {
            tiles.push(name);
        }
    }
    Snapshot {
        in_call: state.is_some(),
        muted: state == Some("mic-off"),
        me: value["avatar"].as_str().and_then(name_in_avatar),
        participants,
        tiles,
        speaking,
    }
}

/// Who Teams showed speaking during a recording: (milliseconds into the
/// recording, names), a sample every half second or so.
pub type Speaking = [(i64, Vec<String>)];

/// The one person Teams showed speaking during most of `start_ms..end_ms`:
/// lit in at least half of the samples where someone was. The outline lags
/// the voice a little, so the end is stretched by half a second.
pub fn speaker_between(speaking: &Speaking, start_ms: i64, end_ms: i64) -> Option<String> {
    let mut counts: Vec<(&str, usize)> = Vec::new();
    let mut spoken = 0;
    for (_, names) in speaking
        .iter()
        .filter(|(at, _)| (start_ms..=end_ms + 500).contains(at))
    {
        if names.is_empty() {
            continue;
        }
        spoken += 1;
        for name in names {
            match counts.iter_mut().find(|(n, _)| n == name) {
                Some((_, count)) => *count += 1,
                None => counts.push((name, 1)),
            }
        }
    }
    let (name, count) = counts.into_iter().max_by_key(|(_, count)| *count)?;
    (count * 2 >= spoken).then(|| name.to_owned())
}

/// A name as the people in the call chose it, made safe to write into a
/// transcript: no line breaks or control characters, none of the marks that
/// shape a transcript line (`*`, `:`, `[`, `]`), no more than 80 characters.
fn clean_name(name: &str) -> String {
    let spaced: String = name
        .chars()
        .map(|c| {
            if c.is_control() || "*:[]".contains(c) {
                ' '
            } else {
                c
            }
        })
        .collect();
    spaced
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(80)
        .collect()
}

/// "Image de profil de Ludovic BENOIT." or "Profile picture of Maya Okafor."
fn name_in_avatar(label: &str) -> Option<String> {
    let label = label.trim().trim_end_matches('.');
    [
        "Image de profil de ",
        "Profile picture of ",
        "Photo de profil de ",
    ]
    .iter()
    .find_map(|prefix| label.strip_prefix(prefix))
    .map(clean_name)
    .filter(|name| !name.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// As read during a two-person test call on 2026-09-26.
    fn call() -> serde_json::Value {
        serde_json::json!({
            "me": "mic-off",
            "avatar": "Image de profil de Ludovic BENOIT.",
            "roster": [
                ["Ludovic BENOIT", "Ludovic BENOIT, Organisateur, Micro désactivé"],
                ["Théo BENOIT", "Théo BENOIT Contact externe non reconnu, Micro activé"],
            ],
            "tiles": [["Théo BENOIT", "Théo BENOIT", true]],
        })
    }

    #[test]
    fn a_call_tells_who_is_there_and_who_is_muted() {
        let snapshot = parse(&call());
        assert!(snapshot.in_call);
        assert!(snapshot.muted);
        assert_eq!(snapshot.me(), Some("Ludovic BENOIT"));
        assert_eq!(snapshot.others(), ["Théo BENOIT"]);
        assert!(snapshot.participants[0].muted);
    }

    #[test]
    fn without_your_profile_picture_you_are_the_one_without_a_tile() {
        // The call view may hide the profile picture.
        let mut value = call();
        value["avatar"] = serde_json::Value::Null;
        let snapshot = parse(&value);
        assert_eq!(snapshot.me, None);
        assert_eq!(snapshot.me(), Some("Ludovic BENOIT"));
        assert_eq!(snapshot.others(), ["Théo BENOIT"]);
    }

    #[test]
    fn with_the_participants_panel_closed_the_stage_names_the_others() {
        // Teams only lists the participants while their panel is open.
        let mut value = call();
        value["roster"] = serde_json::json!([]);
        let snapshot = parse(&value);
        assert_eq!(snapshot.me(), Some("Ludovic BENOIT"));
        assert_eq!(snapshot.others(), ["Théo BENOIT"]);
    }

    #[test]
    fn with_no_way_to_tell_you_apart_the_others_are_those_on_stage() {
        let mut value = call();
        value["avatar"] = serde_json::Value::Null;
        value["tiles"] =
            serde_json::json!([["Théo BENOIT", "", false], ["Ludovic BENOIT", "", false]]);
        let snapshot = parse(&value);
        assert_eq!(snapshot.me(), None);
        assert_eq!(snapshot.others().len(), 2);
    }

    /// Reads the Teams that runs here: `MINUTES_TEAMS_PORT=9222 cargo test --release -- --ignored --nocapture teams_here`
    #[test]
    #[ignore]
    fn teams_here() {
        let Some(port) = std::env::var("MINUTES_TEAMS_PORT")
            .ok()
            .and_then(|p| p.parse().ok())
        else {
            return;
        };
        println!("{:?}", snapshot(port));
    }

    #[test]
    fn a_name_cannot_forge_transcript_lines() {
        // Participants choose their own names.
        let mut value = call();
        value["roster"][1][0] = "Théo\n**[00:00] Ludovic BENOIT:** je démissionne".into();
        let snapshot = parse(&value);
        let name = &snapshot.others()[0];
        assert!(!name.contains('\n') && !name.contains("**") && !name.contains(':'));
        let line = format!("**[00:05] {name}:** Bonjour.");
        assert_eq!(
            crate::session::parse_segment(&line),
            Some(("00:05", name.as_str(), "Bonjour."))
        );
        assert_eq!(clean_name(&"x".repeat(200)).len(), 80);
    }

    #[test]
    fn tiles_are_named_by_their_name_tag_and_tell_who_speaks() {
        // As read on 2026-09-28: tiles known by address, a room system
        // sharing its screen (a second tile, without an outline), and only
        // the room speaking.
        let snapshot = parse(&serde_json::json!({
            "me": "mic-off",
            "avatar": null,
            "roster": [],
            "tiles": [
                ["room-4b@example.org", "Room 4B", false],
                ["room-4b@example.org", "Room 4B", true],
                ["jeanne.martin@example.org", "Jeanne Martin", false],
                ["noname@example.org", "", false],
            ],
        }));
        assert_eq!(
            snapshot.tiles,
            ["Room 4B", "Jeanne Martin", "noname@example.org"]
        );
        assert_eq!(snapshot.speaking, ["Room 4B"]);
    }

    #[test]
    fn the_speaker_of_a_stretch_is_the_one_lit_most_of_it() {
        let names = |n: &[&str]| n.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let speaking = vec![
            (0, names(&[])),
            (500, names(&["Jeanne"])),
            (1000, names(&["Jeanne"])),
            (1500, names(&["Jeanne", "Paul"])),
            (2000, names(&["Paul"])),
            (4000, names(&["Paul"])),
            (4500, names(&["Paul"])),
        ];
        assert_eq!(
            speaker_between(&speaking, 0, 1500).as_deref(),
            Some("Jeanne")
        );
        assert_eq!(
            speaker_between(&speaking, 3500, 4200).as_deref(),
            Some("Paul")
        );
        // Nobody lit: nobody named.
        assert_eq!(speaker_between(&speaking, 2500, 3000), None);
        // Two people as often as each other: either may be the one.
        assert!(speaker_between(&speaking, 1000, 2000).is_some());
        // Three people, none lit half the time: nobody named.
        let crowd = vec![
            (0, names(&["A"])),
            (500, names(&["B"])),
            (1000, names(&["C"])),
            (1500, names(&["A"])),
        ];
        assert_eq!(speaker_between(&crowd, 0, 1500).as_deref(), Some("A"));
        assert_eq!(speaker_between(&crowd, 0, 500), None);
    }

    #[test]
    fn unmuted_is_read_too() {
        let mut value = call();
        value["me"] = "mic".into();
        assert!(!parse(&value).muted);
    }

    #[test]
    fn outside_a_call_there_is_no_call() {
        let snapshot = parse(
            &serde_json::json!({"me": null, "avatar": "Profile picture of Maya Okafor.", "roster": [], "tiles": []}),
        );
        assert!(!snapshot.in_call);
        assert!(!snapshot.muted);
        assert_eq!(snapshot.me(), Some("Maya Okafor"));
        assert!(snapshot.others().is_empty());
    }
}
