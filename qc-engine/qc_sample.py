"""Complete sample export: require, normalize and validate sensor sidecars.

The shared nohand.sample module owns lossless video splicing. Its
optional sensor-file lookup is replaced here by a validated staging task, so a
successful sample always includes both camera timestamps and the selected IMU.
All other source files are copied unchanged, including auxiliary sensor files.
"""
import argparse
import json
import os
import shutil
import statistics
import tempfile
from pathlib import Path

from nohand import sample
from qc_trim import clock_values, read_csv, write_csv


def find(parent, name):
    """Match case without relying on the host filesystem's case sensitivity."""
    return next((p for p in parent.iterdir() if p.name.casefold() == name.casefold()), None) if parent.is_dir() else None


def load_sensors(task, views, scans, segments):
    directory = find(task, "timestamps")
    imu_dir = find(task, "imu")
    imu_file = find(imu_dir, "imu.csv") if imu_dir else None
    if imu_file is None:
        raise ValueError(f"生成完整样例需要 IMU 文件：{task / 'imu' / 'imu.csv'}。请导入包含传感器数据的任务目录。")
    imu_fields, imu_rows = read_csv(imu_file)
    imu_times = clock_values(imu_rows, "device_timestamp_unwrapped_us", str(imu_file))
    imu_step = statistics.median(b - a for a, b in zip(imu_times, imu_times[1:])) if len(imu_times) > 1 else 0
    if "sample_index" not in imu_fields:
        raise ValueError(f"{imu_file} 缺少 sample_index")
    imu_fields = ["sample_index", *[f for f in imu_fields if f != "sample_index"]]
    cameras = {}
    for view in views:
        path = next((p for name in [f"{view}_camera.csv", f"{view}.csv", "camera.csv"]
                     if directory and (p := find(directory, name)) is not None), None)
        if path is None:
            raise ValueError(f"缺少 {view} 的 timestamps：{task / 'timestamps' / (view + '_camera.csv')}。也支持 <视角>.csv 或共用的 camera.csv。")
        fields, rows = read_csv(path)
        if len(rows) != scans[view].frames:
            raise ValueError(f"{path} 有 {len(rows)} 行，与 {view} 视频的 {scans[view].frames} 帧不一致")
        if "frame_index" not in fields or any(int(row["frame_index"]) != i for i, row in enumerate(rows)):
            raise ValueError(f"{path} 的 frame_index 必须从 0 连续编号")
        times = clock_values(rows, "exposure_end_device_us", str(path))
        for a, b in segments:
            if not imu_times[0] - imu_step <= times[a] <= times[b - 1] <= imu_times[-1] + imu_step:
                raise ValueError(f"{imu_file} 的时间范围未覆盖 {view} 片段 [{a}, {b})，无法生成同步 IMU")
        fields = ["frame_index", *[f for f in fields if f != "frame_index"]]
        cameras[view] = path, fields, rows
    return cameras, imu_file, imu_fields, imu_rows


def link_or_copy(src, dst):
    # Staging is a sibling of the task, so hard links normally stay on one volume.
    # No stage file is modified by nohand.sample; originals remain intact.
    try:
        os.link(src, dst)
    except OSError:
        shutil.copy2(src, dst)


def validate_output(output, views, frames):
    for view in views:
        path = output / "timestamps" / f"{view}_camera.csv"
        if not path.is_file():
            raise ValueError(f"样例缺少 timestamps：{path}")
        _, rows = read_csv(path)
        if len(rows) != frames:
            raise ValueError(f"{path} 行数与样例视频帧数不一致")
    path = output / "imu" / "imu.csv"
    if not path.is_file() or not read_csv(path)[1]:
        raise ValueError(f"样例缺少有效 IMU 数据：{path}")


def preserve_other_files(task, output, transformed):
    """Copy independent files, including hidden files and empty directories.

    Only the exact stream files are excluded; descriptions/sidecars inside
    videos, timestamps and imu must survive as well. Copies are independent,
    so later editing a sample cannot modify its source through a hard link.
    """
    def ignore(directory, names):
        return [name for name in names if (Path(directory) / name).relative_to(task) in transformed]
    shutil.copytree(task, output, dirs_exist_ok=True, ignore=ignore)


