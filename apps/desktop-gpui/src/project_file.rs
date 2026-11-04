//! `.sayplay` project files: a whole studio project in one file, so a teacher
//! can send it like any document and the receiver opens it with a
//! double-click. The file is a zip archive of the project folder, stored
//! uncompressed (the videos inside already are), so any unzip tool can read
//! it too.
//!
//! No gpui in here: [`crate::share_project`] runs these off the UI thread and
//! turns the errors into dialogs.

use std::{
    fs::File,
    io::{self, BufReader, BufWriter, Read, Seek, Write},
    path::{Component, Path, PathBuf},
};

use cap_project::{RecordingMeta, RecordingMetaInner, StudioRecordingStatus};
use serde_json::Value;

pub const EXTENSION: &str = "sayplay";

const META_FILE: &str = "recording-meta.json";
const CONFIG_FILE: &str = "project-config.json";
/// Written into every unpacked copy, so opening the same file again reopens
/// that copy instead of importing a duplicate.
const SHARE_ID_FILE: &str = ".sayplay-share-id";
/// Background pictures that lived outside the project on the sender's PC.
const ASSETS_DIR: &str = "shared-assets";
/// Renders the editor writes into the project; not part of the project.
const OUTPUT_DIR: &str = "output";
/// Far above any real metadata or project config; stops a crafted file from
/// making the reader allocate gigabytes.
const MAX_JSON_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug)]
pub enum ProjectFileError {
    /// Not a `.sayplay` file, or a damaged one.
    NotAProject,
    InProgress,
    Instant,
    Io(io::Error),
}

impl From<io::Error> for ProjectFileError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// A project ready for the editor; `failed` asks the caller to confirm
/// first, the way the Recordings page's Edit button does.
#[derive(Debug)]
pub struct Unpacked {
    pub path: PathBuf,
    pub failed: bool,
}

pub fn has_project_file_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case(EXTENSION))
}

/// The project file a command-line argument names, as Explorer passes it
/// when a `.sayplay` file is double-clicked.
pub fn project_file_argument(argument: &str) -> Option<PathBuf> {
    let path = std::path::absolute(argument).ok()?;
    (has_project_file_extension(&path) && path.is_file()).then_some(path)
}

/// A fresh id for [`SHARE_ID_FILE`]: 32 hex digits.
pub fn new_share_id() -> String {
    use std::hash::{BuildHasher, RandomState};

    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    let state = RandomState::new();
    let half = |part: u8| state.hash_one((nanos, std::process::id(), part));
    format!("{:016x}{:016x}", half(0), half(1))
}

fn is_share_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// `Ok(true)` for a failed recording, which the editor opens after asking.
fn editable(meta: &RecordingMeta) -> Result<bool, ProjectFileError> {
    let RecordingMetaInner::Studio(studio) = &meta.inner else {
        return Err(ProjectFileError::Instant);
    };
    match studio.status() {
        StudioRecordingStatus::InProgress => Err(ProjectFileError::InProgress),
        StudioRecordingStatus::Failed { .. } => Ok(true),
        StudioRecordingStatus::Complete | StudioRecordingStatus::NeedsRemux => Ok(false),
    }
}

// ---------------------------------------------------------------------------
// Writing
// ---------------------------------------------------------------------------

