//! 处理记录：每个任务目录下的隐藏文件 `.qcworker.json`，记录每个视频的质检结果和关键点状态。
//!
//! 记录跟着目录走，复制或移动到其它磁盘、电脑后仍然有效。按视频文件名 + 大小 + 修改时间判断
//! 视频有没有变过，没变就直接用记录里的结果，不重新处理。
//!
//! 旧版（裁剪）写的 `trimSeconds` 等字段会原样保留，新版不读取。

use crate::handpose;
use crate::qc::{QcResult, Scene, Segment};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const MARK_FILE: &str = ".qcworker.json";

/// 视频文件的身份：大小 + 修改时间（毫秒）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Fingerprint {
    pub size: u64,
    pub mtime_ms: u64,
}

impl Fingerprint {
    pub fn of(video: &Path) -> io::Result<Self> {
        let meta = fs::metadata(video)?;
        let mtime_ms = meta.modified()?.duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
        Ok(Fingerprint { size: meta.len(), mtime_ms })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QcRecord {
    pub engine_version: String,
    pub checked_at: u64,
    pub scenes: BTreeMap<Scene, Vec<Segment>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HandposeRecord {
    /// 相对任务目录的结果文件路径，如 `handpose/top_left.json`
    pub path: String,
    pub stats: handpose::Stats,
    pub created_at: u64,
}

/// 一个视频的处理结果；`source` 和当前文件不一致时视为未处理
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoRecord {
    pub source: Fingerprint,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qc: Option<QcRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handpose: Option<HandposeRecord>,
}

/// 一个任务目录的处理记录
pub struct FolderMark {
    folder: PathBuf,
    path: PathBuf,
    /// 记录文件的原始内容，保留不认识的字段（如旧版的裁剪记录）
    raw: Map<String, Value>,
}

impl FolderMark {
    pub fn load(folder: &Path) -> Self {
        let path = folder.join(MARK_FILE);
        let raw = fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default();
        FolderMark { folder: folder.to_path_buf(), path, raw }
    }

    /// 视频的当前记录；视频变过（大小或修改时间不同）时返回 None
    pub fn get(&self, video: &Path) -> Option<VideoRecord> {
        let source = Fingerprint::of(video).ok()?;
        let record: VideoRecord = serde_json::from_value(self.processed()?.get(&file_key(video)?)?.clone()).ok()?;
        if record.source != source {
            return None;
        }
        // 关键点结果文件被删掉时也视为没做过
        if let Some(h) = &record.handpose {
            if !self.folder.join(&h.path).is_file() {
                return Some(VideoRecord { handpose: None, ..record });
            }
        }
        Some(record)
    }

    pub fn set_qc(&mut self, video: &Path, source: Fingerprint, result: &QcResult) -> io::Result<()> {
        let qc = QcRecord { engine_version: result.engine_version.clone(), checked_at: now_ms(), scenes: result.scenes.clone() };
        self.update(video, source, |r| r.qc = Some(qc))
    }

    pub fn set_handpose(&mut self, video: &Path, source: Fingerprint, path: &str, stats: handpose::Stats) -> io::Result<()> {
        let record = HandposeRecord { path: path.to_string(), stats, created_at: now_ms() };
        self.update(video, source, |r| r.handpose = Some(record))
    }

    fn update(&mut self, video: &Path, source: Fingerprint, change: impl FnOnce(&mut VideoRecord)) -> io::Result<()> {
        let key = file_key(video).ok_or_else(|| io::Error::other("无效的文件名"))?;
        // 视频在处理过程中变了（例如还在拷贝），之前的结果作废
        let mut record = self.get(video).filter(|r| r.source == source)
            .unwrap_or(VideoRecord { source, qc: None, handpose: None });
        change(&mut record);
        let processed = self.raw.entry("processed").or_insert_with(|| Value::Object(Map::new()));
        if !processed.is_object() {
            *processed = Value::Object(Map::new());
        }
        processed.as_object_mut().unwrap().insert(key, serde_json::to_value(record).map_err(io::Error::other)?);
        self.raw.insert("version".into(), Value::from(2));
        self.save()
    }

    fn processed(&self) -> Option<&Map<String, Value>> {
        self.raw.get("processed")?.as_object()
    }

    /// 写入记录文件（先写临时文件再替换，避免写一半损坏）
    fn save(&self) -> io::Result<()> {
        let json = serde_json::to_vec_pretty(&self.raw).map_err(io::Error::other)?;
        let tmp = self.path.with_extension("json.tmp");
        write_hidden(&tmp, &json)?;
        if let Err(e) = fs::rename(&tmp, &self.path) {
            let _ = fs::remove_file(&tmp);
            return Err(e);
        }
        Ok(())
    }
}

fn file_key(video: &Path) -> Option<String> {
    video.file_name().map(|n| n.to_string_lossy().into_owned())
}

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// 写入文件；Windows 上同时设为隐藏（macOS 上 `.` 开头的文件本身就是隐藏的）
fn write_hidden(path: &Path, data: &[u8]) -> io::Result<()> {
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
        opts.attributes(FILE_ATTRIBUTE_HIDDEN);
    }
    let mut file = opts.open(path)?;
    file.write_all(data)?;
    file.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scanner::tests::TempDir;

    fn qc_result() -> QcResult {
        let seg = Segment { start: 1.0, end: 7.0, duration: 6.0, reason: None, unreliable_due_to_light: Some(false) };
        QcResult { engine_version: "1.1.0".into(), scenes: BTreeMap::from([(Scene::NoHand, vec![seg]), (Scene::Light, vec![])]), elapsed_sec: 3.0 }
    }

    fn stats() -> handpose::Stats {
        handpose::Stats { frames: 901, frames_with_hands: 420, hand_ratio: 0.466 }
    }

    #[test]
    fn records_qc_and_handpose_and_persists() {
        let t = TempDir::new();
        let video = t.file("a/videos/top_left.mp4", b"abc");
        t.file("a/handpose/top_left.json", b"{}");
        let folder = t.0.join("a");
        let source = Fingerprint::of(&video).unwrap();

        let mut mark = FolderMark::load(&folder);
        assert!(mark.get(&video).is_none());
        mark.set_qc(&video, source, &qc_result()).unwrap();
        mark.set_handpose(&video, source, "handpose/top_left.json", stats()).unwrap();

        let record = FolderMark::load(&folder).get(&video).unwrap();
        assert_eq!(record.qc.unwrap().scenes[&Scene::NoHand].len(), 1);
        assert_eq!(record.handpose.unwrap().stats, stats());
        let json = fs::read_to_string(folder.join(MARK_FILE)).unwrap();
        assert!(json.contains("\"no_hand\""));
    }

    #[test]
    fn changed_video_or_missing_output_needs_redo() {
        let t = TempDir::new();
        let video = t.file("a/videos/top_left.mp4", b"abc");
        let folder = t.0.join("a");
        let source = Fingerprint::of(&video).unwrap();
        let mut mark = FolderMark::load(&folder);
        mark.set_qc(&video, source, &qc_result()).unwrap();
        // 结果文件不存在：关键点视为未做，质检结果保留
        mark.set_handpose(&video, source, "handpose/top_left.json", stats()).unwrap();
        let record = FolderMark::load(&folder).get(&video).unwrap();
        assert!(record.qc.is_some() && record.handpose.is_none());

        fs::write(&video, b"a different, longer file").unwrap();
        assert!(FolderMark::load(&folder).get(&video).is_none());
    }

    #[test]
    fn keeps_unknown_fields_from_old_version() {
        let t = TempDir::new();
        let video = t.file("a/videos/top_left.mp4", b"abc");
        let old = r#"{"version":1,"videos":{"top_left.mp4":{"size":3,"trimSeconds":3.0,"trimmedAt":1}}}"#;
        t.file(&format!("a/{MARK_FILE}"), old.as_bytes());
        let folder = t.0.join("a");
        let mut mark = FolderMark::load(&folder);
        assert!(mark.get(&video).is_none());
        mark.set_qc(&video, Fingerprint::of(&video).unwrap(), &qc_result()).unwrap();
        let json: Value = serde_json::from_slice(&fs::read(folder.join(MARK_FILE)).unwrap()).unwrap();
        assert_eq!(json["videos"]["top_left.mp4"]["trimSeconds"], 3.0);
        assert!(json["processed"]["top_left.mp4"]["qc"].is_object());
    }

    #[test]
    fn corrupt_record_file_is_ignored() {
        let t = TempDir::new();
        let video = t.file("a/videos/top_left.mp4", b"abc");
        t.file(&format!("a/{MARK_FILE}"), b"{not json");
        assert!(FolderMark::load(&t.0.join("a")).get(&video).is_none());
    }
}
