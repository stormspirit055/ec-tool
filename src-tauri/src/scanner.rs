//! 找出拖入路径里的任务目录和视频。
//!
//! 任务目录 = 含 `videos` 子目录的文件夹，例如 `整理后/橘子采摘/videos/top_left.mp4`。
//! 拖入任务目录本身、它的 `videos` 子目录、单个视频，或者上一级目录（里面每个任务都处理）都可以。

use std::fs;
use std::path::{Path, PathBuf};

pub const VIDEO_EXTENSIONS: [&str; 3] = ["mp4", "mov", "m4v"];

/// 一个待处理的视频
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoItem {
    pub folder: PathBuf,
    pub video: PathBuf,
}

impl VideoItem {
    /// 视角名：文件名去掉扩展名，例如 `top_left`
    pub fn view(&self) -> String {
        self.video.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
    }
}

/// 展开拖入的路径，返回所有视频（按任务目录、文件名排序，去重）
pub fn expand(paths: &[PathBuf]) -> Vec<VideoItem> {
    let mut folders = Vec::new();
    let mut singles = Vec::new();
    for path in paths {
        if path.is_file() {
            if is_video(path) {
                // 单个视频：所在的 videos 目录的上一级就是任务目录
                let parent = path.parent().unwrap_or(Path::new("."));
                let folder = if is_videos_dir(parent) { parent.parent().unwrap_or(parent) } else { parent };
                singles.push(VideoItem { folder: folder.to_path_buf(), video: path.clone() });
                // 拖入任一目时一并加载同目录的另一目，导入仍然只用于预览。
                if matches!(path.file_stem().and_then(|s| s.to_str()), Some("top_left" | "top_right")) {
                    singles.extend(visible_entries(parent)
                        .filter(|(p, dir)| !dir && is_video(p) && matches!(p.file_stem().and_then(|s| s.to_str()), Some("top_left" | "top_right")))
                        .map(|(video, _)| VideoItem { folder: folder.to_path_buf(), video }));
                }
            }
        } else if path.is_dir() {
            if is_videos_dir(path) {
                folders.push(path.parent().unwrap_or(path).to_path_buf());
            } else if videos_dir(path).is_some() {
                folders.push(path.clone());
            } else {
                folders.extend(find_task_folders(path));
            }
        }
    }
    let mut items: Vec<VideoItem> = folders
        .into_iter()
        .flat_map(|folder| videos(&folder).into_iter().map(move |video| VideoItem { folder: folder.clone(), video }))
        .chain(singles)
        .collect();
    items.sort_by(|a, b| (&a.folder, &a.video).cmp(&(&b.folder, &b.video)));
    items.dedup();
    items
}

/// 根目录下所有任务目录（遇到 `videos` 子目录就停止往下找）
pub fn find_task_folders(root: &Path) -> Vec<PathBuf> {
    let mut folders = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for (path, is_dir) in visible_entries(&dir) {
            if !is_dir {
                continue;
            }
            if is_videos_dir(&path) {
                folders.push(dir.clone());
            } else {
                stack.push(path);
            }
        }
    }
    folders.sort();
    folders.dedup();
    folders
}

/// 任务目录下 `videos` 子目录中的视频文件，按文件名排序
pub fn videos(folder: &Path) -> Vec<PathBuf> {
    let Some(dir) = videos_dir(folder) else { return Vec::new() };
    let mut items: Vec<PathBuf> = visible_entries(&dir)
        .filter(|(path, is_dir)| !is_dir && is_video(path))
        .map(|(path, _)| path)
        .collect();
    items.sort_by_key(|p| p.file_name().map(|n| n.to_os_string()));
    items
}

pub fn videos_dir(folder: &Path) -> Option<PathBuf> {
    visible_entries(folder)
        .find(|(path, is_dir)| *is_dir && is_videos_dir(path))
        .map(|(path, _)| path)
}

fn is_videos_dir(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case("videos"))
}

fn is_video(path: &Path) -> bool {
    !path.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.'))
        && path.extension().is_some_and(|ext| {
            let ext = ext.to_string_lossy();
            VIDEO_EXTENSIONS.iter().any(|v| ext.eq_ignore_ascii_case(v))
        })
}

/// 目录下的非隐藏条目：(路径, 是否目录)。不跟随符号链接。
fn visible_entries(dir: &Path) -> impl Iterator<Item = (PathBuf, bool)> {
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| !is_hidden(entry))
        .filter_map(|entry| {
            let ty = entry.file_type().ok()?;
            if ty.is_symlink() {
                return None;
            }
            Some((entry.path(), ty.is_dir()))
        })
}

fn is_hidden(entry: &fs::DirEntry) -> bool {
    if entry.file_name().to_string_lossy().starts_with('.') {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
        if let Ok(meta) = entry.metadata() {
            return meta.file_attributes() & FILE_ATTRIBUTE_HIDDEN != 0;
        }
    }
    false
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// 测试用的临时目录，离开作用域时删除
    pub struct TempDir(pub PathBuf);

    impl TempDir {
        pub fn new() -> Self {
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let dir = std::env::temp_dir().join(format!(
                "qcworker-test-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }

        pub fn file(&self, rel: &str, content: &[u8]) -> PathBuf {
            let path = self.0.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, content).unwrap();
            path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn layout() -> TempDir {
        let t = TempDir::new();
        t.file("整理后/橘子采摘/videos/top_left.mp4", b"x");
        t.file("整理后/橘子采摘/videos/top_right.mp4", b"x");
        t.file("整理后/橘子采摘/videos/.hidden.mp4", b"x");
        t.file("整理后/橘子采摘/imu/imu.csv", b"x");
        t.file("整理后/汽修/videos/top_left.MOV", b"x");
        t.file("整理后/说明.txt", b"x");
        t.file("整理后/.trash/x/videos/a.mp4", b"x");
        t
    }

    fn names(items: &[VideoItem]) -> Vec<String> {
        items.iter().map(|i| format!("{}/{}", i.folder.file_name().unwrap().to_string_lossy(), i.view())).collect()
    }

    #[test]
    fn expands_task_folder_parent_videos_dir_and_single_file() {
        let t = layout();
        let root = t.0.join("整理后");
        assert_eq!(names(&expand(&[root.join("橘子采摘")])), ["橘子采摘/top_left", "橘子采摘/top_right"]);
        assert_eq!(names(&expand(&[root.clone()])), ["橘子采摘/top_left", "橘子采摘/top_right", "汽修/top_left"]);
        assert_eq!(names(&expand(&[root.join("橘子采摘/videos")])), ["橘子采摘/top_left", "橘子采摘/top_right"]);
        let single = expand(&[root.join("橘子采摘/videos/top_right.mp4")]);
        assert_eq!(names(&single), ["橘子采摘/top_left", "橘子采摘/top_right"]);
        assert_eq!(single[0].folder, root.join("橘子采摘"));
    }

    #[test]
    fn duplicates_and_non_videos_are_dropped() {
        let t = layout();
        let root = t.0.join("整理后");
        let items = expand(&[root.join("橘子采摘"), root.join("橘子采摘/videos/top_left.mp4"), root.join("说明.txt"),
                             root.join("橘子采摘/imu/imu.csv"), t.0.join("不存在")]);
        assert_eq!(names(&items), ["橘子采摘/top_left", "橘子采摘/top_right"]);
    }
}