/// Packs `project` into `dest`, replacing it. `is_bundled_wallpaper` tells
/// the app's own wallpapers, which every install has, from pictures that
/// must travel inside the file. Blocking; run it off the UI thread.
pub fn write_project_file(
    project: &Path,
    dest: &Path,
    share_id: &str,
    is_bundled_wallpaper: &dyn Fn(&str) -> bool,
) -> Result<(), ProjectFileError> {
    let meta =
        RecordingMeta::load_for_project(project).map_err(|_| ProjectFileError::NotAProject)?;
    editable(&meta)?;
    let meta_bytes = std::fs::read(project.join(META_FILE))?;
    let (config, assets) = match std::fs::read(project.join(CONFIG_FILE)) {
        Ok(bytes) => pack_config(bytes, is_bundled_wallpaper),
        Err(error) if error.kind() == io::ErrorKind::NotFound => (None, Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut files = Vec::new();
    collect_project_files(project, "", &mut files)?;
    files.extend(assets);

    let partial = partial_path(dest);
    let result = write_archive(&partial, &files, config.as_deref(), share_id, &meta_bytes)
        .and_then(|()| std::fs::rename(&partial, dest));
    if result.is_err() {
        let _ = std::fs::remove_file(&partial);
    }
    Ok(result?)
}

fn partial_path(dest: &Path) -> PathBuf {
    let mut name = dest.file_name().unwrap_or_default().to_os_string();
    name.push(".part");
    dest.with_file_name(name)
}

fn is_project_file_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name.ends_with(".sayplay") || name.ends_with(".sayplay.part")
}

/// Every file of the project but the metadata and the config (both written
/// separately) and what only makes sense on this PC: the dot-named recovery
/// state at the top level and the editor's renders. Project files saved
/// inside the project are left out too, or each share would nest the last.
fn collect_project_files(
    dir: &Path,
    prefix: &str,
    files: &mut Vec<(String, PathBuf)>,
) -> io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{} has a name that can't be shared", entry.path().display()),
            ));
        };
        if prefix.is_empty()
            && (name.starts_with('.')
                || name == OUTPUT_DIR
                || name == META_FILE
                || name == CONFIG_FILE)
        {
            continue;
        }
        let zip_name = format!("{prefix}{name}");
        if kind.is_dir() {
            collect_project_files(&entry.path(), &format!("{zip_name}/"), files)?;
        } else if kind.is_file() && !is_project_file_name(name) {
            files.push((zip_name, entry.path()));
        }
    }
    Ok(())
}

/// The config as it goes into the file, plus the pictures to add: every
/// background picture outside the project is stored under `shared-assets/`
/// and pointed at relatively. A config that isn't JSON goes in untouched.
fn pack_config(
    bytes: Vec<u8>,
    is_bundled_wallpaper: &dyn Fn(&str) -> bool,
) -> (Option<Vec<u8>>, Vec<(String, PathBuf)>) {
    let Ok(mut config) = serde_json::from_slice::<Value>(&bytes) else {
        return (Some(bytes), Vec::new());
    };
    let mut assets: Vec<(String, PathBuf)> = Vec::new();
    visit_background_paths(&mut config, &mut |kind, path| {
        if kind == "wallpaper" && is_bundled_wallpaper(path) {
            return false;
        }
        let source = PathBuf::from(path.as_str());
        if !source.is_absolute() || !source.is_file() {
            return false;
        }
        let name = match assets.iter().find(|(_, existing)| *existing == source) {
            Some((name, _)) => name.clone(),
            None => {
                let file = source
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("background");
                let name = format!("{ASSETS_DIR}/{}-{file}", assets.len());
                assets.push((name.clone(), source));
                name
            }
        };
        *path = name;
        true
    });
    if assets.is_empty() {
        return (Some(bytes), assets);
    }
    match serde_json::to_vec_pretty(&config) {
        Ok(packed) => (Some(packed), assets),
        Err(_) => (Some(bytes), Vec::new()),
    }
}

/// Points the app's own wallpapers in `config` at this PC's install wherever
/// the path it names doesn't exist here. Returns whether anything changed.
pub(crate) fn localize_wallpapers(
    config: &mut Value,
    local_wallpaper: &dyn Fn(&str) -> Option<String>,
) -> bool {
    visit_background_paths(config, &mut |kind, path| {
        if kind == "wallpaper"
            && !Path::new(path.as_str()).exists()
            && let Some(local) = local_wallpaper(path)
        {
            *path = local;
            true
        } else {
            false
        }
    })
}

/// Calls `visit` with the kind and path of every image or wallpaper
/// background: the project's own and any style segment's override. `visit`
/// returns whether it changed the path; so does this.
fn visit_background_paths(
    value: &mut Value,
    visit: &mut dyn FnMut(&str, &mut String) -> bool,
) -> bool {
    let mut changed = false;
    match value {
        Value::Object(object) => {
            let kind = object
                .get("type")
                .and_then(Value::as_str)
                .filter(|kind| matches!(*kind, "image" | "wallpaper"))
                .map(str::to_owned);
            if let Some(kind) = kind
                && let Some(Value::String(path)) = object.get_mut("path")
            {
                changed |= visit(&kind, path);
            }
            for child in object.values_mut() {
                changed |= visit_background_paths(child, visit);
            }
        }
        Value::Array(items) => {
            for item in items {
                changed |= visit_background_paths(item, visit);
            }
        }
        _ => {}
    }
    changed
}

