use crate::project::KaicutProject;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};
use tauri::{AppHandle, Manager};
use tempfile::NamedTempFile;

const MAX_RECENT_PROJECTS: usize = 12;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RecentProject {
    pub path: String,
    pub name: String,
    pub updated_at: String,
}

fn recent_path(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(app
        .path()
        .app_local_data_dir()
        .map_err(|error| format!("无法定位应用数据目录：{error}"))?
        .join("recent-projects.json"))
}

fn read_recent(app: &AppHandle) -> Result<Vec<RecentProject>, String> {
    let path = recent_path(app)?;
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let bytes = fs::read(&path).map_err(|error| format!("无法读取最近项目：{error}"))?;
    if bytes.len() > 256 * 1024 {
        return Err("最近项目列表文件过大".into());
    }
    serde_json::from_slice(&bytes).map_err(|error| format!("最近项目列表无效：{error}"))
}

fn write_recent(path: &Path, items: &[RecentProject]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "最近项目目录无效".to_string())?;
    fs::create_dir_all(parent).map_err(|error| format!("无法创建最近项目目录：{error}"))?;
    let bytes =
        serde_json::to_vec_pretty(items).map_err(|error| format!("无法序列化最近项目：{error}"))?;
    let mut temporary = NamedTempFile::new_in(parent)
        .map_err(|error| format!("无法创建最近项目临时文件：{error}"))?;
    temporary
        .write_all(&bytes)
        .map_err(|error| format!("写入最近项目失败：{error}"))?;
    temporary
        .flush()
        .map_err(|error| format!("刷新最近项目失败：{error}"))?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| format!("同步最近项目失败：{error}"))?;
    temporary
        .persist(path)
        .map_err(|error| format!("替换最近项目失败：{}", error.error))?;
    Ok(())
}

pub fn record(app: &AppHandle, path: &Path, project: &KaicutProject) -> Result<(), String> {
    let path_string = path
        .to_str()
        .ok_or("项目路径必须是有效 Unicode")?
        .to_string();
    let mut items = read_recent(app)?;
    items.retain(|item| item.path != path_string);
    items.insert(
        0,
        RecentProject {
            path: path_string,
            name: project.name.clone(),
            updated_at: project.updated_at.clone(),
        },
    );
    items.truncate(MAX_RECENT_PROJECTS);
    write_recent(&recent_path(app)?, &items)
}

pub fn list(app: &AppHandle) -> Result<Vec<RecentProject>, String> {
    read_recent(app)
}

#[cfg(test)]
mod tests {
    use super::RecentProject;

    #[test]
    fn recent_project_serializes_with_camel_case_fields() {
        let item = RecentProject {
            path: "E:/项目.kaicut".into(),
            name: "项目".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
        };
        let value = serde_json::to_value(item).unwrap();
        assert!(value.get("updatedAt").is_some());
        assert!(value.get("updated_at").is_none());
    }
}
