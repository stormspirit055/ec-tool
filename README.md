# QCWorker 视频质检 · 手部关键点

把任务目录或视频拖进窗口，即可同步预览左右目。导入不会开始质检；点击任务的「开始质检」后，才进行模型质检和手部关键点提取。支持先裁剪再质检，支持 macOS（Apple 芯片）和 Windows（x64）。

## 使用

1. 打开 QCWorker，把任务目录拖进窗口，或点击拖放区选择。任务目录的结构如下（与 `demo/整理后/橘子采摘` 相同）：

   ```
   橘子采摘/
     videos/top_left.mp4
     videos/top_right.mp4
     calibration/  imu/  timestamps/   （裁剪及生成样例时同步处理）
   ```

   也可以拖入上一级目录（如 `整理后`），里面每个任务目录都会处理；拖入 `videos` 目录或单个视频也行。

2. 拖入 `top_left` 或 `top_right` 任一视频时，自动加载同目录的另一目。两目同时显示，主目控制播放、暂停、跳转和倍速。点击「开始质检」后，每个视频依次完成两步，左侧列表显示进度：
   - **模型质检**：无手（手连续不可见 ≥5 秒）、光线异常（操作区过曝、欠曝或对比度过低 ≥5 秒）。
   - **手部关键点**：MediaPipe 逐帧提取 21 个手部关键点。

   同一时间只处理一个视频。可以随时继续拖入；新导入的视频保持「待质检」，需要点击按钮才会排队。再次导入失败的视频也不会自动重试。

3. 点左侧的某一目，可控制该目的播放和查看质检结果，另一目同步预览。质检完成后，画面上叠加当前帧的手部骨架（绿色左手、橙色右手），下方可显示 3D 视图（拖动旋转）及质检时间轴，点击异常色块可跳转。

### 裁剪视频

点击任务旁的「裁剪」，在同步双目预览下方的时间轴选择位置，点击「确定起点」（I），再选择终点并点击「确定终点」（O），即可生成一个**待删除片段**。终点包含当前帧。可以连续标记多段，也可以拖动片段或两端调整，Delete 删除选中的标记。未确定终点、未标记片段或将删除整个视频时，不能提交。

点击「完成裁剪」后，统一删除标记区间及其对应的两目视频帧、timestamps 行和 IMU 数据。重叠及相邻区间先合并，再从后往前应用删除，所有边界始终使用原始帧号，避免多次删除造成错位。任意帧边界均可使用；视频重编码为 H.264，保留各源帧的帧间隔，输出从零连续计时，并校验两目的帧数和时间。

结果写入任务旁边的 `<任务名>_裁剪` 目录，重名自动加 `_2` 等后缀，原目录保留。timestamps 的 `frame_index`、IMU 的 `sample_index` 连续编号，保留源编号及设备时间；`video_time_sec` 是与裁剪后视频对应的连续时间，设备时间在删除处有间隔。`trim.json` 记录删除区间及保留源帧。calibration 和传感器说明文件保留；旧质检和关键点结果不复制，以免与裁剪后视频错位。裁剪结果会自动加入并选中预览，需点击其「开始质检」才处理。

当前裁剪面向无音轨的采集视频；含音轨、两目帧数/时序不一致、传感器时间不单调、timestamps 与帧数不符，或有 IMU 但缺少对应 timestamps 时，会报错并保留原数据，不生成正式输出目录。

### 生成样例

点任务名旁边的「生成样例」，打开剪裁窗口：

生成样例就是从源任务截取所选片段：视频、IMU、timestamps 同步截取，其余文件按原路径、原内容复制，保留整个目录结构。包括 `metadata.json`、`streams_manifest.json`、`README`、校验文件、calibration、evidence、qa、隐藏文件、空目录，以及数据目录中的说明文件。副本与源文件相互独立。

完整样例必须包含每一目的 timestamps 和 IMU。支持 `timestamps/<视角>_camera.csv`、`timestamps/<视角>.csv`，也支持电焊 demo 这类双目共用的 `timestamps/camera.csv`；截取后的文件保留源文件名，同时提供 `<视角>_camera.csv` 供应用按视角读取。读取 `imu/imu.csv`，文件名和传感器目录名忽略大小写。生成前检查帧数、设备时间和 IMU 覆盖范围，完成后再次检查输出；缺少数据会明确报错，不再静默生成只有视频的样例。

