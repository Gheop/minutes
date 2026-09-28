//! Minutes: records a meeting as two tracks (your microphone and what the
//! computer plays) and writes the transcript on this computer.

mod window;

use std::cell::Cell;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use adw::prelude::*;
use gettextrs::{LocaleCategory, gettext};
use gtk::glib;
use minutes_engine::{diarize, session, transcribe};

pub const APP_ID: &str = "io.github.gheop.Minutes";

/// Started with `--background` (at login): Minutes stays without a window to
/// notice calls, and closing the window hides it.
pub static BACKGROUND: AtomicBool = AtomicBool::new(false);
const DOMAIN: &str = "minutes";

/// Where the translations are: `share/locale` next to the binary's `bin`
/// (`/usr`, `~/.local`), else the ones built with it while developing, else
/// the system's.
fn locale_dir() -> PathBuf {
    let installed = std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.parent()?.join("share/locale")))
        .filter(|dir| dir.join("fr/LC_MESSAGES/minutes.mo").is_file());
    let built = PathBuf::from(concat!(env!("OUT_DIR"), "/locale"));
    installed
        .or_else(|| built.is_dir().then_some(built))
        .unwrap_or_else(|| PathBuf::from("/usr/share/locale"))
}

/// Sends warnings to the journal unless a terminal shows them: the instance
/// started with the session has its output thrown away.
fn log_to_journal() {
    glib::log_set_writer_func(|level, fields| {
        // Like GLib's own writer: debug and info only with G_MESSAGES_DEBUG,
        // or GTK fills the journal with its debug lines.
        if matches!(level, glib::LogLevel::Debug | glib::LogLevel::Info)
            && std::env::var_os("G_MESSAGES_DEBUG").is_none()
        {
            return glib::LogWriterOutput::Handled;
        }
        if !std::io::stderr().is_terminal()
            && glib::log_writer_journald(level, fields) == glib::LogWriterOutput::Handled
        {
            return glib::LogWriterOutput::Handled;
        }
        glib::log_writer_default(level, fields)
    });
}

fn main() -> glib::ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let command = args.get(1).map(String::as_str);
    // The command-line tools keep their warnings on stderr, for whoever runs them.
    if !matches!(
        command,
        Some("transcribe" | "transcribe-file" | "diarize" | "write-up")
    ) {
        log_to_journal();
    }
    minutes_engine::move_old_paths();
    // The engine's command-line tools, for scripts and the bench.
    match command {
        Some("transcribe") => return transcribe::cli(&args[2..]),
        Some("transcribe-file") => return transcribe::cli_file(&args[2..]),
        Some("diarize") => return diarize::cli(&args[2..]),
        Some("write-up") => return session::cli(&args[2..]),
        _ => {}
    }

    // SAFETY: setlocale must not race other threads; none runs yet.
    unsafe { gettextrs::setlocale(LocaleCategory::LcAll, "") };
    let _ = gettextrs::bindtextdomain(DOMAIN, locale_dir());
    let _ = gettextrs::bind_textdomain_codeset(DOMAIN, "UTF-8");
    let _ = gettextrs::textdomain(DOMAIN);
    glib::set_application_name(&gettext("Minutes"));

    let app = adw::Application::builder().application_id(APP_ID).build();
    let background = args.get(1).is_some_and(|a| a == "--background");
    if background {
        BACKGROUND.store(true, Ordering::Relaxed);
        // For the life of the process: without a window shown, GTK would quit.
        std::mem::forget(app.hold());
    }
    // `minutes <meeting folder or file>` opens that meeting.
    let open = args.get(1).map(PathBuf::from).filter(|p| p.exists());
    // Started in the background, the first activation only sets things up;
    // later ones (the launcher, a notification) show the window.
    let quiet_start = Cell::new(background);
    app.connect_activate(move |app| {
        let window = app
            .windows()
            .into_iter()
            .find_map(|w| w.downcast::<window::MinutesWindow>().ok())
            .unwrap_or_else(|| window::MinutesWindow::new(app));
        if quiet_start.replace(false) {
            return;
        }
        if let Some(path) = &open {
            window.show_meeting(path);
        }
        let window = window.upcast::<gtk::Window>();
        window.present();
        if let Ok(path) = std::env::var("MINUTES_SCREENSHOT") {
            screenshot_and_quit(&window, PathBuf::from(path));
        }
    });
    let about = gtk::gio::ActionEntry::builder("about")
        .activate(|app: &adw::Application, _, _| show_about(app))
        .build();
    app.add_action_entries([about]);
    add_outside_actions(&app);
    app.set_accels_for_action("win.record", &["<Control>r"]);
    app.set_accels_for_action("win.stop", &["<Control>s"]);
    app.set_accels_for_action("win.pause", &["<Control>p"]);
    app.set_accels_for_action("win.copy", &["<Control><Shift>c"]);
    app.set_accels_for_action("window.close", &["<Control>w"]);
    // GTK would take the engine's arguments for its own.
    app.run_with_args(&args[..1])
}

