//! Explicit, revision-checked project file operations for the opt-in MCP bridge.

use crate::{
    commands::{allow_project_media_assets, with_session},
    project::{self, MAX_PROJECT_BYTES, ProjectSession},
    recent, recovery,
};
use serde::Deserialize;
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};
use tauri::AppHandle;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectPathArgs {
    pub project_id: String,
    pub revision: u64,
    pub path: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImportPathsArgs {
    pub project_id: String,
    pub revision: u64,
    pub paths: Vec<String>,
}

pub(crate) fn validate_context(
    session: &ProjectSession,
    project_id: &str,
    revision: u64,
) -> Result<(), String> {
    if session.project.project_id != project_id || session.revision != revision {
        return Err("Project state changed; refresh the state before retrying".into());
    }
    if revision >= project::MAX_SAFE_INTEGER {
        return Err("Invalid project revision".into());
    }
    Ok(())
}

fn validate_absolute_path(path: &Path) -> Result<(), String> {
    let text = path.to_str().ok_or("Path must be valid Unicode")?;
    if !path.is_absolute() || text.len() > 32768 || text.chars().any(char::is_control) {
        return Err("An absolute filesystem path of at most 32768 bytes is required".into());
    }
    #[cfg(windows)]
    if let Some(std::path::Component::Prefix(prefix)) = path.components().next() {
        if matches!(
            prefix.kind(),
            std::path::Prefix::DeviceNS(_) | std::path::Prefix::Verbatim(_)
        ) {
            return Err("Windows device paths are not supported".into());
        }
    }
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("Path must name a file")?;
    if name
        .chars()
        .any(|character| matches!(character, ':' | '<' | '>' | '"' | '|' | '?' | '*'))
        || name.ends_with([' ', '.'])
    {
        return Err("Unsupported filename".into());
    }
    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.len() == 4
            && matches!(stem.as_bytes()[3], b'1'..=b'9'))
    {
        return Err("Reserved device filenames are not supported".into());
    }
    Ok(())
}

fn open_session(
    session: &mut ProjectSession,
    project_id: &str,
    revision: u64,
    path: &Path,
) -> Result<(), String> {
    validate_context(session, project_id, revision)?;
    if session.dirty {
        return Err(
            "The current project has unsaved changes; save it before opening another project"
                .into(),
        );
    }
    validate_absolute_path(path)?;
    let loaded = project::read_project(path)?;
    let path_string = path
        .to_str()
        .ok_or("Project path must be valid Unicode")?
        .to_owned();
    let status = format!("已打开 {}", loaded.name);
    session.replace_project(loaded, Some(path_string), false);
    session.status = status;
    Ok(())
}

fn import_paths_session(
    session: &mut ProjectSession,
    project_id: &str,
    revision: u64,
    paths: &[String],
) -> Result<(), String> {
    validate_context(session, project_id, revision)?;
    if paths.is_empty() || paths.len() > 100 {
        return Err("Import requires between 1 and 100 media paths".into());
    }
    let mut validated = Vec::with_capacity(paths.len());
    for text in paths {
        let path = PathBuf::from(text);
        validate_absolute_path(&path)?;
        let extension = path
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or("");
        if project::media_kind(extension).is_none() {
            return Err("Unsupported media type; use mp4/mov/mp3/wav/png/jpg/jpeg".into());
        }
        let metadata =
            fs::metadata(&path).map_err(|error| format!("Cannot read requested media: {error}"))?;
        if !metadata.is_file() {
            return Err("Every imported media path must be a regular file".into());
        }
        validated.push(path);
    }
    // import_media constructs and validates its own candidate before replacing the project.
    let (added, skipped) = project::import_media(&mut session.project, validated)?;
    if added > 0 {
        session.dirty = true;
    }
    session.status = if skipped > 0 {
        format!("已导入 {added} 项素材，跳过 {skipped} 项重复文件")
    } else {
        format!("已导入 {added} 项素材")
    };
    Ok(())
}

