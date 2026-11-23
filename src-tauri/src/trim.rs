//! 逐帧裁剪，支持任意帧边界；输出到新目录，原任务保持完整。
use crate::{handpose::Engine, sample::{Probe, Segment}};
use std::{path::{Path, PathBuf}, process::Stdio, time::Duration};
use tokio::{io::{AsyncBufReadExt, BufReader}, process::Command};

async fn run(engine: &Engine, task: &Path, videos: &[PathBuf], segments: Option<&[Segment]>, progress: impl Fn(f64)) -> Result<serde_json::Value, String> {
    let mut cmd = Command::new(&engine.program);
    cmd.arg("trim").arg(task).arg("--videos").arg(serde_json::to_string(videos).map_err(|e| e.to_string())?)
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    if let Some(segments) = segments {
        cmd.arg("--segments").arg(serde_json::to_string(segments).map_err(|e| e.to_string())?);
    } else { cmd.arg("--probe"); }
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000);
    let mut child = cmd.spawn().map_err(|e| format!("无法运行裁剪程序：{e}"))?;
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    // FFmpeg 的诊断可能很多，持续消费 stderr，避免管道阻塞。
    let mut stderr = child.stderr.take().unwrap();
    let read = async {
        let read_out = async {
            let mut last = None;
            while let Some(line) = lines.next_line().await.map_err(|e| e.to_string())? {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) {
                    if let Some(p) = v["progress"].as_f64() { progress(p); }
                    else { last = Some(v); }
                }
            }
            Ok::<_, String>(last)
        };
        let mut sink = tokio::io::sink();
        let drain = tokio::io::copy(&mut stderr, &mut sink);
        let (last, _) = tokio::join!(read_out, drain);
        let status = child.wait().await.map_err(|e| e.to_string())?;
        let v = last?.ok_or_else(|| format!("裁剪程序异常退出（{status}），请重新构建 handpose-engine"))?;
        if !status.success() || v["ok"].as_bool() != Some(true) {
            return Err(v["error"].as_str().unwrap_or("裁剪失败").to_string());
        }
        Ok(v)
    };
    tokio::time::timeout(Duration::from_secs(4 * 3600), read).await.map_err(|_| "裁剪超时".to_string())?
}

pub async fn probe(engine: &Engine, task: &Path, videos: &[PathBuf]) -> Result<Probe, String> {
    serde_json::from_value(run(engine, task, videos, None, |_| {}).await?).map_err(|e| format!("无法读取视频信息：{e}"))
}

pub async fn make(engine: &Engine, task: &Path, videos: &[PathBuf], segments: &[Segment], progress: impl Fn(f64)) -> Result<PathBuf, String> {
    let v = run(engine, task, videos, Some(segments), progress).await?;
    let output = PathBuf::from(v["output"].as_str().ok_or("裁剪程序未返回输出目录")?);
    if !output.join("videos").is_dir() { return Err("未找到裁剪后的视频".into()); }
    Ok(output)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::{jobs::{Jobs, Stage}, scanner::tests::TempDir};

    #[tokio::test(flavor = "multi_thread")]
    async fn trims_stereo_with_sensors_and_imports_preview_only() {
        let Some(engine) = Engine::locate(&Path::new(env!("CARGO_MANIFEST_DIR")).join("resources")) else {
            eprintln!("跳过：未构建 handpose-engine");
            return;
        };
        let Some(video) = crate::qc::tests::sample_video() else { return };
        let source = video.parent().unwrap().parent().unwrap();
        let temp = TempDir::new();
        let task = temp.0.join("裁剪任务");
        std::fs::create_dir_all(task.join("videos")).unwrap();
        for video in crate::scanner::videos(source) {
            std::fs::hard_link(&video, task.join("videos").join(video.file_name().unwrap())).unwrap();
        }
        for folder in ["imu", "timestamps", "calibration"] {
            if source.join(folder).exists() {
                std::os::unix::fs::symlink(source.join(folder), task.join(folder)).unwrap();
            }
        }
        let jobs = Jobs::new(None, Some(engine), |_| {}, |_| {});
        assert_eq!(jobs.add(&[task.clone()]).unwrap(), 2);
        let info = jobs.probe_trim(&task).await.unwrap();
        assert!(info.frames > 90);
        jobs.add_trim(&task, vec![Segment { start: 90, end: info.frames }, Segment { start: 20, end: 60 }, Segment { start: 0, end: 7 }]).unwrap();
        assert!(jobs.start(&task).unwrap_err().contains("裁剪"));
        let started = std::time::Instant::now();
        let job = loop {
            let job = jobs.samples()[0].clone();
            if matches!(job.status, "done" | "failed") { break job; }
            assert!(started.elapsed().as_secs() < 180, "裁剪超时");
            tokio::time::sleep(Duration::from_millis(200)).await;
        };
        assert_eq!(job.status, "done", "{:?}", job.error);
        let output = PathBuf::from(job.output.unwrap());
        let cropped = jobs.probe_trim(&output).await.unwrap();
        assert_eq!(cropped.frames, 43);
        assert_eq!(jobs.list().len(), 4);
        assert!(jobs.list().iter().all(|i| i.stage == Stage::Ready), "裁剪不得自动质检");
        assert!(output.join("timestamps/top_left_camera.csv").is_file());
        assert!(output.join("imu/imu.csv").is_file());
        assert!(!output.join(crate::mark::MARK_FILE).exists());
        let metadata: serde_json::Value = serde_json::from_slice(&std::fs::read(output.join("trim.json")).unwrap()).unwrap();
        assert_eq!(metadata["frames"], 43);
        assert_eq!(jobs.probe_trim(&task).await.unwrap().frames, info.frames, "原视频不变");
    }
}
