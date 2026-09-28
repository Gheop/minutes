//! The main window: the meetings on the left; on the right, getting ready,
//! recording, writing the transcript, and reading it.

use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use adw::prelude::*;
use adw::subclass::prelude::*;
use gettextrs::gettext;
use gtk::{gio, glib};
use minutes_engine::audio::{self, Source};
use minutes_engine::calls::{self, Change, Tracker};
use minutes_engine::export::Format;
use minutes_engine::live::{Preview, Update};
use minutes_engine::meeting;
use minutes_engine::session::{self, Note};
use minutes_engine::teams;
use minutes_engine::transcribe::{self, Abort, CANCELLED, Event, LANGUAGES, Segment, Transcript};

/// A recording in progress.
pub struct Recording {
    staging: PathBuf,
    note: Note,
    /// Recorded time before the current stretch, pauses left out.
    before: Duration,
    /// When the current stretch started; None while paused.
    since: Option<Instant>,
}

/// What Teams showed during a recording.
#[derive(Default)]
pub struct TeamsSeen {
    /// You are muted in Teams now.
    pub muted: bool,
    /// Your name in Teams.
    pub me: Option<String>,
    /// Everyone else seen in the call.
    pub others: BTreeSet<String>,
    /// Teams said at its last read that you are in a call.
    pub in_call: bool,
}

impl TeamsSeen {
    /// The name of the other side, when a single person was there.
    fn other(&self) -> Option<&str> {
        (self.others.len() == 1)
            .then(|| self.others.iter().next().map(String::as_str))
            .flatten()
    }
}

impl Recording {
    fn elapsed(&self) -> Duration {
        self.before + self.since.map_or(Duration::ZERO, |s| s.elapsed())
    }
}

mod imp {
    use super::*;

    #[derive(Default, gtk::CompositeTemplate)]
    #[template(file = "window.ui")]
    pub struct MinutesWindow {
        #[template_child]
        pub toasts: TemplateChild<adw::ToastOverlay>,
        #[template_child]
        pub split_view: TemplateChild<adw::NavigationSplitView>,
        #[template_child]
        pub sidebar_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub meetings: TemplateChild<gtk::ListBox>,
        #[template_child]
        pub content_page: TemplateChild<adw::NavigationPage>,
        #[template_child]
        pub copy_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub recovery_banner: TemplateChild<adw::Banner>,
        #[template_child]
        pub pages: TemplateChild<gtk::Stack>,
        #[template_child]
        pub title_row: TemplateChild<adw::EntryRow>,
        #[template_child]
        pub language_row: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub ready_mic_level: TemplateChild<gtk::LevelBar>,
        #[template_child]
        pub ready_system_level: TemplateChild<gtk::LevelBar>,
        #[template_child]
        pub mic_level: TemplateChild<gtk::LevelBar>,
        #[template_child]
        pub system_level: TemplateChild<gtk::LevelBar>,
        #[template_child]
        pub recording_page: TemplateChild<adw::StatusPage>,
        #[template_child]
        pub pause_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub mute_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub transcribing_page: TemplateChild<adw::StatusPage>,
        #[template_child]
        pub progress: TemplateChild<gtk::ProgressBar>,
        #[template_child]
        pub transcript: TemplateChild<gtk::ListBox>,
        #[template_child]
        pub live_group: TemplateChild<adw::PreferencesGroup>,
        #[template_child]
        pub live_scroller: TemplateChild<gtk::ScrolledWindow>,
        #[template_child]
        pub live_list: TemplateChild<gtk::ListBox>,
        #[template_child]
        pub preview_actions: TemplateChild<gtk::Box>,