fn write_archive(
    path: &Path,
    files: &[(String, PathBuf)],
    config: Option<&[u8]>,
    share_id: &str,
    meta: &[u8],
) -> io::Result<()> {
    let mut zip = zip::ZipWriter::new(BufWriter::new(File::create(path)?));
    for (name, source) in files {
        let mut source = File::open(source)?;
        let length = source.metadata()?.len();
        zip.start_file(name.as_str(), entry_options(length))
            .map_err(zip_error)?;
        io::copy(&mut source, &mut zip)?;
    }
    let small_files = config
        .map(|config| (CONFIG_FILE, config))
        .into_iter()
        .chain([(SHARE_ID_FILE, share_id.as_bytes()), (META_FILE, meta)]);
    for (name, bytes) in small_files {
        zip.start_file(name, entry_options(bytes.len() as u64))
            .map_err(zip_error)?;
        zip.write_all(bytes)?;
    }
    let mut writer = zip.finish().map_err(zip_error)?;
    writer.flush()?;
    writer
        .into_inner()
        .map_err(|error| error.into_error())?
        .sync_all()
}

fn entry_options(length: u64) -> zip::write::SimpleFileOptions {
    zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored)
        .large_file(length >= u64::from(u32::MAX))
}

fn zip_error(error: zip::result::ZipError) -> io::Error {
    match error {
        zip::result::ZipError::Io(error) => error,
        error => io::Error::other(error),
    }
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

/// Unpacks `source` into a new folder in `recordings_dir`, or finds the copy
/// an earlier open of the same file made in one of `library_dirs`.
/// `local_wallpaper` maps a wallpaper path from the sender's install to this
/// PC's. Blocking; run it off the UI thread.
pub fn unpack_project_file(
    source: &Path,
    recordings_dir: &Path,
    library_dirs: &[PathBuf],
    local_wallpaper: &dyn Fn(&str) -> Option<String>,
) -> Result<Unpacked, ProjectFileError> {
    let mut archive = zip::ZipArchive::new(BufReader::new(File::open(source)?))
        .map_err(|_| ProjectFileError::NotAProject)?;
    let meta_bytes =
        read_small_entry(&mut archive, META_FILE)?.ok_or(ProjectFileError::NotAProject)?;
    let meta: RecordingMeta =
        serde_json::from_slice(&meta_bytes).map_err(|_| ProjectFileError::NotAProject)?;
    let failed = editable(&meta)?;

    let share_id = read_small_entry(&mut archive, SHARE_ID_FILE)?
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .map(|id| id.trim().to_owned())
        .filter(|id| is_share_id(id));
    if let Some(id) = &share_id
        && let Some(copy) = find_unpacked_copy(library_dirs, id)
    {
        return Ok(copy);
    }

    std::fs::create_dir_all(recordings_dir)?;
    let dest = crate::import::unique_project_path(recordings_dir, &project_name(source, &meta));
    std::fs::create_dir(&dest)?;
    if let Err(error) = extract(&mut archive, &dest, &meta_bytes, local_wallpaper) {
        if let Err(cleanup) = std::fs::remove_dir_all(&dest) {
            tracing::warn!(%cleanup, path = %dest.display(), "could not remove a partly unpacked project");
        }
        return Err(error);
    }
    Ok(Unpacked { path: dest, failed })
}

/// The file's name, as the teacher saw it when they opened it.
fn project_name(source: &Path, meta: &RecordingMeta) -> String {
    let stem = source
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or_default();
    [
        stem.strip_suffix(".cap").unwrap_or(stem),
        meta.pretty_name.as_str(),
    ]
    .into_iter()
    .map(crate::import::sanitize_filename)
    .find(|name| !name.trim().is_empty())
    .unwrap_or_else(|| crate::sayplay::UNTITLED_RECORDING.to_string())
}

fn find_unpacked_copy(library_dirs: &[PathBuf], id: &str) -> Option<Unpacked> {
    library_dirs
        .iter()
        .filter_map(|dir| std::fs::read_dir(dir).ok())
        .flatten()
        .flatten()
        .find_map(|entry| {
            let path = entry.path();
            let marker = std::fs::read_to_string(path.join(SHARE_ID_FILE)).ok()?;
            if marker.trim() != id {
                return None;
            }
            let meta = RecordingMeta::load_for_project(&path).ok()?;
            let failed = editable(&meta).ok()?;
            Some(Unpacked { path, failed })
        })
}

fn read_small_entry<R: Read + Seek>(
    archive: &mut zip::ZipArchive<R>,
    name: &str,
) -> Result<Option<Vec<u8>>, ProjectFileError> {
    let mut entry = match archive.by_name(name) {
        Ok(entry) => entry,
        Err(zip::result::ZipError::FileNotFound) => return Ok(None),
        Err(_) => return Err(ProjectFileError::NotAProject),
    };
    read_json_entry(&mut entry)
        .map(Some)
        .map_err(|_| ProjectFileError::NotAProject)
}

fn read_json_entry(entry: &mut impl Read) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    entry.take(MAX_JSON_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_JSON_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "project metadata is too large",
        ));
    }
    Ok(bytes)
}

