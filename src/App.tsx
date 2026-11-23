import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { open } from "@tauri-apps/plugin-dialog";
import * as api from "./api";
import type { HandposeData, Item, LogLine, SampleJob, Scene } from "./api";
import { Player } from "./Player";
import { SampleEditor } from "./SampleEditor";

const MAX_LOGS = 300;
/** 生成的样例目录名：<任务名>_样例<秒>s，重名时加 _2、_3… */
const SAMPLE_DIR = /_样例\d+s(_\d+)?$/;
const baseName = (path: string) => path.split(/[\\/]/).filter(Boolean).pop() ?? path;
const STAGE_LABEL: Record<Item["stage"], string> = {
  ready: "待质检", queued: "排队中", qc: "质检中", handpose: "关键点", done: "完成", failed: "失败",
};
const VIEW_LABEL: Record<string, string> = { top_left: "左目", top_right: "右目" };
export const SCENE_LABEL: Record<Scene, string> = { no_hand: "无手", light: "光线异常" };

export default function App() {
  const [items, setItems] = useState<Item[]>([]);
  const [samples, setSamples] = useState<SampleJob[]>([]);
  const [ready, setReady] = useState({ qc: true, handpose: true });
  const [selected, setSelected] = useState<number | null>(null);
  const [dragging, setDragging] = useState(false);
  const [logs, setLogs] = useState<(LogLine & { time: number })[]>([]);
  const [notice, setNotice] = useState("");
  const [pose, setPose] = useState<{ id: number; data: HandposeData } | null>(null);
  const [poseError, setPoseError] = useState("");
  // 正在剪裁样例的任务目录
  const [editing, setEditing] = useState<string | null>(null);
  const [editMode, setEditMode] = useState<"sample" | "trim">("sample");

  const upsert = useCallback((item: Item) => {
    setItems((prev) => {
      const i = prev.findIndex((x) => x.id === item.id);
      if (i < 0) return [...prev, item];
      const next = prev.slice();
      next[i] = item;
      return next;
    });
  }, []);

  const add = useCallback(async (paths: string[]) => {
    if (!paths.length) return;
    try {
      const n = await api.addPaths(paths);
      setNotice(n ? "" : "这些视频已在列表中");
    } catch (error) {
      setNotice(String(error));
    }
  }, []);

  useEffect(() => {
    const unlisten = [
      api.onItem(upsert),
      api.onSample((job) => {
        setSamples((prev) => [...prev.filter((j) => j.folder !== job.folder), job]);
        if (job.kind === "trim" && job.status === "done" && job.output) {
          api.getStatus().then((s) => {
            setItems(s.items);
            const first = s.items.find((i) => i.folder === job.output);
            if (first) setSelected(first.id);
          }).catch((e) => setNotice(String(e)));
        }
      }),
      api.onLog((line) => setLogs((prev) => [...prev, { ...line, time: Date.now() }].slice(-MAX_LOGS))),
      // 拖放由窗口接收：Tauri 拿到的是文件系统路径（网页的 File 对象没有路径）
      getCurrentWebview().onDragDropEvent((event) => {
        const p = event.payload;
        if (p.type === "enter" || p.type === "over") setDragging(true);
        else setDragging(false);
        if (p.type === "drop") add(p.paths);
      }),
    ];
    api.getStatus().then((s) => {
      setReady({ qc: s.qcReady, handpose: s.handposeReady });
      // 订阅期间可能已收到拖入事件；初始快照不能覆盖这些较新的条目。
      setItems((prev) => [...new Map([...s.items, ...prev].map((i) => [i.id, i])).values()]);
      setSamples((prev) => [...new Map([...s.samples, ...prev].map((j) => [j.folder, j])).values()]);
    });
    return () => unlisten.forEach((u) => u.then((fn) => fn()));
  }, [add, upsert]);

  // 第一个可预览的视频自动选中
  useEffect(() => {
    if (selected === null) {
      const first = items[0];
      if (first) setSelected(first.id);
    }
  }, [items, selected]);

  const current = items.find((i) => i.id === selected) ?? null;

  // 选中视频的关键点结果准备好后加载
  useEffect(() => {
    if (!current?.handpose || pose?.id === current.id) return;
    let cancelled = false;
    setPoseError("");
    api.readHandpose(current.id)
      .then((data) => { if (!cancelled) setPose({ id: current.id, data }); })
      .catch((e) => { if (!cancelled) setPoseError(String(e)); });
    return () => { cancelled = true; };
  }, [current?.id, current?.handpose, pose?.id]);

  const reveal = (folder: string) => api.revealFolder(folder).catch((e) => setNotice(String(e)));

  // 本次生成过的样例，按完成先后，最新的在前
  const finished = samples.filter((j) => j.status === "done" && j.output).reverse();

  const choose = async () => {
    const dirs = await open({ directory: true, multiple: true, title: "选择任务目录（含 videos 子目录）" });
    if (dirs) add(Array.isArray(dirs) ? dirs : [dirs]);
  };

  const tasks = useMemo(() => {
    const groups = new Map<string, Item[]>();
    for (const item of items) groups.set(item.folder, [...(groups.get(item.folder) ?? []), item]);
    return [...groups.entries()];
  }, [items]);

  const active = items.some((i) => ["queued", "qc", "handpose"].includes(i.stage))
    || samples.some((j) => j.status === "queued" || j.status === "running");
  const missing = [!ready.qc && "质检程序", !ready.handpose && "关键点程序"].filter(Boolean);

  return (
    <div className="app">
      <header className="header">
        <div className="app-icon" aria-hidden><HandIcon /></div>
        <div className="header-text">
          <h1>视频质检 · 手部关键点</h1>
          <p>拖入视频后同步预览左右目，可先裁剪，再点击「开始质检」</p>
        </div>
        <span className={`badge ${active ? "badge-on" : ""}`}><span className="dot" />{active ? "处理中" : "空闲"}</span>
      </header>

      {missing.length > 0 && <div className="notice error">找不到{missing.join("、")}，请重新安装应用。</div>}
      {notice && <div className="notice">{notice}</div>}

      <div className="workbench">
        <aside className="sidebar">
          <div className={`dropzone ${dragging ? "dragging" : ""}`} onClick={choose} role="button" tabIndex={0}
               onKeyDown={(e) => (e.key === "Enter" || e.key === " ") && choose()}>
            <FolderIcon />
            <strong>拖入任务目录</strong>
            <span>例如 整理后/橘子采摘（含 videos/top_left.mp4）<br />也可拖入上一级目录，或点击选择</span>
          </div>

          <section className="card tasks">
            <div className="card-head">
              <h2>任务</h2>
              <span className="count">{items.length}</span>
              <span className="grow" />
              {active && <button className="btn btn-plain" onClick={() => api.cancelAll()}>全部停止</button>}
              {!active && items.length > 0 && (
                <button className="btn btn-plain" onClick={() => api.clearFinished().then(() => { setItems([]); setSelected(null); setPose(null); })}>清空</button>
              )}
            </div>
            {tasks.length === 0 ? (
              <div className="empty"><span className="muted">还没有任务</span></div>
            ) : (
              <div className="task-list">
                {tasks.map(([folder, views]) => (
                  <div key={folder} className="task">
                    <div className="task-head">
                      <span className="task-name" title={folder}>{views[0].task}</span>
                      <button className="icon-btn" title={`在访达中打开 ${folder}`} aria-label="打开目录" onClick={() => reveal(folder)}>
                        <FolderOpenIcon />
                      </button>
                      <button className="btn btn-small" disabled={!ready.handpose || views.some((v) => ["queued", "qc", "handpose"].includes(v.stage)) || samples.some((j) => j.folder === folder && ["queued", "running"].includes(j.status))}
                              onClick={() => { setNotice(""); setEditMode("trim"); setEditing(folder); }}>裁剪</button>
                      <SampleButton job={samples.find((j) => j.folder === folder)} isSample={SAMPLE_DIR.test(folder)}
                                    disabled={!ready.handpose} onClick={() => { setNotice(""); setEditMode("sample"); setEditing(folder); }} />
                    </div>
                    <div className="task-controls">
                      <button className="btn btn-small btn-primary" disabled={!ready.qc || !ready.handpose || !views.some((v) => v.stage === "ready" || v.stage === "failed") || samples.some((j) => j.folder === folder && j.kind === "trim" && ["queued", "running"].includes(j.status))}
                              onClick={() => api.startQc(folder).catch((e) => setNotice(String(e)))}>开始质检</button>
                      <span className="muted small">左右目依次质检并提取关键点</span>
                    </div>
                    <SampleStatus job={samples.find((j) => j.folder === folder)} onReveal={reveal} />
                    {views.map((item) => (
                      <ViewRow key={item.id} item={item} active={item.id === selected} onSelect={() => setSelected(item.id)} />
                    ))}
                  </div>
                ))}
              </div>
            )}
          </section>

          {finished.length > 0 && (
            <section className="card made-samples">
              <div className="card-head"><h2>已生成的结果</h2><span className="count">{finished.length}</span></div>
              <div className="made-list">
                {finished.map((j) => (
                  <div key={j.output} className="made-row">
                    <div className="made-text">
                      <span className="made-name" title={j.output ?? ""}>{baseName(j.output!)}</span>
                      <span className="muted small">来自 {baseName(j.folder)} · {j.segments.length} 段</span>
                    </div>
                    <button className="btn btn-small" onClick={() => reveal(j.output!)}>打开目录</button>
                  </div>
                ))}
              </div>
            </section>
          )}
        </aside>

        <main className="main">
          {current ? (
            <Player key={current.id} item={current} companion={items.find((i) => i.folder === current.folder && i.id !== current.id && (current.view === "top_left" ? i.view === "top_right" : current.view === "top_right" ? i.view === "top_left" : false))}
                    data={pose?.id === current.id ? pose.data : null} error={poseError} />
          ) : (
            <section className="card placeholder">
              <strong>选择左侧的视频预览</strong>
              <span className="muted">导入后即可同步预览左右目，质检通过按钮手动启动</span>
            </section>
          )}
          <LogPanel logs={logs} />
        </main>
      </div>

      {editing && (() => {
        const views = items.filter((i) => i.folder === editing);
        return (
          <SampleEditor folder={editing} task={views[0]?.task ?? ""} views={views} mode={editMode}
                        onClose={() => setEditing(null)} onSubmitted={() => setEditing(null)} />
        );
      })()}
    </div>
  );
}

