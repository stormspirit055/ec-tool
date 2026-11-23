//! 生成样例：调用 `handpose-engine sample`（model-qc-worker 的 nohand.sample），从任务目录剪出用户标记的
//! 一段或多段，按时间顺序拼接成一份样例，放在任务目录旁边（`<任务名>_样例<秒>s`，重名时加 _2、_3…），
//! 结构与 demo/样例30s 相同，另有 segments.json 记录每段在原视频和样例里的位置。
//!
//! 原视频不重编码，片段起点必须是关键帧（这批视频每 30 帧一个）；不另外生成网页版，预览直接播放 videos/。
//! 时间戳和 IMU 按片段截取后依次拼接、重新编号，设备时间保持原值。
//! 生成样例与质检相互独立：只截取视频、IMU 和 timestamps，其余文件原样复制。
//! 包括 metadata、calibration、evidence、qa 和已有处理结果，保持源目录结构与文件内容。

use crate::handpose::Engine;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

const TIMEOUT: Duration = Duration::from_secs(3600);

/// 帧号区间 [start, end)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Segment {
    pub start: u64,
    pub end: u64,
}

/// 选片段用的视频信息：每帧时间和关键帧位置（各视角相同）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Probe {
    pub views: Vec<String>,
    pub frames: u64,
    pub fps: f64,
    pub duration: f64,
    pub times: Vec<f64>,
    pub keyframes: Vec<u64>,
}

#[derive(Debug)]
pub struct Made {
    pub output: PathBuf,
    pub frames: u64,
    pub duration: f64,
}

