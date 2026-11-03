//! Settings presets: the recording and app settings as a file, so one
//! computer's setup can be loaded on another from Settings.
//!
//! Only the keys listed here travel, each checked against the shape the app
//! (and the Tauri app sharing the store) reads, so a hand-edited file can't
//! leave a value either app would fail to parse. Folders, sign-in, licence,
//! instance id, devices and first-run flags stay on each computer.

use std::path::Path;

use serde_json::{Map, Value};

use crate::store::{
    self, AUDIO_ENHANCEMENT, AppTheme, CAMERA_MAX_FPS_KEY, CAPTURE_METHOD_KEY, CaptureMethod,
    EditorPreviewQuality, GENERAL_SETTINGS, MainWindowStartBehaviour, PostDeletionBehaviour,
    PostScreenshotBehaviour, PostStudioBehaviour, RECORDING_SETTINGS, RECORDING_START_SAFETY,
    STUDIO_SOUND_BY_DEFAULT_KEY, SettingsEnum, StudioQuality, UpdateDownloadPolicy,
    WindowCaptureStyle,
};

const VERSION_KEY: &str = "sayplaySettingsPreset";
const VERSION: u64 = 1;
/// A preset is a few kilobytes; anything far larger is not one.
const MAX_FILE_BYTES: u64 = 1024 * 1024;

pub const FILE_NAME: &str = "SayPlay settings.json";

type Check = fn(&Value) -> bool;

const GENERAL_KEYS: &[(&str, Check)] = &[
    ("theme", is_choice::<AppTheme>),
    (crate::i18n::SETTING_KEY, is_opt_string),
    ("enableNotifications", Value::is_boolean),
    ("studioRecordingQuality", is_choice::<StudioQuality>),
    ("recordingCountdown", is_opt_integer),
    (
        "mainWindowRecordingStartBehaviour",
        is_choice::<MainWindowStartBehaviour>,
    ),
    ("windowCaptureStyle", is_choice::<WindowCaptureStyle>),
    ("focusCenter", Value::is_boolean),
    (
        "postStudioRecordingBehaviour",
        is_choice::<PostStudioBehaviour>,
    ),
    (
        "postScreenshotBehaviour",
        is_choice::<PostScreenshotBehaviour>,
    ),
    ("postDeletionBehaviour", is_choice::<PostDeletionBehaviour>),
    ("crashRecoveryRecording", Value::is_boolean),
    ("custom_cursor_capture2", Value::is_boolean),
    ("autoZoomOnClicks", Value::is_boolean),
    ("defaultZoomAmount", is_opt_number),
    ("captureKeyboardEvents", Value::is_boolean),
    ("maxFps", is_integer),
    (CAMERA_MAX_FPS_KEY, is_integer),
    (CAPTURE_METHOD_KEY, is_choice::<CaptureMethod>),
    ("defaultProjectNameTemplate", is_opt_string),
    ("excludedWindows", Value::is_array),
    ("updateDownloadPolicy", is_choice::<UpdateDownloadPolicy>),
    ("enableTelemetry", Value::is_boolean),
    ("editorPreviewQuality", is_choice::<EditorPreviewQuality>),
    ("outOfProcessMuxer", Value::is_boolean),
];

const SAFETY_KEYS: &[(&str, Check)] =
    &[("confirmBeforeRecordingWithoutMicrophone", Value::is_boolean)];

const AUDIO_KEYS: &[(&str, Check)] = &[
    (STUDIO_SOUND_BY_DEFAULT_KEY, Value::is_boolean),
    ("isolation", is_isolation),
];

const RECORDING_KEYS: &[(&str, Check)] = &[("systemAudio", Value::is_boolean)];

/// Each section's keys, and whether a key the preset leaves out goes back to
/// its default. Only `general_settings` does: every key there is optional in
/// both apps, which is not known of the other sections.
const SECTIONS: &[(&str, &[(&str, Check)], bool)] = &[
    (GENERAL_SETTINGS, GENERAL_KEYS, true),
    (RECORDING_START_SAFETY, SAFETY_KEYS, false),
    (AUDIO_ENHANCEMENT, AUDIO_KEYS, false),
    (RECORDING_SETTINGS, RECORDING_KEYS, false),
];

fn is_choice<T: SettingsEnum>(value: &Value) -> bool {
    value.as_str().and_then(T::from_json).is_some()
}

fn is_integer(value: &Value) -> bool {
    value
        .as_u64()
        .is_some_and(|value| value <= u64::from(u32::MAX))
}

