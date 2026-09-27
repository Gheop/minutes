//! Which whisper model transcribes, where it is on disk, and fetching it.
//!
//! The model is picked with `--model` on the command line, or `model = "…"`
//! in `~/.config/omarchy-meeting-recorder/config.toml`, and is
//! `large-v3-turbo` otherwise. A name from `MODELS` is looked for in the app's
//! own model folder and in voxtype's (same files, no need to have them twice),
//! and downloaded when it is in neither. A path to a `.bin` file is used as is.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use whisper_rs::DtwModelPreset;

use crate::transcribe::{Abort, Events, download, models_dir};

pub const DEFAULT: &str = "large-v3-turbo";

pub struct Model {
    pub name: &'static str,
    /// Download size in MB, for the text in the app.
    pub size_mb: u32,
    preset: DtwModelPreset,
    /// SHA-256 of the file at `REVISION`; a download that differs is refused.
    sha256: &'static str,
}

/// The revision of github.com/ggerganov/whisper.cpp's model repository the
/// files are taken from: fixed, so what is downloaded is what was checked.
const REVISION: &str = "5359861c739e955e79d9a303bcbc70fb988958b1";

pub const MODELS: [Model; 10] = [
    Model {
        name: "tiny",
        size_mb: 75,
        preset: DtwModelPreset::Tiny,
        sha256: "be07e048e1e599ad46341c8d2a135645097a538221678b7acdd1b1919c6e1b21",
    },
    Model {
        name: "tiny.en",
        size_mb: 75,
        preset: DtwModelPreset::TinyEn,
        sha256: "921e4cf8686fdd993dcd081a5da5b6c365bfde1162e72b08d75ac75289920b1f",
    },
    Model {
        name: "base",
        size_mb: 142,
        preset: DtwModelPreset::Base,
        sha256: "60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe",
    },
    Model {
        name: "base.en",
        size_mb: 142,
        preset: DtwModelPreset::BaseEn,
        sha256: "a03779c86df3323075f5e796cb2ce5029f00ec8869eee3fdfb897afe36c6d002",
    },
    Model {
        name: "small",
        size_mb: 466,
        preset: DtwModelPreset::Small,
        sha256: "1be3a9b2063867b937e64e2ec7483364a79917e157fa98c5d94b5c1fffea987b",
    },
    Model {
        name: "small.en",
        size_mb: 466,
        preset: DtwModelPreset::SmallEn,
        sha256: "c6138d6d58ecc8322097e0f987c32f1be8bb0a18532a3f88f734d1bbf9c41e5d",
    },
    Model {
        name: "medium",
        size_mb: 1500,
        preset: DtwModelPreset::Medium,
        sha256: "6c14d5adee5f86394037b4e4e8b59f1673b6cee10e3cf0b11bbdbee79c156208",
    },
    Model {
        name: "medium.en",
        size_mb: 1500,
        preset: DtwModelPreset::MediumEn,
        sha256: "cc37e93478338ec7700281a7ac30a10128929eb8f427dda2e865faa8f6da4356",
    },
    Model {
        name: "large-v3",
        size_mb: 3100,
        preset: DtwModelPreset::LargeV3,
        sha256: "64d182b440b98d5203c4f9bd541544d84c605196c4f7b845dfa11fb23594d1e2",
    },
    Model {
        name: "large-v3-turbo",
        size_mb: 1600,
        preset: DtwModelPreset::LargeV3Turbo,
        sha256: "1fc70f774d38eb169993ac391eea357ef47c88757ef72ee5943879b7e8e2bc69",
    },
];

/// Set by `--model`; wins over the config file.
static OVERRIDE: Mutex<Option<String>> = Mutex::new(None);
/// Held while a model downloads, so a second caller waits instead of
/// fetching the same file again.
static DOWNLOADING: Mutex<()> = Mutex::new(());

pub fn set_override(name: &str) {
    *OVERRIDE.lock().unwrap() = Some(name.trim().to_owned());
}

pub fn config_file() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| glib::home_dir().join(".config"))
        .join(crate::APP_NAME)
        .join("config.toml")
}

