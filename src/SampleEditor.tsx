import { useEffect, useMemo, useRef, useState } from "react";
import * as api from "./api";
import type { FrameSegment, Item, SampleProbe } from "./api";
import { useStereoSync } from "./useStereoSync";

/**
 * 生成样例：在时间轴上标记要保留的片段（可多段），按时间顺序拼接成一份样例。
 *
 * 原视频不重编码，片段起点必须是关键帧（这批视频每 30 帧一个，约 1 秒），所以起点会吸附到最近的关键帧；
 * 终点可以是任意帧。重叠或首尾相接的片段在生成时合并。
 */

const VIEW_LABEL: Record<string, string> = { top_left: "左目", top_right: "右目" };
const DEFAULT_SECONDS = 10;

function timecode(seconds: number, decimals = true) {
  const v = Math.max(0, seconds || 0);
  const m = Math.floor(v / 60);
  const s = decimals ? (v % 60).toFixed(2).padStart(5, "0") : String(Math.floor(v % 60)).padStart(2, "0");
  return `${String(m).padStart(2, "0")}:${s}`;
}

/** 不晚于 t 的最后一帧 */
function frameAt(times: number[], t: number) {
  let lo = 0, hi = times.length - 1;
  if (t <= times[0]) return 0;
  while (lo < hi) {
    const mid = (lo + hi + 1) >> 1;
    if (times[mid] <= t) lo = mid; else hi = mid - 1;
  }
  return lo;
}

/** 离 frame 最近的关键帧 */
function nearestKeyframe(keyframes: number[], frame: number) {
  return keyframes.reduce((best, k) => (Math.abs(k - frame) < Math.abs(best - frame) ? k : best), keyframes[0]);
}

function merge(segments: FrameSegment[]) {
  const sorted = [...segments].sort((a, b) => a.start - b.start);
  const out: FrameSegment[] = [];
  for (const s of sorted) {
    const last = out[out.length - 1];
    if (last && s.start <= last.end) last.end = Math.max(last.end, s.end);
    else out.push({ ...s });
  }
  return out;
}

interface Props {
  folder: string;
  task: string;
  views: Item[];
  mode?: "sample" | "trim";
  onClose: () => void;
  onSubmitted: () => void;
}