- 播放视频，在要开始的位置点「在播放位置加片段」（快捷键 I），默认 10 秒；拖动片段两端或整段调整，按 O 把选中片段的终点设到播放位置，Delete 删除。可以标记多段。
- 片段起点会吸附到关键帧（这批视频每 30 帧一个，约 1 秒），终点可以是任意帧。原视频不重编码，所以起点只能在关键帧上。
- 点「生成样例」，各片段按原视频的时间顺序拼接成一份样例；重叠或首尾相接的片段自动合并。

样例放在任务目录旁边的 `<任务名>_样例<总秒数>s`（重名时加 `_2`、`_3`…），结构与 `demo/样例30s` 相同：

```
橘子采摘_样例35s/
  videos/top_left.mp4      各片段的原始码流直接拼接，不重编码（与原视频对应帧逐字节相同），也直接用于播放预览
  timestamps/              各片段对应的行依次拼接，frame_index 从 0 连续编号，原帧号写入 source_frame_index
  imu/imu.csv              各片段覆盖曝光时间的 IMU 行依次拼接，sample_index 从 0 连续编号；imu_info.yaml 原样复制
  calibration/             原样复制
  metadata.json 等         源任务中存在的其他文件原样保留
  evidence/、qa/ 等        源任务中存在的其他目录原样保留
  handpose/                源任务中存在时原样保留，不截取或重新计算
  segments.json            每段在原视频和样例里的帧范围、时间、IMU 行数
```

拼接后视频从 0 连续计时，片段之间无缝衔接（沿用原视频的帧间隔）。timestamps 和 IMU 里的设备时间保持原值，所以拼接处会跳变；按 `segments.json` 或 `source_frame_index` 可以找到每段的边界。

生成样例和质检相互独立：生成样例不会开始质检，也不会做手部检测。已有 `handpose/`、`.qcworker.json` 和其他分析记录都保持源文件内容；其中记录仍对应源任务，生成时不解析或修改。需要样例的质检结果时，把样例目录拖进窗口后手动开始质检。

视频使用原始码流拼接，不重编码。生成时还会复制其他源文件，耗时取决于这些文件的大小。已有 `segments.json` 也会原样保留，本次截取记录会另存为 `sample_segments.json`（必要时加数字后缀），避免覆盖源记录；源目录没有同名文件时，仍使用 `segments.json`。

生成完成后，任务下方显示「已生成样例 …」，左侧「已生成的样例」列出本次生成的所有样例，点「打开目录」在访达中打开。任务名旁边的文件夹图标打开任务目录本身。

## 输出

结果写在任务目录里，跟着目录走，复制或移动后仍然有效。**原视频不会被修改。**

- `handpose/top_left.json`、`handpose/top_right.json`：每帧的手部关键点，格式见 model-qc-worker 的 `docs/handpose.md`，与它的 Web Demo 样例库通用。
- `.qcworker.json`（隐藏文件）：每个视频的质检片段和关键点统计，按文件大小加修改时间识别视频。再次拖入没变过的视频后，点击「开始质检」会读取之前的结果，不重新计算；视频变了会重新处理。删掉这个文件或 `handpose/` 可以强制重做。

旧版（裁剪视频开头）写在 `.qcworker.json` 里的裁剪记录会原样保留，新版不读取。

## 速度

本机（Apple 芯片）实测，1920×1200 HEVC：

- 质检：30 秒视频约 30 秒（含加载模型）。
- 关键点：约 2.3–2.7 倍实时，30 秒视频约 80 秒，7.8 分钟视频约 18 分钟；耗时主要在 YUV→RGB 转换（约 40 ms/帧）和左右分块检测。
- `整理后/橘子采摘`（两目各 7.8 分钟）整个任务约 39 分钟。

关键点进度按处理到的视频时间显示，质检没有进度百分比。电脑睡眠（例如合上笔记本）时处理会暂停，唤醒后继续。

## 预览说明

