use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::core::{error::CoreError, merge::sanitize_filename, playlist::MediaPlaylist};

pub const TASK_DIRECTORY_NAME: &str = ".cat-catch-tasks";
pub const MANIFEST_FILE_NAME: &str = "manifest.json";
pub const DEBUG_DIRECTORY_NAME: &str = "_debug";
pub const TASK_REGISTRY_FILE_NAME: &str = "tasks.json";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskManifest {
    pub id: u64,
    pub source_url: String,
    pub output_name: String,
    pub output_directory: PathBuf,
    pub max_workers: usize,
    pub request_headers: HashMap<String, String>,
    pub playlist: Option<MediaPlaylist>,
    #[serde(default)]
    pub completed: bool,
    /// 被用户从列表清除的未完成任务，重启后不再自动续传。
    #[serde(default)]
    pub dismissed: bool,
    #[serde(default)]
    pub output_path: Option<PathBuf>,
}

impl TaskManifest {
    pub fn new(
        id: u64,
        source_url: &str,
        output_name: &str,
        output_directory: &Path,
        max_workers: usize,
        request_headers: HashMap<String, String>,
    ) -> Result<Self, CoreError> {
        let output_name = sanitize_filename(output_name);
        let task_directory = task_directory(output_directory, id, &output_name);
        fs::create_dir_all(&task_directory)
            .map_err(|_| CoreError::Io("创建任务临时目录失败".into()))?;
        let manifest = Self {
            id,
            source_url: source_url.to_string(),
            output_name,
            output_directory: output_directory.to_path_buf(),
            max_workers,
            request_headers,
            playlist: None,
            completed: false,
            dismissed: false,
            output_path: None,
        };
        manifest.save()?;
        Ok(manifest)
    }

    pub fn save(&self) -> Result<(), CoreError> {
        let content = serde_json::to_string_pretty(self)
            .map_err(|_| CoreError::Io("序列化任务信息失败".into()))?;
        fs::write(self.manifest_path(), content)
            .map_err(|_| CoreError::Io("保存任务信息失败".into()))
    }

    pub fn load(path: &Path) -> Result<Self, CoreError> {
        let content =
            fs::read_to_string(path).map_err(|_| CoreError::Io("读取任务信息失败".into()))?;
        serde_json::from_str(&content).map_err(|_| CoreError::Io("任务信息格式无效".into()))
    }

    pub fn task_directory(&self) -> PathBuf {
        task_directory(&self.output_directory, self.id, &self.output_name)
    }

    pub fn manifest_path(&self) -> PathBuf {
        self.task_directory().join(MANIFEST_FILE_NAME)
    }

    pub fn segment_path(&self, index: usize) -> PathBuf {
        self.task_directory()
            .join(format!("segment_{index:05}.seg"))
    }

    pub fn initialization_path(&self) -> PathBuf {
        self.task_directory().join("init.mp4")
    }

    /// 解密失败分片的归档目录，用于事后排查密钥或 IV 问题。
    pub fn debug_directory(&self) -> PathBuf {
        self.task_directory().join(DEBUG_DIRECTORY_NAME)
    }

    pub fn debug_path(&self, index: usize) -> PathBuf {
        self.debug_directory()
            .join(format!("undecrypted_{index:05}.bin"))
    }

    pub fn completed_segment_count(&self) -> Result<usize, CoreError> {
        let Some(playlist) = &self.playlist else {
            return Ok(0);
        };
        let mut completed = 0;
        for segment in &playlist.segments {
            if self.segment_path(segment.index).is_file() {
                completed += 1;
            }
        }
        Ok(completed)
    }

    /// 请求头的 JSON 文本形式，供界面编辑使用。
    pub fn request_headers_json(&self) -> String {
        serde_json::to_string(&self.request_headers).unwrap_or_default()
    }

    pub fn total_segment_count(&self) -> usize {
        self.playlist
            .as_ref()
            .map(|playlist| playlist.segments.len())
            .unwrap_or(0)
    }