        /// The mic and the computer audio, listened to while the window is
        /// shown (the meters show that both arrive before anything is
        /// recorded) or a recording goes on; closed otherwise, so the
        /// microphone is not in use while Minutes waits in the background.
        pub sources: RefCell<Option<(Source, Source)>>,
        pub recording: RefCell<Option<Recording>>,
        pub abort: RefCell<Option<Abort>>,
        /// Where the transcript's progress arrives; Cancel ends the wait
        /// through it, even when whisper is stuck where it cannot see `abort`.
        pub progress_events: RefCell<Option<async_channel::Sender<Event>>>,
        /// The transcript on screen, for Copy.
        pub markdown: RefCell<String>,
        /// Calls seen in the PipeWire graph, to offer recording them.
        pub calls: RefCell<Tracker>,
        /// The preview written while recording, and its lines so far.
        pub preview: RefCell<Option<Preview>>,
        pub live: RefCell<Vec<Segment>>,
        /// The sentence still being said on each side: (speaker, start in ms,
        /// text, its row), always at the bottom of the preview.
        pub drafts: RefCell<Vec<(&'static str, i64, String, gtk::ListBoxRow)>>,
        /// The preview saved at Stop, for Copy and Open.
        pub preview_file: RefCell<Option<PathBuf>>,
        /// Your microphone muted with the Mute button.
        pub muted_here: Cell<bool>,
        /// Your microphone has sent nothing for a while during the recording.
        pub mic_silent: Cell<bool>,
        /// What Teams showed during this recording.
        pub teams: RefCell<TeamsSeen>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MinutesWindow {
        const NAME: &'static str = "MinutesWindow";
        type Type = super::MinutesWindow;
        type ParentType = adw::ApplicationWindow;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for MinutesWindow {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().setup();
        }
    }

    impl WidgetImpl for MinutesWindow {}
    impl WindowImpl for MinutesWindow {
        fn close_request(&self) -> glib::Propagation {
            // Closing while recording would lose nothing (the raw audio stays
            // for recovery), but it would surprise: stop first.
            if self.recording.borrow().is_some() || self.abort.borrow().is_some() {
                self.obj()
                    .toast(&gettext("Stop the recording before closing"));
                return glib::Propagation::Stop;
            }
            // In the background, Minutes stays to notice calls; the window
            // only goes out of sight, and the microphone is let go.
            if crate::BACKGROUND.load(Ordering::Relaxed) {
                self.obj().set_visible(false);
                self.obj().release_sources();
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        }
    }
    impl ApplicationWindowImpl for MinutesWindow {}
    impl AdwApplicationWindowImpl for MinutesWindow {}
}

glib::wrapper! {
    pub struct MinutesWindow(ObjectSubclass<imp::MinutesWindow>)
        @extends adw::ApplicationWindow, gtk::ApplicationWindow, gtk::Window, gtk::Widget,
        @implements gio::ActionGroup, gio::ActionMap, gtk::Accessible, gtk::Buildable,
                    gtk::ConstraintTarget, gtk::Native, gtk::Root, gtk::ShortcutManager;
}

impl MinutesWindow {
    pub fn new(app: &adw::Application) -> Self {
        glib::Object::builder().property("application", app).build()
    }

    fn setup(&self) {
        let imp = self.imp();
        let names: Vec<String> = LANGUAGES
            .iter()
            .map(|(code, _)| language_name(code))
            .collect();
        imp.language_row.set_model(Some(&gtk::StringList::new(
            &names.iter().map(String::as_str).collect::<Vec<_>>(),
        )));

        let action = |name: &str, run: fn(&MinutesWindow)| {
            gio::ActionEntry::builder(name)
                .activate(move |win: &MinutesWindow, _, _| run(win))
                .build()
        };
        self.add_action_entries([
            action("record", Self::record),
            action("pause", Self::toggle_pause),
            action("stop", Self::stop),
            action("cancel", Self::cancel),
            action("copy", Self::copy),
            action("new", Self::show_ready),
            action("open-folder", Self::open_folder),
            action("copy-preview", Self::copy_preview),
            action("mute-mic", Self::toggle_mute),
            action("recover", Self::ask_recovery),
            action("open-preview", Self::open_preview),
        ]);
        self.set_busy(false);

        self.connect_map(|win| {
            win.sources();
        });
        let weak = self.downgrade();
        glib::timeout_add_local(Duration::from_millis(50), move || {
            let Some(win) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            win.tick();
            glib::ControlFlow::Continue
        });

        let weak = self.downgrade();
        imp.meetings.connect_row_activated(move |_, row| {
            if let Some(win) = weak.upgrade()
                && let Some(dir) = unsafe { row.data::<PathBuf>("dir") }
            {
                let dir = unsafe { dir.as_ref() }.clone();
                win.show_meeting(&dir);
            }
        });
        self.load_meetings();
        self.offer_recovery();
        self.watch_calls();
    }

    /// Looks at the PipeWire graph every few seconds for an app in a call,
    /// and offers to record it; offers to stop when a call being recorded ends.
    fn watch_calls(&self) {
        let weak = self.downgrade();
        glib::spawn_future_local(async move {
            loop {
                let dump = gio::spawn_blocking(|| {
                    let out = std::process::Command::new("pw-dump").output().ok()?;
                    serde_json::from_slice::<serde_json::Value>(&out.stdout).ok()
                })
                .await
                .ok()
                .flatten();
                let Some(win) = weak.upgrade() else {
                    return;
                };
                let Some(dump) = dump else {
                    minutes_engine::warn(
                        "pw-dump is missing or failed; calls will not be detected",
                    );
                    return;
                };
                let now = glib::monotonic_time() as f64 / 1e6;
                let changes = win
                    .imp()
                    .calls
                    .borrow_mut()
                    .update(now, &calls::calls_in(&dump));
                for change in changes {
                    win.call_changed(change);
                }
                drop(win);
                glib::timeout_future_seconds(3).await;
            }
        });
    }

    /// The call being recorded has ended: stop, and say so. `why` goes to
    /// the journal, so a recording stopped too soon can be explained.
    fn call_over(&self, name: &str, why: &str) {
        let Some(app) = self.application() else {
            return;
        };
        if self.imp().recording.borrow().is_none() {
            return;
        }
        minutes_engine::warn(format!("recording stopped on its own: {why}"));
        self.stop();
        let notification = gio::Notification::new(&gettext("Recording stopped"));
        notification.set_body(Some(
            &gettext("The call in %s ended; the transcript is being written.").replace("%s", name),
        ));
        app.send_notification(Some("call"), &notification);
    }

    fn call_changed(&self, change: Change) {
        let Some(app) = self.application() else {
            return;
        };
        let recording = self.imp().recording.borrow().is_some();
        match change {
            Change::Started(_, name) if !recording => {
                let notification =
                    gio::Notification::new(&gettext("Call in %s").replace("%s", &name));
                notification.set_body(Some(&gettext(
                    "Record it with Minutes? Tell the others first.",
                )));
                notification.add_button(&gettext("Record"), "app.record");
                app.send_notification(Some("call"), &notification);
            }
            // A call waiting in silence, or muted, can close its audio for a
            // while: when Teams says the call goes on, it goes on.
            Change::Ended(_, name) if recording && self.imp().teams.borrow().in_call => {
                minutes_engine::warn(format!(
                    "{name} closed its audio, but Teams is still in the call: recording goes on"
                ));
            }
            Change::Ended(_, name) if recording => {
                self.call_over(&name, &format!("{name} had no call audio for 15 s"))
            }
            Change::Ended(..) => app.withdraw_notification("call"),
            Change::Started(..) => {}
        }
    }

    /// Tells the outside (the Shell extension, over D-Bus) what Minutes is
    /// doing: `app.status` holds (state, seconds, progress). While recording,
    /// the seconds are the Unix time the recording would have started without
    /// its pauses, so a clock can run from it; while paused, the time recorded.
    fn publish(&self, state: &str, seconds: i64, progress: f64) {
        let Some(action) = self
            .application()
            .and_then(|app| app.lookup_action("status"))
            .and_downcast::<gio::SimpleAction>()
        else {
            return;
        };
        let status = (state, seconds, progress).to_variant();
        if action.state().as_ref() != Some(&status) {
            action.set_state(&status);
        }
    }

    /// The mic and the computer audio, started when they are not listened to yet.
    fn sources(&self) -> (Source, Source) {
        self.imp()
            .sources
            .borrow_mut()
            .get_or_insert_with(|| {
                (
                    Source::spawn("@DEFAULT_SOURCE@"),
                    Source::spawn("@DEFAULT_MONITOR@"),
                )
            })
            .clone()
    }

    /// Lets the microphone and the computer audio go, unless a recording needs them.
    fn release_sources(&self) {
        if self.imp().recording.borrow().is_some() {
            return;
        }
        if let Some((mic, system)) = self.imp().sources.borrow_mut().take() {
            mic.stop();
            system.stop();
        }
    }

    fn toast(&self, message: &str) {
        self.imp().toasts.add_toast(adw::Toast::new(message));
    }

    /// Recording or transcribing: nothing else may start meanwhile.
    fn set_busy(&self, busy: bool) {
        let recording = self.imp().recording.borrow().is_some();
        let transcribing = self.imp().abort.borrow().is_some();
        for (name, enabled) in [
            ("record", !busy),
            ("new", !busy),
            ("recover", !busy),
            ("pause", recording),
            ("stop", recording),
            ("mute-mic", recording),
            ("cancel", transcribing),
        ] {
            if let Some(action) = self.lookup_action(name).and_downcast::<gio::SimpleAction>() {
                action.set_enabled(enabled);
            }
        }
        self.imp().meetings.set_sensitive(!busy);
    }

    /// The meters and the clock, twenty times a second.
    fn tick(&self) {
        let imp = self.imp();
        let Some((mic, system)) = imp.sources.borrow().clone() else {
            return;
        };
        let (mic, system) = (
            audio::to_meter(mic.recent_peak(3)),
            audio::to_meter(system.recent_peak(3)),
        );
        for (bar, value) in [
            (&imp.ready_mic_level, mic),
            (&imp.mic_level, mic),
            (&imp.ready_system_level, system),
            (&imp.system_level, system),
        ] {
            bar.set_value(value);
        }
        if let Some(recording) = imp.recording.borrow().as_ref() {
            imp.recording_page
                .set_title(&clock(recording.elapsed().as_secs()));
        }
        // A headset asleep or taken off sends nothing at all; say so while
        // recording, since the track then only gets silence.
        let silent = imp.recording.borrow().is_some()
            && imp
                .sources
                .borrow()
                .as_ref()
                .is_some_and(|(mic, _)| mic.silent_for() > Duration::from_secs(5));
        if silent != imp.mic_silent.get() {
            imp.mic_silent.set(silent);
            self.describe_recording();
        }
    }

    fn show_ready(&self) {
        let imp = self.imp();
        imp.pages.set_visible_child_name("ready");
        imp.content_page.set_title(&gettext("New Recording"));
        imp.copy_button.set_visible(false);
        imp.meetings.unselect_all();
        imp.split_view.set_show_content(true);
    }

    fn language(&self) -> &'static str {
        LANGUAGES
            .get(self.imp().language_row.selected() as usize)
            .map_or("auto", |(code, _)| code)
    }

    fn record(&self) {
        let imp = self.imp();
        if imp.recording.borrow().is_some() {
            return;
        }
        let (mic, system) = self.sources();
        let now = glib::DateTime::now_local().ok();
        let title = match imp.title_row.text().trim() {
            "" => now
                .as_ref()
                .and_then(|t| t.format(&gettext("Meeting %H:%M")).ok())
                .map_or_else(|| gettext("Meeting"), |s| s.to_string()),
            typed => typed.to_owned(),
        };
        let started_at = now.map_or(0, |t| t.to_unix());
        let note = Note {
            title: title.clone(),
            started_at,
            format: Format::Mono,
            language: self.language().to_owned(),
        };
        let staging = session::staging_root().join(started_at.to_string());
        let (mic_raw, system_raw) = session::raw_tracks(&staging);
        let started = session::private_dir(&session::staging_root(), &staging)
            .and_then(|_| mic.start_recording(&mic_raw))
            .and_then(|_| system.start_recording(&system_raw))
            .and_then(|_| note.write(&staging));
        if let Err(e) = started {
            mic.stop_recording();
            system.stop_recording();
            self.toast(&format!("{}: {e}", gettext("Could not start recording")));
            return;
        }
        imp.muted_here.set(false);
        // Your name carries over from one call to the next.
        let me = imp.teams.borrow().me.clone();
        *imp.teams.borrow_mut() = TeamsSeen {
            me,
            ..TeamsSeen::default()
        };
        self.apply_mute();
        self.start_preview(&staging, &note.language);
        *imp.recording.borrow_mut() = Some(Recording {
            staging,
            note,
            before: Duration::ZERO,
            since: Some(Instant::now()),
        });
        self.publish("recording", glib::real_time() / 1_000_000, 0.0);
        self.watch_teams();
        imp.content_page.set_title(&title);
        imp.pause_button.set_label(&gettext("_Pause"));
        imp.recording_page
            .set_description(Some(&gettext("Recording")));
        imp.pages.set_visible_child_name("recording");
        self.set_busy(true);
    }

    fn toggle_pause(&self) {
        let imp = self.imp();
        let Some((mic, system)) = imp.sources.borrow().clone() else {
            return;
        };
        let mut guard = imp.recording.borrow_mut();
        let Some(recording) = guard.as_mut() else {
            return;
        };
        let paused = match recording.since.take() {
            Some(since) => {
                recording.before += since.elapsed();
                true
            }
            None => {
                recording.since = Some(Instant::now());
                false
            }
        };
        mic.set_paused(paused);
        system.set_paused(paused);
        let recorded = recording.elapsed().as_secs() as i64;
        if paused {
            self.publish("paused", recorded, 0.0);
        } else {
            self.publish("recording", glib::real_time() / 1_000_000 - recorded, 0.0);
        }
        imp.pause_button.set_label(&if paused {
            gettext("_Resume")
        } else {
            gettext("_Pause")
        });
        drop(guard);
        self.describe_recording();
    }

    /// The line under the clock: paused, muted, or a microphone sending nothing.
    fn describe_recording(&self) {
        let imp = self.imp();
        let Some(paused) = imp.recording.borrow().as_ref().map(|r| r.since.is_none()) else {
            return;
        };
        let muted = imp.muted_here.get() || imp.teams.borrow().muted;
        let text = if paused {
            gettext("Paused")
        } else if imp.muted_here.get() {
            gettext("Recording, your microphone muted")
        } else if muted {
            gettext("Recording, your microphone muted as in Teams")
        } else if imp.mic_silent.get() {
            gettext("Recording, but your microphone sends nothing: a headset asleep or taken off?")
        } else {
            gettext("Recording")
        };
        imp.recording_page.set_description(Some(&text));
    }

    fn stop(&self) {
        let imp = self.imp();
        let Some(recording) = imp.recording.borrow_mut().take() else {
            return;
        };
        if let Some((mic, system)) = imp.sources.borrow().clone() {
            mic.stop_recording();
            system.stop_recording();
            mic.set_paused(false);
            system.set_paused(false);
            mic.set_muted(false);
        }
        self.publish_mute(false);
        if !self.is_visible() {
            self.release_sources();
        }
        self.write_up(recording);
    }

    /// Recordings a crash, a logout or a power cut left unfinished, oldest
    /// first; not the one being recorded now.
    fn unfinished(&self) -> Vec<PathBuf> {
        let current = self
            .imp()
            .recording
            .borrow()
            .as_ref()
            .map(|r| r.staging.clone());
        session::unfinished(&session::staging_root())
            .into_iter()
            .filter(|dir| Some(dir) != current.as_ref())
            .collect()
    }

    /// Says when a recording was left unfinished: a banner, and a
    /// notification when Minutes waits in the background.
    fn offer_recovery(&self) {
        let imp = self.imp();
        let Some(dir) = self.unfinished().into_iter().next() else {
            imp.recovery_banner.set_revealed(false);
            return;
        };
        let minutes = (session::raw_duration(&dir) + 59) / 60;
        imp.recovery_banner.set_title(
            &gettext("An interrupted recording was found (%s min)")
                .replace("%s", &minutes.to_string()),
        );
        imp.recovery_banner.set_revealed(true);
        if !self.is_visible()
            && let Some(app) = self.application()
        {
            let notification =
                gio::Notification::new(&gettext("An interrupted recording was found"));
            notification.set_body(Some(&gettext(
                "Open Minutes to write its transcript or delete it.",
            )));
            app.send_notification(Some("recovery"), &notification);
        }
    }

    fn ask_recovery(&self) {
        let Some(dir) = self.unfinished().into_iter().next() else {
            self.offer_recovery();
            return;
        };
        let note = Note::read(&dir);
        let title = note
            .as_ref()
            .map_or_else(|| gettext("Recovered Recording"), |n| n.title.clone());
        let minutes = (session::raw_duration(&dir) + 59) / 60;
        let dialog = adw::AlertDialog::new(
            Some(&gettext("Recover the interrupted recording?")),
            Some(&format!("{title}, {minutes} min")),
        );
        dialog.add_responses(&[
            ("later", &gettext("_Later")),
            ("delete", &gettext("_Delete")),
            ("recover", &gettext("_Recover")),
        ]);
        dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
        dialog.set_response_appearance("recover", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("recover"));
        dialog.set_close_response("later");
        let weak = self.downgrade();
        dialog.connect_response(None, move |_, response| {
            let Some(win) = weak.upgrade() else {
                return;
            };
            match response {
                "recover" => win.recover(dir.clone(), note.clone()),
                "delete" => {
                    let _ = std::fs::remove_dir_all(&dir);
                    win.toast(&gettext("Interrupted recording deleted"));
                    win.offer_recovery();
                }
                _ => {}
            }
        });
        dialog.present(Some(self));
    }

    /// Writes up a recording found interrupted, as if it had just been stopped.
    fn recover(&self, staging: PathBuf, note: Option<Note>) {
        let imp = self.imp();
        if imp.recording.borrow().is_some() || imp.abort.borrow().is_some() {
            self.toast(&gettext("Finish the current recording first"));
            return;
        }
        // Without its note (a crash right at the start), the folder's name
        // is the time it started.
        let note = note.unwrap_or_else(|| Note {
            title: gettext("Recovered Recording"),
            started_at: staging
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.parse().ok())
                .unwrap_or_else(|| glib::real_time() / 1_000_000),
            format: Format::Mono,
            language: "auto".into(),
        });
        imp.recovery_banner.set_revealed(false);
        imp.content_page.set_title(&note.title);
        imp.split_view.set_show_content(true);
        self.write_up(Recording {
            staging,
            note,
            before: Duration::ZERO,
            since: None,
        });
    }

