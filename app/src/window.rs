//! The main window: the meetings on the left; on the right, getting ready,
//! recording, writing the transcript, and reading it.

use std::cell::{OnceCell, RefCell};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use adw::prelude::*;
use adw::subclass::prelude::*;
use gettextrs::gettext;
use gtk::{gio, glib};
use minutes_engine::audio::{self, Source};
use minutes_engine::export::Format;
use minutes_engine::meeting::{self, Manifest};
use minutes_engine::session::{self, Note};
use minutes_engine::transcribe::{Abort, CANCELLED, Event, LANGUAGES};

/// A recording in progress.
pub struct Recording {
    staging: PathBuf,
    note: Note,
    /// Recorded time before the current stretch, pauses left out.
    before: Duration,
    /// When the current stretch started; None while paused.
    since: Option<Instant>,
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
        pub transcribing_page: TemplateChild<adw::StatusPage>,
        #[template_child]
        pub progress: TemplateChild<gtk::ProgressBar>,
        #[template_child]
        pub transcript: TemplateChild<gtk::ListBox>,

        /// The mic and the computer audio, listened to from the start so the
        /// meters show that both arrive before anything is recorded.
        pub sources: OnceCell<(Source, Source)>,
        pub recording: RefCell<Option<Recording>>,
        pub abort: RefCell<Option<Abort>>,
        /// The transcript on screen, for Copy.
        pub markdown: RefCell<String>,
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
                self.obj().toast(&gettext("Stop the recording before closing"));
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
        let names: Vec<String> = LANGUAGES.iter().map(|(code, _)| language_name(code)).collect();
        imp.language_row
            .set_model(Some(&gtk::StringList::new(&names.iter().map(String::as_str).collect::<Vec<_>>())));

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
        ]);
        self.set_busy(false);

        let _ = imp.sources.set((
            Source::spawn("@DEFAULT_SOURCE@"),
            Source::spawn("@DEFAULT_MONITOR@"),
        ));
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
            ("pause", recording),
            ("stop", recording),
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
        let Some((mic, system)) = imp.sources.get() else {
            return;
        };
        let (mic, system) = (audio::to_meter(mic.recent_peak(3)), audio::to_meter(system.recent_peak(3)));
        for (bar, value) in [
            (&imp.ready_mic_level, mic),
            (&imp.mic_level, mic),
            (&imp.ready_system_level, system),
            (&imp.system_level, system),
        ] {
            bar.set_value(value);
        }
        if let Some(recording) = imp.recording.borrow().as_ref() {
            imp.recording_page.set_title(&clock(recording.elapsed().as_secs()));
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
        let Some((mic, system)) = imp.sources.get() else {
            return;
        };
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
        let started = std::fs::create_dir_all(&staging)
            .and_then(|_| mic.start_recording(&mic_raw))
            .and_then(|_| system.start_recording(&system_raw))
            .and_then(|_| note.write(&staging));
        if let Err(e) = started {
            mic.stop_recording();
            system.stop_recording();
            self.toast(&format!("{}: {e}", gettext("Could not start recording")));
            return;
        }
        *imp.recording.borrow_mut() = Some(Recording {
            staging,
            note,
            before: Duration::ZERO,
            since: Some(Instant::now()),
        });
        self.publish("recording", glib::real_time() / 1_000_000, 0.0);
        imp.content_page.set_title(&title);
        imp.pause_button.set_label(&gettext("_Pause"));
        imp.recording_page.set_description(Some(&gettext("Recording")));
        imp.pages.set_visible_child_name("recording");
        self.set_busy(true);
    }

    fn toggle_pause(&self) {
        let imp = self.imp();
        let Some((mic, system)) = imp.sources.get() else {
            return;
        };
        let mut recording = imp.recording.borrow_mut();
        let Some(recording) = recording.as_mut() else {
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
        imp.pause_button
            .set_label(&if paused { gettext("_Resume") } else { gettext("_Pause") });
        imp.recording_page
            .set_description(Some(&if paused { gettext("Paused") } else { gettext("Recording") }));
    }

    fn stop(&self) {
        let imp = self.imp();
        let Some(recording) = imp.recording.borrow_mut().take() else {
            return;
        };
        if let Some((mic, system)) = imp.sources.get() {
            mic.stop_recording();
            system.stop_recording();
            mic.set_paused(false);
            system.set_paused(false);
        }
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
            win.load_meetings();
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
                    win.toast(&format!("{}: {e}", gettext("Could not write the transcript")));
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
        let (audio_out, audio_staging) = (out.clone(), staging.clone());
        let saved = gio::spawn_blocking(move || session::save_audio(&audio_staging, &audio_out, note.format))
            .await
            .unwrap_or((false, false));
        let mut manifest = Manifest {
            title: note.title.clone(),
            started_at: note.started_at,
            duration_secs: session::raw_duration(&staging),
            format: note.format,
            language: note.language.clone(),
            speakers: vec![meeting::DEFAULT_YOU.to_owned(), meeting::DEFAULT_REMOTE.to_owned()],
            labels: Vec::new(),
            imported: None,
            speaker_count: None,
            model: None,
            chapters: Vec::new(),
            chapters_by: None,
        };
        let _ = meeting::write(&out, &manifest);

        let (events_tx, events_rx) = async_channel::unbounded::<Event>();
        let (done_tx, done_rx) = async_channel::bounded(1);
        let (tracks, dir) = (staging.clone(), out.clone());
        std::thread::spawn(move || {
            let language = manifest.language.clone();
            let result = session::transcribe_into(&tracks, &dir, &mut manifest, &language, &events_tx, &abort);
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
        let result = done_rx
            .recv()
            .await
            .unwrap_or_else(|_| Err("the transcription stopped unexpectedly".into()));
        // The kept tracks are enough to transcribe again; the raw files can go.
        if saved == (true, true) {
            let _ = std::fs::remove_dir_all(&staging);
        }
        result.map(|()| out)
    }

    fn cancel(&self) {
        if let Some(abort) = self.imp().abort.borrow().as_ref() {
            abort.store(true, Ordering::Relaxed);
        }
    }

    /// The meeting folders, newest first.
    fn load_meetings(&self) {
        let imp = self.imp();
        imp.meetings.remove_all();
        let mut dirs: Vec<PathBuf> = std::fs::read_dir(session::meetings_root())
            .map(|entries| entries.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect())
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
            imp.transcript.append(&transcript_row("", "", &gettext("Nobody was heard in this meeting.")));
        }
        *imp.markdown.borrow_mut() = markdown;
        imp.content_page.set_title(&manifest.title);
        imp.copy_button.set_visible(true);
        imp.pages.set_visible_child_name("transcript");
        imp.split_view.set_show_content(true);
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
        let _ = std::fs::create_dir_all(&root);
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
    gtk::ListBoxRow::builder().child(&body).activatable(false).build()
}

/// `mm:ss`, or `h:mm:ss` from an hour on.
pub fn clock(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m:02}:{s:02}") }
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
        assert_eq!(imp.language_row.model().unwrap().n_items() as usize, LANGUAGES.len());
        assert!(win.is_action_enabled("record"));
        assert!(!win.is_action_enabled("stop"));
        assert!(!win.is_action_enabled("pause"));

        let dir = std::env::temp_dir().join(format!("minutes-window-{}/202609261000 Budget", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("transcript.md"),
            "# Budget\n\n**[00:01] You:** Bonjour.\n\n**[00:04] Remote:** Salut.\n",
        )
        .unwrap();
        win.show_meeting(&dir);
        assert_eq!(imp.pages.visible_child_name().as_deref(), Some("transcript"));
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