- 视频通过 Tauri 的 asset 协议播放。程序只放行用户拖入的视频文件本身，其它本地文件不能被页面读取。
- 原视频是 HEVC（hvc1）。macOS 可以直接播放；Windows 的 WebView2 需要系统装有「HEVC 视频扩展」，否则提示无法播放，质检和关键点数据照常生成。
- 3D 视图里手的位置由画面坐标换算，深度方向只反映手指之间的前后关系，不是手到相机的真实距离（MediaPipe 不提供手在相机坐标系中的绝对位置）。

## 技术栈

Tauri 2（Rust）+ React + TypeScript + Vite，3D 用 three.js。两个推理引擎来自同一个 [model-qc-worker](../model-qc-worker) 源码，用 PyInstaller 打包随应用分发，只用 CPU：

| 引擎 | 内容 | 模型 |
|---|---|---|
| `qc-engine` | nohand 质检命令行（无手 + 光线异常） | NVIDIA RT-DETR-Hand-Detector-v1（77 MB，OpenMDW） |
| `handpose-engine` | nohand.handpose（MediaPipe Hand Landmarker）；`sample` 子命令由 qc_sample 校验传感器文件，再调用 nohand.sample 无损拼接 | hand_landmarker.task（7.8 MB，Apache-2.0） |

两个引擎依赖的 OpenCV 版本冲突，所以分别打包。安装包约 870 MB，其中两个引擎合计约 690 MB。

## 开发

需要 Node 24+、pnpm、Rust（stable）、[uv](https://docs.astral.sh/uv/)，以及与本项目同级的 `model-qc-worker` 源码（或用 `MODEL_QC_DIR` 指定）。

```bash
pnpm install
pnpm build-qc-engine        # 打包两个引擎和模型到 src-tauri/resources/
pnpm tauri icon app-icon.svg
pnpm tauri dev
```

`build-qc-engine` 默认复用 model-qc-worker 里的 `.venv`（质检）和 `.venv-handpose`（关键点），没有时用 uv 新建；模型优先取 `model-qc-worker/models/` 下校验通过的文件，没有再下载并校验 SHA-256。可以只构建一个：`node scripts/build-qc-engine.mjs handpose-engine`。PyPI 慢时加 `UV_INDEX_URL=https://mirrors.aliyun.com/pypi/simple/`。PyInstaller 不能交叉编译，Windows 版要在 Windows 上构建。

测试（裁剪测试复用引擎的 PyAV / NumPy 虚拟环境）：

```bash
cargo test --manifest-path src-tauri/Cargo.toml --lib
pnpm test
../model-qc-worker/.venv-handpose/bin/python -m unittest discover -s qc-engine -p 'test_qc_trim.py' -v
PYTHONPATH=../model-qc-worker ../model-qc-worker/.venv-handpose/bin/python -m unittest discover -s qc-engine -p 'test_qc_sample.py' -v
```

其中三个集成测试会调用真实引擎处理 `demo/样例30s/汽修/videos/top_left.mp4`（可用 `QC_SAMPLE_VIDEO` 指定），引擎未构建或视频不存在时自动跳过。对任意目录走一遍和 app 拖入完全相同的流程（结果写进该目录）：

```bash
QC_E2E_DIR=/path/to/橘子采摘 cargo test --manifest-path src-tauri/Cargo.toml --lib e2e_dir -- --ignored --nocapture
```

## 构建

- **macOS**：`pnpm tauri build`，产物在 `src-tauri/target/release/bundle/`（`.app`、`.dmg`）。
- **Windows**：在 Windows 上执行同样的命令，产物为 `.msi` 和 NSIS `.exe`。
- **GitHub Actions**：`.github/workflows/build.yml` 同时构建两个平台，需要配置 secret `MODEL_QC_GIT_URL`（带只读令牌的 model-qc-worker 克隆地址）。

应用未做正式签名。macOS 首次打开需要右键选择「打开」，Windows 会出现 SmartScreen 提示，选择「仍要运行」。

## 许可

引擎目录里附带 model-qc-worker 的 `LICENSES/`（模型许可证、模型卡、来源说明，详见其中的 `NOTICE.md`）。PyAV、OpenCV 的 wheel 自带 FFmpeg 和 GPL 的 x264/x265 库；内部使用没有限制，对外分发前请法务评估。three.js 为 MIT 许可。