    /// Saves the audio of a recording and writes its transcript, showing how
    /// far it is: after Stop, or for a recording found interrupted.
    fn write_up(&self, recording: Recording) {
        let imp = self.imp();
        let abort = Abort::default();
        *imp.abort.borrow_mut() = Some(abort.clone());
        self.set_busy(true);
        imp.progress.set_fraction(0.0);
        imp.progress.set_text(Some(&gettext("Saving the audio")));
        imp.pages.set_visible_child_name("transcribing");
        self.publish("transcribing", 0, 0.0);

        let win = self.clone();
        glib::spawn_future_local(async move {
            let result = win.finish(recording, abort).await;
            *win.imp().abort.borrow_mut() = None;
            win.set_busy(false);
            win.publish("idle", 0, 0.0);
            win.imp().preview_actions.set_visible(false);
            win.imp().live.borrow_mut().clear();
            win.imp().drafts.borrow_mut().clear();
            win.publish_live();
            win.load_meetings();
            win.offer_recovery();
            match result {
                Ok(dir) => {
                    win.show_meeting(&dir);
                    win.notify_ready(&dir);
                }
                Err(e) if e == CANCELLED => {
                    win.toast(&gettext("Transcript cancelled; the audio is kept"));
                    win.show_ready();
                }
                Err(e) => {
                    win.toast(&format!(
                        "{}: {e}",
                        gettext("Could not write the transcript")
                    ));
                    win.show_ready();
                }
            }
        });
    }

