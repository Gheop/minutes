//! The colours of the current Omarchy theme.
//!
//! Every Omarchy theme writes its palette to `colors.toml` in
//! `~/.local/state/omarchy/current/theme/` (background, foreground, accent,
//! the terminal colours). The app maps those onto libadwaita's colour
//! variables, uses the theme's blue and orange for the two speakers, and
//! follows a theme switch while it runs. Outside Omarchy it falls back to
//! plain libadwaita.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;

use gtk::prelude::*;
use gtk::{gio, glib};

pub type Rgb = (f64, f64, f64);

#[derive(Clone, Debug, Default)]
pub struct Theme {
    pub dark: bool,
    colors: HashMap<String, Rgb>,
}

thread_local! {
    static CURRENT: RefCell<Option<Theme>> = const { RefCell::new(None) };
}

impl Theme {
    pub fn get(&self, name: &str) -> Option<Rgb> {
        self.colors.get(name).copied()
    }
}

fn dir() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| glib::home_dir().join(".local/state"))
        .join("omarchy/current/theme")
}

/// Reads `colors.toml`: `key = "#rrggbb"` lines and `mode = "dark"`.
fn load() -> Option<Theme> {
    let text = std::fs::read_to_string(dir().join("colors.toml")).ok()?;
    let mut theme = Theme {
        dark: true,
        ..Default::default()
    };
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let (key, value) = (key.trim(), value.trim().trim_matches('"'));
        if key == "mode" {
            theme.dark = value != "light";
        } else if let Some(rgb) = parse_hex(value) {
            theme.colors.insert(key.to_owned(), rgb);
        }
    }
    theme.get("background").map(|_| theme)
}

fn parse_hex(value: &str) -> Option<Rgb> {
    let hex = value.strip_prefix('#')?;
    if hex.len() != 6 {
        return None;
    }
    let channel = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    Some((
        f64::from(channel(0)?) / 255.0,
        f64::from(channel(2)?) / 255.0,
        f64::from(channel(4)?) / 255.0,
    ))
}

fn hex((r, g, b): Rgb) -> String {
    let byte = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}", byte(r), byte(g), byte(b))
}

/// `a` blended towards `b` by `amount`.
pub fn mix(a: Rgb, b: Rgb, amount: f64) -> Rgb {
    (
        a.0 + (b.0 - a.0) * amount,
        a.1 + (b.1 - a.1) * amount,
        a.2 + (b.2 - a.2) * amount,
    )
}

