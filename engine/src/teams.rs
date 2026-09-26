//! What Microsoft Teams shows about a call, read from its window through the
//! Chrome DevTools Protocol: whether you are muted, your name, and who else
//! is in the call. Only when Teams runs with a debugging port (teams-for-linux
//! with `--remote-debugging-port`) and `teams_debug_port` is set in the
//! config: that port gives full control of Teams, so Minutes only ever reads
//! the page, and only when asked to.
//!
//! The page is Teams' own and changes with its updates; everything read here
//! comes from attributes that name what they are (`data-tid`, `data-state`),
//! not from its styling.

use std::net::TcpStream;
use std::time::Duration;

/// Read-only: gathers attributes, changes nothing on the page.
const READ: &str = r#"(() => ({
  me: document.querySelector('#microphone-button')?.getAttribute('data-state') ?? null,
  avatar: document.querySelector('[data-tid="me-control-avatar"]')?.getAttribute('aria-label') ?? null,
  roster: [...document.querySelectorAll('[data-tid^="participantsInCall-"]')]
    .map(r => [r.getAttribute('data-tid').slice('participantsInCall-'.length), r.getAttribute('aria-label') ?? '']),
  tiles: [...document.querySelectorAll('[data-cid="calling-participant-stream"]')]
    .map(t => [t.getAttribute('data-tid') ?? '', t.querySelector('[data-tid="voice-level-stream-outline"]')?.getAttribute('class') ?? null]),
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
}

impl Snapshot {
    /// Everyone in the call but you.
    pub fn others(&self) -> Vec<&Participant> {
        self.participants
            .iter()
            .filter(|p| self.me.as_deref() != Some(p.name.as_str()))
            .collect()
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
        (page["type"] == "page" && (url.contains("teams.cloud.microsoft") || url.contains("teams.microsoft.com")))
            .then(|| page["webSocketDebuggerUrl"].as_str().map(str::to_owned))
            .flatten()
    })?;
    // Only a local port: the address must be 127.0.0.1, whatever the list says.
    let path = socket.strip_prefix(&format!("ws://127.0.0.1:{port}"))?.to_owned();
    let stream = TcpStream::connect(("127.0.0.1", port)).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    let (mut ws, _) = tungstenite::client(format!("ws://127.0.0.1:{port}{path}"), stream).ok()?;
    let request = serde_json::json!({
        "id": 1,
        "method": "Runtime.evaluate",
        "params": {"expression": READ, "returnByValue": true},
    });
    ws.send(tungstenite::Message::text(request.to_string())).ok()?;
    let value = loop {
        let message = ws.read().ok()?;
        let Ok(text) = message.to_text() else { continue };
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
            let name = entry[0].as_str()?.trim();
            let label = entry[1].as_str().unwrap_or("").to_lowercase();
            (!name.is_empty()).then(|| Participant {
                name: name.to_owned(),
                muted: ["micro désactivé", "muted", "microphone off", "mic off"].iter().any(|m| label.contains(m)),
            })
        })
        .collect();
    Snapshot {
        in_call: state.is_some(),
        muted: state == Some("mic-off"),
        me: value["avatar"].as_str().and_then(name_in_avatar),
        participants,
    }
}

/// "Image de profil de Ludovic BENOIT." or "Profile picture of Maya Okafor."
fn name_in_avatar(label: &str) -> Option<String> {
    let label = label.trim().trim_end_matches('.');
    ["Image de profil de ", "Profile picture of ", "Photo de profil de "]
        .iter()
        .find_map(|prefix| label.strip_prefix(prefix))
        .map(|name| name.trim().to_owned())
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
            "tiles": [["Théo BENOIT", "___1884sgo f9pox2d f11ef69 f1ac6l4y fs357bs"]],
        })
    }

    #[test]
    fn a_call_tells_who_is_there_and_who_is_muted() {
        let snapshot = parse(&call());
        assert!(snapshot.in_call);
        assert!(snapshot.muted);
        assert_eq!(snapshot.me.as_deref(), Some("Ludovic BENOIT"));
        assert_eq!(
            snapshot.others(),
            [&Participant { name: "Théo BENOIT".into(), muted: false }]
        );
        assert!(snapshot.participants[0].muted);
    }

    /// Reads the Teams that runs here: `MINUTES_TEAMS_PORT=9222 cargo test --release -- --ignored --nocapture teams_here`
    #[test]
    #[ignore]
    fn teams_here() {
        let Some(port) = std::env::var("MINUTES_TEAMS_PORT").ok().and_then(|p| p.parse().ok()) else {
            return;
        };
        println!("{:?}", snapshot(port));
    }

    #[test]
    fn unmuted_is_read_too() {
        let mut value = call();
        value["me"] = "mic".into();
        assert!(!parse(&value).muted);
    }

    #[test]
    fn outside_a_call_there_is_no_call() {
        let snapshot = parse(&serde_json::json!({"me": null, "avatar": "Profile picture of Maya Okafor.", "roster": [], "tiles": []}));
        assert!(!snapshot.in_call);
        assert!(!snapshot.muted);
        assert_eq!(snapshot.me.as_deref(), Some("Maya Okafor"));
        assert!(snapshot.others().is_empty());
    }
}