function SampleButton({ job, isSample, disabled, onClick }:
  { job?: SampleJob; isSample: boolean; disabled: boolean; onClick: () => void }) {
  const busy = job?.status === "queued" || job?.status === "running";
  return (
    <span className="sample-actions">
      {isSample && <span className="tag">样例</span>}

      <button className="btn btn-small" disabled={disabled || busy} onClick={onClick}
              title={"在时间轴上标记要保留的片段（可多段），按时间顺序拼接成一份样例，放在任务目录旁边。"
                + "\n视频、IMU 和 timestamps 按所选片段截取，其他文件及目录原样保留。"
                + "\n只生成样例，不会开始质检。"}>
        {busy ? "生成中…" : "生成样例"}
      </button>
    </span>
  );
}

function SampleStatus({ job, onReveal }: { job?: SampleJob; onReveal: (folder: string) => void }) {
  if (!job) return null;
  const action = job.kind === "trim" ? "裁剪" : "生成样例";
  if (job.status === "done" && job.output) {
    return (
      <div className="sample-status done">
        <span title={job.output}>{job.kind === "trim" ? "已完成裁剪" : "已生成样例"} {baseName(job.output)}</span>
        <button className="btn btn-plain btn-inline" onClick={() => onReveal(job.output!)}>打开目录</button>
      </div>
    );
  }
  if (job.status === "done") return null;
  if (job.status === "failed") return <div className="sample-status error" title={job.error ?? ""}>{action}失败：{job.error}</div>;
  const pct = Math.round((job.progress ?? 0) * 100);
  return (
    <div className="sample-status">
      <span>{job.status === "queued" ? `${action}排队中` : `正在${action} ${job.segments.length} 段 ${pct}%`}</span>
      <span className="mini-progress static"><span style={{ width: `${pct}%` }} /></span>
    </div>
  );
}

