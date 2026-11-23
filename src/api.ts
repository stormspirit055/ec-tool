import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

/** 与 nohand 质检程序的场景 ID 一致 */
export type Scene = "no_hand" | "light";

export interface Segment {
  start: number;
  end: number;
  duration: number;
  reason?: string;
  unreliableDueToLight?: boolean;
}

export interface HandposeStats {
  frames: number;
  framesWithHands: number;
  handRatio: number;
}

export type Stage = "ready" | "queued" | "qc" | "handpose" | "done" | "failed";

export interface Item {
  id: number;
  task: string;
  folder: string;
  view: string;
  video: string;
  stage: Stage;
  progress: number | null;
  qc: Partial<Record<Scene, Segment[]>> | null;
  qcError: string | null;
  handpose: string | null;
  handposeStats: HandposeStats | null;
  handposeError: string | null;
  cached: boolean;
}

/** 帧号区间 [start, end)；仅生成样例时 start 必须是关键帧，裁剪支持任意帧 */
export interface FrameSegment {
  start: number;
  end: number;
}

/** 任务视频的帧时间和关键帧（各视角相同），剪裁时间轴用 */
export interface SampleProbe {
  views: string[];
  frames: number;
  fps: number;
  duration: number;
  times: number[];
  keyframes: number[];
}

export interface SampleJob {
  kind: "sample" | "trim";
  /** 源任务目录 */
  folder: string;
  /** 样例目录：任务目录旁边的 <任务名>_样例<秒>s；生成完成前为 null */
  output: string | null;
  segments: FrameSegment[];
  status: "queued" | "running" | "done" | "failed";
  progress: number | null;
  error: string | null;
}

export interface Status {
  qcReady: boolean;
  handposeReady: boolean;
  items: Item[];
  samples: SampleJob[];
}

export interface LogLine {
  text: string;
  error: boolean;
}

/** handpose-engine 输出的关键点 JSON（model-qc-worker nohand.handpose，见 docs/handpose.md） */
export interface Hand {
  side: "Left" | "Right";
  score: number;
  image: number[][];
  world: number[][];
}

export interface HandposeFrame {
  t: number;
  f: number;
  hands: Hand[];
}

export interface HandposeData {
  width: number;
  height: number;
  fps: number;
  duration_sec: number;
  connections: [number, number][];
  frames: HandposeFrame[];
  stats: { frames: number; frames_with_hands: number; hand_ratio: number };
  assumptions: string[];
}

export const addPaths = (paths: string[]) => invoke<number>("add_paths", { paths });
export const startQc = (folder: string) => invoke<number>("start_qc", { folder });
export const probeTrim = (folder: string) => invoke<SampleProbe>("probe_trim", { folder });
export const trimVideos = (folder: string, segments: FrameSegment[]) => invoke<void>("trim_videos", { folder, segments });
export const getStatus = () => invoke<Status>("get_status");
export const probeSample = (folder: string) => invoke<SampleProbe>("probe_sample", { folder });
export const makeSample = (folder: string, segments: FrameSegment[]) => invoke<void>("make_sample", { folder, segments });
/** 在访达中打开目录（只允许拖入的任务目录和生成的样例目录） */
export const revealFolder = (folder: string) => invoke<void>("reveal_folder", { folder });
export const cancelAll = () => invoke<void>("cancel_all");
export const clearFinished = () => invoke<void>("clear_finished");

export async function readHandpose(id: number): Promise<HandposeData> {
  const bytes = await invoke<ArrayBuffer>("read_handpose", { id });
  return JSON.parse(new TextDecoder().decode(bytes));
}

/** 本地视频的播放地址（Tauri asset 协议；后端只放行拖入的视频） */
export const videoSrc = (path: string) => convertFileSrc(path);

export const onItem = (cb: (item: Item) => void): Promise<UnlistenFn> =>
  listen<Item>("jobs://item", (e) => cb(e.payload));

export const onSample = (cb: (job: SampleJob) => void): Promise<UnlistenFn> =>
  listen<SampleJob>("jobs://sample", (e) => cb(e.payload));

export const onLog = (cb: (line: LogLine) => void): Promise<UnlistenFn> =>
  listen<LogLine>("jobs://log", (e) => cb(e.payload));
