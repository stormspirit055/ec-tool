mod handpose;
mod jobs;
mod mark;
mod qc;
mod sample;
mod scanner;
mod trim;

use jobs::{Event, Item, Jobs, SampleJob};
use serde::Serialize;
use std::path::PathBuf;
use tauri::{Emitter, Manager, RunEvent, State};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Status {
    qc_ready: bool,
    handpose_ready: bool,
    items: Vec<Item>,
    samples: Vec<SampleJob>,
}

#[tauri::command]
fn add_paths(jobs: State<'_, Jobs>, paths: Vec<PathBuf>) -> Result<usize, String> {
    jobs.add(&paths)
}

#[tauri::command]
fn start_qc(jobs: State<'_, Jobs>, folder: PathBuf) -> Result<usize, String> { jobs.start(&folder) }

#[tauri::command]
async fn probe_trim(jobs: State<'_, Jobs>, folder: PathBuf) -> Result<sample::Probe, String> { jobs.probe_trim(&folder).await }

#[tauri::command]
fn trim_videos(jobs: State<'_, Jobs>, folder: PathBuf, segments: Vec<sample::Segment>) -> Result<(), String> { jobs.add_trim(&folder, segments) }

#[tauri::command]
fn get_status(jobs: State<'_, Jobs>) -> Status {
    let (qc_ready, handpose_ready) = jobs.engines_ready();
    Status { qc_ready, handpose_ready, items: jobs.list(), samples: jobs.samples() }
}

/// 读取任务视频的帧时间和关键帧，供剪裁时间轴使用
#[tauri::command]
async fn probe_sample(jobs: State<'_, Jobs>, folder: PathBuf) -> Result<sample::Probe, String> {
    jobs.probe(&folder).await
}

/// 把标记的片段按时间顺序拼接成样例，放在任务目录旁边（`<任务名>_样例<秒>s`）
#[tauri::command]
fn make_sample(jobs: State<'_, Jobs>, folder: PathBuf, segments: Vec<sample::Segment>) -> Result<(), String> {
    jobs.add_sample(&folder, segments)
}

#[tauri::command]
fn cancel_all(jobs: State<'_, Jobs>) {
    jobs.cancel_all();
}

#[tauri::command]
fn clear_finished(jobs: State<'_, Jobs>) {
    jobs.clear_finished();
}

/// 在访达（Windows 资源管理器）中打开目录。只允许打开拖入的任务目录和生成的样例目录，不接受任意路径。
#[tauri::command]
fn reveal_folder(app: tauri::AppHandle, jobs: State<'_, Jobs>, folder: PathBuf) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    if !jobs.knows_folder(&folder) {
        return Err("只能打开任务目录或生成的样例目录".into());
    }
    if !folder.is_dir() {
        return Err(format!("目录不存在：{}", folder.display()));
    }
    app.opener().open_path(folder.to_string_lossy(), None::<&str>).map_err(|e| format!("无法打开目录：{e}"))
}

/// 读取关键点结果。只允许读列表里已完成视频对应的文件，不接受任意路径。
#[tauri::command]
async fn read_handpose(jobs: State<'_, Jobs>, id: u64) -> Result<tauri::ipc::Response, String> {
    let path = jobs
        .list()
        .into_iter()
        .find(|i| i.id == id)
        .and_then(|i| i.handpose)
        .ok_or("该视频还没有关键点结果")?;
    let bytes = tokio::fs::read(&path).await.map_err(|e| format!("无法读取关键点结果：{e}"))?;
    Ok(tauri::ipc::Response::new(bytes))
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let resources = app.path().resource_dir()?;
            let qc_engine = qc::Engine::locate(&resources);
            let handpose_engine = handpose::Engine::locate(&resources);
            let scope = app.asset_protocol_scope();
            let handle = app.handle().clone();
            let jobs = Jobs::new(
                qc_engine,
                handpose_engine,
                // 只放行拖入的视频文件本身，网页才能用 asset 协议播放
                move |video| {
                    let _ = scope.allow_file(video);
                },
                move |event| {
                    let _ = match event {
                        Event::Item(item) => handle.emit("jobs://item", item),
                        Event::Sample(job) => handle.emit("jobs://sample", job),
                        Event::Log { text, error } => {
                            handle.emit("jobs://log", serde_json::json!({ "text": text, "error": error }))
                        }
                    };
                },
            );
            app.manage(jobs);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![add_paths, start_qc, probe_trim, trim_videos, get_status, probe_sample, make_sample, reveal_folder, cancel_all, clear_finished, read_handpose])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            // 退出时结束正在运行的质检和关键点程序
            if let RunEvent::Exit = event {
                app.state::<Jobs>().shutdown();
            }
        });
}