    pub fn mark_completed(&mut self, output_path: PathBuf) -> Result<(), CoreError> {
        self.completed = true;
        self.output_path = Some(output_path);
        self.save()
    }

    /// 标记任务已被用户清除，避免重启后重新载入。
    pub fn mark_dismissed(&mut self) -> Result<(), CoreError> {
        self.dismissed = true;
        self.save()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TaskRegistry {
    pub directories: Vec<PathBuf>,
}

impl TaskRegistry {
    pub fn load(path: &Path) -> Result<Self, CoreError> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let content =
            fs::read_to_string(path).map_err(|_| CoreError::Io("读取任务注册表失败".into()))?;
        serde_json::from_str(&content).map_err(|_| CoreError::Io("任务注册表格式无效".into()))
    }

    pub fn register(path: &Path, output_directory: &Path) -> Result<(), CoreError> {
        let mut registry = Self::load(path).unwrap_or_default();
        let directory =
            fs::canonicalize(output_directory).unwrap_or_else(|_| output_directory.to_path_buf());
        if !registry.directories.contains(&directory) {
            registry.directories.push(directory);
            registry.directories.sort();
            registry.save(path)?;
        }
        Ok(())
    }

    fn save(&self, path: &Path) -> Result<(), CoreError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|_| CoreError::Io("创建任务注册表目录失败".into()))?;
        }
        let content = serde_json::to_string_pretty(self)
            .map_err(|_| CoreError::Io("序列化任务注册表失败".into()))?;
        fs::write(path, content).map_err(|_| CoreError::Io("保存任务注册表失败".into()))
    }
}

pub fn task_root(output_directory: &Path) -> PathBuf {
    output_directory.join(TASK_DIRECTORY_NAME)
}

pub fn task_directory(output_directory: &Path, id: u64, output_name: &str) -> PathBuf {
    task_root(output_directory).join(format!("{id:016}-{output_name}"))
}

pub fn discover_task_manifests(output_directory: &Path) -> Vec<TaskManifest> {
    let root = task_root(output_directory);
    let Ok(entries) = fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut manifests = Vec::new();
    for entry in entries.flatten() {
        let manifest_path = entry.path().join(MANIFEST_FILE_NAME);
        if let Ok(manifest) = TaskManifest::load(&manifest_path) {
            manifests.push(manifest);
        }
    }
    manifests.sort_by_key(|manifest| manifest.id);
    manifests
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_and_reloads_manifest() {
        let directory = std::env::temp_dir().join(format!(
            "cat-catch-manifest-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let manifest = TaskManifest::new(
            7,
            "https://example.com/a.m3u8",
            "视频/名称",
            &directory,
            2,
            HashMap::new(),
        )
        .unwrap();
        assert_eq!(manifest.output_name, "视频_名称");
        let loaded = TaskManifest::load(&manifest.manifest_path()).unwrap();
        assert_eq!(loaded.id, 7);
        assert_eq!(loaded.output_name, "视频_名称");
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn registers_and_reloads_output_directories() {
        let directory = std::env::temp_dir().join(format!(
            "cat-catch-registry-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let registry_path = directory.join(TASK_REGISTRY_FILE_NAME);
        TaskRegistry::register(&registry_path, &directory).unwrap();
        TaskRegistry::register(&registry_path, &directory).unwrap();
        let registry = TaskRegistry::load(&registry_path).unwrap();
        assert_eq!(
            registry.directories,
            vec![directory.canonicalize().unwrap()]
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn persists_completion_metadata() {
        let directory = std::env::temp_dir().join(format!(
            "cat-catch-completion-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut manifest = TaskManifest::new(
            9,
            "https://example.com/a.m3u8",
            "completed",
            &directory,
            2,
            HashMap::new(),
        )
        .unwrap();
        let output = directory.join("completed.mp4");
        manifest.mark_completed(output.clone()).unwrap();
        let loaded = TaskManifest::load(&manifest.manifest_path()).unwrap();
        assert!(loaded.completed);
        assert_eq!(loaded.output_path, Some(output));
        std::fs::remove_dir_all(directory).unwrap();
    }
}
