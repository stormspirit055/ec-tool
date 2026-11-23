//! 模型质检：调用随应用打包的 nohand 质检程序（model-qc-worker，PyInstaller 打包）。
//!
//! 质检程序输出一个 JSON，这里只保留界面和处理记录需要的字段。
//! 场景 ID 与 nohand 的注册表（nohand/scenes/__init__.py）一致。

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

/// 单个视频质检的超时时间，与 nohand 内部服务一致
const TIMEOUT: Duration = Duration::from_secs(3600);
/// 结果 JSON 的大小上限，与 nohand 内部服务一致
const MAX_OUTPUT: u64 = 4 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scene {
    NoHand,
    Light,
}

impl Scene {
    pub fn id(self) -> &'static str {
        match self {
            Scene::NoHand => "no_hand",
            Scene::Light => "light",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Scene::NoHand => "无手",
            Scene::Light => "光线异常",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Segment {
    pub start: f64,
    pub end: f64,
    pub duration: f64,
    /// 光线异常原因：过曝 / 欠曝 / 对比度过低
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// 无手片段是否因光线问题不可靠；未评估光线时为 None
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unreliable_due_to_light: Option<bool>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct QcResult {
    pub engine_version: String,
    pub scenes: BTreeMap<Scene, Vec<Segment>>,
    pub elapsed_sec: f64,
}

#[derive(Debug)]
pub enum QcError {
    /// 质检程序报告分析失败（文件损坏、不是视频等）
    Analysis(String),
    Failed(String),
}

impl fmt::Display for QcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            QcError::Analysis(msg) => write!(f, "分析失败：{msg}"),
            QcError::Failed(msg) => write!(f, "{msg}"),
        }
    }
}

/// 随应用打包的质检程序
#[derive(Debug, Clone)]
pub struct Engine {
    pub program: PathBuf,
    pub model: PathBuf,
}

impl Engine {
    /// 在资源目录 `qc-engine/` 下查找质检程序和模型；任一缺失返回 None
    pub fn locate(resource_dir: &Path) -> Option<Self> {
        let dir = resource_dir.join("qc-engine");
        let engine = Engine {
            program: dir.join(format!("qc-engine{}", std::env::consts::EXE_SUFFIX)),
            model: dir.join("models").join("hand_detector.onnx"),
        };
        (engine.program.is_file() && engine.model.is_file()).then_some(engine)
    }

    /// 质检一个视频。任务被取消（future 被丢弃）时会结束质检进程。
    pub async fn run(&self, video: &Path, scenes: &[Scene]) -> Result<QcResult, QcError> {
        if scenes.is_empty() {
            return Err(QcError::Failed("未选择质检场景".into()));
        }
        let threads = std::thread::available_parallelism().map(|n| n.get().min(8)).unwrap_or(4);

        let mut cmd = Command::new(&self.program);
        cmd.arg(video)
            .arg("--scenes")
            .args(scenes.iter().map(|s| s.id()))
            .args(["--threads", &threads.to_string()])
            .env("NOHAND_MODEL", &self.model)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        #[cfg(windows)]
        {
            // 不为子进程弹出控制台窗口
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }

        let mut child = cmd.spawn().map_err(|e| QcError::Failed(format!("无法运行质检程序：{e}")))?;
        let mut stdout = child.stdout.take().expect("stdout is piped");

        let run = async {
            let mut buf = Vec::new();
            (&mut stdout).take(MAX_OUTPUT + 1).read_to_end(&mut buf).await?;
            let status = child.wait().await?;
            Ok::<_, std::io::Error>((buf, status))
        };
        let (buf, status) = tokio::time::timeout(TIMEOUT, run)
            .await
            .map_err(|_| QcError::Failed(format!("质检超时（超过 {} 分钟）", TIMEOUT.as_secs() / 60)))?
            .map_err(|e| QcError::Failed(format!("质检程序运行出错：{e}")))?;

        if buf.len() as u64 > MAX_OUTPUT {
            return Err(QcError::Failed("质检结果超过大小上限".into()));
        }
        // 退出码 1 表示分析失败，JSON 里有原因；其它非 0 退出码说明程序本身出错
        match status.code() {
            Some(0) | Some(1) => parse_output(&buf, scenes),
            _ => Err(QcError::Failed(format!("质检程序异常退出（{status}）"))),
        }
    }
}