    /// Saves the audio of a stopped recording into its meeting folder and
    /// transcribes it there. Returns the folder.
    async fn finish(&self, recording: Recording, abort: Abort) -> Result<PathBuf, String> {
        let Recording { staging, note, .. } = recording;
        let out = session::meeting_dir(&session::meetings_root(), note.started_at, &note.title);
        // The preview's model has to leave the GPU before the final one comes.
        let preview = self.imp().preview.borrow_mut().take();
        let lines = gio::spawn_blocking(move || preview.map(Preview::finish).unwrap_or_default())
            .await
            .unwrap_or_default();
        if let Err(e) = session::private_dir(&session::meetings_root(), &out) {
            minutes_engine::warn(format!("could not make {}: {e}", out.display()));
        }
        self.save_preview(&out, &note, lines);
        let speakers = {
            let seen = self.imp().teams.borrow();
            [
                seen.me
                    .clone()
                    .unwrap_or_else(|| meeting::DEFAULT_YOU.to_owned()),
                seen.other().unwrap_or(meeting::DEFAULT_REMOTE).to_owned(),
            ]
        };

        let (events_tx, events_rx) = async_channel::unbounded::<Event>();
        let (done_tx, done_rx) = async_channel::bounded(1);
        *self.imp().progress_events.borrow_mut() = Some(events_tx.clone());
        let (dir, thread_abort) = (out.clone(), abort.clone());
        std::thread::spawn(move || {
            let result =
                session::write_up(&staging, &dir, &note, speakers, &events_tx, &thread_abort);
            let _ = done_tx.send_blocking(result);
            let _ = events_tx.send_blocking(Event::Finished);
        });
        let imp = self.imp();
        while let Ok(event) = events_rx.recv().await {
            match event {
                Event::Stage(stage) => imp.progress.set_text(Some(&stage_label(&stage))),
                Event::Progress(fraction) => {
                    imp.progress.set_fraction(fraction);
                    // Whole percents: enough for a label, and few D-Bus signals.
                    self.publish("transcribing", 0, (fraction * 100.0).floor() / 100.0);
                }
                Event::Segment(_) => {}
                Event::Finished => break,
            }
        }
        imp.progress_events.borrow_mut().take();
        // Cancelled while whisper was out of reach (loading the model, say):
        // the thread is left to finish or not on its own, the audio is kept.
        if abort.load(Ordering::Relaxed) && done_rx.is_empty() {
            minutes_engine::warn("transcript cancelled before whisper stopped; left running");
            return Err(CANCELLED.into());
        }
        let result = done_rx
            .recv()
            .await
            .unwrap_or_else(|_| Err("the transcription stopped unexpectedly".into()));
        result.map(|()| out)
    }

