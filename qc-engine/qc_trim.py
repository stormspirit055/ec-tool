"""Delete marked frame intervals from every view and its sensor data.

All ranges refer to the immutable source. Deletions are applied backwards so
earlier frame indices never move. Video is encoded at arbitrary frame boundaries;
hardware clocks remain intact, while video_time_sec records the continuous clock.
"""
import argparse
import bisect
import csv
import json
import shutil
import statistics
import tempfile
from fractions import Fraction
from pathlib import Path

import av


def emit(**value):
    print(json.dumps(value, ensure_ascii=False), flush=True)


def scan(video):
    with av.open(str(video)) as container:
        stream = container.streams.video[0]
        if container.streams.audio:
            raise ValueError(f"{video.name} 包含音轨，当前采集视频裁剪仅支持无音轨视频")
        packets = sorted((p.pts, p.duration or 0) for p in container.demux(stream) if p.size and p.pts is not None)
        pts = [p for p, _ in packets]
        if not pts or any(b <= a for a, b in zip(pts, pts[1:])):
            raise ValueError(f"{video.name} 的视频时间戳为空或不严格递增")
        step = int(statistics.median(b - a for a, b in zip(pts, pts[1:]))) if len(pts) > 1 else max(1, int(1 / (stream.average_rate or 30) / stream.time_base))
        times = [Fraction(p - pts[0]) * stream.time_base for p in pts]
        return {"times": times, "duration": times[-1] + (packets[-1][1] or step) * stream.time_base,
                "fps": stream.average_rate or Fraction(30), "frames": len(pts)}


def scan_all(videos):
    if not videos or len({p.stem for p in videos}) != len(videos):
        raise ValueError("没有视频，或视频视角名称重复")
    scans = [scan(p) for p in videos]
    first = scans[0]
    for video, info in zip(videos, scans):
        if info["frames"] != first["frames"] or abs(info["duration"] - first["duration"]) > 0.002 or any(abs(a - b) > 0.002 for a, b in zip(info["times"], first["times"])):
            raise ValueError(f"{video.name} 与另一目的帧数或时序不一致，无法同步裁剪")
    return scans


def normalize(segments, total):
    ranges = sorted((s["start"], s["end"]) for s in segments)
    if not ranges:
        raise ValueError("请至少标记一个裁剪片段")
    merged = []
    for a, b in ranges:
        if type(a) is not int or type(b) is not int or not 0 <= a < b <= total:
            raise ValueError(f"无效的裁剪范围 [{a}, {b})，视频共 {total} 帧")
        if merged and a <= merged[-1][1]:
            merged[-1][1] = max(b, merged[-1][1])
        else:
            merged.append([a, b])
    if sum(b - a for a, b in merged) >= total:
        raise ValueError("不能删除整个视频，请至少保留一帧")
    return merged


def retained(total, ranges):
    indices = list(range(total))
    for a, b in reversed(ranges):
        del indices[a:b]
    return indices


def timeline(info, ranges):
    boundaries = [*info["times"], info["duration"]]
    kept = retained(info["frames"], ranges)
    output, offset, previous = [], Fraction(0), None
    for i in kept:
        if previous is not None:
            offset += boundaries[previous + 1] - boundaries[previous]
        output.append(offset)
        previous = i
    duration = output[-1] + boundaries[kept[-1] + 1] - boundaries[kept[-1]]
    return kept, output, duration


def read_csv(path):
    with path.open(encoding="utf-8-sig", newline="") as file:
        reader = csv.DictReader(file)
        if not reader.fieldnames:
            raise ValueError(f"{path.name} 缺少表头")
        return list(reader.fieldnames), list(reader)


def write_csv(path, fields, rows):
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", encoding="utf-8", newline="") as file:
        writer = csv.DictWriter(file, fieldnames=fields)
        writer.writeheader()
        writer.writerows(rows)


def clock_values(rows, column, name):
    try:
        values = [int(r[column]) for r in rows]
    except (KeyError, TypeError, ValueError) as error:
        raise ValueError(f"{name} 缺少有效的 {column}") from error
    if not values or any(b <= a for a, b in zip(values, values[1:])):
        raise ValueError(f"{name} 的设备时间必须严格递增")
    return values


