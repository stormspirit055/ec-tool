//! 手部关键点：调用随应用打包的 handpose-engine（model-qc-worker 的 nohand.handpose，MediaPipe）。
//!
//! 结果 JSON 直接写到任务目录的 `handpose/<视角>.json`，格式与 Web Demo 样例库一致；
//! 这里只读回统计信息，完整数据由前端按需加载。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

/// 单个视频的超时时间。实测本机约 2 倍实时（1920×1200 HEVC，整帧加左右分块），留足余量
const TIMEOUT: Duration = Duration::from_secs(4 * 3600);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Stats {
    pub frames: u64,
    pub frames_with_hands: u64,
    pub hand_ratio: f64,
}

#[derive(Debug, Clone)]
pub struct Engine {
    pub program: PathBuf,
    pub model: PathBuf,
}

impl Engine {
    /// 在资源目录 `handpose-engine/` 下查找程序和模型；任一缺失返回 None
    pub fn locate(resource_dir: &Path) -> Option<Self> {
        let dir = resource_dir.join("handpose-engine");
        let engine = Engine {
            program: dir.join(format!("handpose-engine{}", std::env::consts::EXE_SUFFIX)),
            model: dir.join("models").join("hand_landmarker.task"),
        };
        (engine.program.is_file() && engine.model.is_file()).then_some(engine)
    }