    fn cancel(&self) {
        let imp = self.imp();
        if let Some(abort) = imp.abort.borrow().as_ref() {
            abort.store(true, Ordering::Relaxed);
        }
        if let Some(events) = imp.progress_events.borrow().as_ref() {
            let _ = events.try_send(Event::Finished);
        }
    }

    /// The meeting folders, newest first.
    fn load_meetings(&self) {
        let imp = self.imp();
        imp.meetings.remove_all();
        let mut dirs: Vec<PathBuf> = std::fs::read_dir(session::meetings_root())
            .map(|entries| {
                entries
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| p.is_dir())
                    .collect()
            })
            .unwrap_or_default();
        dirs.sort();
        dirs.reverse();
        let mut shown = 0;
        for dir in dirs {
            let Some((_, manifest)) = meeting::open(&dir) else {
                continue;
            };
            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(&manifest.title))
                .subtitle(date_of(manifest.started_at))
                .activatable(true)
                .build();
            unsafe { row.set_data("dir", dir) };
            imp.meetings.append(&row);
            shown += 1;
        }
        imp.sidebar_stack
            .set_visible_child_name(if shown == 0 { "empty" } else { "list" });
    }

    pub fn show_meeting(&self, dir: &Path) {
        let imp = self.imp();
        let Some((dir, manifest)) = meeting::open(dir) else {
            self.toast(&gettext("This meeting could not be read"));
            return;
        };
        let markdown = std::fs::read_to_string(dir.join("transcript.md")).unwrap_or_default();
        imp.transcript.remove_all();
        for line in markdown.lines() {
            if let Some((time, speaker, text)) = session::parse_segment(line) {
                imp.transcript.append(&transcript_row(time, speaker, text));
            }
        }
        if imp.transcript.first_child().is_none() {
            imp.transcript.append(&transcript_row(
                "",
                "",
                &gettext("Nobody was heard in this meeting."),
            ));
        }
        *imp.markdown.borrow_mut() = markdown;
        imp.content_page.set_title(&manifest.title);
        imp.copy_button.set_visible(true);
        imp.pages.set_visible_child_name("transcript");
        imp.split_view.set_show_content(true);
    }

    /// A side's label as the preview shows it: the names Teams gave, when it did.
    fn side_name(&self, label: &str) -> String {
        let seen = self.imp().teams.borrow();
        match label {
            meeting::DEFAULT_YOU => seen.me.clone().unwrap_or_else(|| gettext("You")),
            meeting::DEFAULT_REMOTE => seen
                .other()
                .map_or_else(|| gettext("Others"), str::to_owned),
            other => other.to_owned(),
        }
    }

    fn toggle_mute(&self) {
        let imp = self.imp();
        imp.muted_here.set(!imp.muted_here.get());
        self.apply_mute();
    }

    /// Silence on your track while you mute it here or in Teams.
    fn apply_mute(&self) {
        let imp = self.imp();
        let muted = imp.muted_here.get() || imp.teams.borrow().muted;
        if let Some((mic, _)) = imp.sources.borrow().as_ref() {
            mic.set_muted(muted);
        }
        imp.mute_button.set_label(&if imp.muted_here.get() {
            gettext("_Unmute My Microphone")
        } else {
            gettext("_Mute My Microphone")
        });
        self.describe_recording();
        self.publish_mute(imp.muted_here.get());
    }

    /// `app.mute`: whether you muted your microphone here, for the Shell extension.
    fn publish_mute(&self, muted: bool) {
        if let Some(action) = self
            .application()
            .and_then(|app| app.lookup_action("mute"))
            .and_downcast::<gio::SimpleAction>()
        {
            action.set_state(&muted.to_variant());
        }
    }

    /// While recording, reads Teams twice a second when `teams_debug_port`
    /// is set: whether you are muted there, and who is in the call.
    fn watch_teams(&self) {
        let Some(port) = minutes_engine::models::config_value("teams_debug_port")
            .and_then(|p| p.parse::<u16>().ok())
        else {
            return;
        };
        let weak = self.downgrade();
        glib::spawn_future_local(async move {
            // Reads in a row showing Teams out of the call, after it was in one.
            let (mut was_in_call, mut out) = (false, 0);
            loop {
                let snapshot = gio::spawn_blocking(move || teams::snapshot(port))
                    .await
                    .ok()
                    .flatten();
                let Some(win) = weak.upgrade() else {
                    return;
                };
                if win.imp().recording.borrow().is_none() {
                    return;
                }
                // Teams no longer in the call it was in, three reads in a row
                // (a page redrawn for a moment is not a call left): stop.
                // Unreachable Teams proves nothing and changes nothing.
                match &snapshot {
                    Some(s) if s.in_call => (was_in_call, out) = (true, 0),
                    Some(_) if was_in_call => out += 1,
                    _ => {}
                }
                if let Some(s) = &snapshot {
                    win.imp().teams.borrow_mut().in_call = s.in_call;
                }
                if out >= 3 {
                    win.call_over("Teams", "Teams showed no call three reads in a row");
                    return;
                }
                if let Some(snapshot) = snapshot.filter(|s| s.in_call) {
                    let changed = {
                        let mut seen = win.imp().teams.borrow_mut();
                        let before = (seen.muted, seen.me.clone(), seen.others.len());
                        seen.muted = snapshot.muted;
                        if let Some(me) = snapshot.me() {
                            seen.me = Some(me.to_owned());
                        }
                        seen.others.extend(snapshot.others());
                        // Once your name is known, it is no other.
                        if let Some(me) = seen.me.clone() {
                            seen.others.remove(&me);
                        }
                        before != (seen.muted, seen.me.clone(), seen.others.len())
                    };
                    if changed {
                        win.apply_mute();
                        win.publish_live();
                    }
                }
                drop(win);
                glib::timeout_future(Duration::from_millis(500)).await;
            }
        });
    }

    /// Starts the preview of a recording and shows its lines as they come.
    fn start_preview(&self, staging: &Path, language: &str) {
        let imp = self.imp();
        imp.live.borrow_mut().clear();
        imp.drafts.borrow_mut().clear();
        imp.live_list.remove_all();
        imp.live_group.set_visible(false);
        *imp.preview_file.borrow_mut() = None;
        let (tx, rx) = async_channel::unbounded::<Update>();
        *imp.preview.borrow_mut() = Some(Preview::start(staging, language, tx));
        let weak = self.downgrade();
        glib::spawn_future_local(async move {
            while let Ok(update) = rx.recv().await {
                let Some(win) = weak.upgrade() else {
                    return;
                };
                match update {
                    Update::Lines(lines) => win.add_live(lines),
                    Update::Draft {
                        speaker,
                        start_ms,
                        text,
                        ..
                    } => win.set_draft(speaker, start_ms, text),
                }
            }
        });
    }

    fn add_live(&self, lines: Vec<Segment>) {
        let imp = self.imp();
        imp.live_group.set_visible(true);
        // The drafts stay below the lines that are done.
        for (_, _, _, row) in imp.drafts.borrow().iter() {
            imp.live_list.remove(row);
        }
        for line in &lines {
            let time = clock((line.start_ms / 1000).max(0) as u64);
            imp.live_list.append(&transcript_row(
                &time,
                &self.side_name(&line.speaker),
                &line.text,
            ));
        }
        for (_, _, _, row) in imp.drafts.borrow().iter() {
            imp.live_list.append(row);
        }
        imp.live.borrow_mut().extend(lines);
        self.live_changed();
    }

    /// Shows what is being said on one side, greyed, until it is done; empty
    /// text takes it away.
    fn set_draft(&self, speaker: &'static str, start_ms: i64, text: String) {
        let imp = self.imp();
        let mut drafts = imp.drafts.borrow_mut();
        if let Some(i) = drafts.iter().position(|d| d.0 == speaker) {
            imp.live_list.remove(&drafts.remove(i).3);
        }
        if !text.is_empty() {
            imp.live_group.set_visible(true);
            let time = clock((start_ms / 1000).max(0) as u64);
            let row = transcript_row(&time, &self.side_name(speaker), &format!("{text} …"));
            row.add_css_class("dimmed");
            imp.live_list.append(&row);
            drafts.push((speaker, start_ms, text, row));
        }
        drop(drafts);
        self.live_changed();
    }

    fn live_changed(&self) {
        self.publish_live();
        // Follow the newest line once it is laid out.
        let adjustment = self.imp().live_scroller.vadjustment();
        glib::idle_add_local_once(move || adjustment.set_value(adjustment.upper()));
    }

    /// The last lines of the preview, for the Shell extension: `app.live`
    /// holds (time, speaker, text) for each.
    fn publish_live(&self) {
        let Some(action) = self
            .application()
            .and_then(|app| app.lookup_action("live"))
            .and_downcast::<gio::SimpleAction>()
        else {
            return;
        };
        let live = self.imp().live.borrow();
        let mut lines: Vec<(String, String, String)> = live
            .iter()
            .skip(live.len().saturating_sub(40))
            .map(|l| {
                (
                    clock((l.start_ms / 1000).max(0) as u64),
                    self.side_name(&l.speaker),
                    l.text.clone(),
                )
            })
            .collect();
        for (speaker, start_ms, text, _) in self.imp().drafts.borrow().iter() {
            lines.push((
                clock((start_ms / 1000).max(0) as u64),
                self.side_name(speaker),
                format!("{text} …"),
            ));
        }
        action.set_state(&lines.to_variant());
    }

    /// Writes the preview into the meeting folder, to use while the final
    /// transcript is made.
    fn save_preview(&self, out: &Path, note: &Note, lines: Vec<Segment>) {
        let imp = self.imp();
        if lines.is_empty() {
            return;
        }
        let date = glib::DateTime::from_unix_local(note.started_at)
            .and_then(|t| t.format("%Y-%m-%d %H:%M"))
            .map(|s| s.to_string())
            .unwrap_or_default();
        // With the names Teams gave, as the preview showed them.
        let lines = lines
            .into_iter()
            .map(|l| Segment {
                speaker: self.side_name(&l.speaker),
                ..l
            })
            .collect();
        let preview = Transcript {
            segments: lines,
            language: note.language.clone(),
            duration_secs: 0,
        };
        let title = format!("{} ({})", note.title, gettext("preview"));
        let path = out.join(PREVIEW_FILE);
        if std::fs::write(&path, transcribe::to_markdown(&title, &date, &preview)).is_ok() {
            *imp.preview_file.borrow_mut() = Some(path);
            imp.preview_actions.set_visible(true);
        }
    }

    fn copy_preview(&self) {
        let Some(path) = self.imp().preview_file.borrow().clone() else {
            return;
        };
        if let Ok(text) = std::fs::read_to_string(path) {
            self.clipboard().set_text(&text);
            self.toast(&gettext("Preview copied"));
        }
    }

    fn open_preview(&self) {
        let Some(path) = self.imp().preview_file.borrow().clone() else {
            return;
        };
        gtk::FileLauncher::new(Some(&gio::File::for_path(path))).launch(
            Some(self),
            gio::Cancellable::NONE,
            |_| {},
        );
    }

    /// A notification when the transcript is ready; clicking it opens the meeting.
    fn notify_ready(&self, dir: &Path) {
        let Some(app) = self.application() else {
            return;
        };
        if self.is_active() {
            return;
        }
        let title = meeting::open(dir).map(|(_, m)| m.title).unwrap_or_default();
        let notification = gio::Notification::new(&gettext("Transcript ready"));
        notification.set_body(Some(&title));
        notification.set_default_action_and_target_value(
            "app.open-meeting",
            Some(&dir.to_string_lossy().to_variant()),
        );
        app.send_notification(Some("transcript-ready"), &notification);
    }

    fn copy(&self) {
        self.clipboard().set_text(&self.imp().markdown.borrow());
        self.toast(&gettext("Transcript copied"));
    }

    fn open_folder(&self) {
        let root = session::meetings_root();
        let _ = session::private_dir(&root, &root);
        gtk::FileLauncher::new(Some(&gio::File::for_path(root))).launch(
            Some(self),
            gio::Cancellable::NONE,
            |_| {},
        );
    }
}