function ViewRow({ item, active, onSelect }: { item: Item; active: boolean; onSelect: () => void }) {
  const counts = item.qc ? (Object.entries(item.qc) as [Scene, api.Segment[]][]).map(([s, segs]) => `${SCENE_LABEL[s]} ${segs.length}`) : [];
  const detail =
    item.stage === "handpose" && item.progress !== null ? `关键点 ${Math.round(item.progress * 100)}%`
    : item.stage === "failed" ? (item.qcError || item.handposeError || "失败")
    : item.stage === "done" ? [counts.join(" · "), item.handposeStats && `${Math.round(item.handposeStats.handRatio * 100)}% 帧有手`].filter(Boolean).join(" · ")
    : counts.join(" · ");
  return (
    <button className={`view-row ${active ? "active" : ""}`} onClick={onSelect}>
      <span className={`stage stage-${item.stage}`}>{STAGE_LABEL[item.stage]}</span>
      <span className="view-name">{VIEW_LABEL[item.view] ?? item.view}</span>
      <span className="view-detail" title={detail}>{detail}{item.cached && item.stage === "done" ? " · 已处理过" : ""}</span>
      {item.stage === "handpose" && item.progress !== null && (
        <span className="mini-progress"><span style={{ width: `${item.progress * 100}%` }} /></span>
      )}
    </button>
  );
}

