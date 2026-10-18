use crate::{
    commands::with_session,
    project::{KaicutProject, MAX_PROJECT_BYTES, parse_project_value},
};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{Read, Write},
    path::PathBuf,
};
use tauri::{AppHandle, Manager};
use tempfile::NamedTempFile;

const RECOVERY_FORMAT_VERSION: u32 = 1;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RecoveryEnvelope {
    format_version: u32,
    saved_at: String,
    project_path: Option<String>,
    #[serde(deserialize_with = "deserialize_project")]
    project: KaicutProject,
}

fn deserialize_project<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<KaicutProject, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    parse_project_value(value).map_err(serde::de::Error::custom)
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryStatus {
    pub available: bool,
    pub project_id: Option<String>,
    pub project_name: Option<String>,
    pub saved_at: Option<String>,
}

fn recovery_dir(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(app
        .path()
        .app_local_data_dir()
        .map_err(|error| format!("无法定位应用数据目录：{error}"))?
        .join("recovery"))
}

fn recovery_path(app: &AppHandle, project_id: &str) -> Result<PathBuf, String> {
    Ok(recovery_dir(app)?.join(format!("{project_id}.json")))
}

fn read_latest(app: &AppHandle) -> Result<Option<(PathBuf, RecoveryEnvelope)>, String> {
    let directory = recovery_dir(app)?;
    if !directory.is_dir() {
        return Ok(None);
    }
    let mut latest: Option<(PathBuf, RecoveryEnvelope)> = None;
    for entry in fs::read_dir(&directory).map_err(|error| format!("无法读取恢复目录：{error}"))?
    {
        let entry = entry.map_err(|error| format!("无法读取恢复文件列表：{error}"))?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let file = match fs::File::open(&path) {
            Ok(file) => file,
            Err(_) => continue,
        };
        let mut bytes = Vec::new();
        // The envelope adds a small path/timestamp overhead to the bounded project JSON.
        let limit = MAX_PROJECT_BYTES + 65_536;
        if file.take(limit + 1).read_to_end(&mut bytes).is_err() || bytes.len() as u64 > limit {
            continue;
        }
        let envelope: RecoveryEnvelope = match serde_json::from_slice::<RecoveryEnvelope>(&bytes) {
            Ok(envelope) if envelope.format_version == RECOVERY_FORMAT_VERSION => envelope,
            _ => continue,
        };
        if envelope.project.validate().is_err() {
            continue;
        }
        if latest
            .as_ref()
            .is_none_or(|(_, current)| envelope.saved_at > current.saved_at)
        {
            latest = Some((path, envelope));
        }
    }
    Ok(latest)
}

fn write_recovery(path: &PathBuf, envelope: &RecoveryEnvelope) -> Result<(), String> {
    let parent = path.parent().ok_or_else(|| "恢复目录无效".to_string())?;
    fs::create_dir_all(parent).map_err(|error| format!("无法创建恢复目录：{error}"))?;
    let bytes = serde_json::to_vec_pretty(envelope)
        .map_err(|error| format!("无法序列化恢复快照：{error}"))?;
    let mut temporary =
        NamedTempFile::new_in(parent).map_err(|error| format!("无法创建恢复临时文件：{error}"))?;
    temporary
        .write_all(&bytes)
        .map_err(|error| format!("写入恢复快照失败：{error}"))?;
    temporary
        .flush()
        .map_err(|error| format!("刷新恢复快照失败：{error}"))?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| format!("同步恢复快照失败：{error}"))?;
    temporary
        .persist(path)
        .map_err(|error| format!("替换恢复快照失败：{}", error.error))?;
    Ok(())
}

#[tauri::command]
pub async fn project_recovery_checkpoint(app: AppHandle) -> Result<(), String> {
    let session = with_session(app.clone(), |_, _| Ok(())).await?;
    let path = recovery_path(&app, &session.project.project_id)?;
    if !session.dirty {
        let _ = fs::remove_file(path);
        return Ok(());
    }
    let envelope = RecoveryEnvelope {
        format_version: RECOVERY_FORMAT_VERSION,
        saved_at: crate::project::timestamp(),
        project_path: session.path,
        project: session.project,
    };
    tauri::async_runtime::spawn_blocking(move || write_recovery(&path, &envelope))
        .await
        .map_err(|error| format!("恢复快照未能完成：{error}"))??;
    Ok(())
}

#[tauri::command]
pub async fn project_recovery_status(app: AppHandle) -> Result<RecoveryStatus, String> {
    let latest = read_latest(&app)?;
    Ok(match latest {
        Some((_, envelope)) => RecoveryStatus {
            available: true,
            project_id: Some(envelope.project.project_id),
            project_name: Some(envelope.project.name),
            saved_at: Some(envelope.saved_at),
        },
        None => RecoveryStatus {
            available: false,
            project_id: None,
            project_name: None,
            saved_at: None,
        },
    })
}

#[tauri::command]
pub async fn project_recovery_restore(
    app: AppHandle,
) -> Result<crate::project::ProjectSession, String> {
    let Some((_, envelope)) = read_latest(&app)? else {
        return Err("没有可恢复的项目快照".into());
    };
    with_session(app, move |app, session| {
        session.replace_project(envelope.project, envelope.project_path, true);
        crate::commands::allow_project_media_assets(app, &session.project);
        session.status = "已恢复自动保存项目，尚未保存".into();
        Ok(())
    })
    .await
}

#[tauri::command]
pub async fn project_recovery_discard(app: AppHandle) -> Result<(), String> {
    if let Some((path, _)) = read_latest(&app)? {
        fs::remove_file(path).map_err(|error| format!("无法删除恢复快照：{error}"))?;
    }
    Ok(())
}

pub fn clear_for_project(app: &AppHandle, project_id: &str) {
    if let Ok(path) = recovery_path(app, project_id) {
        let _ = fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::{RECOVERY_FORMAT_VERSION, RecoveryEnvelope};
    use crate::project::KaicutProject;

    #[test]
    fn recovery_envelope_roundtrips_project_references_only() {
        let envelope = RecoveryEnvelope {
            format_version: RECOVERY_FORMAT_VERSION,
            saved_at: crate::project::timestamp(),
            project_path: Some("E:/项目/示例.kaicut".into()),
            project: KaicutProject::new(),
        };
        let bytes = serde_json::to_vec(&envelope).unwrap();
        let parsed: RecoveryEnvelope = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(parsed.format_version, RECOVERY_FORMAT_VERSION);
        assert_eq!(parsed.project_path.as_deref(), Some("E:/项目/示例.kaicut"));
        assert!(parsed.project.media_refs.is_empty());
    }

    #[test]
    fn recovery_envelope_rejects_unknown_fields() {
        let mut value = serde_json::to_value(RecoveryEnvelope {
            format_version: RECOVERY_FORMAT_VERSION,
            saved_at: crate::project::timestamp(),
            project_path: None,
            project: KaicutProject::new(),
        })
        .unwrap();
        value["unexpected"] = true.into();
        assert!(serde_json::from_value::<RecoveryEnvelope>(value).is_err());
    }

    #[test]
    fn legacy_recovery_uses_the_same_project_migration_and_validation() {
        let mut value = serde_json::json!({
            "formatVersion": 1, "savedAt": crate::project::timestamp(), "projectPath": null,
            "project": KaicutProject::new()
        });
        value["project"]["schemaVersion"] = 1.into();
        let restored: RecoveryEnvelope = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(restored.project.schema_version, 2);
        value["project"]["tracks"][0]["clips"] = serde_json::json!([{}]);
        assert!(serde_json::from_value::<RecoveryEnvelope>(value).is_err());
    }
}