def sensor_data(task, videos, info, ranges, kept, output_times):
    """Validate all sensors before encoding. Never carry untrimmed/stale CSVs."""
    timestamps, spans, reference = [], [], None
    recognized = set()
    for video in videos:
        candidates = [task / "timestamps" / f"{video.stem}_camera.csv", task / "timestamps" / f"{video.stem}.csv"]
        src = next((p for p in candidates if p.is_file()), None)
        if src is None:
            if (task / "imu" / "imu.csv").is_file():
                raise ValueError(f"缺少 {video.stem} 的 timestamps，无法安全同步裁剪 IMU")
            continue
        recognized.add(src.name)
        fields, rows = read_csv(src)
        if len(rows) != info["frames"]:
            raise ValueError(f"{src.name} 行数与视频帧数不一致")
        device = clock_values(rows, "exposure_end_device_us", src.name)
        if reference is None:
            reference = device
        step = int(statistics.median(b - a for a, b in zip(device, device[1:]))) if len(device) > 1 else round(1e6 / info["fps"])
        bounds = [*device, device[-1] + step]
        # Also remove IMU guard samples outside the video when deleting a head/tail.
        spans.extend((bounds[a] if a else float("-inf"), bounds[b] if b < info["frames"] else float("inf")) for a, b in ranges)
        for column in ["source_frame_index", "video_time_sec"]:
            if column not in fields:
                fields.append(column)
        result = []
        for index, (source, t) in enumerate(zip(kept, output_times)):
            row = dict(rows[source])
            row.setdefault("source_frame_index", row.get("frame_index", str(source)))
            row["frame_index"], row["video_time_sec"] = str(index), f"{float(t):.9f}"
            result.append(row)
        if "frame_index" not in fields:
            fields.insert(0, "frame_index")
        timestamps.append((src.name, fields, result))
    extra = {p.name for p in (task / "timestamps").glob("*.csv")} - recognized
    if extra:
        raise ValueError("存在未匹配视频的 timestamps，不能同步裁剪：" + ", ".join(sorted(extra)))
    imu = None
    src = task / "imu" / "imu.csv"
    if src.is_file():
        fields, rows = read_csv(src)
        device = clock_values(rows, "device_timestamp_unwrapped_us", src.name)
        for column in ["sample_index", "source_sample_index", "video_time_sec"]:
            if column not in fields:
                fields.append(column)
        # Union of both eyes' removed exposure intervals: none of their IMU survives.
        spans.sort()
        merged = []
        for a, b in spans:
            if merged and a <= merged[-1][1]:
                merged[-1][1] = max(b, merged[-1][1])
            else:
                merged.append([a, b])
        starts = [a for a, _ in merged]
        result = []
        media = info["times"]
        boundaries = [*media, info["duration"]]
        for source, (row, t) in enumerate(zip(rows, device)):
            pos = bisect.bisect_right(starts, t) - 1
            if pos >= 0 and t < merged[pos][1]:
                continue
            # Map the reference camera device clock to its media clock, then close gaps.
            i = max(0, min(len(reference) - 2, bisect.bisect_right(reference, t) - 1))
            if len(reference) > 1:
                media_t = media[i] + Fraction(t - reference[i], reference[i + 1] - reference[i]) * (media[i + 1] - media[i])
            else:
                media_t = Fraction(t - reference[0], 1_000_000)
            removed = sum((boundaries[b] - boundaries[a] for a, b in ranges if boundaries[b] <= media_t), Fraction(0))
            row.setdefault("source_sample_index", row.get("sample_index", str(source)))
            row["sample_index"], row["video_time_sec"] = str(len(result)), f"{float(media_t - removed):.9f}"
            result.append(row)
        imu = fields, result
    return timestamps, imu