def make(task, output=None, frames=sample.FRAMES, start=0, handpose=True, handpose_views=None, segments=None):
    task = Path(task).absolute()
    views, scans = sample.scan_views(task)
    scan = scans[views[0]]
    segments = sample.normalize_segments(segments if segments is not None else [(start, start + frames)], scan.frames)
    if any(a not in scan.keyframes for a, _ in segments):
        raise ValueError("片段起点必须是关键帧，无法不重编码剪裁")
    cameras, imu_file, imu_fields, imu_rows = load_sensors(task, views, scans, segments)
    _, total_pts = sample.plan(scan, segments)
    output = Path(output).absolute() if output else sample.default_output(task, float(total_pts * scan.time_base))
    if output.resolve().is_relative_to(task.resolve()):
        raise ValueError("样例输出目录不能位于源任务内部，以免递归复制")
    if output.exists():
        raise FileExistsError(f"输出目录已存在：{output}")
    # Publish only after BOTH video and sensor exports pass validation.
    with tempfile.TemporaryDirectory(prefix=".qc-sample-", dir=output.parent) as temp:
        work = Path(temp)
        staged = work / "source" / task.name
        (staged / "videos").mkdir(parents=True)
        for view in views:
            link_or_copy(task / "videos" / f"{view}.mp4", staged / "videos" / f"{view}.mp4")
            write_csv(staged / "timestamps" / f"{view}_camera.csv", *cameras[view][1:])
        write_csv(staged / "imu" / "imu.csv", imu_fields, imu_rows)
        # Legacy handpose arguments remain accepted by the CLI, but handpose is
        # now one of the files to preserve, rather than another stream to edit.
        result = sample.make(staged, work / "result", frames, start, False, [], segments)
        generated = Path(result["output"])
        validate_output(generated, views, result["frames"])
        value = json.loads((generated / "segments.json").read_text(encoding="utf-8"))
        transformed = {imu_file.relative_to(task)}
        for view in views:
            transformed.add(Path("videos") / f"{view}.mp4")
            source_name = cameras[view][0].relative_to(task)
            transformed.add(source_name)
            # Keep the source filename (including shared camera.csv), while
            # retaining normalized names used by the app's video lookup.
            clipped = generated / "timestamps" / f"{view}_camera.csv"
            named = generated / source_name
            named.parent.mkdir(parents=True, exist_ok=True)
            if not named.exists() or not named.samefile(clipped):
                shutil.copy2(clipped, named)
        preserve_other_files(task, generated, transformed)
        # Existing manifests are source files too: allocate another name for
        # this export's own mapping instead of replacing a previous manifest.
        manifest_name, suffix = "segments.json", 1
        while (task / manifest_name).exists():
            manifest_name = "sample_segments.json" if suffix == 1 else f"sample_segments_{suffix}.json"
            suffix += 1
        value["source_path"] = str(task)
        (generated / manifest_name).write_text(json.dumps(value, ensure_ascii=False, indent=2), encoding="utf-8")
        if output.exists():
            raise FileExistsError(f"输出目录已存在：{output}")
        generated.rename(output)
        result["output"] = str(output)
        result["manifest"] = manifest_name
    return result


def main(argv=None):
    parser = argparse.ArgumentParser(description="生成包含双目视频、timestamps 和 IMU 的完整样例")
    parser.add_argument("task", type=Path)
    parser.add_argument("output", nargs="?")
    parser.add_argument("--probe", action="store_true")
    parser.add_argument("--segments")
    parser.add_argument("--frames", type=int, default=sample.FRAMES)
    parser.add_argument("--start", type=int, default=0)
    parser.add_argument("--no-handpose", action="store_true")
    parser.add_argument("--handpose-views")
    args = parser.parse_args(argv)
    try:
        if args.probe:
            sample.emit(ok=True, **sample.probe(args.task))
        else:
            views = [v for v in args.handpose_views.split(",") if v] if args.handpose_views is not None else None
            sample.emit(**make(args.task, args.output, args.frames, args.start, not args.no_handpose,
                               views, sample.parse_segments(args.segments) if args.segments else None))
    except Exception as error:
        sample.emit(ok=False, error=str(error))
        raise SystemExit(1)