fn save_as_session(
    session: &mut ProjectSession,
    project_id: &str,
    revision: u64,
    path: &Path,
) -> Result<(), String> {
    validate_context(session, project_id, revision)?;
    validate_absolute_path(path)?;
    project::require_project_extension(path)?;
    let path_string = path
        .to_str()
        .ok_or("Project path must be valid Unicode")?
        .to_owned();
    let parent = path
        .parent()
        .ok_or("Project destination requires an existing directory")?;
    if !parent.is_dir() {
        return Err("Project destination directory does not exist".into());
    }
    match fs::symlink_metadata(path) {
        Ok(_) => {
            return Err("Project destination already exists; choose a new .kaicut filename".into());
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("Cannot inspect project destination: {error}")),
    }
    let mut snapshot = session.project.clone();
    snapshot.updated_at = project::timestamp();
    snapshot.validate()?;
    let bytes = serde_json::to_vec_pretty(&snapshot)
        .map_err(|error| format!("Cannot serialize project: {error}"))?;
    if bytes.len() as u64 > MAX_PROJECT_BYTES {
        return Err("Project file exceeds 10 MiB".into());
    }
    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .map_err(|error| format!("Cannot create project temporary file: {error}"))?;
    temporary
        .write_all(&bytes)
        .map_err(|error| format!("Cannot write project: {error}"))?;
    temporary
        .flush()
        .map_err(|error| format!("Cannot flush project: {error}"))?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| format!("Cannot sync project: {error}"))?;
    // Even if a destination appears after the existence check, it must survive.
    temporary.persist_noclobber(path).map_err(|error| {
        format!(
            "Cannot publish project without overwriting a file: {}",
            error.error
        )
    })?;
    // No fallible work follows publication: the in-memory state can now commit.
    session.project = snapshot;
    session.path = Some(path_string);
    session.dirty = false;
    session.status = "项目已另存为新文件".into();
    Ok(())
}

pub async fn open_path(app: AppHandle, args: ProjectPathArgs) -> Result<ProjectSession, String> {
    with_session(app, move |app, session| {
        let path = Path::new(&args.path);
        open_session(session, &args.project_id, args.revision, path)?;
        allow_project_media_assets(app, &session.project);
        if let Err(error) = recent::record(app, path, &session.project) {
            log::warn!("无法更新最近项目：{error}");
        }
        Ok(())
    })
    .await
}

pub async fn import_paths(app: AppHandle, args: ImportPathsArgs) -> Result<ProjectSession, String> {
    with_session(app, move |app, session| {
        import_paths_session(session, &args.project_id, args.revision, &args.paths)?;
        allow_project_media_assets(app, &session.project);
        Ok(())
    })
    .await
}