fn luminance((r, g, b): Rgb) -> f64 {
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

/// A theme colour, or `fallback` when there is no Omarchy theme or it lacks the key.
pub fn color(name: &str, fallback: Rgb) -> Rgb {
    CURRENT.with(|c| {
        c.borrow()
            .as_ref()
            .and_then(|t| t.get(name))
            .unwrap_or(fallback)
    })
}

/// libadwaita colour variables for `theme`.
fn css(theme: &Theme) -> String {
    let get = |name: &str, fallback: &str| theme.get(name).or_else(|| theme.get(fallback));
    let (Some(bg), Some(fg)) = (theme.get("background"), theme.get("foreground")) else {
        return String::new();
    };
    let accent = get("accent", "blue").unwrap_or(fg);
    let dark_bg = get("dark_background", "background").unwrap_or(bg);
    let lighter = get("lighter_background", "background").unwrap_or(bg);
    let red = get("red", "bright_red").unwrap_or((0.9, 0.3, 0.3));
    let green = get("green", "bright_green").unwrap_or((0.3, 0.7, 0.4));
    let yellow = get("yellow", "bright_yellow").unwrap_or((0.9, 0.7, 0.2));
    // Text on an accent fill: whichever of background and foreground reads best.
    let on = |fill: Rgb| {
        if (luminance(fill) - luminance(bg)).abs() > (luminance(fill) - luminance(fg)).abs() {
            bg
        } else {
            fg
        }
    };
    let blue = get("blue", "bright_blue").unwrap_or(accent);
    let orange = get("orange", "yellow").unwrap_or(red);
    let card = mix(bg, lighter, 0.55);
    let popover = mix(bg, lighter, 0.35);
    format!(
        ":root {{
            --window-bg-color: {bg}; --window-fg-color: {fg};
            --view-bg-color: {dark_bg}; --view-fg-color: {fg};
            --headerbar-bg-color: {bg}; --headerbar-fg-color: {fg};
            --headerbar-backdrop-color: {bg};
            --card-bg-color: {card}; --card-fg-color: {fg};
            --popover-bg-color: {popover}; --popover-fg-color: {fg};
            --dialog-bg-color: {popover}; --dialog-fg-color: {fg};
            --sidebar-bg-color: {bg}; --sidebar-fg-color: {fg};
            --accent-bg-color: {accent}; --accent-fg-color: {on_accent}; --accent-color: {accent};
            --destructive-bg-color: {red}; --destructive-fg-color: {on_red}; --destructive-color: {red};
            --error-bg-color: {red}; --error-fg-color: {on_red}; --error-color: {red};
            --success-bg-color: {green}; --success-fg-color: {on_green}; --success-color: {green};
            --warning-bg-color: {yellow}; --warning-fg-color: {on_yellow}; --warning-color: {yellow};
        }}
        .speaker-0 {{ color: {blue}; }} .speaker-1 {{ color: {orange}; }}
        .speaker-2 {{ color: {green}; }} .speaker-3 {{ color: {magenta}; }}
        .speaker-4 {{ color: {cyan}; }} .speaker-5 {{ color: {yellow}; }}",
        bg = hex(bg),
        fg = hex(fg),
        dark_bg = hex(dark_bg),
        card = hex(card),
        popover = hex(popover),
        accent = hex(accent),
        on_accent = hex(on(accent)),
        red = hex(red),
        on_red = hex(on(red)),
        green = hex(green),
        on_green = hex(on(green)),
        yellow = hex(yellow),
        on_yellow = hex(on(yellow)),
        blue = hex(blue),
        orange = hex(orange),
        magenta = hex(get("magenta", "bright_magenta").unwrap_or(accent)),
        cyan = hex(get("cyan", "bright_cyan").unwrap_or(accent)),
    )
}

/// Applies the current theme and keeps following it. `changed` runs after
/// every switch, so custom-drawn widgets can repaint.
pub fn follow(changed: impl Fn() + 'static) {
    let provider = gtk::CssProvider::new();
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1,
        );
    }
    let apply = move || {
        let theme = load();
        let manager = adw::StyleManager::default();
        manager.set_color_scheme(match &theme {
            Some(t) if t.dark => adw::ColorScheme::ForceDark,
            Some(_) => adw::ColorScheme::ForceLight,
            None => adw::ColorScheme::Default,
        });
        provider.load_from_string(&theme.as_ref().map(css).unwrap_or_default());
        CURRENT.with(|c| *c.borrow_mut() = theme);
    };
    apply();

    // A theme switch rewrites the files in the theme directory; debounce the burst.
    let file = gio::File::for_path(dir());
    let Ok(monitor) =
        file.monitor_directory(gio::FileMonitorFlags::WATCH_MOVES, gio::Cancellable::NONE)
    else {
        return;
    };
    let pending = std::rc::Rc::new(std::cell::Cell::new(false));
    let apply = std::rc::Rc::new(apply);
    let changed = std::rc::Rc::new(changed);
    monitor.connect_changed(move |_, _, _, _| {
        if pending.replace(true) {
            return;
        }
        let (pending, apply, changed) = (pending.clone(), apply.clone(), changed.clone());
        glib::timeout_add_local_once(std::time::Duration::from_millis(400), move || {
            pending.set(false);
            apply();
            changed();
        });
    });
    // The monitor has to outlive this function.
    std::mem::forget(monitor);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_hex_colours() {
        assert_eq!(parse_hex("#ff0000"), Some((1.0, 0.0, 0.0)));
        assert_eq!(parse_hex("ff0000"), None);
        assert_eq!(hex((1.0, 0.5, 0.0)), "#ff8000");
    }

    #[test]
    fn css_uses_the_palette() {
        let mut theme = Theme {
            dark: true,
            ..Default::default()
        };
        theme.colors.insert("background".into(), (0.0, 0.0, 0.0));
        theme.colors.insert("foreground".into(), (1.0, 1.0, 1.0));
        theme.colors.insert("accent".into(), (1.0, 0.0, 0.0));
        let css = css(&theme);
        assert!(css.contains("--accent-bg-color: #ff0000"));
        assert!(css.contains("--window-bg-color: #000000"));
    }
}