#[derive(Deserialize)]
struct RawOutput {
    ok: bool,
    #[serde(default)]
    version: String,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    scene_results: BTreeMap<String, RawScene>,
    #[serde(default)]
    elapsed_sec: f64,
}

#[derive(Deserialize)]
struct RawScene {
    #[serde(default)]
    segments: Vec<RawSegment>,
}

#[derive(Deserialize)]
struct RawSegment {
    start: f64,
    end: f64,
    duration: f64,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    unreliable_due_to_light: Option<bool>,
}

/// 解析质检程序的 JSON 输出。所选场景缺少结果时视为失败，不把部分结果当作完整结果。
pub fn parse_output(stdout: &[u8], scenes: &[Scene]) -> Result<QcResult, QcError> {
    let raw: RawOutput = serde_json::from_slice(stdout)
        .map_err(|e| QcError::Failed(format!("无法解析质检结果：{e}")))?;
    if !raw.ok {
        return Err(QcError::Analysis(raw.error.unwrap_or_else(|| "未知错误".into())));
    }
    let mut results = BTreeMap::new();
    let mut scene_results = raw.scene_results;
    for &scene in scenes {
        let found = scene_results
            .remove(scene.id())
            .ok_or_else(|| QcError::Failed(format!("质检结果缺少「{}」", scene.label())))?;
        let segments = found
            .segments
            .into_iter()
            .map(|s| Segment {
                start: s.start,
                end: s.end,
                duration: s.duration,
                reason: s.reason.filter(|r| !r.is_empty()),
                unreliable_due_to_light: s.unreliable_due_to_light,
            })
            .collect();
        results.insert(scene, segments);
    }
    Ok(QcResult { engine_version: raw.version, scenes: results, elapsed_sec: raw.elapsed_sec })
}