/// One paragraph of the transcript: who and when above, what below.
fn transcript_row(time: &str, speaker: &str, text: &str) -> gtk::ListBoxRow {
    let body = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .build();
    if !speaker.is_empty() {
        let who = gtk::Label::builder()
            .label(format!("{speaker} · {time}"))
            .xalign(0.0)
            .css_classes(["caption-heading", "dimmed"])
            .build();
        body.append(&who);
    }
    let what = gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .selectable(true)
        .build();
    body.append(&what);
    gtk::ListBoxRow::builder()
        .child(&body)
        .activatable(false)
        .build()
}

/// The preview written during the call, next to the final transcript.
pub const PREVIEW_FILE: &str = "transcript-preview.md";

/// `mm:ss`, or `h:mm:ss` from an hour on.
pub fn clock(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

fn date_of(started_at: i64) -> String {
    glib::DateTime::from_unix_local(started_at)
        .and_then(|t| t.format("%x %H:%M"))
        .map(|s| s.to_string())
        .unwrap_or_default()
}

/// The engine names its stages in English; show them in the user's language.
pub fn stage_label(stage: &str) -> String {
    match stage {
        "Loading audio" => gettext("Loading the audio"),
        "Finding speakers" => gettext("Telling the voices apart"),
        "Loading model" => gettext("Loading the speech model"),
        "Transcribing" => gettext("Transcribing"),
        "Downloading model" => gettext("Downloading the speech model"),
        "Downloading the speaker model" => gettext("Downloading the speaker model"),
        other => other.to_owned(),
    }
}

fn language_name(code: &str) -> String {
    match code {
        "auto" => gettext("Detect Automatically"),
        "en" => gettext("English"),
        "fr" => gettext("French"),
        "nl" => gettext("Dutch"),
        "de" => gettext("German"),
        "es" => gettext("Spanish"),
        "it" => gettext("Italian"),
        "pt" => gettext("Portuguese"),
        other => other.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_clock_shows_hours_only_when_there_are_some() {
        assert_eq!(clock(0), "00:00");
        assert_eq!(clock(65), "01:05");
        assert_eq!(clock(3_725), "1:02:05");
    }

    /// The whole window in one test: GTK runs on one thread, the first one
    /// that starts it. Skipped without a display; the CI gives it one.
    #[test]
    fn the_window_builds_and_shows_a_meeting() {
        if gtk::init().is_err() {
            // The CI gives it a display; there, not running is a failure.
            assert!(
                std::env::var_os("MINUTES_REQUIRE_DISPLAY").is_none(),
                "no display for the window test"
            );
            eprintln!("no display: window test skipped");
            return;
        }
        adw::init().unwrap();
        let app = adw::Application::builder()
            .application_id("io.github.gheop.Minutes.Test")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        app.register(gio::Cancellable::NONE).unwrap();
        let win = MinutesWindow::new(&app);
        let imp = win.imp();

        assert_eq!(imp.pages.visible_child_name().as_deref(), Some("ready"));
        assert_eq!(
            imp.language_row.model().unwrap().n_items() as usize,
            LANGUAGES.len()
        );
        assert!(win.is_action_enabled("record"));
        assert!(!win.is_action_enabled("stop"));
        assert!(!win.is_action_enabled("pause"));

        // The preview shows its lines as they come, under the recording.
        assert!(!imp.live_group.property::<bool>("visible"));
        let line = |start_ms, speaker: &str, text: &str| Segment {
            start_ms,
            end_ms: start_ms + 2000,
            speaker: speaker.into(),
            text: text.into(),
        };
        win.add_live(vec![
            line(1000, "You", "Bonjour."),
            line(4000, "Remote", "Salut."),
        ]);
        assert!(imp.live_group.property::<bool>("visible"));
        assert_eq!(imp.live.borrow().len(), 2);
        assert!(imp.live_list.row_at_index(1).is_some() && imp.live_list.row_at_index(2).is_none());
        // A draft goes below, stays below new lines, and goes when emptied.
        win.set_draft("Remote", 6000, "Je voulais dire".into());
        win.add_live(vec![line(5000, "You", "Oui.")]);
        assert_eq!(
            imp.live_list.row_at_index(3),
            Some(imp.drafts.borrow()[0].3.clone())
        );
        win.set_draft("Remote", 6000, String::new());
        assert!(imp.drafts.borrow().is_empty() && imp.live_list.row_at_index(3).is_none());

        let dir = std::env::temp_dir().join(format!(
            "minutes-window-{}/202609261000 Budget",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("transcript.md"),
            "# Budget\n\n**[00:01] You:** Bonjour.\n\n**[00:04] Remote:** Salut.\n",
        )
        .unwrap();
        win.show_meeting(&dir);
        assert_eq!(
            imp.pages.visible_child_name().as_deref(),
            Some("transcript")
        );
        assert_eq!(imp.content_page.title(), "Budget");
        assert!(imp.copy_button.property::<bool>("visible"));
        let mut rows = 0;
        let mut child = imp.transcript.first_child();
        while let Some(row) = child {
            rows += 1;
            child = row.next_sibling();
        }
        assert_eq!(rows, 2);
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    }

    #[test]
    fn every_engine_language_has_a_name() {
        for (code, _) in LANGUAGES {
            assert_ne!(language_name(code), code, "no name for {code}");
        }
    }
}
