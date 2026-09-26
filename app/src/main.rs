//! Minutes: records a meeting as two tracks (your microphone and what the
//! computer plays) and writes the transcript on this computer.

mod window;

use std::path::PathBuf;

use adw::prelude::*;
use gettextrs::{LocaleCategory, gettext};
use gtk::glib;
use minutes_engine::{diarize, transcribe};

pub const APP_ID: &str = "io.github.gheop.Minutes";
const DOMAIN: &str = "minutes";

/// The translations built next to the binary while developing, else the installed ones.
fn locale_dir() -> PathBuf {
    let built = PathBuf::from(concat!(env!("OUT_DIR"), "/locale"));
    if built.is_dir() { built } else { PathBuf::from("/usr/share/locale") }
}

fn main() -> glib::ExitCode {
    let args: Vec<String> = std::env::args().collect();
    // The engine's command-line tools, for scripts and the bench.
    match args.get(1).map(String::as_str) {
        Some("transcribe") => return transcribe::cli(&args[2..]),
        Some("transcribe-file") => return transcribe::cli_file(&args[2..]),
        Some("diarize") => return diarize::cli(&args[2..]),
        _ => {}
    }

    // SAFETY: setlocale must not race other threads; none runs yet.
    unsafe { gettextrs::setlocale(LocaleCategory::LcAll, "") };
    let _ = gettextrs::bindtextdomain(DOMAIN, locale_dir());
    let _ = gettextrs::bind_textdomain_codeset(DOMAIN, "UTF-8");
    let _ = gettextrs::textdomain(DOMAIN);
    glib::set_application_name(&gettext("Minutes"));

    let app = adw::Application::builder().application_id(APP_ID).build();
    // `minutes <meeting folder or file>` opens that meeting.
    let open = args.get(1).map(PathBuf::from).filter(|p| p.exists());
    app.connect_activate(move |app| {
        let window = app
            .active_window()
            .and_downcast::<window::MinutesWindow>()
            .unwrap_or_else(|| window::MinutesWindow::new(app));
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
    app.set_accels_for_action("win.record", &["<Control>r"]);
    app.set_accels_for_action("win.stop", &["<Control>s"]);
    app.set_accels_for_action("win.pause", &["<Control>p"]);
    app.set_accels_for_action("win.copy", &["<Control><Shift>c"]);
    app.set_accels_for_action("window.close", &["<Control>w"]);
    // GTK would take the engine's arguments for its own.
    app.run_with_args(&args[..1])
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
            eprintln!("minutes: could not save the screenshot to {}", path.display());
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
