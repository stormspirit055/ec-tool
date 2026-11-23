import contextlib
import io
import json
import tempfile
import unittest
from fractions import Fraction
from pathlib import Path
from unittest.mock import patch

import av
import numpy as np

import qc_sample
from qc_trim import read_csv, write_csv


class SampleTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.task = Path(self.temp.name) / "完整任务"
        for view in ["top_left", "top_right"]:
            path = self.task / "videos" / f"{view}.mp4"
            path.parent.mkdir(parents=True, exist_ok=True)
            with av.open(str(path), "w") as container:
                stream = container.add_stream("libx264", rate=4)
                stream.width, stream.height, stream.pix_fmt = 64, 48, "yuv420p"
                stream.codec_context.gop_size = 4
                stream.codec_context.max_b_frames = 0
                for index in range(16):
                    frame = av.VideoFrame.from_ndarray(np.full((48, 64, 3), index * 10, dtype=np.uint8), format="rgb24")
                    frame.pts, frame.time_base = index, Fraction(1, 4)
                    for packet in stream.encode(frame):
                        container.mux(packet)
                for packet in stream.encode():
                    container.mux(packet)
            rows = [{"frame_index": str(i), "exposure_end_device_us": str(1_000_000 + i * 250_000), "source_frame_index": str(100 + i)} for i in range(16)]
            write_csv(self.task / "timestamps" / f"{view}_camera.csv", list(rows[0]), rows)
        rows = [{"sample_index": str(i), "device_timestamp_unwrapped_us": str(1_000_000 + i * 25_000), "source_sample_index": str(1000 + i), "ax": str(i)} for i in range(160)]
        write_csv(self.task / "imu" / "imu.csv", list(rows[0]), rows)
        (self.task / "imu" / "imu_info.yaml").write_text("imu-info")
        (self.task / "calibration").mkdir()
        (self.task / "calibration" / "intrinsics.yaml").write_text("camera-info")
        self.other_files = {
            "metadata.json": b'{"frames":16,"source":"original"}',
            "streams_manifest.json": b'{"source-manifest":true}',
            "checksums.sha256": b"source-checksums\n",
            "README.txt": b"source documentation",
            ".session-id": b"hidden-source-info",
            "evidence/protocol/raw.bin": bytes(range(256)),
            "qa/results/check.json": b'{"source-check":true}',
            "videos/codec-notes.txt": b"video-sidecar",
            "imu/description.json": b"imu-sidecar",
            "timestamps/README.txt": b"timestamp-sidecar",
        }
        for name, content in self.other_files.items():
            path = self.task / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(content)
        (self.task / "evidence" / "empty").mkdir()

    def make(self):
        with contextlib.redirect_stdout(io.StringIO()):
            return qc_sample.make(self.task, segments=[(8, 12), (0, 4)], handpose_views=[])

    def assert_complete(self, result):
        output = Path(result["output"])
        self.assertEqual(result["frames"], 8)
        self.assertEqual(output.parent, self.task.parent)
        for view in ["top_left", "top_right"]:
            _, rows = read_csv(output / "timestamps" / f"{view}_camera.csv")
            self.assertEqual([int(r["frame_index"]) for r in rows], list(range(8)))
            self.assertEqual([int(r["source_frame_index"]) for r in rows], [100, 101, 102, 103, 108, 109, 110, 111])
            self.assertEqual([int(r["exposure_end_device_us"]) for r in rows], [1_000_000 + i * 250_000 for i in [0, 1, 2, 3, 8, 9, 10, 11]])
            def packets(path):
                with av.open(str(path)) as container:
                    return [bytes(p) for p in container.demux(video=0) if p.size]
            source = packets(self.task / "videos" / f"{view}.mp4")
            self.assertEqual(packets(output / "videos" / f"{view}.mp4"), source[:4] + source[8:12])
        _, imu = read_csv(output / "imu" / "imu.csv")
        expected = [*range(31), *range(80, 111)]
        self.assertEqual([int(r["source_sample_index"]) for r in imu], [1000 + i for i in expected])
        self.assertEqual([int(r["sample_index"]) for r in imu], list(range(len(expected))))
        self.assertEqual((output / "imu" / "imu_info.yaml").read_text(), "imu-info")
        self.assertEqual((output / "calibration" / "intrinsics.yaml").read_text(), "camera-info")
        self.assertEqual(json.loads((output / result["manifest"]).read_text())["source_path"], str(self.task))
        for name, content in self.other_files.items():
            self.assertEqual((output / name).read_bytes(), content, f"必须原样保留 {name}")
        self.assertTrue((output / "evidence" / "empty").is_dir())
        self.assertFalse((output / "handpose").exists())
        return output

    def test_complete_stereo_sample_and_originals_untouched(self):
        original = {p: p.read_bytes() for p in self.task.rglob("*") if p.is_file()}
        self.assert_complete(self.make())
        self.assertEqual(original, {p: p.read_bytes() for p in original})

    def test_alias_filenames_and_shared_camera_timestamps(self):
        (self.task / "timestamps" / "top_left_camera.csv").rename(self.task / "timestamps" / "top_left.csv")
        (self.task / "timestamps" / "top_right_camera.csv").rename(self.task / "timestamps" / "camera.csv")
        output = self.assert_complete(self.make())
        for name in ["top_left.csv", "camera.csv"]:
            self.assertEqual(len(read_csv(output / "timestamps" / name)[1]), 8, "原文件名的 timestamps 必须也经过截取")

    def test_source_handpose_and_records_are_preserved_without_detection(self):
        (self.task / "handpose").mkdir()
        # Source results are deliberately not parsed or transformed.
        pose = b'{"frames":[{"source-frame":500}]}'
        (self.task / "handpose" / "top_left.json").write_bytes(pose)
        record = b'{"processed":"original-source-record"}'
        (self.task / ".qcworker.json").write_bytes(record)
        with patch.object(qc_sample.sample, "crop_handpose", side_effect=AssertionError("不能修改其他文件")):
            result = self.make()
        output = Path(result["output"])
        self.assertEqual((output / "handpose" / "top_left.json").read_bytes(), pose)
        self.assertEqual((output / ".qcworker.json").read_bytes(), record)

    def test_existing_manifests_are_preserved_and_new_mapping_gets_another_name(self):
        first = b'{"original":"segments"}'
        second = b'{"original":"sample-segments"}'
        (self.task / "segments.json").write_bytes(first)
        (self.task / "sample_segments.json").write_bytes(second)
        result = self.make()
        output = self.assert_complete(result)
        self.assertEqual(result["manifest"], "sample_segments_2.json")
        self.assertEqual((output / "segments.json").read_bytes(), first)
        self.assertEqual((output / "sample_segments.json").read_bytes(), second)

    def test_other_files_are_independent_copies(self):
        output = self.assert_complete(self.make())
        (output / "metadata.json").write_text("edited sample")
        self.assertEqual((self.task / "metadata.json").read_bytes(), self.other_files["metadata.json"])

    def test_nested_output_is_rejected_to_prevent_recursive_copy(self):
        with self.assertRaisesRegex(ValueError, "源任务内部"):
            qc_sample.make(self.task, output=self.task / "sample", segments=[(0, 4)])
        self.assertFalse((self.task / "sample").exists())

    def test_missing_eye_timestamps_does_not_publish_partial_output(self):
        (self.task / "timestamps" / "top_right_camera.csv").unlink()
        with self.assertRaisesRegex(ValueError, "top_right.*timestamps"):
            self.make()
        self.assertEqual(list(self.task.parent.iterdir()), [self.task])

    def test_missing_imu_does_not_publish_partial_output(self):
        (self.task / "imu" / "imu.csv").unlink()
        with self.assertRaisesRegex(ValueError, "IMU 文件"):
            self.make()
        self.assertEqual(list(self.task.parent.iterdir()), [self.task])

    def test_mismatched_timestamps_and_imu_coverage_fail(self):
        path = self.task / "timestamps" / "top_left_camera.csv"
        fields, rows = read_csv(path)
        write_csv(path, fields, rows[:-1])
        with self.assertRaisesRegex(ValueError, "帧不一致"):
            self.make()
        write_csv(path, fields, rows)
        path = self.task / "imu" / "imu.csv"
        fields, rows = read_csv(path)
        write_csv(path, fields, rows[:50])
        with self.assertRaisesRegex(ValueError, "未覆盖"):
            self.make()

    def test_export_validation_catches_silent_sensor_omission(self):
        with patch.object(qc_sample.sample, "crop_timestamps", return_value=None):
            with self.assertRaisesRegex(ValueError, "样例缺少 timestamps"):
                self.make()
        self.assertEqual(list(self.task.parent.iterdir()), [self.task])

    def test_repeated_generation_keeps_existing_sample(self):
        first = self.assert_complete(self.make())
        previous = (first / "segments.json").read_bytes()
        second = self.assert_complete(self.make())
        self.assertNotEqual(first, second)
        self.assertEqual((first / "segments.json").read_bytes(), previous)


if __name__ == "__main__":
    unittest.main()