/// What the Shell extension and other programs reach over D-Bus, on
/// `/io/github/gheop/Minutes`: `status`, a state (state, seconds, progress)
/// that changes as Minutes records and transcribes; `live`, the last lines of
/// the preview; `mute`, whether you muted your microphone here; and `record`,
/// `pause`, `stop`, `mute-mic`, `copy-preview` and `open-meeting` (a folder)
/// for the window.
fn add_outside_actions(app: &adw::Application) {
    let status =
        gtk::gio::SimpleAction::new_stateful("status", None, &("idle", 0i64, 0.0f64).to_variant());
    app.add_action(&status);
    // The last lines of the preview while recording: (time, speaker, text).
    let live = gtk::gio::SimpleAction::new_stateful(
        "live",
        None,
        &Vec::<(String, String, String)>::new().to_variant(),
    );
    app.add_action(&live);
    // Whether you muted your microphone in Minutes.
    let mute = gtk::gio::SimpleAction::new_stateful("mute", None, &false.to_variant());
    app.add_action(&mute);
    // The window may be hidden (Minutes in the background): no active window then.
    let window = |app: &adw::Application| {
        app.windows()
            .into_iter()
            .find_map(|w| w.downcast::<window::MinutesWindow>().ok())
            .unwrap_or_else(|| window::MinutesWindow::new(app))
    };
    for name in ["record", "pause", "stop", "copy-preview", "mute-mic"] {
        let action = gtk::gio::SimpleAction::new(name, None);
        action.connect_activate(glib::clone!(
            #[weak]
            app,
            move |_, _| {
                let window = window(&app);
                window.present();
                let _ = WidgetExt::activate_action(&window, &format!("win.{name}"), None);
            }
        ));
        app.add_action(&action);
    }
    let open = gtk::gio::SimpleAction::new("open-meeting", Some(glib::VariantTy::STRING));
    open.connect_activate(glib::clone!(
        #[weak]
        app,
        move |_, folder| {
            let window = window(&app);
            if let Some(folder) = folder.and_then(|f| f.str()) {
                window.show_meeting(std::path::Path::new(folder));
            }
            window.present();
        }
    ));
    app.add_action(&open);
}

/// With `MINUTES_SCREENSHOT=file.png`, renders the window into that file once
/// it is drawn and quits: for the README's pictures and for checking the
/// interface without looking at a screen.
fn screenshot_and_quit(window: &gtk::Window, path: PathBuf) {
    let window = window.clone();
    glib::timeout_add_local_once(std::time::Duration::from_millis(1500), move || {
        let paintable = gtk::WidgetPaintable::new(Some(&window));
        let (width, height) = (window.width(), window.height());
        let snapshot = gtk::Snapshot::new();
        paintable.snapshot(&snapshot, f64::from(width), f64::from(height));
        let saved = snapshot.to_node().and_then(|node| {
            let renderer = window.native()?.renderer()?;
            let texture = renderer.render_texture(&node, None);
            texture.save_to_png(&path).ok()
        });
        if saved.is_none() {
            minutes_engine::warn(format!(
                "could not save the screenshot to {}",
                path.display()
            ));
        }
        window.application().inspect(|app| app.quit());
    });
}

fn show_about(app: &adw::Application) {
    let about = adw::AboutDialog::builder()
        .application_name(gettext("Minutes"))
        .application_icon(APP_ID)
        .developer_name("Gheop")
        .version(env!("CARGO_PKG_VERSION"))
        .website("https://github.com/Gheop/minutes")
        .issue_url("https://github.com/Gheop/minutes/issues")
        .license_type(gtk::License::MitX11)
        .build();
    about.add_credit_section(
        Some(&gettext("Based on")),
        &["omarchy-meeting-recorder by Jankees van Woezik https://github.com/jankeesvw/omarchy-meeting-recorder"],
    );
    about.add_credit_section(
        Some(&gettext("Speech and speakers")),
        &[
            "whisper.cpp https://github.com/ggml-org/whisper.cpp",
            "NVIDIA Nemotron 3 Diarization https://huggingface.co/nvidia/Nemotron-3-Diarization",
        ],
    );
    about.present(app.active_window().as_ref());
}
