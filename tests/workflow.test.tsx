// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import App from "../src/App";
import { SampleEditor } from "../src/SampleEditor";
import * as api from "../src/api";
import type { Item, SampleJob } from "../src/api";

const callbacks = vi.hoisted(() => ({ drop: null as any, item: null as any, sample: null as any }));
vi.mock("@tauri-apps/api/webview", () => ({ getCurrentWebview: () => ({ onDragDropEvent: (cb: any) => { callbacks.drop = cb; return Promise.resolve(() => {}); } }) }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn() }));
vi.mock("../src/api", () => ({
  addPaths: vi.fn(), startQc: vi.fn(), getStatus: vi.fn(), probeTrim: vi.fn(), probeSample: vi.fn(), trimVideos: vi.fn(), makeSample: vi.fn(),
  readHandpose: vi.fn(), revealFolder: vi.fn(), cancelAll: vi.fn(), clearFinished: vi.fn(), videoSrc: (path: string) => path,
  onItem: (cb: any) => { callbacks.item = cb; return Promise.resolve(() => {}); },
  onSample: (cb: any) => { callbacks.sample = cb; return Promise.resolve(() => {}); }, onLog: () => Promise.resolve(() => {}),
}));

const views: Item[] = ["left", "right"].map((side, index) => ({
  id: index + 1, task: "双目", folder: "/task", view: `top_${side}`, video: `/task/videos/top_${side}.mp4`, stage: "ready",
  progress: null, qc: null, qcError: null, handpose: null, handposeStats: null, handposeError: null, cached: false,
}));
const probe = { views: ["top_left", "top_right"], frames: 10, fps: 1, duration: 10, times: Array.from({ length: 10 }, (_, i) => i), keyframes: [0, 5] };

