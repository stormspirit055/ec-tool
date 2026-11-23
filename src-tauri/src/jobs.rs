//! 导入只预览；点击开始质检后，每个视频依次做模型质检和手部关键点提取。
//!
//! 同一时间只处理一个视频，避免两个 CPU 推理程序争抢。已处理过且视频没变的，直接读记录显示结果。

use crate::handpose;
use crate::mark::{Fingerprint, FolderMark};
use crate::qc::{self, Scene, Segment};
use crate::sample;
use crate::scanner::{self, VideoItem};
use serde::Serialize;
use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tauri::async_runtime::{self, JoinHandle};

const SCENES: [Scene; 2] = [Scene::NoHand, Scene::Light];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Ready,
    Queued,
    Qc,
    Handpose,
    Done,
    Failed,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    pub id: u64,
    /// 任务目录名，例如 `橘子采摘`
    pub task: String,
    pub folder: String,
    /// 视角，例如 `top_left`
    pub view: String,
    pub video: String,
    pub stage: Stage,
    /// 关键点进度 0–1；其它阶段为 None
    pub progress: Option<f64>,
    pub qc: Option<BTreeMap<Scene, Vec<Segment>>>,
    pub qc_error: Option<String>,
    /// 关键点结果文件的绝对路径
    pub handpose: Option<String>,
    pub handpose_stats: Option<handpose::Stats>,
    pub handpose_error: Option<String>,
    /// 结果来自之前的处理记录（没有重新计算）
    pub cached: bool,
}

/// 生成样例或裁剪的状态。裁剪结果自动加入预览，质检始终需要手动启动。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SampleJob {
    pub kind: &'static str,
    /// 源任务目录
    pub folder: String,
    /// 样例目录；生成完成前为空
    pub output: Option<String>,
    /// 标记的片段（原视频帧号）
    pub segments: Vec<sample::Segment>,
    /// queued / running / done / failed
    pub status: &'static str,
    pub progress: Option<f64>,
    pub error: Option<String>,
}

pub enum Event {
    Item(Item),
    Sample(SampleJob),
    /// 某个视频的一条日志，例如失败原因
    Log { text: String, error: bool },
}

/// 队列里的一项：处理一个视频，或为一个任务目录生成样例
#[derive(Debug, Clone, PartialEq)]
enum Work {
    Video(u64),
    Sample(String, Vec<sample::Segment>),
    Trim(String, Vec<sample::Segment>),
}

struct State {
    items: Vec<Item>,
    samples: Vec<SampleJob>,
    queue: VecDeque<Work>,
    next_id: u64,
    worker: Option<JoinHandle<()>>,
}

#[derive(Clone)]
pub struct Jobs {
    state: Arc<Mutex<State>>,
    emit: Arc<dyn Fn(Event) + Send + Sync>,
    qc: Option<Arc<qc::Engine>>,
    handpose: Option<Arc<handpose::Engine>>,
    /// 新加入的视频需要放行给前端播放（Tauri asset protocol 作用域）
    allow: Arc<dyn Fn(&Path) + Send + Sync>,
}

impl Jobs {
    pub fn new(
        qc: Option<qc::Engine>,
        handpose: Option<handpose::Engine>,
        allow: impl Fn(&Path) + Send + Sync + 'static,
        emit: impl Fn(Event) + Send + Sync + 'static,
    ) -> Self {
        Jobs {
            state: Arc::new(Mutex::new(State {
                items: Vec::new(),
                samples: Vec::new(),
                queue: VecDeque::new(),
                next_id: 1,
                worker: None,
            })),
            emit: Arc::new(emit),
            qc: qc.map(Arc::new),
            handpose: handpose.map(Arc::new),
            allow: Arc::new(allow),
        }
    }

    pub fn engines_ready(&self) -> (bool, bool) {
        (self.qc.is_some(), self.handpose.is_some())
    }

    pub fn list(&self) -> Vec<Item> {
        self.state.lock().unwrap().items.clone()
    }

