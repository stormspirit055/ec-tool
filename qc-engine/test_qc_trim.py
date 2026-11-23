import contextlib
import io
import json
import tempfile
import unittest
from fractions import Fraction
from pathlib import Path

import av
import numpy as np

import qc_trim as trim


class TrimTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.task = Path(self.temp.name) / "双目任务"
        self.videos = [self.task / "videos" / f"top_{eye}.mp4" for eye in ["left", "right"]]
        self.times = [Fraction(i, 10) for i in range(12)]
        for video in self.videos:
            self.video(video, self.times)
            rows = [{"frame_index": str(i), "exposure_end_device_us": str(1_000_000 + round(t * 1e6))} for i, t in enumerate(self.times)]
            trim.write_csv(self.task / "timestamps" / f"{video.stem}_camera.csv", list(rows[0]), rows)
        rows = [{"sample_index": str(i), "device_timestamp_unwrapped_us": str(1_000_000 + i * 10_000), "ax": str(i)} for i in range(120)]
        trim.write_csv(self.task / "imu" / "imu.csv", list(rows[0]), rows)
        (self.task / "calibration").mkdir()
        (self.task / "calibration" / "camera.yaml").write_text("calibration")

    def video(self, path, times, bframes=0):
        path.parent.mkdir(parents=True, exist_ok=True)
        with av.open(str(path), "w") as out:
            stream = out.add_stream("libx264", rate=10)
            stream.width, stream.height, stream.pix_fmt = 64, 48, "yuv420p"
            stream.time_base = stream.codec_context.time_base = Fraction(1, 1000)
            stream.codec_context.max_b_frames = bframes
            for i, t in enumerate(times):
                frame = av.VideoFrame.from_ndarray(np.full((48, 64, 3), i * 15, dtype=np.uint8), format="rgb24")
                frame.pts, frame.time_base = int(t * 1000), Fraction(1, 1000)
                for packet in stream.encode(frame):
                    out.mux(packet)
            for packet in stream.encode():
                out.mux(packet)

    def make(self, ranges):
        with contextlib.redirect_stdout(io.StringIO()):
            result = trim.make(self.task, self.videos, [{"start": a, "end": b} for a, b in ranges])
        return Path(result["output"])

    def check(self, ranges, expected):
        originals = {p: p.read_bytes() for p in self.task.rglob("*") if p.is_file()}
        out = self.make(ranges)
        for video in self.videos:
            target = out / "videos" / video.name
            info = trim.scan(target)
            self.assertEqual(info["frames"], len(expected))
            self.assertEqual(info["times"], [Fraction(i, 10) for i in range(len(expected))])
            self.assertAlmostEqual(float(info["duration"]), len(expected) / 10, places=5)
            with av.open(str(target)) as container:
                intensities = [float(f.to_ndarray(format="rgb24").mean()) for f in container.decode(video=0)]
            for mean, source in zip(intensities, expected):
                self.assertLess(abs(mean - source * 15), 5, "保留的画面必须与源帧对应")
            _, rows = trim.read_csv(out / "timestamps" / f"{video.stem}_camera.csv")
            self.assertEqual([int(r["source_frame_index"]) for r in rows], expected)
            self.assertEqual([int(r["frame_index"]) for r in rows], list(range(len(expected))))
            self.assertEqual([float(r["video_time_sec"]) for r in rows], [i / 10 for i in range(len(expected))])
        _, rows = trim.read_csv(out / "imu" / "imu.csv")
        expected_imu = [i for i in range(120) if i // 10 in expected]
        self.assertEqual([int(r["source_sample_index"]) for r in rows], expected_imu)
        self.assertEqual([int(r["sample_index"]) for r in rows], list(range(len(rows))))
        self.assertEqual([float(r["video_time_sec"]) for r in rows], [i / 100 for i in range(len(rows))])
        self.assertEqual((out / "calibration" / "camera.yaml").read_text(), "calibration")
        self.assertEqual(json.loads((out / "trim.json").read_text())["source_frames"], expected)
        self.assertFalse((out / "handpose").exists())
        self.assertFalse((out / ".qcworker.json").exists())
        self.assertEqual(originals, {p: p.read_bytes() for p in originals})
        return out

    def test_unsorted_overlapping_and_adjacent_deletions(self):
        self.check([(8, 10), (3, 5), (2, 4), (5, 6)], [0, 1, 6, 7, 10, 11])

    def test_head_tail_and_single_frame_retained(self):
        self.check([(0, 4), (5, 12)], [4])

    def test_head_tail_remove_imu_guard_samples_too(self):
        path = self.task / "imu" / "imu.csv"
        fields, rows = trim.read_csv(path)
        rows = [{**rows[0], "sample_index": "-1", "device_timestamp_unwrapped_us": "999950"}, *rows,
                {**rows[-1], "sample_index": "120", "device_timestamp_unwrapped_us": "2200050"}]
        trim.write_csv(path, fields, rows)
        out = self.make([(0, 4), (5, 12)])
        _, result = trim.read_csv(out / "imu" / "imu.csv")
        self.assertEqual([int(r["source_sample_index"]) for r in result], list(range(40, 50)))
        self.assertEqual([float(r["video_time_sec"]) for r in result], [i / 100 for i in range(10)])

    def test_repeated_output_uses_new_name(self):
        self.assertNotEqual(self.make([(2, 3)]), self.make([(2, 3)]))

    def test_invalid_ranges_and_no_partial_output(self):
        for ranges in [[], [(0, 12)], [(2, 2)], [(-1, 2)], [(3, 13)]]:
            with self.assertRaises(ValueError):
                self.make(ranges)
        self.assertEqual(list(self.task.parent.iterdir()), [self.task])

    def test_mismatched_stereo_and_missing_timestamps_fail(self):
        self.video(self.videos[1], self.times[:-1])
        with self.assertRaisesRegex(ValueError, "时序不一致"):
            self.make([(2, 3)])
        self.video(self.videos[1], self.times)
        (self.task / "timestamps" / "top_left_camera.csv").unlink()
        with self.assertRaisesRegex(ValueError, "timestamps"):
            self.make([(2, 3)])

    def test_vfr_and_bframes_have_exact_presentation_times(self):
        for path in (self.task / "timestamps").glob("*.csv"):
            path.unlink()
        (self.task / "imu" / "imu.csv").unlink()
        times = [Fraction(t, 1000) for t in [0, 80, 200, 300, 450, 510, 650, 720, 800, 920, 1030, 1150]]
        for video in self.videos:
            self.video(video, times, bframes=2)
        info = trim.scan(self.videos[0])
        _, expected, duration = trim.timeline(info, [[2, 4], [7, 9]])
        out = self.make([(2, 4), (7, 9)])
        for video in self.videos:
            actual = trim.scan(out / "videos" / video.name)
            self.assertEqual(actual["times"], expected)
            self.assertEqual(actual["duration"], duration)


if __name__ == "__main__":
    unittest.main()