const timeFormat = new Intl.DateTimeFormat("zh-CN", { hour: "2-digit", minute: "2-digit", second: "2-digit", hour12: false });

function LogPanel({ logs }: { logs: (LogLine & { time: number })[] }) {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => { if (ref.current) ref.current.scrollTop = ref.current.scrollHeight; }, [logs]);
  return (
    <section className="card logs">
      <div className="card-head"><h2>日志</h2><span className="count">{logs.length}</span></div>
      <div className="log-list" ref={ref}>
        {logs.length === 0 ? <div className="log-row muted">处理结果会显示在这里</div> : logs.map((l, i) => (
          <div key={i} className={`log-row ${l.error ? "error" : ""}`}><time>{timeFormat.format(l.time)}</time><span>{l.text}</span></div>
        ))}
      </div>
    </section>
  );
}

function HandIcon() {
  return (
    <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
      <path d="M7 11V5.5a1.5 1.5 0 0 1 3 0V11V4a1.5 1.5 0 0 1 3 0v7V5.5a1.5 1.5 0 0 1 3 0V12V8.5a1.5 1.5 0 0 1 3 0V15a6 6 0 0 1-6 6h-1.5a6 6 0 0 1-4.6-2.2L3.6 14a1.5 1.5 0 0 1 2.3-1.9L7 13.5" />
    </svg>
  );
}

function FolderOpenIcon() {
  return (
    <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
      <path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v1" /><path d="M3 7v10a2 2 0 0 0 2 2h12.5a2 2 0 0 0 1.9-1.4L21.8 12a1 1 0 0 0-1-1.3H8.2a2 2 0 0 0-1.9 1.4L3 19" />
    </svg>
  );
}

function FolderIcon() {
  return (
    <svg width="26" height="26" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
      <path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2Z" /><path d="M12 10v6m-3-3 3 3 3-3" />
    </svg>
  );
}