    /// 导入只用于预览；只有 start 才将视频放入处理队列。
    pub fn add(&self, paths: &[PathBuf]) -> Result<usize, String> {
        let videos = scanner::expand(paths);
        if videos.is_empty() {
            return Err("没有找到视频。请拖入任务目录（内含 videos/top_left.mp4 等），或它的上一级目录".into());
        }
        let mut added = Vec::new();
        {
            let mut s = self.state.lock().unwrap();
            for v in videos {
                let video = v.video.to_string_lossy().into_owned();
                if s.items.iter().any(|i| i.video == video) {
                    continue;
                }
                (self.allow)(&v.video);
                let item = new_item(s.next_id, &v);
                s.next_id += 1;
                added.push(item.id);
                s.items.push(item);
            }
        }
        for id in &added {
            self.emit_item(*id);
        }
        Ok(added.len())
    }

    pub fn start(&self, folder: &Path) -> Result<usize, String> {
        let key = folder.to_string_lossy();
        let ids = {
            let mut s = self.state.lock().unwrap();
            if s.samples.iter().any(|j| j.folder == key && j.kind == "trim" && matches!(j.status, "queued" | "running")) {
                return Err("请等待裁剪完成后再开始质检".into());
            }
            let ids: Vec<_> = s.items.iter_mut()
                .filter(|i| i.folder == key && matches!(i.stage, Stage::Ready | Stage::Failed))
                .map(|i| { reset(i); i.stage = Stage::Queued; i.id }).collect();
            for id in &ids { s.queue.push_back(Work::Video(*id)); }
            ids
        };
        for id in &ids { self.emit_item(*id); }
        if !ids.is_empty() { self.ensure_worker(); }
        Ok(ids.len())
    }

    /// 停止全部：结束正在运行的程序，清空队列；已完成的结果保留
    pub fn cancel_all(&self) {
        let worker = {
            let mut s = self.state.lock().unwrap();
            s.queue.clear();
            for job in s.samples.iter_mut().filter(|j| matches!(j.status, "queued" | "running")) {
                job.status = "failed";
                job.progress = None;
                job.error = Some("已停止".into());
            }
            for item in s.items.iter_mut().filter(|i| matches!(i.stage, Stage::Queued | Stage::Qc | Stage::Handpose)) {
                item.stage = Stage::Failed;
                item.progress = None;
                if item.qc.is_none() && item.qc_error.is_none() {
                    item.qc_error = Some("已停止".into());
                }
                if item.handpose.is_none() && item.handpose_error.is_none() {
                    item.handpose_error = Some("已停止".into());
                }
            }
            s.worker.take()
        };
        if let Some(worker) = worker {
            worker.abort();
        }
        for item in self.list() {
            (self.emit)(Event::Item(item));
        }
        for job in self.samples() {
            (self.emit)(Event::Sample(job));
        }
    }

    /// 从列表移除已结束的视频和样例记录（不删除任何文件）
    pub fn clear_finished(&self) {
        let mut s = self.state.lock().unwrap();
        s.items.retain(|i| !matches!(i.stage, Stage::Ready | Stage::Done | Stage::Failed));
        s.samples.retain(|j| !matches!(j.status, "done" | "failed"));
    }

    /// 应用退出时调用：结束子进程
    pub fn shutdown(&self) {
        if let Some(worker) = self.state.lock().unwrap().worker.take() {
            worker.abort();
            async_runtime::block_on(async {
                let _ = tokio::time::timeout(std::time::Duration::from_secs(3), worker).await;
            });
        }
    }

    fn ensure_worker(&self) {
        let mut s = self.state.lock().unwrap();
        if s.worker.as_ref().is_some_and(|w| !w.inner().is_finished()) {
            return;
        }
        let this = self.clone();
        s.worker = Some(async_runtime::spawn(async move { this.run().await }));
    }

    async fn run(&self) {
        loop {
            let next = {
                let mut s = self.state.lock().unwrap();
                let next = s.queue.pop_front();
                if next.is_none() { s.worker = None; }
                next
            };
            match next {
                None => break,
                Some(Work::Sample(folder, segments)) => self.make_sample(folder, segments).await,
                Some(Work::Trim(folder, segments)) => self.trim(folder, segments).await,
                Some(Work::Video(id)) => {
                    let item = self.state.lock().unwrap().items.iter().find(|i| i.id == id).cloned();
                    if let Some(item) = item {
                        self.process(item).await;
                    }
                }
            }
        }
    }

