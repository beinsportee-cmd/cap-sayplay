//! "Share project" in the editor, and opening a `.sayplay` file from the
//! Recordings page or a double-click in Explorer: the dialogs and windows
//! around [`crate::project_file`].

use std::{
    path::{Path, PathBuf},
    sync::{Mutex, PoisonError},
};

use gpui::{App, AsyncApp, AsyncWindowContext, Task};

use crate::{
    i18n::tr,
    project_file::{self, ProjectFileError},
};

const FILTER_EXTENSIONS: &[&str] = &[project_file::EXTENSION];

/// Files being unpacked right now, so a second double-click while the first
/// is still copying doesn't import the project twice.
static OPENING: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// Catalogue ids are written with `/` whatever the platform.
fn bundled_wallpaper_id(path: &str) -> Option<&'static str> {
    crate::editor_sidebar::wallpaper_id_for_path(&path.replace('\\', "/"))
}

fn local_wallpaper(path: &str) -> Option<String> {
    let path = crate::editor_sidebar::wallpaper_path(bundled_wallpaper_id(path)?)?;
    Some(path.to_string_lossy().into_owned())
}

/// The save dialog, suggesting the project's name. Always ends in
/// `.sayplay`, so a name typed without it still opens with a double-click.
pub async fn choose_destination(pretty_name: &str, cx: &mut AsyncWindowContext) -> Option<PathBuf> {
    let suggested = format!(
        "{}.{}",
        crate::import::sanitize_filename(pretty_name.trim()),
        project_file::EXTENSION
    );
    #[cfg(target_os = "windows")]
    let chosen = {
        let _ = cx;
        rfd::FileDialog::new()
            .set_file_name(&suggested)
            .add_filter(tr::project_file::filter(), FILTER_EXTENSIONS)
            .save_file()
    };
    #[cfg(not(target_os = "windows"))]
    let chosen = crate::platform::save_file_panel_async(&suggested, FILTER_EXTENSIONS, cx).await;
    chosen.map(|path| {
        if project_file::has_project_file_extension(&path) {
            return path;
        }
        let mut name = path.into_os_string();
        name.push(".");
        name.push(project_file::EXTENSION);
        name.into()
    })
}

/// Packs the project into `dest`. Blocking; run it off the UI thread.
pub fn write_project_file(project: &Path, dest: &Path) -> Result<(), String> {
    let is_bundled_wallpaper = |path: &str| bundled_wallpaper_id(path).is_some();
    project_file::write_project_file(
        project,
        dest,
        &project_file::new_share_id(),
        &is_bundled_wallpaper,
    )
    .map_err(|error| {
        tracing::warn!(?error, dest = %dest.display(), "could not write a project file");
        let detail = match error {
            ProjectFileError::Io(error) => error.to_string(),
            ProjectFileError::NotAProject => "recording-meta.json could not be read".to_string(),
            error => format!("{error:?}"),
        };
        tr::project_file::share_failed(detail)
    })
}

/// The Recordings page's "Open project" picker.
pub async fn pick_project_file(cx: &mut AsyncApp) -> Option<PathBuf> {
    crate::import::pick_import_file(&[(tr::project_file::filter(), FILTER_EXTENSIONS)], cx).await
}

fn open_error_message(error: ProjectFileError) -> String {
    match error {
        ProjectFileError::NotAProject => {
            tr::settings::recordings::open_project_invalid().to_string()
        }
        ProjectFileError::InProgress => {
            tr::settings::recordings::open_project_in_progress().to_string()
        }
        ProjectFileError::Instant => tr::settings::recordings::open_project_instant().to_string(),
        ProjectFileError::Io(error) => tr::settings::recordings::open_project_copy_failed(error),
    }
}

/// Unpacks a `.sayplay` file into the library (or finds the copy an earlier
/// open made) and opens it in the editor, asking first for a failed
/// recording the way the Recordings page's Edit button does. Errors are
/// shown here; the task ends once the editor is on its way.
pub fn open_project_file(path: PathBuf, cx: &mut App) -> Task<()> {
    let key = dunce::canonicalize(&path).unwrap_or_else(|_| path.clone());
    {
        let mut opening = OPENING.lock().unwrap_or_else(PoisonError::into_inner);
        if opening.contains(&key) {
            return Task::ready(());
        }
        opening.push(key.clone());
    }
    let recordings_dir = crate::recording::recordings_dir();
    let library_dirs = crate::library::known_recordings_dirs();
    cx.spawn(async move |cx| {
        let result = cx
            .background_executor()
            .spawn(async move {
                project_file::unpack_project_file(
                    &path,
                    &recordings_dir,
                    &library_dirs,
                    &local_wallpaper,
                )
            })
            .await;
        OPENING
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|opening| *opening != key);
        let project = match result {
            Ok(project) => project,
            Err(error) => {
                let message = open_error_message(error);
                tracing::warn!(%message, "could not open a project file");
                crate::platform::alert_dialog(
                    tr::settings::recordings::open_project_error_title(),
                    &message,
                );
                return;
            }
        };
        tracing::info!(path = %project.path.display(), "opened a project file");
        cx.update(crate::import::refresh_libraries);
        if project.failed
            && !crate::platform::confirm_dialog(
                tr::settings::recordings::corrupted_title(),
                tr::settings::recordings::corrupted_body(),
                tr::settings::prompt::ok(),
                tr::common::cancel(),
                true,
            )
        {
            return;
        }
        let path = project.path;
        cx.update(|cx| cx.defer(move |cx| crate::app_windows::open_editor(path, cx)));
    })
}