    /// 提取 `video` 每一帧的手部关键点写到 `output`。`progress(已处理秒, 总秒)` 约每秒回调一次。
    /// 任务被取消（future 被丢弃）时会结束子进程，不留下写了一半的结果。
    pub async fn run(&self, video: &Path, output: &Path, progress: impl Fn(f64, f64)) -> Result<Stats, String> {
        let dir = output.parent().ok_or("无效的输出路径")?;
        std::fs::create_dir_all(dir).map_err(|e| format!("无法创建 {}：{e}", dir.display()))?;
        // 先写到隐藏的临时文件，成功后再改名，失败或取消时不会留下半成品
        let name = output.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let tmp = TempFile(dir.join(format!(".{name}.part")));
        let progress_file = TempFile(dir.join(format!(".{name}.progress")));

        let mut cmd = Command::new(&self.program);
        cmd.arg(video)
            .arg("--json")
            .arg(&tmp.0)
            .arg("--progress")
            .arg(&progress_file.0)
            .env("HANDPOSE_MODEL", &self.model)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        #[cfg(windows)]
        {
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = cmd.spawn().map_err(|e| format!("无法运行关键点程序：{e}"))?;

        let wait = async {
            loop {
                tokio::select! {
                    status = child.wait() => break status,
                    _ = tokio::time::sleep(Duration::from_secs(1)) => {
                        if let Some((done, total)) = read_progress(&progress_file.0) {
                            progress(done, total);
                        }
                    }
                }
            }
        };
        let status = tokio::time::timeout(TIMEOUT, wait)
            .await
            .map_err(|_| format!("关键点提取超时（超过 {} 小时）", TIMEOUT.as_secs() / 3600))?
            .map_err(|e| format!("关键点程序运行出错：{e}"))?;

        let summary = read_summary(&tmp.0).map_err(|e| {
            if status.success() { format!("无法读取关键点结果：{e}") } else { format!("关键点程序异常退出（{status}）") }
        })?;
        match summary {
            Summary::Failed(error) => Err(format!("分析失败：{error}")),
            Summary::Ok(stats) => {
                // 结果跟着任务目录走：把引擎写入的绝对路径换成相对任务目录的路径，不带本机路径
                if let Some(folder) = dir.parent() {
                    let relative = video.strip_prefix(folder).unwrap_or(video).to_string_lossy().replace('\\', "/");
                    relativize_video(&tmp.0, &relative).map_err(|e| format!("无法写入关键点结果：{e}"))?;
                }
                tmp.persist(output).map_err(|e| format!("无法写入 {}：{e}", output.display()))?;
                Ok(stats)
            }
        }
    }
}

enum Summary {
    Ok(Stats),
    Failed(String),
}

#[derive(Deserialize)]
struct RawSummary {
    ok: bool,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    stats: Option<RawStats>,
}

#[derive(Deserialize)]
struct RawStats {
    frames: u64,
    frames_with_hands: u64,
    hand_ratio: f64,
}

fn read_summary(path: &Path) -> std::io::Result<Summary> {
    let raw: RawSummary = serde_json::from_slice(&std::fs::read(path)?).map_err(std::io::Error::other)?;
    if !raw.ok {
        return Ok(Summary::Failed(raw.error.unwrap_or_else(|| "未知错误".into())));
    }
    let s = raw.stats.ok_or_else(|| std::io::Error::other("结果缺少 stats"))?;
    Ok(Summary::Ok(Stats { frames: s.frames, frames_with_hands: s.frames_with_hands, hand_ratio: s.hand_ratio }))
}

/// 把结果 JSON 顶层的 `video` 改成相对路径（与 Web Demo 样例库的格式一致）。
/// 只解析和重写一次，几十 MB 的结果也只需零点几秒。
fn relativize_video(path: &Path, relative: &str) -> std::io::Result<()> {
    let mut value: serde_json::Value = serde_json::from_slice(&std::fs::read(path)?).map_err(std::io::Error::other)?;
    if let Some(obj) = value.as_object_mut() {
        obj.insert("video".into(), serde_json::Value::from(relative));
    }
    std::fs::write(path, serde_json::to_vec(&value).map_err(std::io::Error::other)?)
}

fn read_progress(path: &Path) -> Option<(f64, f64)> {
    let value: serde_json::Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    Some((value.get("done")?.as_f64()?, value.get("total")?.as_f64()?))
}

/// 离开作用域时自动删除；`persist` 改名为目标文件
struct TempFile(PathBuf);

impl TempFile {
    fn persist(self, target: &Path) -> std::io::Result<()> {
        std::fs::rename(&self.0, target)?;
        std::mem::forget(self);
        Ok(())
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scanner::tests::TempDir;

    #[test]
    fn reads_summary_and_progress() {
        let t = TempDir::new();
        let ok = t.file("a.json", br#"{"ok":true,"frames":[],"stats":{"frames":901,"frames_with_hands":420,"hand_ratio":0.4662}}"#);
        assert!(matches!(read_summary(&ok).unwrap(), Summary::Ok(Stats { frames: 901, frames_with_hands: 420, .. })));
        let failed = t.file("b.json", br#"{"ok":false,"error":"InvalidDataError: bad"}"#);
        assert!(matches!(read_summary(&failed).unwrap(), Summary::Failed(e) if e.contains("bad")));
        assert!(read_summary(&t.file("c.json", b"{")).is_err());
        let progress = t.file("p.json", br#"{"done":12.5,"total":30}"#);
        assert_eq!(read_progress(&progress), Some((12.5, 30.0)));
    }

    #[test]
    fn video_path_is_made_relative() {
        let t = TempDir::new();
        let path = t.file("r.json", br#"{"ok":true,"video":"/Users/x/task/videos/top_left.mp4","frames":[{"t":0}]}"#);
        relativize_video(&path, "videos/top_left.mp4").unwrap();
        let value: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(value["video"], "videos/top_left.mp4");
        assert_eq!(value["frames"][0]["t"], 0);
    }

    /// 需要先运行 `pnpm build-qc-engine`；未构建时跳过
    #[tokio::test(flavor = "multi_thread")]
    async fn extracts_real_sample_video() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("resources");
        let Some(engine) = Engine::locate(&dir) else {
            eprintln!("跳过：未找到 {}", dir.join("handpose-engine").display());
            return;
        };
        let Some(video) = crate::qc::tests::sample_video() else { return };
        let t = TempDir::new();
        let output = t.0.join("handpose").join("top_left.json");
        let calls = std::sync::atomic::AtomicU32::new(0);
        let stats = engine
            .run(&video, &output, |_, _| {
                calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            })
            .await
            .unwrap();
        eprintln!("{stats:?}，进度回调 {} 次", calls.into_inner());
        assert!(output.is_file());
        assert!(stats.frames > 0 && stats.frames_with_hands > 0);
        let json = std::fs::read_to_string(&output).unwrap();
        assert!(!json.contains(&*t.0.to_string_lossy()), "结果里不应带本机绝对路径");
        // 不留临时文件
        let leftovers: Vec<_> = std::fs::read_dir(output.parent().unwrap()).unwrap().flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with('.')).collect();
        assert!(leftovers.is_empty());
    }
}