    pub fn samples(&self) -> Vec<SampleJob> {
        self.state.lock().unwrap().samples.clone()
    }

    /// 是否为本次运行中出现过的目录：拖入的任务目录，或生成的样例目录。打开目录只允许这些
    pub fn knows_folder(&self, folder: &Path) -> bool {
        let s = self.state.lock().unwrap();
        let key = folder.to_string_lossy();
        s.items.iter().any(|i| i.folder == key) || s.samples.iter().any(|j| j.output.as_deref() == Some(&*key))
    }

    /// 读取任务视频的帧时间和关键帧，供界面在时间轴上标记片段
    pub async fn probe(&self, folder: &Path) -> Result<sample::Probe, String> {
        let engine = self.handpose.clone().ok_or("找不到样例程序，请重新安装应用")?;
        sample::probe(&engine, folder).await
    }

    fn task_videos(&self, folder: &Path) -> Result<Vec<PathBuf>, String> {
        let key = folder.to_string_lossy();
        let videos: Vec<_> = self.list().iter().filter(|i| i.folder == key).map(|i| PathBuf::from(&i.video)).collect();
        if videos.is_empty() { return Err("请先导入要裁剪的视频".into()); }
        Ok(videos)
    }

    pub async fn probe_trim(&self, folder: &Path) -> Result<sample::Probe, String> {
        let engine = self.handpose.clone().ok_or("找不到裁剪程序，请重新安装应用")?;
        crate::trim::probe(&engine, folder, &self.task_videos(folder)?).await
    }

    pub fn add_trim(&self, folder: &Path, segments: Vec<sample::Segment>) -> Result<(), String> {
        if self.handpose.is_none() { return Err("找不到裁剪程序，请重新安装应用".into()); }
        self.task_videos(folder)?;
        if segments.is_empty() || segments.iter().any(|s| s.start >= s.end) {
            return Err("请标记有效的裁剪片段".into());
        }
        let key = folder.to_string_lossy().into_owned();
        let job = {
            let mut s = self.state.lock().unwrap();
            if s.items.iter().any(|i| i.folder == key && matches!(i.stage, Stage::Queued | Stage::Qc | Stage::Handpose))
                || s.samples.iter().any(|j| j.folder == key && matches!(j.status, "queued" | "running")) {
                return Err("该任务正在处理，请先停止或等待完成后再裁剪".into());
            }
            s.samples.retain(|j| j.folder != key);
            let job = SampleJob { kind: "trim", folder: key.clone(), output: None, segments: segments.clone(), status: "queued", progress: None, error: None };
            s.samples.push(job.clone());
            s.queue.push_back(Work::Trim(key, segments));
            job
        };
        (self.emit)(Event::Sample(job));
        self.ensure_worker();
        Ok(())
    }

    async fn trim(&self, folder: String, segments: Vec<sample::Segment>) {
        let Some(engine) = self.handpose.clone() else { return };
        self.update_sample(&folder, |j| { j.status = "running"; j.progress = Some(0.0); });
        let task = PathBuf::from(&folder);
        let result = match self.task_videos(&task) {
            Ok(videos) => crate::trim::make(&engine, &task, &videos, &segments,
                |p| self.update_sample(&folder, |j| j.progress = Some(p))).await,
            Err(e) => Err(e),
        };
        match result {
            Ok(output) => {
                // 裁剪后的任务只加入预览，质检仍需手动启动。
                if let Err(e) = self.add(&[output.clone()]) { self.log(e, true); }
                self.log(format!("裁剪完成：{}（视频、timestamps 和 IMU 已同步删除所选片段，原目录保留）", output.display()), false);
                self.update_sample(&folder, |j| { j.status = "done"; j.progress = None; j.output = Some(output.to_string_lossy().into_owned()); });
            }
            Err(e) => {
                self.log(format!("裁剪失败：{e}"), true);
                self.update_sample(&folder, |j| { j.status = "failed"; j.progress = None; j.error = Some(e); });
            }
        }
    }