fn is_opt_integer(value: &Value) -> bool {
    value.is_null() || is_integer(value)
}

fn is_opt_number(value: &Value) -> bool {
    value.is_null() || value.is_number()
}

fn is_opt_string(value: &Value) -> bool {
    value.is_null() || value.is_string()
}

fn is_isolation(value: &Value) -> bool {
    serde_json::from_value::<cap_project::VoiceIsolation>(value.clone()).is_ok()
}

#[derive(Debug)]
pub enum PresetError {
    /// The file couldn't be read or written.
    File(std::io::Error),
    /// Not JSON, or JSON without the preset marker.
    NotAPreset,
    /// Written by a newer SayPlay with a format this one doesn't know.
    Newer,
    /// The settings store couldn't be read or written.
    Store,
}

/// The listed settings from `sections` (store section name to its keys).
fn preset_from(sections: impl Fn(&str) -> Map<String, Value>) -> Value {
    let mut preset = Map::new();
    preset.insert(VERSION_KEY.into(), Value::from(VERSION));
    for (section, keys, _) in SECTIONS {
        let stored = sections(section);
        let picked: Map<String, Value> = keys
            .iter()
            .filter_map(|(key, check)| {
                stored
                    .get(*key)
                    .filter(|value| check(value))
                    .map(|value| (key.to_string(), value.clone()))
            })
            .collect();
        preset.insert(section.to_string(), Value::Object(picked));
    }
    Value::Object(preset)
}

fn parse(bytes: &[u8]) -> Result<Map<String, Value>, PresetError> {
    let Ok(Value::Object(preset)) = serde_json::from_slice::<Value>(bytes) else {
        return Err(PresetError::NotAPreset);
    };
    match preset.get(VERSION_KEY).and_then(Value::as_u64) {
        Some(VERSION) => Ok(preset),
        Some(version) if version > VERSION => Err(PresetError::Newer),
        _ => Err(PresetError::NotAPreset),
    }
}

/// Makes `store` hold the preset's settings: each listed key takes the
/// preset's value or, in sections that allow it, goes back to its default
/// when the preset leaves it out, so the result matches the computer it came
/// from. A value of the wrong shape leaves that one setting as it was.
/// Everything else in the store is untouched, and no section is created
/// just to stay empty.
fn apply_to(store: &mut Map<String, Value>, preset: &Map<String, Value>) {
    let empty = Map::new();
    for (section, keys, reset_missing) in SECTIONS {
        let incoming = preset
            .get(*section)
            .and_then(Value::as_object)
            .unwrap_or(&empty);
        let changes: Vec<(&str, Option<&Value>)> = keys
            .iter()
            .filter_map(|(key, check)| match incoming.get(*key) {
                Some(value) if check(value) => Some((*key, Some(value))),
                Some(_) => None,
                None => reset_missing.then_some((*key, None)),
            })
            .collect();
        if changes.is_empty() {
            continue;
        }
        if changes.iter().all(|(_, value)| value.is_none()) {
            if let Some(Value::Object(target)) = store.get_mut(*section) {
                for (key, _) in &changes {
                    target.remove(*key);
                }
            }
            continue;
        }
        let entry = store
            .entry(section.to_string())
            .or_insert_with(|| Value::Object(Map::new()));
        if !entry.is_object() {
            *entry = Value::Object(Map::new());
        }
        let Some(target) = entry.as_object_mut() else {
            continue;
        };
        for (key, value) in changes {
            match value {
                Some(value) => {
                    target.insert(key.to_string(), value.clone());
                }
                None => {
                    target.remove(key);
                }
            }
        }
    }
}

/// Writes this computer's preset to `path`.
pub fn export(path: &Path) -> Result<(), PresetError> {
    let preset = preset_from(store::store_section);
    let bytes = serde_json::to_vec_pretty(&preset).map_err(|_| PresetError::Store)?;
    std::fs::write(path, bytes).map_err(PresetError::File)
}