fn command(engine: &Engine) -> Command {
    let mut cmd = Command::new(&engine.program);
    cmd.arg("sample")
        .env("HANDPOSE_MODEL", &engine.model)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// 读取任务视频的帧时间和关键帧（只读包头，几百毫秒）
pub async fn probe(engine: &Engine, task: &Path) -> Result<Probe, String> {
    let output = command(engine)
        .arg("--probe")
        .arg(task)
        .output()
        .await
        .map_err(|e| format!("无法运行样例程序：{e}"))?;
    let last = String::from_utf8_lossy(&output.stdout).lines().last().unwrap_or_default().to_string();
    let value: serde_json::Value = serde_json::from_str(&last).map_err(|_| "无法读取视频信息".to_string())?;
    if value["ok"].as_bool() != Some(true) {
        return Err(value["error"].as_str().unwrap_or("无法读取视频信息").to_string());
    }
    serde_json::from_value(value).map_err(|e| format!("无法读取视频信息：{e}"))
}

/// 生成样例；只截取三个数据流，其余文件由引擎原样复制。
/// `progress(0–1)` 在剪裁和转码过程中回调。
pub async fn make(
    engine: &Engine,
    task: &Path,
    segments: &[Segment],
    progress: impl Fn(f64),
) -> Result<Made, String> {
    if segments.is_empty() {
        return Err("请至少标记一个片段".into());
    }
    let spec = segments.iter().map(|s| format!("{}-{}", s.start, s.end)).collect::<Vec<_>>().join(",");
    let views = crate::scanner::videos(task).len().max(1) as f64;
    let mut cmd = command(engine);
    cmd.arg(task).arg("--segments").arg(&spec);
    let mut child = cmd.spawn().map_err(|e| format!("无法运行样例程序：{e}"))?;
    let stdout = child.stdout.take().expect("stdout is piped");

    let read = async {
        let mut lines = BufReader::new(stdout).lines();
        let mut last = None;
        // 每目一步（拼接原视频），按完成的视角数估算整体进度
        let mut steps: Vec<(String, String)> = Vec::new();
        while let Ok(Some(line)) = lines.next_line().await {
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
            if let (Some(step), Some(view), Some(done), Some(total)) = (
                value["step"].as_str(), value["view"].as_str(), value["done"].as_f64(), value["total"].as_f64(),
            ) {
                let key = (step.to_string(), view.to_string());
                if !steps.contains(&key) {
                    steps.push(key);
                }
                let finished = (steps.len() - 1) as f64;
                progress(((finished + done / total.max(1.0)) / views).clamp(0.0, 1.0));
            } else {
                last = Some(value);
            }
        }
        let status = child.wait().await;
        (last, status)
    };
    let (last, status) = tokio::time::timeout(TIMEOUT, read)
        .await
        .map_err(|_| format!("生成样例超时（超过 {} 分钟）", TIMEOUT.as_secs() / 60))?;
    let status = status.map_err(|e| format!("样例程序运行出错：{e}"))?;
    match last {
        Some(v) if status.success() && v["ok"].as_bool() == Some(true) => {
            let made = Made {
                output: PathBuf::from(v["output"].as_str().unwrap_or_default()),
                frames: v["frames"].as_u64().unwrap_or(0),
                duration: v["duration_sec"].as_f64().unwrap_or(0.0),
            };
            validate_output(task, &made)?;
            Ok(made)
        }
        Some(v) => Err(v["error"].as_str().unwrap_or("未知错误").to_string()),
        None => Err(format!("样例程序异常退出（{status}）")),
    }
}

/// 即使误用了旧引擎，也不能将缺少传感器文件的目录报告为完整样例。
fn validate_output(task: &Path, made: &Made) -> Result<(), String> {
    if made.frames == 0 { return Err("样例未包含有效视频帧".into()); }
    let mut required = vec![made.output.join("imu/imu.csv")];
    for video in crate::scanner::videos(task) {
        let view = video.file_stem().unwrap_or_default().to_string_lossy();
        required.push(made.output.join("videos").join(format!("{view}.mp4")));
        required.push(made.output.join("timestamps").join(format!("{view}_camera.csv")));
    }
    for path in required {
        if !path.metadata().is_ok_and(|m| m.is_file() && m.len() > 0) {
            return Err(format!("样例缺少必要文件：{}。请检查源任务的 timestamps 和 imu，并重新生成。", path.display()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scanner::tests::TempDir;

    #[test]
    fn incomplete_sample_is_rejected_even_if_engine_claims_success() {
        let temp = TempDir::new();
        let source = temp.0.join("source");
        temp.file("source/videos/top_left.mp4", b"source-video");
        temp.file("source/videos/top_right.mp4", b"source-video");
        temp.file("output/videos/top_left.mp4", b"sample-video");
        temp.file("output/videos/top_right.mp4", b"sample-video");
        let made = Made { output: temp.0.join("output"), frames: 30, duration: 1.0 };
        assert!(validate_output(&source, &made).unwrap_err().contains("imu.csv"));
        temp.file("output/imu/imu.csv", b"imu-data");
        assert!(validate_output(&source, &made).unwrap_err().contains("top_left_camera.csv"));
        temp.file("output/timestamps/top_left_camera.csv", b"timestamp-data");
        assert!(validate_output(&source, &made).unwrap_err().contains("top_right_camera.csv"));
        temp.file("output/timestamps/top_right_camera.csv", b"timestamp-data");
        assert!(validate_output(&source, &made).is_ok());
    }

    /// 需要构建好的 handpose-engine 和 demo/整理后/橘子采摘（或 QC_SAMPLE_TASK 指定的任务目录）；否则跳过
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn probes_and_stitches_real_task() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("resources");
        let Some(engine) = Engine::locate(&dir) else {
            eprintln!("跳过：未构建 handpose-engine");
            return;
        };
        let source = std::env::var_os("QC_SAMPLE_TASK").map(PathBuf::from)
            .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../demo/整理后/橘子采摘"));
        if !source.join("videos").is_dir() {
            eprintln!("跳过：未找到任务目录 {}", source.display());
            return;
        }
        // 在临时目录里用符号链接指向原数据，样例生成到临时目录，不碰原目录
        let t = TempDir::new();
        let task = t.0.join("任务");
        std::fs::create_dir(&task).unwrap();
        for sub in ["videos", "timestamps", "imu", "calibration", "handpose"] {
            if source.join(sub).is_dir() {
                std::os::unix::fs::symlink(source.join(sub), task.join(sub)).unwrap();
            }
        }
        let info = probe(&engine, &task).await.unwrap();
        assert_eq!(info.times.len() as u64, info.frames);
        assert_eq!(info.keyframes[0], 0);
        assert!(info.keyframes.len() > 2);

        let segments = [Segment { start: info.keyframes[2], end: info.keyframes[2] + 45 }, Segment { start: 0, end: 30 }];
        let calls = std::sync::Mutex::new(Vec::new());
        let made = make(&engine, &task, &segments, |p| calls.lock().unwrap().push(p)).await.unwrap();
        assert_eq!(made.frames, 75);
        assert_eq!(made.output.parent().unwrap(), t.0);
        assert!(made.output.file_name().unwrap().to_string_lossy().starts_with("任务_样例"));
        for f in [format!("videos/{}.mp4", info.views[0]), format!("timestamps/{}_camera.csv", info.views[0]), "imu/imu.csv".into(), "segments.json".into()] {
            assert!(made.output.join(&f).is_file(), "缺少 {f}");
        }
        assert!(!made.output.join("preview").exists(), "不再单独生成 preview/");
        if task.join("handpose").is_dir() { assert!(made.output.join("handpose").is_dir(), "原文件必须保留"); }
        let calls = calls.into_inner().unwrap();
        assert!(calls.windows(2).all(|w| w[0] <= w[1] + 1e-9), "进度应单调递增：{calls:?}");

        // 起点不是关键帧：报错，不生成
        let bad = make(&engine, &task, &[Segment { start: 5, end: 40 }], |_| {}).await.unwrap_err();
        assert!(bad.contains("关键帧"), "{bad}");
    }
}