pub async fn save_as(app: AppHandle, args: ProjectPathArgs) -> Result<ProjectSession, String> {
    with_session(app, move |app, session| {
        let path = Path::new(&args.path);
        save_as_session(session, &args.project_id, args.revision, path)?;
        if let Err(error) = recent::record(app, path, &session.project) {
            log::warn!("无法更新最近项目：{error}");
        }
        recovery::clear_for_project(app, &session.project.project_id);
        Ok(())
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::read_project;

    #[test]
    fn save_as_writes_an_openable_project_and_clears_dirty_only_on_success() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("新项目.kaicut");
        let mut session = ProjectSession::default();
        session.dirty = true;
        let id = session.project.project_id.clone();
        save_as_session(&mut session, &id, 0, &path).unwrap();
        assert!(!session.dirty);
        assert_eq!(read_project(&path).unwrap(), session.project);
        assert_eq!(session.path.as_deref(), path.to_str());
        let mut opened = ProjectSession::default();
        let opened_id = opened.project.project_id.clone();
        open_session(&mut opened, &opened_id, 0, &path).unwrap();
        assert_eq!(opened.project, session.project);
        assert_eq!(opened.revision, 1);
        assert!(!opened.dirty);
        assert!(!opened.can_undo);
    }

    #[test]
    fn project_operations_reject_stale_context_without_touching_state_or_files() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("project.kaicut");
        let media = directory.path().join("clip.mp4");
        fs::write(&media, b"source").unwrap();
        let mut session = ProjectSession::default();
        let id = session.project.project_id.clone();
        let before = session.clone();
        assert!(save_as_session(&mut session, &id, 1, &destination).is_err());
        assert!(open_session(&mut session, "wrong-project", 0, &destination).is_err());
        assert!(
            import_paths_session(
                &mut session,
                &id,
                1,
                &[media.to_string_lossy().into_owned()]
            )
            .is_err()
        );
        assert_eq!(session, before);
        assert!(!destination.exists());
        assert_eq!(fs::read(media).unwrap(), b"source");
    }

    #[test]
    fn opening_never_discards_dirty_state_and_rejects_invalid_projects() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("invalid.kaicut");
        fs::write(&path, b"{\"schemaVersion\":99}").unwrap();
        let mut session = ProjectSession::default();
        let id = session.project.project_id.clone();
        let clean = session.clone();
        assert!(open_session(&mut session, &id, 0, &path).is_err());
        assert_eq!(session, clean);
        session.dirty = true;
        let dirty = session.clone();
        assert!(
            open_session(&mut session, &id, 0, &path)
                .unwrap_err()
                .contains("unsaved")
        );
        assert_eq!(session, dirty);
        fs::write(&path, serde_json::to_vec(&session.project).unwrap()).unwrap();
        assert!(open_session(&mut session, &id, 0, &path).is_err());
        assert_eq!(session, dirty);
    }

    #[test]
    fn paths_must_be_absolute_and_correctly_typed_with_existing_parent() {
        let directory = tempfile::tempdir().unwrap();
        let mut session = ProjectSession::default();
        let id = session.project.project_id.clone();
        let before = session.clone();
        for path in [
            PathBuf::from("relative.kaicut"),
            directory.path().join("bad.json"),
            directory.path().join("missing/project.kaicut"),
        ] {
            assert!(save_as_session(&mut session, &id, 0, &path).is_err());
            assert_eq!(session, before);
        }
        assert!(!directory.path().join("missing").exists());
        for path in [
            PathBuf::from("relative.kaicut"),
            directory.path().join("absent.kaicut"),
            directory.path().join("bad.json"),
        ] {
            assert!(open_session(&mut session, &id, 0, &path).is_err());
            assert_eq!(session, before);
        }
        assert!(validate_absolute_path(&directory.path().join("NUL.kaicut")).is_err());
        assert!(validate_absolute_path(&directory.path().join("project:stream.kaicut")).is_err());
    }

    #[test]
    fn import_validates_every_path_before_mutating_and_deduplicates_repeat_imports() {
        let directory = tempfile::tempdir().unwrap();
        let media = directory.path().join("片段.mp4");
        let unknown = directory.path().join("not-media.txt");
        let missing = directory.path().join("missing.wav");
        let folder = directory.path().join("folder.mp4");
        fs::write(&media, b"source").unwrap();
        fs::write(&unknown, b"text").unwrap();
        fs::create_dir(&folder).unwrap();
        let mut session = ProjectSession::default();
        let id = session.project.project_id.clone();
        let before = session.clone();
        for invalid in [&unknown, &missing, &folder] {
            assert!(
                import_paths_session(
                    &mut session,
                    &id,
                    0,
                    &[
                        media.to_string_lossy().into_owned(),
                        invalid.to_string_lossy().into_owned()
                    ]
                )
                .is_err()
            );
            assert_eq!(session, before);
        }
        assert!(import_paths_session(&mut session, &id, 0, &[]).is_err());
        assert!(
            import_paths_session(
                &mut session,
                &id,
                0,
                &vec![media.to_string_lossy().into_owned(); 101]
            )
            .is_err()
        );
        assert!(import_paths_session(&mut session, &id, 0, &["relative.mp4".into()]).is_err());
        assert_eq!(session, before);
        let paths = [media.to_string_lossy().into_owned()];
        import_paths_session(&mut session, &id, 0, &paths).unwrap();
        assert_eq!(session.project.media_refs.len(), 1);
        assert!(session.dirty);
        let imported = session.project.clone();
        import_paths_session(&mut session, &id, 0, &paths).unwrap();
        assert_eq!(session.project, imported);
        assert!(session.status.contains("跳过 1"));
        assert_eq!(fs::read(media).unwrap(), b"source");
    }

    #[test]
    fn save_as_preserves_existing_file_directory_hardlink_and_session() {
        let directory = tempfile::tempdir().unwrap();
        let original = directory.path().join("original.kaicut");
        let alias = directory.path().join("alias.kaicut");
        let folder = directory.path().join("folder.kaicut");
        fs::write(&original, b"must survive").unwrap();
        fs::hard_link(&original, &alias).unwrap();
        fs::create_dir(&folder).unwrap();
        let mut session = ProjectSession::default();
        session.dirty = true;
        let id = session.project.project_id.clone();
        let before = session.clone();
        for path in [&original, &alias, &folder] {
            assert!(save_as_session(&mut session, &id, 0, path).is_err());
            assert_eq!(session, before);
        }
        assert_eq!(fs::read(&original).unwrap(), b"must survive");
        assert_eq!(fs::read(&alias).unwrap(), b"must survive");
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 3);
    }

    #[test]
    fn invalid_project_cannot_leave_a_new_file_or_clear_dirty_state() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("invalid.kaicut");
        let mut session = ProjectSession::default();
        session.dirty = true;
        session.project.schema_version = 99;
        let id = session.project.project_id.clone();
        let before = session.clone();
        assert!(save_as_session(&mut session, &id, 0, &path).is_err());
        assert!(!path.exists());
        assert_eq!(session, before);
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn save_as_never_follows_existing_or_dangling_symlinks() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.kaicut");
        let link = directory.path().join("link.kaicut");
        fs::write(&source, b"source").unwrap();
        std::os::unix::fs::symlink(&source, &link).unwrap();
        let mut session = ProjectSession::default();
        let id = session.project.project_id.clone();
        assert!(save_as_session(&mut session, &id, 0, &link).is_err());
        fs::remove_file(&source).unwrap();
        assert!(save_as_session(&mut session, &id, 0, &link).is_err());
        assert!(fs::symlink_metadata(link).unwrap().file_type().is_symlink());
    }
}
