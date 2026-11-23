import { useEffect, useRef, useState } from "react";
import * as api from "./api";
import type { HandposeData, HandposeFrame, Item, Scene } from "./api";
import { SCENE_LABEL } from "./App";
import type { HandPose3D } from "./handpose3d";
import { useStereoSync } from "./useStereoSync";

const SIDE_COLOR: Record<string, string> = { Left: "#3c9b74", Right: "#d18b3c" };
const SIDE_LABEL: Record<string, string> = { Left: "左", Right: "右" };
const SCENES: Scene[] = ["no_hand", "light"];

function timecode(seconds: number, decimals = false) {
  const v = Math.max(0, seconds || 0);
  const m = Math.floor(v / 60);
  const s = decimals ? (v % 60).toFixed(2).padStart(5, "0") : String(Math.floor(v % 60)).padStart(2, "0");
  return `${String(m).padStart(2, "0")}:${s}`;
}

/** 播放视频：视频上叠加当前帧的 2D 骨架，旁边并排显示 3D 视图，下方是质检时间轴 */
export function Player({ item, companion, data, error }: { item: Item; companion?: Item; data: HandposeData | null; error: string }) {
  const videoRef = useRef<HTMLVideoElement>(null);
  const peerRef = useRef<HTMLVideoElement>(null);
  useStereoSync(videoRef, peerRef, `${item.video}:${companion?.video}`);
  const stageRef = useRef<HTMLDivElement>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const view3dRef = useRef<HTMLDivElement>(null);
  const poseRef = useRef<HandPose3D | null>(null);
  const [overlay, setOverlay] = useState(true);
  const [show3d, setShow3d] = useState(true);
  const [info, setInfo] = useState("");
  const [time, setTime] = useState(0);
  const [duration, setDuration] = useState(0);
  const [playError, setPlayError] = useState("");

  // 3D 视图：有数据后才加载 three.js
  useEffect(() => {
    if (!data || !show3d || !view3dRef.current) return;
    let disposed = false;
    import("./handpose3d").then(({ HandPose3D }) => {
      if (disposed || !view3dRef.current) return;
      const view = new HandPose3D(view3dRef.current);
      view.load(data);
      poseRef.current = view;
      render(videoRef.current?.currentTime ?? 0);
    });
    return () => {
      disposed = true;
      poseRef.current?.dispose();
      poseRef.current = null;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [data, show3d]);

  // 二分查找当前时间对应的关键点帧（3D 视图未加载时也要能画 2D 叠加）
  const frameAt = (t: number): HandposeFrame | null => {
    if (!data) return null;
    const f = data.frames;
    if (!f.length || t < f[0].t) return null;
    let lo = 0, hi = f.length - 1;
    while (lo < hi) {
      const mid = (lo + hi + 1) >> 1;
      if (f[mid].t <= t) lo = mid; else hi = mid - 1;
    }
    return t - f[lo].t <= 0.5 ? f[lo] : null;
  };

  const drawOverlay = (frame: HandposeFrame | null) => {
    const video = videoRef.current, canvas = canvasRef.current, stage = stageRef.current;
    if (!video || !canvas || !stage || !video.videoWidth) return;
    // 视频按 contain 显示：算出画面实际所在矩形，叠加层与之对齐，不落到黑边
    const sw = stage.clientWidth, sh = stage.clientHeight;
    const scale = Math.min(sw / video.videoWidth, sh / video.videoHeight);
    const w = video.videoWidth * scale, h = video.videoHeight * scale;
    Object.assign(canvas.style, { left: `${(sw - w) / 2}px`, top: `${(sh - h) / 2}px`, width: `${w}px`, height: `${h}px` });
    const ratio = Math.min(2, window.devicePixelRatio || 1);
    const cw = Math.round(w * ratio), ch = Math.round(h * ratio);
    if (canvas.width !== cw || canvas.height !== ch) { canvas.width = cw; canvas.height = ch; }
    const ctx = canvas.getContext("2d")!;
    ctx.clearRect(0, 0, cw, ch);
    if (!overlay || !data || !frame) return;
    for (const hand of frame.hands) {
      const pts = hand.image.map(([x, y]) => [x * cw, y * ch] as const);
      ctx.strokeStyle = SIDE_COLOR[hand.side] ?? "#fff";
      ctx.lineWidth = 2 * ratio;
      ctx.beginPath();
      for (const [a, b] of data.connections) { ctx.moveTo(...pts[a]); ctx.lineTo(...pts[b]); }
      ctx.stroke();
      ctx.fillStyle = "#fff";
      pts.forEach(([x, y], k) => {
        ctx.beginPath();
        ctx.arc(x, y, (k % 4 === 0 ? 3.5 : 2.5) * ratio, 0, Math.PI * 2);
        ctx.fill();
      });
    }
  };

  const render = (t: number) => {
    const frame = frameAt(t);
    poseRef.current?.update(t);
    drawOverlay(frame);
    if (data) {
      const hands = frame?.hands.map((h) => `${SIDE_LABEL[h.side]} ${h.score.toFixed(2)}`).join(" · ");
      setInfo(frame ? hands || "无手" : "该时间没有关键点数据");
    }
  };

  // 同步：每显示一帧回调一次，并给出这一帧的准确时间；不支持时退回按屏幕刷新取 currentTime
  useEffect(() => {
    const video = videoRef.current;
    if (!video || !data) return;
    let handle = 0, stopped = false;
    const hasVfc = "requestVideoFrameCallback" in HTMLVideoElement.prototype;
    if (hasVfc) {
      const onFrame = (_now: number, meta: VideoFrameCallbackMetadata) => {
        if (stopped) return;
        render(meta.mediaTime);
        handle = video.requestVideoFrameCallback(onFrame);
      };
      handle = video.requestVideoFrameCallback(onFrame);
    } else {
      const step = () => { if (!video.paused) render(video.currentTime); handle = requestAnimationFrame(step); };
      handle = requestAnimationFrame(step);
    }
    const redraw = () => render(video.currentTime);
    for (const type of ["seeked", "pause", "loadedmetadata"]) video.addEventListener(type, redraw);
    const observer = new ResizeObserver(redraw);
    if (stageRef.current) observer.observe(stageRef.current);
    redraw();
    return () => {
      stopped = true;
      if (hasVfc) video.cancelVideoFrameCallback(handle); else cancelAnimationFrame(handle);
      for (const type of ["seeked", "pause", "loadedmetadata"]) video.removeEventListener(type, redraw);
      observer.disconnect();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [data, overlay]);

  const seek = (t: number) => {
    const video = videoRef.current;
    if (!video) return;
    video.currentTime = t;
    video.play().catch(() => {});
  };

  const total = duration || data?.duration_sec || 0;
  const label = `${item.task} · ${item.view === "top_left" ? "左目" : item.view === "top_right" ? "右目" : item.view}`;

  return (
    <section className="card player">
      <div className="card-head">
        <h2>{label}</h2>
        <span className="muted small path" title={item.video}>{item.video}</span>
      </div>

      <div className="stage-row">
        <div className="video-stage" ref={stageRef} style={{ order: item.view === "top_right" ? 1 : 0 }}>
          <video ref={videoRef} src={api.videoSrc(item.video)} controls playsInline preload="metadata"
                 onLoadedMetadata={(e) => setDuration(e.currentTarget.duration)}
                 onTimeUpdate={(e) => setTime(e.currentTarget.currentTime)}
                 onError={() => setPlayError("无法播放此视频（可能是系统不支持该编码，Windows 需安装 HEVC 视频扩展）。质检和关键点数据不受影响。")} />
          <canvas className="pose-overlay" ref={canvasRef} aria-hidden />
          <span className="stage-label">{item.view === "top_left" ? "左目" : item.view === "top_right" ? "右目" : item.view} · 播放控制</span>
        </div>
        {companion && <div className="video-stage">
          <video ref={peerRef} src={api.videoSrc(companion.video)} playsInline muted preload="auto"
                 onError={() => setPlayError("另一目视频无法播放，请检查文件或系统视频编码支持。")} />
          <span className="stage-label">{companion.view === "top_left" ? "左目" : companion.view === "top_right" ? "右目" : companion.view} · 同步预览</span>
        </div>}
      </div>
      {show3d && data && <div className="stage-row pose-3d-row">
          <div className="view3d" ref={view3dRef}>
            <span className="stage-label">3D 手部骨架 · 拖动旋转</span>
            {!data && <div className="view3d-empty">{error || (item.handpose ? "正在加载关键点…" : stageText(item))}</div>}
          </div>
      </div>}

      <div className="pose-bar">
        <label className="switch"><input type="checkbox" checked={overlay} onChange={(e) => setOverlay(e.target.checked)} />视频上叠加骨架</label>
        <label className="switch"><input type="checkbox" checked={show3d} onChange={(e) => setShow3d(e.target.checked)} />显示 3D 视图</label>
        {show3d && data && <button className="btn btn-plain" onClick={() => poseRef.current?.resetView()}>重置视角</button>}
        <span className="legend"><i className="dot left" />左手<i className="dot right" />右手</span>
        <span className="frame-info">{timecode(time, true)}  {info}</span>
      </div>
      {playError && <div className="notice error">{playError}</div>}

      <div className="timeline">
        <div className="timeline-head">
          <h3>质检结果</h3>
          <span className="muted small">{item.qc ? "点击色块跳转" : item.qcError ?? stageText(item)}</span>
        </div>
        {SCENES.map((scene) => {
          const segments = item.qc?.[scene];
          return (
            <div key={scene} className="timeline-row">
              <span className="timeline-label">{SCENE_LABEL[scene]}</span>
              <div className="timeline-track">
                {total > 0 && segments?.map((s, i) => (
                  <button key={i} className={`timeline-bar ${scene}`} onClick={() => seek(s.start)}
                          style={{ left: `${(s.start / total) * 100}%`, width: `${Math.max(0.3, (s.duration / total) * 100)}%` }}
                          title={`${SCENE_LABEL[scene]} ${timecode(s.start, true)}–${timecode(s.end, true)}${s.reason ? ` · ${s.reason}` : ""}${s.unreliableDueToLight ? " · 光线差" : ""}`} />
                ))}
                {total > 0 && <span className="playhead" style={{ left: `${(time / total) * 100}%` }} />}
              </div>
              <span className="timeline-count">{segments ? `${segments.length} 段` : "—"}</span>
            </div>
          );
        })}
        <div className="time-labels"><span>00:00</span><span>{timecode(total)}</span></div>
      </div>
    </section>
  );
}

function stageText(item: Item) {
  if (item.stage === "ready") return "仅预览，点击「开始质检」后处理";
  if (item.stage === "queued") return "排队中…";
  if (item.stage === "qc") return "正在做模型质检…";
  if (item.stage === "handpose") return `正在提取关键点 ${Math.round((item.progress ?? 0) * 100)}%`;
  if (item.handposeError) return `关键点提取失败：${item.handposeError}`;
  return "";
}