    /// 为任务目录生成样例（排队执行）：把标记的片段按时间顺序拼接成一份。与质检相互独立：
    /// 只截取视频、IMU 和 timestamps，其余源文件原样保留，不触发质检或关键点检测。
    pub fn add_sample(&self, folder: &Path, segments: Vec<sample::Segment>) -> Result<(), String> {
        if self.handpose.is_none() {
            return Err("找不到样例程序，请重新安装应用".into());
        }
        if scanner::videos(folder).is_empty() {
            return Err("任务目录里没有 videos/*.mp4".into());
        }
        if segments.is_empty() {
            return Err("请至少标记一个片段".into());
        }
        let key = folder.to_string_lossy().into_owned();
        let job = {
            let mut s = self.state.lock().unwrap();
            if s.samples.iter().any(|j| j.folder == key && matches!(j.status, "queued" | "running")) {
                return Err("该任务的样例正在生成".into());
            }
            s.samples.retain(|j| j.folder != key);
            let job = SampleJob { kind: "sample", folder: key.clone(), output: None, segments: segments.clone(), status: "queued", progress: None, error: None };
            s.samples.push(job.clone());
            s.queue.push_back(Work::Sample(key, segments));
            job
        };
        (self.emit)(Event::Sample(job));
        self.ensure_worker();
        Ok(())
    }

    async fn make_sample(&self, folder: String, segments: Vec<sample::Segment>) {
        let Some(engine) = self.handpose.clone() else { return };
        let task = PathBuf::from(&folder);
        let name = task.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        self.update_sample(&folder, |j| { j.status = "running"; j.progress = Some(0.0); });
        let started = std::time::Instant::now();
        let result = sample::make(&engine, &task, &segments, |p| self.update_sample(&folder, |j| j.progress = Some(p))).await;
        match result {
            Ok(made) => {
                self.log(format!("📦 {name}：已生成样例 {}（{} 段，共 {:.1} 秒 {} 帧，视频、IMU 和 timestamps 已截取，其余文件原样保留，耗时 {:.0} 秒）",
                                 made.output.display(), segments.len(), made.duration, made.frames,
                                 started.elapsed().as_secs_f64()), false);
                let output = made.output.to_string_lossy().into_owned();
                self.update_sample(&folder, |j| { j.status = "done"; j.progress = None; j.output = Some(output); });
            }
            Err(e) => {
                self.log(format!("❌ {name}：生成样例失败，{e}"), true);
                self.update_sample(&folder, |j| { j.status = "failed"; j.progress = None; j.error = Some(e); });
            }
        }
    }

    fn update_sample(&self, folder: &str, change: impl FnOnce(&mut SampleJob)) {
        let job = {
            let mut s = self.state.lock().unwrap();
            let Some(job) = s.samples.iter_mut().find(|j| j.folder == folder) else { return };
            change(job);
            job.clone()
        };
        (self.emit)(Event::Sample(job));
    }

