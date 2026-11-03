//! Which Windows API records the screen. Windows Graphics Capture is the
//! original one; Desktop Duplication (GDI for screenshots) is the backup for
//! PCs that refuse it. Other platforms have one API and ignore the choice.

use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CaptureMethod {
    /// Windows Graphics Capture, switching to the backup when Windows
    /// refuses it.
    #[default]
    Automatic,
    /// Windows Graphics Capture, failing rather than switching.
    GraphicsCapture,
    /// The backup only. It can't isolate a window, so window recordings
    /// still use Windows Graphics Capture.
    DesktopDuplication,
}

impl CaptureMethod {
    fn to_u8(self) -> u8 {
        match self {
            Self::Automatic => 0,
            Self::GraphicsCapture => 1,
            Self::DesktopDuplication => 2,
        }
    }

    fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::GraphicsCapture,
            2 => Self::DesktopDuplication,
            _ => Self::Automatic,
        }
    }

    /// `SAYPLAY_CAPTURE_BACKEND`: `duplication`/`dxgi` or `wgc`/`graphics`.
    fn from_env() -> Option<Self> {
        let value = std::env::var("SAYPLAY_CAPTURE_BACKEND").ok()?;
        match value.trim().to_ascii_lowercase().as_str() {
            "duplication" | "dxgi" => Some(Self::DesktopDuplication),
            "wgc" | "graphics" => Some(Self::GraphicsCapture),
            _ => None,
        }
    }
}

static CAPTURE_METHOD: AtomicU8 = AtomicU8::new(0);

/// The app's Recording method setting; applies to captures started after it.
pub fn set_capture_method(method: CaptureMethod) {
    CAPTURE_METHOD.store(method.to_u8(), Ordering::Relaxed);
}

/// The method captures use: `SAYPLAY_CAPTURE_BACKEND` when set, for
/// testing, else the setting.
pub fn capture_method() -> CaptureMethod {
    CaptureMethod::from_env()
        .unwrap_or_else(|| CaptureMethod::from_u8(CAPTURE_METHOD.load(Ordering::Relaxed)))
}

/// The API a screen recording actually started with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureApi {
    GraphicsCapture,
    DesktopDuplication,
}

/// 0 until a screen recording has started in this process.
static LAST_CAPTURE_API: AtomicU8 = AtomicU8::new(0);

/// The API the most recent screen recording started with, or `None` when
/// none has started since the app launched. A recording that switched to the
/// backup part-way reports the backup.
pub fn last_capture_api() -> Option<CaptureApi> {
    match LAST_CAPTURE_API.load(Ordering::Relaxed) {
        1 => Some(CaptureApi::GraphicsCapture),
        2 => Some(CaptureApi::DesktopDuplication),
        _ => None,
    }
}

#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub(crate) fn note_capture_api(api: CaptureApi) {
    let value = match api {
        CaptureApi::GraphicsCapture => 1,
        CaptureApi::DesktopDuplication => 2,
    };
    LAST_CAPTURE_API.store(value, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_method_round_trips() {
        for method in [
            CaptureMethod::Automatic,
            CaptureMethod::GraphicsCapture,
            CaptureMethod::DesktopDuplication,
        ] {
            assert_eq!(CaptureMethod::from_u8(method.to_u8()), method);
        }
        assert_eq!(CaptureMethod::from_u8(9), CaptureMethod::Automatic);
    }
}
