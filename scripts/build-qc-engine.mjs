// 构建两个引擎，放到 src-tauri/resources/：
//   qc-engine/        模型质检（nohand：无手 + 光线异常），带 RT-DETR 手部检测模型
//   handpose-engine/  手部关键点（nohand.handpose，MediaPipe），带 hand_landmarker.task
//
// 两个引擎来自同一个 model-qc-worker 源码，但依赖的 OpenCV 版本冲突，所以各用一个虚拟环境、分开打包。
//
// 用法：node scripts/build-qc-engine.mjs
// 环境变量：
//   MODEL_QC_DIR   model-qc-worker 源码目录，默认 ../model-qc-worker
//   PIP_INDEX_URL  PyPI 镜像，例如 https://mirrors.aliyun.com/pypi/simple/
//
// 默认复用 model-qc-worker 里已建好的 .venv（质检）和 .venv-handpose（关键点）；不存在时用 uv 新建。
// 模型优先取 model-qc-worker/models/ 下校验通过的文件，没有再下载。
// PyInstaller 不能交叉编译，Windows 版需在 Windows 上构建。

import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import { cpSync, createReadStream, existsSync, mkdirSync, renameSync, rmSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const src = resolve(process.env.MODEL_QC_DIR || join(root, "..", "model-qc-worker"));
const work = join(root, "qc-engine", ".build");
const resources = join(root, "src-tauri", "resources");
const win = process.platform === "win32";
const exeName = (name) => (win ? `${name}.exe` : name);
const venvPython = (venv) => join(venv, win ? "Scripts/python.exe" : "bin/python");

const PYTHON = "3.12";
const PYINSTALLER = "pyinstaller==6.22.3";

const MODELS = {
  hand_detector: {
    file: "hand_detector.onnx",
    sha256: "85d36428cc1ecf8832ca2d48f7aa04df0659cf94dde358352fb3cf651a319810",
    url: "https://hf-mirror.com/nvidia/RT-DETR-Hand-Detector-v1/resolve/main/hand_detector.onnx",
  },
  hand_landmarker: {
    file: "hand_landmarker.task",
    sha256: "fbc2a30080c3c557093b5ddfc334698132eb341044ccee322ccf8bcf3607cde1",
    url: "https://storage.googleapis.com/mediapipe-models/hand_landmarker/hand_landmarker/float16/1/hand_landmarker.task",
  },
};

const ENGINES = [
  {
    name: "qc-engine",
    spec: "qc-engine.spec",
    venv: join(src, ".venv"),
    requirements: ["-r", join(src, "requirements.txt")],
    smoke: "import nohand.cli, nohand.core, nohand.rtdetr, nohand.scenes.no_hand, nohand.scenes.light",
    model: MODELS.hand_detector,
    check: (exe) => `${execFileSync(exe, ["--version"], { encoding: "utf8" }).trim()}，场景：${JSON.parse(
      execFileSync(exe, ["--list-scenes"], { encoding: "utf8" })).map((s) => s.id).join(", ")}`,
  },
  {
    name: "handpose-engine",
    spec: "handpose-engine.spec",
    venv: join(src, ".venv-handpose"),
    requirements: ["mediapipe==1.2.2", "av==15.1.0", "numpy==2.2.6"],
    smoke: "import nohand.handpose, mediapipe",
    model: MODELS.hand_landmarker,
    check: (exe) => execFileSync(exe, ["--help"], { encoding: "utf8" }).split("\n")[0],
  },
];

function findUv() {
  for (const c of ["uv", join(homedir(), ".local", "bin", exeName("uv")), join(homedir(), ".cargo", "bin", exeName("uv"))]) {
    try {
      execFileSync(c, ["--version"], { stdio: "ignore" });
      return c;
    } catch {}
  }
  throw new Error("找不到 uv，请先安装：https://docs.astral.sh/uv/getting-started/installation/");
}

function run(cmd, args, env = {}) {
  console.log(`$ ${[cmd, ...args].join(" ")}`);
  execFileSync(cmd, args, { stdio: "inherit", env: { ...process.env, ...env } });
}

async function sha256(file) {
  const hash = createHash("sha256");
  for await (const chunk of createReadStream(file)) hash.update(chunk);
  return hash.digest("hex");
}

async function fetchModel(model) {
  for (const file of [join(src, "models", model.file), join(work, model.file)]) {
    if (existsSync(file) && (await sha256(file)) === model.sha256) {
      console.log(`模型校验通过：${file}`);
      return file;
    }
  }
  const dest = join(work, model.file);
  console.log(`下载模型 ${model.url}`);
  const res = await fetch(model.url, { redirect: "follow" });
  if (!res.ok) throw new Error(`模型下载失败 ${res.status}：${model.url}`);
  const buf = Buffer.from(await res.arrayBuffer());
  const actual = createHash("sha256").update(buf).digest("hex");
  if (actual !== model.sha256) throw new Error(`${model.file} 校验失败：期望 ${model.sha256}，实际 ${actual}`);
  writeFileSync(`${dest}.part`, buf);
  renameSync(`${dest}.part`, dest);
  return dest;
}

async function build(engine, uv) {
  console.log(`\n== ${engine.name}`);
  const python = venvPython(engine.venv);
  if (!existsSync(python)) run(uv, ["venv", "--python", PYTHON, engine.venv]);
  // 依赖版本固定；已装好的不会重复下载。PyInstaller 只用于打包，不影响运行时依赖
  run(uv, ["pip", "install", "--python", python, ...engine.requirements, PYINSTALLER]);
  run(python, ["-B", "-c", engine.smoke], { PYTHONPATH: src });

  const model = await fetchModel(engine.model);
  const dist = join(work, "dist");
  run(python, ["-m", "PyInstaller", "--noconfirm", "--clean", "--log-level", "WARN", "--distpath", dist,
    "--workpath", join(work, "pyi", engine.name), join(root, "qc-engine", engine.spec)], { MODEL_QC_DIR: src });

  const out = join(resources, engine.name);
  rmSync(out, { recursive: true, force: true });
  cpSync(join(dist, engine.name), out, { recursive: true, verbatimSymlinks: true });
  mkdirSync(join(out, "models"), { recursive: true });
  cpSync(model, join(out, "models", engine.model.file));
  // OpenMDW / Apache-2.0：随模型分发许可证副本和来源声明
  cpSync(join(src, "LICENSES"), join(out, "LICENSES"), { recursive: true });
  console.log(`已构建 ${out}`);
  console.log(`  ${engine.check(join(out, exeName(engine.name)))}`);
  console.log(`  大小 ${dirSize(out)}`);
}

function dirSize(dir) {
  if (win) {
    const bytes = execFileSync("powershell", ["-NoProfile", "-Command",
      `(Get-ChildItem -Recurse -File '${dir}' | Measure-Object Length -Sum).Sum`], { encoding: "utf8" });
    return `${(Number(bytes.trim()) / 1e6).toFixed(0)} MB`;
  }
  return execFileSync("du", ["-sh", dir], { encoding: "utf8" }).split(/\s+/)[0];
}

async function main() {
  if (!existsSync(join(src, "nohand", "cli.py")) || !existsSync(join(src, "nohand", "handpose.py"))) {
    throw new Error(`model-qc-worker 源码不在 ${src}，请用 MODEL_QC_DIR 指定`);
  }
  mkdirSync(work, { recursive: true });
  const uv = findUv();
  const only = process.argv.slice(2);
  for (const engine of ENGINES) {
    if (!only.length || only.includes(engine.name)) await build(engine, uv);
  }
}

main().catch((err) => {
  console.error(err.message);
  process.exit(1);
});
