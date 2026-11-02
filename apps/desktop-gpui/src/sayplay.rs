//! Say & Play school build: defaults and branding (see SAYPLAY.md at repo root).

#[cfg(sayplay_build)]
pub const PRODUCT_NAME: &str = "SayPlay Recorder";

#[cfg(not(sayplay_build))]
pub const PRODUCT_NAME: &str = "Cap";

/// One-time store seeds so teachers get Studio Mode with mic + system audio.
#[cfg(sayplay_build)]
pub fn apply_school_defaults() {
    use serde_json::Value;

    use crate::store::{
        GENERAL_SETTINGS, RECORDING_SETTINGS, set_recording_mode_slug, set_recording_system_audio,
        set_store_setting, store_section,
    };

    if crate::store::recording_mode_slug().is_none() {
        set_recording_mode_slug("studio");
    }

    if !store_section(RECORDING_SETTINGS).contains_key("systemAudio") {
        set_recording_system_audio(true);
    }

    let general = store_section(GENERAL_SETTINGS);
    let seed = |key: &str, value: Value| {
        if !general.contains_key(key) {
            set_store_setting(GENERAL_SETTINGS, key, value);
        }
    };

    seed("studioRecordingQuality", Value::String("ultra".into()));
    seed("maxFps", Value::Number(60.into()));
    seed("autoZoomOnClicks", Value::Bool(true));
    seed("updateDownloadPolicy", Value::String("never".into()));
    seed("enableTelemetry", Value::Bool(false));
}

#[cfg(not(sayplay_build))]
pub fn apply_school_defaults() {}