export function SampleEditor({ folder, task, views, mode = "sample", onClose, onSubmitted }: Props) {
  const trimming = mode === "trim";
  const [probe, setProbe] = useState<SampleProbe | null>(null);
  const [error, setError] = useState("");
  const [segments, setSegments] = useState<FrameSegment[]>([]);
  const [selected, setSelected] = useState<number | null>(null);
  const [pendingStart, setPendingStart] = useState<number | null>(null);
  const orderedViews = [...views].sort((a, b) => a.view.localeCompare(b.view));
  const primary = orderedViews[0];
  const companion = orderedViews[1];
  const [time, setTime] = useState(0);
  const [submitting, setSubmitting] = useState(false);
  const videoRef = useRef<HTMLVideoElement>(null);
  const peerRef = useRef<HTMLVideoElement>(null);
  useStereoSync(videoRef, peerRef, `${folder}:${companion?.video}`);
  const trackRef = useRef<HTMLDivElement>(null);
  const listRef = useRef<HTMLDivElement>(null);
  const drag = useRef<{ index: number; edge: "start" | "end" | "move"; frame0: number; seg0: FrameSegment } | null>(null);

  useEffect(() => {
    let cancelled = false;
    (trimming ? api.probeTrim(folder) : api.probeSample(folder)).then((p) => { if (!cancelled) setProbe(p); }).catch((e) => { if (!cancelled) setError(String(e)); });
    return () => { cancelled = true; };
  }, [folder, trimming]);

  const duration = probe?.duration ?? 0;
  const times = probe?.times ?? [];
  const keyframes = useMemo(() => trimming ? times.map((_, i) => i) : probe?.keyframes ?? [], [trimming, times, probe]);
  const tOf = (frame: number) => (frame >= times.length ? duration : times[frame]);
  const pct = (t: number) => `${(t / Math.max(duration, 1e-6)) * 100}%`;

  const merged = useMemo(() => merge(segments), [segments]);
  const totalFrames = merged.reduce((n, s) => n + s.end - s.start, 0);
  const totalSeconds = merged.reduce((n, s) => n + tOf(s.end) - tOf(s.start), 0);

  const seek = (t: number) => {
    const v = videoRef.current;
    if (v) v.currentTime = Math.min(Math.max(0, t), duration);
  };

  // 直接读视频元素的当前时间：拖动进度条、跳转后 timeupdate 可能还没触发，state 里的时间会滞后
  const now = () => videoRef.current?.currentTime ?? time;

  /** 在当前播放位置加一段：起点吸附到不晚于当前帧的关键帧，默认 10 秒 */
  const addAtPlayhead = () => {
    if (!probe) return;
    const cur = frameAt(times, now());
    if (trimming) {
      setPendingStart(cur);
      setError("");
      return;
    }
    const start = [...keyframes].reverse().find((k) => k <= cur) ?? 0;
    const end = Math.min(probe.frames, frameAt(times, tOf(start) + DEFAULT_SECONDS) + 1);
    setSegments((prev) => [...prev, { start, end }]);
    setSelected(segments.length);
  };

  const finishAtPlayhead = () => {
    if (!probe || pendingStart === null) return;
    const end = Math.min(probe.frames, frameAt(times, now()) + 1);
    if (end <= pendingStart || now() < tOf(pendingStart)) {
      setError("终点必须在起点之后");
      return;
    }
    setSegments((prev) => [...prev, { start: pendingStart, end }]);
    setSelected(segments.length);
    setPendingStart(null);
    setError("");
  };

  const setEdge = (index: number, edge: "start" | "end", frame: number) => {
    if (!probe) return;
    setSegments((prev) => prev.map((s, i) => {
      if (i !== index) return s;
      if (edge === "start") {
        const start = nearestKeyframe(keyframes.filter((k) => k < s.end), Math.min(frame, s.end - 1));
        return { ...s, start };
      }
      return { ...s, end: Math.max(s.start + 1, Math.min(probe.frames, frame)) };
    }));
  };

  // 拖动片段两端或整段：按鼠标位置换算帧号，起点吸附到关键帧
  const frameAtPointer = (clientX: number) => {
    const rect = trackRef.current!.getBoundingClientRect();
    const t = ((clientX - rect.left) / rect.width) * duration;
    return frameAt(times, Math.max(0, Math.min(duration, t)));
  };
  const onPointerDown = (e: React.PointerEvent, index: number, edge: "start" | "end" | "move") => {
    e.stopPropagation();
    (e.target as Element).setPointerCapture(e.pointerId);
    drag.current = { index, edge, frame0: frameAtPointer(e.clientX), seg0: segments[index] };
    setSelected(index);
  };
  const onPointerMove = (e: React.PointerEvent) => {
    const d = drag.current;
    if (!d || !probe) return;
    const frame = frameAtPointer(e.clientX);
    if (d.edge === "move") {
      const len = d.seg0.end - d.seg0.start;
      const start = nearestKeyframe(keyframes.filter((k) => k + len <= probe.frames), d.seg0.start + frame - d.frame0);
      setSegments((prev) => prev.map((s, i) => (i === d.index ? { start, end: start + len } : s)));
      seek(tOf(start));
    } else {
      setEdge(d.index, d.edge, d.edge === "end" ? frame + 1 : frame);
      seek(tOf(d.edge === "end" ? frame : nearestKeyframe(keyframes, frame)));
    }
  };
  const onPointerUp = () => { drag.current = null; };

  const remove = (index: number) => {
    setSegments((prev) => prev.filter((_, i) => i !== index));
    setSelected(null);
  };

  const submit = async () => {
    setSubmitting(true);
    try {
      if (trimming) await api.trimVideos(folder, merged);
      else await api.makeSample(folder, merged);
      onSubmitted();
    } catch (e) {
      setError(String(e));
      setSubmitting(false);
    }
  };

  // 列表在框内滚动：新加或在时间轴上选中的片段，滚到可见位置
  useEffect(() => {
    if (selected === null) return;
    listRef.current?.querySelector(`[data-index="${selected}"]`)?.scrollIntoView({ block: "nearest" });
  }, [selected, segments.length]);

  // 快捷键：I 在播放头处加片段起点（新片段），O 把选中片段的终点设到播放头，Delete 删除选中片段
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.target as HTMLElement)?.tagName === "INPUT") return;
      if (e.key === "i" || e.key === "I") addAtPlayhead();
      if (e.key === "o" || e.key === "O") {
        if (trimming) finishAtPlayhead();
        else if (selected !== null) setEdge(selected, "end", frameAt(times, now()) + 1);
      }
      if ((e.key === "Delete" || e.key === "Backspace") && selected !== null) remove(selected);
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });

  const ticks = useMemo(() => {
    if (!duration) return [];
    const step = [5, 10, 30, 60, 120, 300].find((s) => duration / s <= 12) ?? 600;
    return Array.from({ length: Math.floor(duration / step) + 1 }, (_, i) => i * step);
  }, [duration]);

  return (
    <div className="modal-backdrop" onClick={onClose}>
      <section className={`card editor ${trimming ? "trim-editor" : ""}`} onClick={(e) => e.stopPropagation()} role="dialog" aria-modal="true" aria-label={trimming ? "裁剪视频" : "生成样例"}>
        <div className="card-head">
          <h2>{trimming ? "裁剪视频" : "生成样例"} · {task}</h2>
          <span className="muted small">{trimming ? "确定起点和终点以标记要删除的片段，完成后统一裁剪" : "在时间轴上标记要保留的片段，按时间顺序拼接成一份样例"}</span>
          <span className="grow" />
          <button className="btn btn-plain" onClick={onClose}>关闭</button>
        </div>

        {error && <div className="notice error editor-notice">{error}</div>}

        <div className="editor-body">
          <div className="editor-stereo">
          <div className="editor-video">
            <video ref={videoRef} src={primary ? api.videoSrc(primary.video) : undefined} controls playsInline preload="metadata"
                   onTimeUpdate={(e) => setTime(e.currentTarget.currentTime)}
                   onSeeked={(e) => setTime(e.currentTarget.currentTime)}
                   onError={() => setError("视频无法播放，请检查文件或系统视频编码支持。")} />
            <span className="stage-label">{VIEW_LABEL[primary?.view] ?? primary?.view} · 播放控制</span>
          </div>
          {companion && <div className="editor-video">
            <video ref={peerRef} src={api.videoSrc(companion.video)} playsInline muted preload="auto"
                   onError={() => setError("另一目视频无法播放，请检查文件或系统视频编码支持。")} />
            <span className="stage-label">{VIEW_LABEL[companion.view] ?? companion.view} · 同步预览</span>
          </div>}
          </div>

          {!probe ? (
            <div className="editor-loading muted">{error ? "" : "正在读取视频…"}</div>
          ) : (
            <>
              <div className="editor-toolbar">
                <button className="btn" onClick={addAtPlayhead}>{trimming ? "确定起点 (I)" : "＋ 在播放位置加片段"}</button>
                {trimming && <button className="btn" disabled={pendingStart === null} onClick={finishAtPlayhead}>确定终点 (O)</button>}
                {trimming && pendingStart !== null && <button className="btn btn-plain" onClick={() => setPendingStart(null)}>取消起点</button>}
                <span className="muted small">{trimming ? (pendingStart === null ? "点击时间轴跳转，可标记多段" : `起点 ${timecode(tOf(pendingStart))}，请选择终点`) : "快捷键：I 加片段 · O 设选中片段的终点 · Delete 删除"}</span>
                <span className="grow" />
                <span className="frame-info">{timecode(time)} · 第 {frameAt(times, time)} 帧</span>
              </div>

              <div className="cut-track" ref={trackRef} onPointerMove={onPointerMove} onPointerUp={onPointerUp}
                   onPointerDown={(e) => { setSelected(null); seek(tOf(frameAtPointer(e.clientX))); }}>
                {segments.map((s, i) => (
                  <div key={i} className={`cut-seg ${selected === i ? "active" : ""}`}
                       style={{ left: pct(tOf(s.start)), width: pct(tOf(s.end) - tOf(s.start)) }}
                       onPointerDown={(e) => onPointerDown(e, i, "move")}
                       title={`${timecode(tOf(s.start))}–${timecode(tOf(s.end))}`}>
                    <span className="cut-handle start" onPointerDown={(e) => onPointerDown(e, i, "start")} />
                    <span className="cut-handle end" onPointerDown={(e) => onPointerDown(e, i, "end")} />
                  </div>
                ))}
                {pendingStart !== null && <span className="pending-start" style={{ left: pct(tOf(pendingStart)) }} title="待确定终点的起点" />}
                <span className="playhead" style={{ left: pct(time) }} />
              </div>
              <div className="cut-ticks">
                {ticks.map((t) => <span key={t} style={{ left: pct(t) }}>{timecode(t, false)}</span>)}
              </div>
              {/* 拼接结果始终占位，没有片段时显示空轨道，片段增减时下面的内容不跳动 */}
              <div className="cut-result" title="生成时按这几段依次拼接；重叠或首尾相接的片段已合并">
                <span className="cut-result-label">{trimming ? "删除区间" : "拼接结果"}</span>
                <div className="cut-result-track">
                  {merged.map((s, i) => (
                    <span key={i} style={{ left: pct(tOf(s.start)), width: pct(tOf(s.end) - tOf(s.start)) }} />
                  ))}
                </div>
                <span className="cut-result-note muted small">
                  {merged.length < segments.length ? `重叠的片段已合并为 ${merged.length} 段` : ""}
                </span>
              </div>

              {/* 片段列表固定高度、在框内滚动：片段增多不挤压视频和时间轴 */}
              <div className="cut-list-head muted small">
                <span>片段 {segments.length}</span>
                <span className="grow" />
                <span>{trimming ? `删除 ${totalSeconds.toFixed(2)} 秒 · 剩余 ${(duration - totalSeconds).toFixed(2)} 秒 / ${probe.frames - totalFrames} 帧` : merged.length > 0 ? `拼接后共 ${totalSeconds.toFixed(2)} 秒 · ${totalFrames} 帧` : ""}</span>
              </div>
              <div className="cut-list" ref={listRef}>
                {segments.length === 0 ? (
                  <div className="cut-empty muted small">{trimming ? "还没有删除片段。在时间轴上选择位置并确定起点，再选择位置并确定终点，即生成一个片段。" : "还没有片段。播放到要开始的位置，点「在播放位置加片段」，再拖动片段两端调整。"}</div>
                ) : segments.map((s, i) => (
                  <div key={i} data-index={i} className={`cut-row ${selected === i ? "active" : ""}`} onClick={() => { setSelected(i); seek(tOf(s.start)); }}>
                    <span className="cut-index">{i + 1}</span>
                    <span>{timecode(tOf(s.start))} – {timecode(tOf(s.end))}</span>
                    <span className="muted small">{(tOf(s.end) - tOf(s.start)).toFixed(2)} 秒 · {s.end - s.start} 帧</span>
                    <span className="grow" />
                    <button className="btn btn-plain" onClick={(e) => { e.stopPropagation(); setSelected(i); setEdge(i, "end", frameAt(times, now()) + 1); }}>终点设为播放位置</button>
                    <button className="btn btn-plain" onClick={(e) => { e.stopPropagation(); remove(i); }}>删除</button>
                  </div>
                ))}
              </div>
            </>
          )}
        </div>

        <div className="editor-foot">
          {/* 说明文字固定，不随片段变化；总时长放在片段列表标题行 */}
          <span className="muted small editor-foot-note">
            {trimming ? "红色片段将从视频、IMU 和 timestamps 中同步删除；结果另存到旁边的「_裁剪」目录，原目录保留。不会自动质检。" : <>
              片段起点吸附到关键帧（约每 1 秒一个）
              {" · "}视频、IMU、timestamps 按选段截取，其余文件原样保留
              {" · "}只生成样例，不会开始质检
            </>}
          </span>
          <span className="grow" />
          <button className="btn btn-primary" disabled={!probe || merged.length === 0 || submitting || (trimming && (pendingStart !== null || totalFrames >= probe.frames))} onClick={submit}>
            {submitting ? "正在提交…" : trimming ? "完成裁剪" : "生成样例"}
          </button>
        </div>
      </section>
    </div>
  );
}