/// Loads the preset at `path` into the settings store.
pub fn import(path: &Path) -> Result<(), PresetError> {
    let len = std::fs::metadata(path).map_err(PresetError::File)?.len();
    if len > MAX_FILE_BYTES {
        return Err(PresetError::NotAPreset);
    }
    let preset = parse(&std::fs::read(path).map_err(PresetError::File)?)?;
    if store::update_store(|store| {
        apply_to(store, &preset);
        true
    }) {
        Ok(())
    } else {
        Err(PresetError::Store)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn object(value: Value) -> Map<String, Value> {
        match value {
            Value::Object(map) => map,
            _ => panic!("not an object"),
        }
    }

    fn section_of(store: &Map<String, Value>) -> impl Fn(&str) -> Map<String, Value> + '_ {
        |name| {
            store
                .get(name)
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default()
        }
    }

    #[test]
    fn export_keeps_listed_settings_and_nothing_private() {
        let store = object(json!({
            "general_settings": {
                "maxFps": 60,
                "sayplayCameraMaxFps": 60,
                "sayplayCaptureMethod": "original",
                "theme": "dark",
                "recordingsPath": "C:\\Users\\teacher\\Videos",
                "commercialLicense": {"licenseKey": "secret"},
                "instanceId": "id",
                "serverUrl": "https://example.com",
                "hasCompletedStartup": true,
                "studioRecordingQuality": "not-a-quality",
            },
            "recording_settings": {"systemAudio": true, "micName": "USB Mic", "mode": "studio"},
            "audio_enhancement": {"enabledByDefault": false, "isolation": "strong"},
            "auth": {"token": "secret"},
        }));
        let preset = preset_from(section_of(&store));
        assert_eq!(
            preset,
            json!({
                "sayplaySettingsPreset": 1,
                "general_settings": {
                    "maxFps": 60,
                    "sayplayCameraMaxFps": 60,
                    "sayplayCaptureMethod": "original",
                    "theme": "dark",
                },
                "recording_start_safety": {},
                "audio_enhancement": {"enabledByDefault": false, "isolation": "strong"},
                "recording_settings": {"systemAudio": true},
            })
        );
    }

    #[test]
    fn import_matches_the_source_and_keeps_this_computers_own_settings() {
        let mut store = object(json!({
            "general_settings": {
                "maxFps": 30,
                "autoZoomOnClicks": true,
                "recordingsPath": "D:\\Recordings",
                "instanceId": "mine",
            },
            "recording_settings": {"systemAudio": false, "micName": "Laptop Mic"},
            "auth": {"token": "mine"},
        }));
        let preset = object(json!({
            "sayplaySettingsPreset": 1,
            "general_settings": {
                "maxFps": 60,
                "sayplayCameraMaxFps": 60,
                "theme": "purple",
                "recordingsPath": "C:\\Elsewhere",
            },
            "recording_settings": {"systemAudio": true},
        }));
        apply_to(&mut store, &preset);
        assert_eq!(
            Value::Object(store),
            json!({
                "general_settings": {
                    "maxFps": 60,
                    "sayplayCameraMaxFps": 60,
                    "recordingsPath": "D:\\Recordings",
                    "instanceId": "mine",
                },
                "recording_settings": {"systemAudio": true, "micName": "Laptop Mic"},
                "auth": {"token": "mine"},
            })
        );
    }

    #[test]
    fn sections_other_than_general_keep_what_the_preset_leaves_out() {
        let mut store = object(json!({
            "audio_enhancement": {"enabledByDefault": false, "isolation": "light"},
        }));
        let preset = object(json!({
            "sayplaySettingsPreset": 1,
            "audio_enhancement": {"isolation": "strong"},
        }));
        apply_to(&mut store, &preset);
        assert_eq!(
            Value::Object(store),
            json!({
                "audio_enhancement": {"enabledByDefault": false, "isolation": "strong"},
            })
        );
    }

    #[test]
    fn exported_preset_round_trips() {
        let source = object(json!({
            "general_settings": {"maxFps": 60, "sayplayCameraMaxFps": 60, "theme": "light"},
            "audio_enhancement": {"enabledByDefault": true, "isolation": "light"},
        }));
        let bytes = serde_json::to_vec(&preset_from(section_of(&source))).unwrap();
        let mut target = Map::new();
        apply_to(&mut target, &parse(&bytes).unwrap());
        assert_eq!(
            preset_from(section_of(&target)),
            preset_from(section_of(&source))
        );
    }

    #[test]
    fn only_preset_files_are_accepted() {
        assert!(matches!(parse(b"not json"), Err(PresetError::NotAPreset)));
        assert!(matches!(parse(b"[]"), Err(PresetError::NotAPreset)));
        assert!(matches!(
            parse(br#"{"general_settings":{}}"#),
            Err(PresetError::NotAPreset)
        ));
        assert!(matches!(
            parse(br#"{"sayplaySettingsPreset":2}"#),
            Err(PresetError::Newer)
        ));
        assert!(parse(br#"{"sayplaySettingsPreset":1}"#).is_ok());
    }
}