def encode(src, dst, kept, times, duration, info, progress):
    wanted = dict(zip(kept, times))
    count = 0
    base = Fraction(1, 1_000_000)
    with av.open(str(src)) as inp, av.open(str(dst), "w", format="mp4", options={"movflags": "+faststart", "video_track_timescale": "1000000"}) as out:
        stream = inp.streams.video[0]
        encoder = out.add_stream("libx264", rate=info["fps"])
        encoder.width, encoder.height = stream.width, stream.height
        encoder.pix_fmt, encoder.time_base = "yuv420p", base
        encoder.codec_context.time_base = base
        encoder.codec_context.max_b_frames = 0
        encoder.options = {"crf": "18", "preset": "fast"}
        ends = [*times[1:], duration]
        durations = {round(t / base): round((end - t) / base) for t, end in zip(times, ends)}
        def mux(packet):
            packet.duration = durations[packet.pts]
            out.mux(packet)
        for index, frame in enumerate(inp.decode(stream)):
            if index not in wanted:
                continue
            frame.pts, frame.time_base = round(wanted[index] / base), base
            frame.pict_type = 0
            for packet in encoder.encode(frame):
                mux(packet)
            count += 1
            if count % 30 == 0:
                progress(count / len(kept))
            if count == len(kept):
                break
        for packet in encoder.encode():
            mux(packet)
    if count != len(kept):
        raise ValueError(f"{src.name} 实际解码帧数与视频索引不一致")
    actual = scan(dst)
    if actual["frames"] != len(kept) or any(abs(a - b) > 0.00001 for a, b in zip(actual["times"], times)):
        raise ValueError(f"{src.name} 裁剪后时序校验失败")
    # The last packet duration is supplied by the encoder's nominal frame rate.
    if abs(actual["duration"] - duration) > 0.00001:
        raise ValueError(f"{src.name} 裁剪后时长校验失败")
    progress(1)


def make(task, videos, segments):
    scans = scan_all(videos)
    info = scans[0]
    ranges = normalize(segments, info["frames"])
    kept, times, duration = timeline(info, ranges)
    timestamps, imu = sensor_data(task, videos, info, ranges, kept, times)
    output = task.with_name(task.name + "_裁剪")
    suffix = 2
    while output.exists():
        output = task.with_name(task.name + f"_裁剪_{suffix}")
        suffix += 1
    work = Path(tempfile.mkdtemp(prefix=".qc-trim-", dir=task.parent))
    try:
        (work / "videos").mkdir()
        for i, (video, scan_info) in enumerate(zip(videos, scans)):
            encode(video, work / "videos" / f"{video.stem}.mp4", kept, times, duration, scan_info,
                   lambda p, i=i: emit(progress=(i + p) / len(videos)))
        for name, fields, rows in timestamps:
            write_csv(work / "timestamps" / name, fields, rows)
        if imu is not None:
            write_csv(work / "imu" / "imu.csv", *imu)
        for name in ["calibration", "imu", "timestamps"]:
            src = task / name
            if src.is_dir():
                shutil.copytree(src, work / name, dirs_exist_ok=True,
                                ignore=shutil.ignore_patterns(".*", "*.csv") if name != "calibration" else shutil.ignore_patterns(".*"))
        (work / "trim.json").write_text(json.dumps({
            "source": str(task), "removed": ranges, "frames": len(kept), "duration_sec": float(duration),
            "source_frames": kept, "notes": "设备时间保持原值；video_time_sec 为裁剪后的连续时间；原目录完整保留，质检和关键点需重新启动。"
        }, ensure_ascii=False, indent=2), encoding="utf-8")
        if output.exists():
            raise FileExistsError(f"输出目录已存在：{output}")
        work.rename(output)
    except BaseException:
        shutil.rmtree(work, ignore_errors=True)
        raise
    return {"ok": True, "output": str(output)}


def main(argv=None):
    parser = argparse.ArgumentParser(description="同步删除视频、timestamps 和 IMU 的标记区间")
    parser.add_argument("task", type=Path)
    parser.add_argument("--videos", required=True)
    parser.add_argument("--segments")
    parser.add_argument("--probe", action="store_true")
    args = parser.parse_args(argv)
    try:
        videos = [Path(p) for p in json.loads(args.videos)]
        if args.probe:
            info = scan_all(videos)[0]
            emit(ok=True, views=[p.stem for p in videos], frames=info["frames"], fps=float(info["fps"]),
                 duration=float(info["duration"]), times=[float(t) for t in info["times"]], keyframes=[])
        else:
            emit(**make(args.task, videos, json.loads(args.segments or "[]")))
    except Exception as error:
        emit(ok=False, error=str(error))
        raise SystemExit(1)