    async fn process(&self, item: Item) {
        let id = item.id;
        let folder = PathBuf::from(&item.folder);
        let video = PathBuf::from(&item.video);
        let label = format!("{}/{}", item.task, item.view);
        let Ok(source) = Fingerprint::of(&video) else {
            self.fail(id, &label, "视频无法读取");
            return;
        };
        let mut mark = FolderMark::load(&folder);
        let record = mark.get(&video);
        let output = folder.join("handpose").join(format!("{}.json", item.view));
        let rel_output = format!("handpose/{}.json", item.view);

        // 1. 模型质检
        match record.as_ref().and_then(|r| r.qc.clone()).filter(|qc| SCENES.iter().all(|s| qc.scenes.contains_key(s))) {
            Some(cached) => self.update(id, |i| { i.qc = Some(cached.scenes); i.cached = true; }),
            None => match &self.qc {
                None => self.update(id, |i| i.qc_error = Some("找不到质检程序，请重新安装应用".into())),
                Some(engine) => {
                    self.update(id, |i| i.stage = Stage::Qc);
                    match engine.run(&video, &SCENES).await {
                        Ok(result) => {
                            if let Err(e) = mark.set_qc(&video, source, &result) {
                                self.log(format!("{label}：质检完成，但无法写入处理记录（{e}）"), true);
                            }
                            self.log(format!("🔍 {label}：{}，耗时 {:.0} 秒", qc::summary(&result.scenes), result.elapsed_sec), false);
                            self.update(id, |i| { i.qc = Some(result.scenes); i.cached = false; });
                        }
                        Err(e) => {
                            self.log(format!("❌ {label}：质检失败，{e}"), true);
                            self.update(id, |i| i.qc_error = Some(e.to_string()));
                        }
                    }
                }
            },
        }

        // 2. 手部关键点
        match record.as_ref().and_then(|r| r.handpose.clone()) {
            Some(cached) => self.update(id, |i| {
                i.handpose = Some(output.to_string_lossy().into_owned());
                i.handpose_stats = Some(cached.stats);
            }),
            None => match &self.handpose {
                None => self.update(id, |i| i.handpose_error = Some("找不到关键点程序，请重新安装应用".into())),
                Some(engine) => {
                    self.update(id, |i| { i.stage = Stage::Handpose; i.progress = Some(0.0); });
                    let result = engine
                        .run(&video, &output, |done, total| {
                            if total > 0.0 {
                                self.update(id, |i| i.progress = Some((done / total).clamp(0.0, 1.0)));
                            }
                        })
                        .await;
                    match result {
                        Ok(stats) => {
                            if let Err(e) = mark.set_handpose(&video, source, &rel_output, stats.clone()) {
                                self.log(format!("{label}：关键点完成，但无法写入处理记录（{e}）"), true);
                            }
                            self.log(format!("✋ {label}：{} 帧中 {} 帧检出手（{:.0}%）", stats.frames, stats.frames_with_hands,
                                             stats.hand_ratio * 100.0), false);
                            self.update(id, |i| {
                                i.handpose = Some(output.to_string_lossy().into_owned());
                                i.handpose_stats = Some(stats);
                                i.cached = false;
                            });
                        }
                        Err(e) => {
                            self.log(format!("❌ {label}：关键点提取失败，{e}"), true);
                            self.update(id, |i| i.handpose_error = Some(e));
                        }
                    }
                }
            },
        }

        self.update(id, |i| {
            i.progress = None;
            i.stage = if i.qc_error.is_some() || i.handpose_error.is_some() { Stage::Failed } else { Stage::Done };
        });
    }

    fn fail(&self, id: u64, label: &str, reason: &str) {
        self.log(format!("❌ {label}：{reason}"), true);
        self.update(id, |i| {
            i.stage = Stage::Failed;
            i.qc_error = Some(reason.into());
        });
    }

    fn update(&self, id: u64, change: impl FnOnce(&mut Item)) {
        let item = {
            let mut s = self.state.lock().unwrap();
            let Some(item) = s.items.iter_mut().find(|i| i.id == id) else { return };
            change(item);
            item.clone()
        };
        (self.emit)(Event::Item(item));
    }

    fn emit_item(&self, id: u64) {
        let item = self.state.lock().unwrap().items.iter().find(|i| i.id == id).cloned();
        if let Some(item) = item {
            (self.emit)(Event::Item(item));
        }
    }

    fn log(&self, text: String, error: bool) {
        (self.emit)(Event::Log { text, error });
    }
}

fn new_item(id: u64, v: &VideoItem) -> Item {
    Item {
        id,
        task: v.folder.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        folder: v.folder.to_string_lossy().into_owned(),
        view: v.view(),
        video: v.video.to_string_lossy().into_owned(),
        stage: Stage::Ready,
        progress: None,
        qc: None,
        qc_error: None,
        handpose: None,
        handpose_stats: None,
        handpose_error: None,
        cached: false,
    }
}