/// The metadata file goes last: the library lists any folder whose
/// `recording-meta.json` parses, so a half-unpacked copy stays invisible.
fn extract<R: Read + Seek>(
    archive: &mut zip::ZipArchive<R>,
    dest: &Path,
    meta: &[u8],
    local_wallpaper: &dyn Fn(&str) -> Option<String>,
) -> Result<(), ProjectFileError> {
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|_| ProjectFileError::NotAProject)?;
        if entry.is_dir() || entry.is_symlink() {
            continue;
        }
        let relative = entry.enclosed_name().ok_or(ProjectFileError::NotAProject)?;
        if relative == Path::new(META_FILE) {
            continue;
        }
        let target = dest.join(&relative);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if relative == Path::new(CONFIG_FILE) {
            let config = read_json_entry(&mut entry).map_err(|_| ProjectFileError::NotAProject)?;
            std::fs::write(&target, localize_config(config, dest, local_wallpaper))?;
        } else {
            io::copy(&mut entry, &mut File::create(&target)?)?;
        }
    }
    std::fs::write(dest.join(META_FILE), meta)?;
    Ok(())
}

/// The config as the unpacked copy needs it: shared pictures point into the
/// copy, and the app's own wallpapers into this PC's install.
fn localize_config(
    bytes: Vec<u8>,
    project: &Path,
    local_wallpaper: &dyn Fn(&str) -> Option<String>,
) -> Vec<u8> {
    let Ok(mut config) = serde_json::from_slice::<Value>(&bytes) else {
        return bytes;
    };
    let changed = visit_background_paths(&mut config, &mut |kind, path| {
        if let Some(asset) = shared_asset_path(path) {
            *path = asset
                .components()
                .fold(project.to_path_buf(), |dir, part| dir.join(part))
                .to_string_lossy()
                .into_owned();
            true
        } else if kind == "wallpaper"
            && !Path::new(path.as_str()).exists()
            && let Some(local) = local_wallpaper(path)
        {
            *path = local;
            true
        } else {
            false
        }
    });
    if !changed {
        return bytes;
    }
    serde_json::to_vec_pretty(&config).unwrap_or(bytes)
}