/// Every `key = "value"` line for `key` in the config file, without quotes or a trailing comment.
pub fn config_values(key: &str) -> Vec<String> {
    std::fs::read_to_string(config_file())
        .unwrap_or_default()
        .lines()
        .filter_map(|line| {
            let (k, value) = line.split_once('=')?;
            (k.trim() == key).then(|| {
                let value = value.split('#').next().unwrap_or("");
                value.trim().trim_matches('"').to_owned()
            })
        })
        .filter(|value| !value.is_empty())
        .collect()
}

/// The first `key = "value"` line for `key` in the config file.
pub fn config_value(key: &str) -> Option<String> {
    config_values(key).into_iter().next()
}

/// The configured model: a name from `MODELS` or a path to a model file.
pub fn configured() -> String {
    if let Some(name) = OVERRIDE.lock().unwrap().clone() {
        return name;
    }
    config_value("model").unwrap_or_else(|| DEFAULT.to_owned())
}

fn known(name: &str) -> Option<&'static Model> {
    let name = name.strip_prefix("ggml-").unwrap_or(name);
    let name = name.strip_suffix(".bin").unwrap_or(name);
    MODELS.iter().find(|m| m.name == name)
}

fn file_name(model: &Model) -> String {
    format!("ggml-{}.bin", model.name)
}

fn data_dir() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| glib::home_dir().join(".local/share"))
}

/// A complete model file: at least most of its expected size.
fn usable(path: &Path, model: Option<&Model>) -> bool {
    let min = model.map_or(10_000_000, |m| u64::from(m.size_mb) * 800_000);
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() >= min)
}

/// The configured model's file, when it is on disk.
pub fn find() -> Option<PathBuf> {
    find_named(&configured())
}

/// The model for the preview while a call goes on: `live_model` in the
/// config, else the usual model when whisper runs on a GPU. On the CPU the
/// usual model is slower than the call itself, so there is no preview unless
/// a smaller one is set.
pub fn preview() -> Option<PathBuf> {
    match config_value("live_model") {
        Some(name) => find_named(&name),
        None if cfg!(any(feature = "vulkan", feature = "cuda")) => find(),
        None => None,
    }
}

/// A model's file by name (see `MODELS`) or path, when it is on disk.
pub fn find_named(name: &str) -> Option<PathBuf> {
    let name = name.to_owned();
    match known(&name) {
        Some(model) => [models_dir(), data_dir().join("voxtype/models")]
            .into_iter()
            .map(|dir| dir.join(file_name(model)))
            .find(|path| usable(path, Some(model))),
        None => {
            let path = PathBuf::from(&name);
            usable(&path, None).then_some(path)
        }
    }
}

/// For the app: the model's name and its size when it still has to be downloaded.
pub fn missing() -> Option<(String, u32)> {
    if find().is_some() {
        return None;
    }
    let name = configured();
    known(&name).map(|m| (m.name.to_owned(), m.size_mb))
}

/// The model file, downloaded first when needed. Blocking.
pub fn ensure(events: &Events, abort: &Abort) -> Result<PathBuf, String> {
    let _one_at_a_time = DOWNLOADING.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(path) = find() {
        return Ok(path);
    }
    let name = configured();
    let Some(model) = known(&name) else {
        let names: Vec<&str> = MODELS.iter().map(|m| m.name).collect();
        return Err(format!(
            "unknown model \"{name}\": use one of {} or a path to a model file",
            names.join(", ")
        ));
    };
    let target = models_dir().join(file_name(model));
    let url = format!(
        "https://huggingface.co/ggerganov/whisper.cpp/resolve/{REVISION}/{}",
        file_name(model)
    );
    download(
        &url,
        &target,
        "Downloading model",
        u64::from(model.size_mb) * 800_000,
        model.sha256,
        events,
        abort,
    )?;
    Ok(target)
}

/// The attention-head preset for word times; None for a model file of unknown kind.
pub fn dtw_preset() -> Option<DtwModelPreset> {
    known(&configured()).map(|m| m.preset.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_file_names_both_resolve() {
        assert_eq!(known("large-v3").map(|m| m.name), Some("large-v3"));
        assert_eq!(known("ggml-small.en.bin").map(|m| m.name), Some("small.en"));
        assert!(known("gpt-5").is_none());
    }
}