beforeEach(() => {
  vi.clearAllMocks();
  vi.spyOn(HTMLMediaElement.prototype, "play").mockImplementation(function (this: HTMLMediaElement) {
    Object.defineProperty(this, "paused", { configurable: true, value: false });
    return Promise.resolve();
  });
  vi.spyOn(HTMLMediaElement.prototype, "pause").mockImplementation(function (this: HTMLMediaElement) {
    Object.defineProperty(this, "paused", { configurable: true, value: true });
  });
  Element.prototype.scrollIntoView = vi.fn();
  vi.stubGlobal("requestAnimationFrame", vi.fn(() => 1));
  vi.stubGlobal("cancelAnimationFrame", vi.fn());
  vi.mocked(api.getStatus).mockResolvedValue({ qcReady: true, handposeReady: true, items: views, samples: [] });
  vi.mocked(api.startQc).mockResolvedValue(2);
  vi.mocked(api.trimVideos).mockResolvedValue();
  vi.mocked(api.makeSample).mockResolvedValue();
  vi.mocked(api.probeTrim).mockResolvedValue(probe);
  vi.mocked(api.probeSample).mockResolvedValue(probe);
});
afterEach(() => { cleanup(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });

describe("导入与手动质检", () => {
  it("导入即同时显示两目，只有点击按钮才启动质检", async () => {
    vi.mocked(api.getStatus).mockResolvedValue({ qcReady: true, handposeReady: true, items: [], samples: [] });
    vi.mocked(api.addPaths).mockImplementation(async () => { views.forEach((v) => callbacks.item(v)); return 2; });
    const { container } = render(<App />);
    await act(async () => { callbacks.drop({ payload: { type: "drop", paths: [views[0].video] } }); });
    await waitFor(() => expect(container.querySelectorAll("video")).toHaveLength(2));
    expect(api.startQc).not.toHaveBeenCalled();
    expect(api.makeSample).not.toHaveBeenCalled();
    fireEvent.click(screen.getByText("开始质检"));
    expect(api.startQc).toHaveBeenCalledWith("/task");
  });

  it("裁剪完成后选中输出双目预览，不自动质检", async () => {
    const { container } = render(<App />);
    await screen.findByText("开始质检");
    await waitFor(() => expect(container.querySelector("video")?.getAttribute("src")).toBe(views[0].video));
    const outputViews = views.map((v) => ({ ...v, id: v.id + 2, folder: "/task_裁剪", task: "裁剪结果", video: v.video.replace("/task/", "/task_裁剪/") }));
    vi.mocked(api.getStatus).mockResolvedValue({ qcReady: true, handposeReady: true, items: [...views, ...outputViews], samples: [] });
    await act(async () => callbacks.sample({ kind: "trim", folder: "/task", output: "/task_裁剪", status: "done", segments: [{ start: 2, end: 4 }], progress: null, error: null } satisfies SampleJob));
    await waitFor(() => expect(container.querySelector("video")?.getAttribute("src")).toContain("/task_裁剪/"));
    expect(container.querySelectorAll("video")).toHaveLength(2);
    expect(api.startQc).not.toHaveBeenCalled();
  });
});

describe("生成样例", () => {
  it("说明仅截取三个数据流并保留其他文件，提交不启动质检", async () => {
    render(<SampleEditor folder="/task" task="双目" views={views} onClose={vi.fn()} onSubmitted={vi.fn()} />);
    await screen.findByText("＋ 在播放位置加片段");
    expect(screen.getByText(/视频、IMU、timestamps 按选段截取，其余文件原样保留/)).toBeTruthy();
    fireEvent.click(screen.getByText("＋ 在播放位置加片段"));
    fireEvent.click(screen.getByText("生成样例"));
    await waitFor(() => expect(api.makeSample).toHaveBeenCalledWith("/task", [{ start: 0, end: 10 }]));
    expect(api.startQc).not.toHaveBeenCalled();
  });
});

describe("裁剪时间轴", () => {
  it("终点确定后才生成片段，点击完成裁剪才提交，保留任意帧边界", async () => {
    const submitted = vi.fn();
    const { container } = render(<SampleEditor folder="/task" task="双目" views={views} mode="trim" onClose={vi.fn()} onSubmitted={submitted} />);
    await screen.findByText("确定起点 (I)");
    expect(container.querySelectorAll("video")).toHaveLength(2);
    const video = container.querySelector("video")!;
    video.currentTime = 2;
    fireEvent.click(screen.getByText("确定起点 (I)"));
    expect(container.querySelectorAll(".cut-row")).toHaveLength(0);
    expect((screen.getByText("完成裁剪") as HTMLButtonElement).disabled).toBe(true);
    video.currentTime = 4;
    fireEvent.click(screen.getByText("确定终点 (O)"));
    expect(container.querySelectorAll(".cut-row")).toHaveLength(1);
    expect(api.trimVideos).not.toHaveBeenCalled();
    video.currentTime = 7;
    fireEvent.click(screen.getByText("确定起点 (I)"));
    video.currentTime = 8;
    fireEvent.click(screen.getByText("确定终点 (O)"));
    fireEvent.click(screen.getByText("完成裁剪"));
    await waitFor(() => expect(api.trimVideos).toHaveBeenCalledWith("/task", [{ start: 2, end: 5 }, { start: 7, end: 9 }]));
    expect(api.startQc).not.toHaveBeenCalled();
    expect(submitted).toHaveBeenCalledOnce();
  });

  it("拒绝反向片段和删除整个视频", async () => {
    const { container } = render(<SampleEditor folder="/task" task="双目" views={views} mode="trim" onClose={vi.fn()} onSubmitted={vi.fn()} />);
    await screen.findByText("确定起点 (I)");
    const video = container.querySelector("video")!;
    video.currentTime = 5;
    fireEvent.click(screen.getByText("确定起点 (I)"));
    video.currentTime = 2;
    fireEvent.click(screen.getByText("确定终点 (O)"));
    expect(screen.getByText("终点必须在起点之后")).toBeTruthy();
    expect(container.querySelectorAll(".cut-row")).toHaveLength(0);
    video.currentTime = 0;
    fireEvent.click(screen.getByText("确定起点 (I)"));
    video.currentTime = 10;
    fireEvent.click(screen.getByText("确定终点 (O)"));
    expect((screen.getByText("完成裁剪") as HTMLButtonElement).disabled).toBe(true);
    expect(api.trimVideos).not.toHaveBeenCalled();
  });

  it("主目播放、跳转、倍速和暂停同步到另一目", async () => {
    const { container } = render(<SampleEditor folder="/task" task="双目" views={views} mode="trim" onClose={vi.fn()} onSubmitted={vi.fn()} />);
    await screen.findByText("确定起点 (I)");
    const [master, peer] = [...container.querySelectorAll("video")];
    Object.defineProperty(peer, "readyState", { configurable: true, value: 4 });
    master.currentTime = 3;
    master.playbackRate = 1.5;
    await act(async () => { await master.play(); fireEvent.play(master); });
    expect(peer.currentTime).toBe(3);
    expect(peer.playbackRate).toBe(1.5);
    expect(peer.paused).toBe(false);
    master.currentTime = 7;
    fireEvent.seeking(master);
    expect(peer.currentTime).toBe(7);
    master.pause();
    fireEvent.pause(master);
    expect(peer.paused).toBe(true);
  });
});