fn shared_asset_path(path: &str) -> Option<&Path> {
    let path = Path::new(path);
    (path.starts_with(ASSETS_DIR)
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_))))
    .then_some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    const META: &str = r#"{
        "platform": "Windows",
        "pretty_name": "Lesson 3",
        "sharing": null,
        "segments": [{
            "display": { "path": "content/segments/segment-0/display.mp4", "fps": 30, "start_time": 0.0 },
            "mic": { "path": "content/segments/segment-0/audio-input.m4a", "start_time": 0.0 }
        }],
        "cursors": {},
        "status": { "status": "Complete" }
    }"#;

    fn temp_dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn make_project(root: &Path, meta: &str, config: &str) -> PathBuf {
        let project = root.join("Lesson 3.cap");
        let segment = project.join("content/segments/segment-0");
        std::fs::create_dir_all(&segment).unwrap();
        std::fs::write(project.join(META_FILE), meta).unwrap();
        std::fs::write(project.join(CONFIG_FILE), config).unwrap();
        std::fs::write(segment.join("display.mp4"), b"display").unwrap();
        std::fs::write(segment.join("audio-input.m4a"), b"mic").unwrap();
        std::fs::create_dir_all(project.join(".recovery-1/staged")).unwrap();
        std::fs::write(project.join(".recovery-1/staged/x"), b"recovery").unwrap();
        std::fs::write(project.join(".recovery.lock"), b"").unwrap();
        std::fs::create_dir_all(project.join("output")).unwrap();
        std::fs::write(project.join("output/result.mp4"), b"render").unwrap();
        std::fs::write(project.join("Old share.sayplay"), b"old").unwrap();
        project
    }

    fn no_wallpapers(_: &str) -> bool {
        false
    }

    fn no_local_wallpaper(_: &str) -> Option<String> {
        None
    }

    fn archive_names(path: &Path) -> Vec<String> {
        let archive = zip::ZipArchive::new(File::open(path).unwrap()).unwrap();
        let mut names: Vec<String> = archive
            .file_names()
            .map(|name| name.unwrap().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn round_trip_keeps_the_project_and_drops_local_state() {
        let root = temp_dir();
        let project = make_project(
            root.path(),
            META,
            r#"{"background":{"source":{"type":"color"}}}"#,
        );
        let file = root.path().join("Shared Lesson.sayplay");
        write_project_file(&project, &file, &new_share_id(), &no_wallpapers).unwrap();
        assert!(!partial_path(&file).exists());

        let mut names = archive_names(&file);
        names.retain(|name| name != SHARE_ID_FILE);
        assert_eq!(
            names,
            [
                "content/segments/segment-0/audio-input.m4a",
                "content/segments/segment-0/display.mp4",
                CONFIG_FILE,
                META_FILE,
            ]
        );

        let library = root.path().join("library");
        let unpacked =
            unpack_project_file(&file, &library, &[library.clone()], &no_local_wallpaper).unwrap();
        assert!(!unpacked.failed);
        assert_eq!(unpacked.path, library.join("Shared Lesson.cap"));
        assert_eq!(
            std::fs::read(unpacked.path.join("content/segments/segment-0/display.mp4")).unwrap(),
            b"display"
        );
        assert_eq!(
            std::fs::read_to_string(unpacked.path.join(CONFIG_FILE)).unwrap(),
            r#"{"background":{"source":{"type":"color"}}}"#
        );
        let meta = RecordingMeta::load_for_project(&unpacked.path).unwrap();
        assert_eq!(meta.pretty_name, "Lesson 3");
    }

    #[test]
    fn opening_the_same_file_again_reopens_the_first_copy() {
        let root = temp_dir();
        let project = make_project(root.path(), META, "{}");
        let file = root.path().join("Lesson.sayplay");
        write_project_file(&project, &file, &new_share_id(), &no_wallpapers).unwrap();

        let library = root.path().join("library");
        let dirs = [library.clone()];
        let first = unpack_project_file(&file, &library, &dirs, &no_local_wallpaper).unwrap();
        let again = unpack_project_file(&file, &library, &dirs, &no_local_wallpaper).unwrap();
        assert_eq!(first.path, again.path);

        let resent = root.path().join("Lesson v2.sayplay");
        write_project_file(&first.path, &resent, &new_share_id(), &no_wallpapers).unwrap();
        let second = unpack_project_file(&resent, &library, &dirs, &no_local_wallpaper).unwrap();
        assert_ne!(first.path, second.path);
        assert_eq!(std::fs::read_dir(&library).unwrap().count(), 2);
    }

    #[test]
    fn a_second_copy_of_the_same_name_gets_a_number() {
        let root = temp_dir();
        let project = make_project(root.path(), META, "{}");
        let library = root.path().join("library");
        for _ in 0..2 {
            let file = root.path().join("Lesson.sayplay");
            write_project_file(&project, &file, &new_share_id(), &no_wallpapers).unwrap();
            unpack_project_file(&file, &library, &[], &no_local_wallpaper).unwrap();
        }
        assert!(library.join("Lesson.cap").is_dir());
        assert!(library.join("Lesson (1).cap").is_dir());
    }

    #[test]
    fn outside_pictures_travel_and_bundled_wallpapers_are_remapped() {
        let root = temp_dir();
        let picture = root.path().join("bg-1-photo.png");
        std::fs::write(&picture, b"png").unwrap();
        let sender_wallpaper =
            "C:/Program Files/SayPlay Recorder/assets/backgrounds/macOS/sequoia-dark.jpg";
        let config = serde_json::json!({
            "background": { "source": { "type": "image", "path": picture.to_string_lossy() } },
            "timeline": { "styleSegments": [
                { "overrides": { "background": { "source": { "type": "image", "path": picture.to_string_lossy() } } } },
                { "overrides": { "background": { "source": { "type": "wallpaper", "path": sender_wallpaper } } } },
                { "overrides": { "background": { "source": { "type": "image", "path": "/missing/picture.png" } } } }
            ] }
        });
        let project = make_project(root.path(), META, &config.to_string());
        let file = root.path().join("Lesson.sayplay");
        let bundled = |path: &str| path.contains("macOS/sequoia-dark");
        write_project_file(&project, &file, &new_share_id(), &bundled).unwrap();
        assert!(archive_names(&file).contains(&"shared-assets/0-bg-1-photo.png".to_string()));

        let library = root.path().join("library");
        let local = |path: &str| {
            path.contains("macOS/sequoia-dark")
                .then(|| "D:/SayPlay/assets/backgrounds/macOS/sequoia-dark.jpg".to_string())
        };
        let unpacked = unpack_project_file(&file, &library, &[], &local).unwrap();
        let config: Value =
            serde_json::from_slice(&std::fs::read(unpacked.path.join(CONFIG_FILE)).unwrap())
                .unwrap();
        let asset = unpacked.path.join(ASSETS_DIR).join("0-bg-1-photo.png");
        assert_eq!(std::fs::read(&asset).unwrap(), b"png");
        let asset = asset.to_string_lossy();
        assert_eq!(config["background"]["source"]["path"], *asset);
        let styles = &config["timeline"]["styleSegments"];
        assert_eq!(
            styles[0]["overrides"]["background"]["source"]["path"],
            *asset
        );
        assert_eq!(
            styles[1]["overrides"]["background"]["source"]["path"],
            "D:/SayPlay/assets/backgrounds/macOS/sequoia-dark.jpg"
        );
        assert_eq!(
            styles[2]["overrides"]["background"]["source"]["path"],
            "/missing/picture.png"
        );
    }

    #[test]
    fn a_camera_background_picture_travels_with_the_project() {
        let root = temp_dir();
        let picture = root.path().join("camera-bg-1-classroom.jpg");
        std::fs::write(&picture, b"jpg").unwrap();
        let sender_wallpaper =
            "C:/Program Files/SayPlay Recorder/assets/backgrounds/macOS/sequoia-dark.jpg";
        let camera_picture = |picture: Value| {
            let blur = serde_json::json!({ "mode": "heavy", "picture": picture });
            serde_json::json!({ "backgroundBlur": blur })
        };
        let config = serde_json::json!({
            "camera": camera_picture(
                serde_json::json!({ "type": "image", "path": picture.to_string_lossy() })
            ),
            "timeline": { "styleSegments": [
                { "overrides": { "camera": camera_picture(
                    serde_json::json!({ "type": "wallpaper", "path": sender_wallpaper })
                ) } }
            ] }
        });
        let project = make_project(root.path(), META, &config.to_string());
        let file = root.path().join("Lesson.sayplay");
        let bundled = |path: &str| path.contains("macOS/sequoia-dark");
        write_project_file(&project, &file, &new_share_id(), &bundled).unwrap();
        assert!(
            archive_names(&file).contains(&"shared-assets/0-camera-bg-1-classroom.jpg".to_string())
        );

        let library = root.path().join("library");
        let local = |path: &str| {
            path.contains("macOS/sequoia-dark")
                .then(|| "D:/SayPlay/assets/backgrounds/macOS/sequoia-dark.jpg".to_string())
        };
        let unpacked = unpack_project_file(&file, &library, &[], &local).unwrap();
        let asset = unpacked
            .path
            .join(ASSETS_DIR)
            .join("0-camera-bg-1-classroom.jpg");
        assert_eq!(std::fs::read(&asset).unwrap(), b"jpg");

        let raw: Value =
            serde_json::from_slice(&std::fs::read(unpacked.path.join(CONFIG_FILE)).unwrap())
                .unwrap();
        let blur: cap_project::BackgroundBlurConfig =
            serde_json::from_value(raw["camera"]["backgroundBlur"].clone()).unwrap();
        assert_eq!(
            blur.active_picture(),
            Some(&cap_project::CameraBackgroundPicture::Image {
                path: asset.to_string_lossy().into_owned()
            })
        );
        assert_eq!(
            raw["timeline"]["styleSegments"][0]["overrides"]["camera"]["backgroundBlur"]["picture"]
                ["path"],
            "D:/SayPlay/assets/backgrounds/macOS/sequoia-dark.jpg"
        );
    }

    #[test]
    fn recordings_that_cannot_be_edited_are_refused() {
        let root = temp_dir();
        let in_progress = META.replace("Complete", "InProgress");
        let project = make_project(root.path(), &in_progress, "{}");
        let file = root.path().join("Lesson.sayplay");
        assert!(matches!(
            write_project_file(&project, &file, &new_share_id(), &no_wallpapers),
            Err(ProjectFileError::InProgress)
        ));
        assert!(!file.exists());
    }

    #[test]
    fn failed_recordings_open_after_asking() {
        let root = temp_dir();
        let failed = META.replace(
            r#"{ "status": "Complete" }"#,
            r#"{ "status": "Failed", "error": "boom" }"#,
        );
        let project = make_project(root.path(), &failed, "{}");
        let file = root.path().join("Lesson.sayplay");
        write_project_file(&project, &file, &new_share_id(), &no_wallpapers).unwrap();
        let library = root.path().join("library");
        assert!(
            unpack_project_file(&file, &library, &[], &no_local_wallpaper)
                .unwrap()
                .failed
        );
    }

    fn write_raw_archive(path: &Path, entries: &[(&str, &[u8])]) {
        let mut zip = zip::ZipWriter::new(File::create(path).unwrap());
        for (name, bytes) in entries {
            zip.start_file(*name, entry_options(bytes.len() as u64))
                .unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap();
    }

    #[test]
    fn files_that_are_not_projects_are_refused_and_leave_nothing_behind() {
        let root = temp_dir();
        let library = root.path().join("library");
        let open = |path: &Path| unpack_project_file(path, &library, &[], &no_local_wallpaper);

        let text = root.path().join("notes.sayplay");
        std::fs::write(&text, b"not a zip").unwrap();
        assert!(matches!(open(&text), Err(ProjectFileError::NotAProject)));

        let no_meta = root.path().join("no-meta.sayplay");
        write_raw_archive(&no_meta, &[("content/x.mp4", b"x")]);
        assert!(matches!(open(&no_meta), Err(ProjectFileError::NotAProject)));

        let escaping = root.path().join("escaping.sayplay");
        write_raw_archive(
            &escaping,
            &[(META_FILE, META.as_bytes()), ("../outside.txt", b"x")],
        );
        assert!(matches!(
            open(&escaping),
            Err(ProjectFileError::NotAProject)
        ));
        assert!(!root.path().join("outside.txt").exists());
        assert_eq!(std::fs::read_dir(&library).unwrap().count(), 0);
    }

    #[test]
    fn only_existing_sayplay_files_are_project_arguments() {
        let root = temp_dir();
        let file = root.path().join("Lesson.SayPlay");
        std::fs::write(&file, b"").unwrap();
        assert_eq!(
            project_file_argument(file.to_str().unwrap()),
            Some(file.clone())
        );
        assert_eq!(
            project_file_argument(root.path().join("missing.sayplay").to_str().unwrap()),
            None
        );
        assert_eq!(project_file_argument("cap-desktop://action?value=1"), None);
        assert_eq!(project_file_argument(""), None);
    }

    #[test]
    fn share_ids_are_distinct_and_well_formed() {
        let first = new_share_id();
        assert!(is_share_id(&first));
        assert_ne!(first, new_share_id());
    }
}