fn reset(item: &mut Item) {
    item.progress = None;
    item.qc = None;
    item.qc_error = None;
    item.handpose = None;
    item.handpose_stats = None;
    item.handpose_error = None;
    item.cached = false;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scanner::tests::TempDir;
    use std::time::Duration;

    fn engines() -> (Option<qc::Engine>, Option<handpose::Engine>) {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("resources");
        (qc::Engine::locate(&dir), handpose::Engine::locate(&dir))
    }

    async fn wait_idle(jobs: &Jobs, limit: Duration) -> Vec<Item> {
        let started = std::time::Instant::now();
        loop {
            let items = jobs.list();
            if items.iter().all(|i| matches!(i.stage, Stage::Done | Stage::Failed)) || started.elapsed() > limit {
                return items;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn missing_engines_fail_without_touching_videos() {
        let t = TempDir::new();
        let video = t.file("任务/videos/top_left.mp4", b"not really a video");
        let jobs = Jobs::new(None, None, |_| {}, |_| {});
        assert!(jobs.add(&[t.0.join("空")]).is_err());
        assert_eq!(jobs.add(&[t.0.join("任务")]).unwrap(), 1);
        tokio::task::yield_now().await;
        assert_eq!(jobs.list()[0].stage, Stage::Ready, "导入不能自动质检");
        assert!(jobs.state.lock().unwrap().worker.is_none());
        assert_eq!(jobs.start(&t.0.join("任务")).unwrap(), 1);
        let items = wait_idle(&jobs, Duration::from_secs(5)).await;
        assert_eq!(items[0].stage, Stage::Failed);
        assert!(items[0].qc_error.as_deref().unwrap().contains("质检程序"));
        assert_eq!(std::fs::read(&video).unwrap(), b"not really a video");
        // 再次导入也不会触发重试；通过按钮重试。
        assert_eq!(jobs.add(&[t.0.join("任务")]).unwrap(), 0);
        assert_eq!(jobs.list()[0].stage, Stage::Failed);
        jobs.start(&t.0.join("任务")).unwrap();
        wait_idle(&jobs, Duration::from_secs(5)).await;
        assert_eq!(jobs.list().len(), 1);
    }

    /// 手动运行，对指定目录走一遍和 app 拖入完全相同的处理流程（结果写进该目录）：
    /// QC_E2E_DIR=/path/to/任务目录 cargo test --lib e2e_dir -- --ignored --nocapture
    #[tokio::test(flavor = "multi_thread")]
    #[ignore]
    async fn e2e_dir() {
        let dir = PathBuf::from(std::env::var("QC_E2E_DIR").expect("设置 QC_E2E_DIR"));
        let (Some(q), Some(h)) = engines() else { panic!("未构建引擎") };
        let started = std::time::Instant::now();
        let jobs = Jobs::new(Some(q), Some(h), |_| {}, move |event| {
            if let Event::Log { text, .. } = event {
                eprintln!("[{:>5.0}s] {text}", started.elapsed().as_secs_f64());
            }
        });
        eprintln!("加入 {} 个视频", jobs.add(&[dir.clone()]).unwrap());
        jobs.start(&dir).unwrap();
        for item in wait_idle(&jobs, Duration::from_secs(4 * 3600)).await {
            eprintln!("{}/{}: {:?} 质检={:?} 关键点={:?} 错误={:?}/{:?}", item.task, item.view, item.stage,
                      item.qc.as_ref().map(qc::summary), item.handpose_stats, item.qc_error, item.handpose_error);
            assert_eq!(item.stage, Stage::Done);
        }
        eprintln!("总耗时 {:.0} 秒", started.elapsed().as_secs_f64());
    }

    /// 测试用任务目录：视频用硬链接（扫描不跟随符号链接），其它数据用符号链接，不复制、不改原目录
    #[cfg(unix)]
    fn link_task(source: &Path, task: &Path) {
        std::fs::create_dir_all(task.join("videos")).unwrap();
        for entry in std::fs::read_dir(source.join("videos")).unwrap().flatten() {
            if !entry.file_name().to_string_lossy().starts_with('.') {
                let dst = task.join("videos").join(entry.file_name());
                std::fs::hard_link(entry.path(), &dst).or_else(|_| std::fs::copy(entry.path(), &dst).map(|_| ())).unwrap();
            }
        }
        for sub in ["timestamps", "imu", "calibration"] {
            std::os::unix::fs::symlink(source.join(sub), task.join(sub)).unwrap();
        }
    }

    async fn wait_sample(jobs: &Jobs) -> SampleJob {
        let started = std::time::Instant::now();
        loop {
            let job = jobs.samples()[0].clone();
            if matches!(job.status, "done" | "failed") {
                return job;
            }
            assert!(started.elapsed() < Duration::from_secs(300));
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    /// 生成样例只截取三个数据流，保留其余文件；包括已有关键点和记录，且不触发检测。
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn sample_only_crops_streams_and_preserves_other_files() {
        let (_, Some(h)) = engines() else {
            eprintln!("跳过：未构建引擎");
            return;
        };
        let Some(video) = crate::qc::tests::sample_video() else { return };
        let source = video.parent().unwrap().parent().unwrap().to_path_buf();
        let t = TempDir::new();
        let task = t.0.join("汽修");
        link_task(&source, &task);
        let jobs = Jobs::new(None, Some(h), |_| {}, |_| {});
        let seg = |a, b| sample::Segment { start: a, end: b };
        // 两段：第 30–300 帧和第 600–901 帧（第 30、600 帧是关键帧）
        let segments = vec![seg(600, 901), seg(30, 300)];

        // 1. 还没做关键点：样例只有视频和数据，不带关键点；不触发质检
        assert!(jobs.add_sample(&task, vec![]).unwrap_err().contains("片段"));
        jobs.add_sample(&task, segments.clone()).unwrap();
        assert!(jobs.add_sample(&task, segments.clone()).unwrap_err().contains("正在生成"));
        let job = wait_sample(&jobs).await;
        assert_eq!(job.status, "done", "{:?}", job.error);
        let out = PathBuf::from(job.output.unwrap());
        assert_eq!(out.parent().unwrap(), t.0);
        assert!(out.join("videos/top_left.mp4").is_file() && out.join("segments.json").is_file());
        assert!(!out.join("handpose").exists());
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert!(jobs.list().is_empty(), "生成样例不应自动开始质检");
        assert!(!out.join(crate::mark::MARK_FILE).exists());

        // 2. 源目录中的其他文件保持原样，生成不读取或变更已有分析结果。
        let pose = br#"{"source-only-frames":[1,2,3]}"#;
        let record = br#"{"source-only-record":true}"#;
        t.file("汽修/handpose/top_left.json", pose);
        t.file("汽修/metadata.json", br#"{"source":true}"#);
        t.file("汽修/evidence/raw.bin", b"source-evidence");
        t.file("汽修/.qcworker.json", record);
        jobs.add(&[task.join("videos/top_left.mp4")]).unwrap();
        jobs.add_sample(&task, segments.clone()).unwrap();
        let job = wait_sample(&jobs).await;
        assert_eq!(job.status, "done", "{:?}", job.error);
        let out2 = PathBuf::from(job.output.unwrap());
        assert_ne!(out2, out);
        assert!(out2.join("handpose/top_left.json").is_file());
        assert!(!out2.join("handpose/top_right.json").exists(), "右目没做过关键点，不应生成");
        assert_eq!(jobs.list().len(), 2, "左右目都用于预览，仍不应自动处理样例");
        assert!(jobs.list().iter().all(|i| i.stage == Stage::Ready), "生成样例不应启动质检");
        assert_eq!(std::fs::read(out2.join("handpose/top_left.json")).unwrap(), pose);
        assert_eq!(std::fs::read(out2.join(crate::mark::MARK_FILE)).unwrap(), record);
        assert_eq!(std::fs::read(out2.join("metadata.json")).unwrap(), std::fs::read(task.join("metadata.json")).unwrap());
        assert_eq!(std::fs::read(out2.join("evidence/raw.bin")).unwrap(), b"source-evidence");
        let info = jobs.probe(&out2).await.unwrap();
        assert_eq!(info.frames, 270 + 301, "视频仍必须按选段截取");
    }

    #[test]
    fn only_known_folders_can_be_revealed() {
        let t = TempDir::new();
        t.file("任务/videos/top_left.mp4", b"x");
        let jobs = Jobs::new(None, None, |_| {}, |_| {});
        assert!(!jobs.knows_folder(&t.0.join("任务")));
        jobs.add(&[t.0.join("任务")]).unwrap();
        assert!(jobs.knows_folder(&t.0.join("任务")));
        assert!(!jobs.knows_folder(&t.0), "上一级目录不在列表里");
        assert!(!jobs.knows_folder(Path::new("/")));
        assert!(!jobs.knows_folder(&t.0.join("任务/videos")));
    }

    /// 端到端：真实样例视频跑质检和关键点，第二次加入时直接用记录。需要两个引擎和样例视频，否则跳过。
    #[tokio::test(flavor = "multi_thread")]
    async fn processes_sample_then_uses_record() {
        let (Some(q), Some(h)) = engines() else {
            eprintln!("跳过：未构建引擎");
            return;
        };
        let Some(sample) = crate::qc::tests::sample_video() else { return };
        let t = TempDir::new();
        let video = t.0.join("汽修/videos/top_left.mp4");
        std::fs::create_dir_all(video.parent().unwrap()).unwrap();
        std::fs::copy(&sample, &video).unwrap();
        let allowed = Arc::new(Mutex::new(Vec::new()));
        let seen = allowed.clone();
        let jobs = Jobs::new(Some(q.clone()), Some(h.clone()), move |p| seen.lock().unwrap().push(p.to_path_buf()), |_| {});
        jobs.add(&[t.0.clone()]).unwrap();
        jobs.start(&t.0.join("汽修")).unwrap();
        let items = wait_idle(&jobs, Duration::from_secs(600)).await;
        let item = &items[0];
        eprintln!("{:?} qc={:?} handpose={:?}", item.stage, item.qc.as_ref().map(qc::summary), item.handpose_stats);
        assert_eq!(item.stage, Stage::Done, "{:?} {:?}", item.qc_error, item.handpose_error);
        assert!(!item.cached);
        assert!(t.0.join("汽修/handpose/top_left.json").is_file());
        assert_eq!(*allowed.lock().unwrap(), vec![video.clone()]);

        let again = Jobs::new(Some(q), Some(h), |_| {}, |_| {});
        again.add(&[t.0.join("汽修")]).unwrap();
        assert_eq!(again.list()[0].stage, Stage::Ready);
        again.start(&t.0.join("汽修")).unwrap();
        let items = wait_idle(&again, Duration::from_secs(30)).await;
        assert_eq!(items[0].stage, Stage::Done);
        assert!(items[0].cached, "第二次应直接使用记录");
    }

    #[test]
    fn ready_previews_can_be_cleared_without_touching_files() {
        let t = TempDir::new();
        let video = t.file("任务/videos/top_left.mp4", b"video");
        let jobs = Jobs::new(None, None, |_| {}, |_| {});
        jobs.add(&[video.clone()]).unwrap();
        jobs.clear_finished();
        assert!(jobs.list().is_empty());
        assert_eq!(std::fs::read(video).unwrap(), b"video");
    }

    #[test]
    fn trimming_blocks_qc_until_finished() {
        let t = TempDir::new();
        t.file("任务/videos/top_left.mp4", b"video");
        let jobs = Jobs::new(None, None, |_| {}, |_| {});
        let folder = t.0.join("任务");
        jobs.add(&[folder.clone()]).unwrap();
        jobs.state.lock().unwrap().samples.push(SampleJob { kind: "trim", folder: folder.to_string_lossy().into_owned(), output: None,
            segments: vec![], status: "running", progress: Some(0.0), error: None });
        assert!(jobs.start(&folder).unwrap_err().contains("裁剪"));
        assert_eq!(jobs.list()[0].stage, Stage::Ready);
    }
}
