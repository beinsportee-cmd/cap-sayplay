//! Say & Play school build: defaults and branding (see SAYPLAY.md at repo root).

use gpui::App;

use crate::main_window::Mode;

#[cfg(sayplay_build)]
pub const PRODUCT_NAME: &str = "SayPlay Recorder";

#[cfg(not(sayplay_build))]
pub const PRODUCT_NAME: &str = "Cap";

/// Modes shown in the main window, tray, overlays, and hotkey cycle.
pub fn ui_modes() -> &'static [Mode] {
    #[cfg(sayplay_build)]
    return &[Mode::Studio, Mode::Screenshot];
    #[cfg(not(sayplay_build))]
    &[Mode::Instant, Mode::Studio, Mode::Screenshot]
}

pub fn default_ui_mode() -> Mode {
    #[cfg(sayplay_build)]
    return Mode::Studio;
    #[cfg(not(sayplay_build))]
    Mode::Instant
}

/// Instant Mode needs Cap cloud sign-in; SayPlay builds drop it entirely.
pub fn coerce_ui_mode(mode: Mode) -> Mode {
    #[cfg(sayplay_build)]
    if mode == Mode::Instant {
        return Mode::Studio;
    }
    mode
}

pub fn instant_recording_enabled() -> bool {
    !cfg!(sayplay_build)
}

/// Cap cloud sign-in, link export, upload queue, and cap.so upsell flows.
pub fn cloud_features_enabled() -> bool {
    !cfg!(sayplay_build)
}

pub fn open_pricing(cx: &mut App) {
    if cfg!(sayplay_build) {
        return;
    }
    cx.open_url(crate::auth::PRICING_URL);
}

pub fn cycle_ui_mode(current: Mode) -> Mode {
    let modes = ui_modes();
    let current = coerce_ui_mode(current);
    let index = modes
        .iter()
        .position(|&mode| mode == current)
        .unwrap_or(0);
    modes[(index + 1) % modes.len()]
}

#[cfg(sayplay_build)]
fn normalize_stored_mode() {
    if matches!(
        crate::store::recording_mode_slug().as_deref(),
        Some("instant")
    ) {
        crate::store::set_recording_mode_slug("studio");
    }
}

#[cfg(not(sayplay_build))]
fn normalize_stored_mode() {}

/// One-time store seeds so teachers get Studio Mode with mic + system audio.
#[cfg(sayplay_build)]
pub fn apply_school_defaults() {
    use serde_json::Value;

    use crate::store::{
        GENERAL_SETTINGS, RECORDING_SETTINGS, set_recording_mode_slug, set_recording_system_audio,
        set_store_setting, store_section,
    };

    normalize_stored_mode();

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
    seed("disableAutoOpenLinks", Value::Bool(true));

    crate::store::migrate_sayplay_excluded_window_titles();
}

#[cfg(not(sayplay_build))]
pub fn apply_school_defaults() {}