/// 日志里的结果摘要，例如「无手 2 段（12.0–18.5s、40.0–47.0s），光线异常 无」
pub fn summary(scenes: &BTreeMap<Scene, Vec<Segment>>) -> String {
    const MAX_LISTED: usize = 5;
    scenes
        .iter()
        .map(|(scene, segments)| {
            if segments.is_empty() {
                return format!("{} 无", scene.label());
            }
            let mut parts: Vec<String> = segments
                .iter()
                .take(MAX_LISTED)
                .map(|s| {
                    let mut text = format!("{:.1}–{:.1}s", s.start, s.end);
                    if let Some(reason) = &s.reason {
                        text.push_str(&format!(" {reason}"));
                    }
                    if s.unreliable_due_to_light == Some(true) {
                        text.push_str(" 光线差");
                    }
                    text
                })
                .collect();
            if segments.len() > MAX_LISTED {
                parts.push(format!("等共 {} 段", segments.len()));
            }
            format!("{} {} 段（{}）", scene.label(), segments.len(), parts.join("、"))
        })
        .collect::<Vec<_>>()
        .join("，")
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// nohand 1.1.0 的输出样例（来自 docs/scene-interface.md 和 README，字段有删减）
    const SAMPLE: &str = r#"{
      "version": "1.1.0", "ok": true, "video": "/data/x.mp4",
      "scenes": ["no_hand", "light"], "frames": 15903, "fps": 30.0, "duration_sec": 530.1,
      "scene_results": {
        "no_hand": {"segments": [
          {"start": 491.25, "end": 497.25, "duration": 6.0, "start_frame": 14738, "end_frame": 14918,
           "unreliable_due_to_light": true}
        ], "stats": {}, "assumptions": []},
        "light": {"segments": [
          {"start": 489.0, "end": 530.1, "duration": 41.1, "start_frame": 14670, "end_frame": 15903, "reason": "过曝"}
        ], "stats": {}, "assumptions": []}
      },
      "no_hand_segments": [], "light_segments": [], "stats": {}, "params": {}, "assumptions": [],
      "elapsed_sec": 33.6
    }"#;

    #[test]
    fn parses_scene_results() {
        let r = parse_output(SAMPLE.as_bytes(), &[Scene::NoHand, Scene::Light]).unwrap();
        assert_eq!(r.engine_version, "1.1.0");
        assert_eq!(r.elapsed_sec, 33.6);
        let hand = &r.scenes[&Scene::NoHand];
        assert_eq!(hand.len(), 1);
        assert_eq!((hand[0].start, hand[0].end), (491.25, 497.25));
        assert_eq!(hand[0].unreliable_due_to_light, Some(true));
        assert_eq!(r.scenes[&Scene::Light][0].reason.as_deref(), Some("过曝"));
        assert_eq!(
            summary(&r.scenes),
            "无手 1 段（491.2–497.2s 光线差），光线异常 1 段（489.0–530.1s 过曝）"
        );
    }

    #[test]
    fn missing_scene_is_error() {
        let out = r#"{"version": "1.1.0", "ok": true, "scene_results": {"light": {"segments": []}}}"#;
        assert!(matches!(parse_output(out.as_bytes(), &[Scene::NoHand]), Err(QcError::Failed(_))));
        let r = parse_output(out.as_bytes(), &[Scene::Light]).unwrap();
        assert_eq!(summary(&r.scenes), "光线异常 无");
    }

    #[test]
    fn analysis_failure_reports_error() {
        let out = r#"{"version": "1.1.0", "ok": false, "error": "ValueError: 视频没有可分析的帧"}"#;
        match parse_output(out.as_bytes(), &[Scene::Light]) {
            Err(QcError::Analysis(msg)) => assert!(msg.contains("没有可分析的帧")),
            other => panic!("unexpected {other:?}"),
        }
        assert!(matches!(parse_output(b"not json", &[Scene::Light]), Err(QcError::Failed(_))));
    }

    #[test]
    fn summary_truncates_long_lists() {
        let seg = |s: f64| Segment { start: s, end: s + 6.0, duration: 6.0, reason: None, unreliable_due_to_light: None };
        let scenes = BTreeMap::from([(Scene::NoHand, (0..7).map(|i| seg(i as f64 * 10.0)).collect())]);
        let text = summary(&scenes);
        assert!(text.starts_with("无手 7 段（0.0–6.0s、"));
        assert!(text.ends_with("等共 7 段）"));
    }

    /// 需要先运行 `pnpm build-qc-engine`；未构建时跳过
    fn built_engine() -> Option<Engine> {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("resources");
        let engine = Engine::locate(&dir);
        if engine.is_none() {
            eprintln!("跳过：未找到 {}", dir.join("qc-engine").display());
        }
        engine
    }

    /// 集成测试用的真实视频：环境变量 QC_SAMPLE_VIDEO，默认 demo/样例30s/汽修 的左目（30 秒）；不存在时跳过
    pub(crate) fn sample_video() -> Option<std::path::PathBuf> {
        let path = std::env::var_os("QC_SAMPLE_VIDEO").map(std::path::PathBuf::from).unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../demo/样例30s/汽修/videos/top_left.mp4")
        });
        if path.is_file() {
            Some(path)
        } else {
            eprintln!("跳过：未找到样例视频 {}（可用 QC_SAMPLE_VIDEO 指定）", path.display());
            None
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn checks_real_sample_video() {
        let Some(engine) = built_engine() else { return };
        let Some(video) = sample_video() else { return };
        let r = engine.run(&video, &[Scene::NoHand, Scene::Light]).await.unwrap();
        eprintln!("{}（耗时 {:.1}s）", summary(&r.scenes), r.elapsed_sec);
        assert_eq!(r.engine_version, "1.1.0");
        assert!(r.scenes.contains_key(&Scene::NoHand) && r.scenes.contains_key(&Scene::Light));

        // 损坏的文件报告分析失败
        let t = crate::scanner::tests::TempDir::new();
        let broken = t.file("videos/broken.mp4", b"not a video");
        assert!(matches!(engine.run(&broken, &[Scene::Light]).await, Err(QcError::Analysis(_))));
    }
}
