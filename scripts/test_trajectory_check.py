#!/usr/bin/env python3
"""trajectory_check 的 fail-closed 回归测试。"""

from __future__ import annotations

import contextlib
import io
import json
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from scripts import trajectory_check


def complete_audio() -> dict:
    return {
        "frames": 2,
        "parsed": 2,
        "substreams": 2,
        "parsed_substreams": 2,
        "failures": 0,
        "first_positions": [
            {
                "frame": 0,
                "substream": 2,
                "objects": [{"object": 1, "x": 0, "y": 31, "z": 0}],
            }
        ],
        "position_timeline": [],
        "position_timeline_truncated": False,
    }


def object_case() -> dict:
    return {
        "case_id": "test",
        "sample_rate": 48000,
        "duration_samples": 4096,
        "encodes": [768],
        "objects": [
            {
                "name": "object",
                "segments": [{"start_samples": 0, "position": [-1.0, 0.0, 0.0]}],
            }
        ],
    }


def trace_report(audio: dict, container: dict) -> dict:
    return {
        "result": {
            "source": {"kind": "mp4", "track": container},
            "validation": {
                "ajoc": {
                    "coverage": {
                        key: audio[key]
                        for key in ("frames", "parsed", "substreams", "parsed_substreams", "failures")
                    },
                    "references": {},
                    "timing": {"fill_bits": {"min": None, "max": None}},
                    "configuration": {},
                    "spectrum": {"scale_factor": {"min": None, "max": None}},
                    "pcm": {},
                    "invariants": {"reconstruction": {}},
                    "observations": {
                        key: value
                        for key, value in audio.items()
                        if key not in ("frames", "parsed", "substreams", "parsed_substreams", "failures")
                    },
                }
            },
        }
    }


class TrajectoryCheckTests(unittest.TestCase):
    def test_missing_media_fails_an_object_case(self):
        with tempfile.TemporaryDirectory() as directory:
            case_dir = Path(directory)
            (case_dir / "case.json").write_text(json.dumps(object_case()))
            with contextlib.redirect_stdout(io.StringIO()):
                self.assertFalse(trajectory_check.check_case(case_dir))

    def test_trace_failure_cannot_pass_on_the_remaining_track(self):
        audio = complete_audio()
        audio["failures"] = 1
        audio["parsed"] = 1
        trace = trace_report(audio, {"sample_count": 2, "media_duration": 4096})
        with tempfile.TemporaryDirectory() as directory:
            case_dir = Path(directory)
            (case_dir / "case.json").write_text(json.dumps(object_case()))
            encoded = case_dir / "encoded"
            encoded.mkdir()
            (encoded / "master_ac4_768K.m4a").touch()
            with mock.patch.object(trajectory_check, "run_trace", return_value=trace):
                with contextlib.redirect_stdout(io.StringIO()):
                    self.assertFalse(trajectory_check.check_case(case_dir))

    def test_check_case_uses_the_container_frame_count(self):
        cases = {
            "A-JOC 帧多于容器样本": (
                object_case(),
                {**complete_audio(), "frames": 3, "parsed": 3},
                {"sample_count": 2, "media_duration": 4096},
                False,
            ),
            "延迟快照仍按完整容器重建": (
                {**object_case(), "duration_samples": 12288},
                {
                    **complete_audio(),
                    "first_positions": [
                        {**complete_audio()["first_positions"][0], "frame": 2}
                    ],
                },
                {"sample_count": 6, "media_duration": 12288},
                True,
            ),
        }
        for name, (case, audio, container, expected) in cases.items():
            with self.subTest(case=name):
                trace = trace_report(audio, container)
                with tempfile.TemporaryDirectory() as directory:
                    case_dir = Path(directory)
                    (case_dir / "case.json").write_text(json.dumps(case))
                    encoded = case_dir / "encoded"
                    encoded.mkdir()
                    (encoded / "master_ac4_768K.m4a").touch()
                    with mock.patch.object(
                        trajectory_check, "run_trace", return_value=trace
                    ):
                        with contextlib.redirect_stdout(io.StringIO()):
                            ok = trajectory_check.check_case(case_dir)
                self.assertEqual(ok, expected)

    def test_zero_ajoc_frames_are_rejected(self):
        audio = complete_audio()
        audio.update(
            frames=0,
            parsed=0,
            substreams=0,
            parsed_substreams=0,
        )

        errors = trajectory_check.audio_integrity_errors(audio, total_frames=2)

        self.assertIn("没有 A-JOC substream 帧", errors)
        self.assertIn("没有可解析的 A-JOC substream", errors)

    def test_each_integrity_field_is_checked_on_its_own(self):
        """逐字段单独触发，断言错误恰好一条。

        统计字段彼此相关，一份「一半帧失败」的输入会同时踩中 `failures` 与
        `parsed != frames`；那样任删其一都不会被发现。断言只有一条错误，才
        能保证每个检查各自成立。
        """
        cases = {
            "failures": ({"failures": 1}, "A-JOC 解析报告 1 个失败帧"),
            "parsed": ({"parsed": 1}, "A-JOC 帧未全部解析：1/2"),
            "parsed_substreams": (
                {"parsed_substreams": 1},
                "A-JOC substream 未全部解析：1/2",
            ),
            "frames": (
                {"frames": 3, "parsed": 3},
                "A-JOC 帧数 3 超过容器样本数 2",
            ),
        }
        for field, (patch, message) in cases.items():
            with self.subTest(field=field):
                audio = complete_audio()
                audio.update(patch)
                self.assertEqual(
                    trajectory_check.audio_integrity_errors(audio, total_frames=2),
                    [message],
                )

    def test_truncated_timeline_is_rejected(self):
        audio = complete_audio()
        audio["position_timeline_truncated"] = True

        with self.assertRaises(trajectory_check.TrajectoryError):
            trajectory_check.rebuild_tracks(audio, 2)

    def test_self_contradictory_timelines_are_rejected(self):
        """快照与变化点互不自洽的输入都不得静默通过。"""
        snapshot = complete_audio()["first_positions"][0]
        change = {"frame": 1, "substream": 2, "object": 1, "x": 62, "y": 31, "z": 0}
        cases = {
            "首位置帧为负": lambda audio: audio["first_positions"][0].update(
                frame=-1
            ),
            "首位置帧超出容器": lambda audio: audio["first_positions"][0].update(
                frame=2
            ),
            "重复快照": lambda audio: audio["first_positions"].append(
                json.loads(json.dumps(snapshot))
            ),
            "变化点没有快照": lambda audio: audio["position_timeline"].append(
                {**change, "object": 7}
            ),
            "变化帧早于快照": lambda audio: (
                audio["first_positions"][0].update(frame=1),
                audio["position_timeline"].append({**change, "frame": 0}),
            ),
            "变化帧超出容器": lambda audio: audio["position_timeline"].append(
                {**change, "frame": 9}
            ),
        }
        for name, mutate in cases.items():
            with self.subTest(case=name):
                audio = complete_audio()
                mutate(audio)
                with self.assertRaises(trajectory_check.TrajectoryError):
                    trajectory_check.rebuild_tracks(audio, 2)

    def test_rebuild_starts_at_the_snapshot_frame(self):
        audio = complete_audio()
        audio["first_positions"][0]["frame"] = 2
        audio["position_timeline"] = [
            {"frame": 4, "substream": 2, "object": 1, "x": 62, "y": 31, "z": 0}
        ]

        track = trajectory_check.rebuild_tracks(audio, 6)[(2, 1)]

        self.assertEqual(track[:2], [None, None])
        self.assertEqual(track[2:4], [(0, 31, 0), (0, 31, 0)])
        self.assertEqual(track[4:], [(62, 31, 0), (62, 31, 0)])

    def test_degenerate_inputs_are_reported_not_raised(self):
        """两种退化输入各有专门分支；少了它们会变成除零或含糊的报错。"""
        cases = {
            "容器帧长无效": (
                {"sample_count": 2, "media_duration": 1},
                complete_audio(),
            ),
            "没有可重建的位置轨迹": (
                {"sample_count": 2, "media_duration": 4096},
                {**complete_audio(), "first_positions": [None]},
            ),
        }
        for message, (container, audio) in cases.items():
            with self.subTest(case=message):
                trace = trace_report(audio, container)
                with tempfile.TemporaryDirectory() as directory:
                    case_dir = Path(directory)
                    (case_dir / "case.json").write_text(json.dumps(object_case()))
                    encoded = case_dir / "encoded"
                    encoded.mkdir()
                    (encoded / "master_ac4_768K.m4a").touch()
                    output = io.StringIO()
                    with mock.patch.object(
                        trajectory_check, "run_trace", return_value=trace
                    ):
                        with contextlib.redirect_stdout(output):
                            ok = trajectory_check.check_case(case_dir)
                self.assertFalse(ok)
                self.assertIn(message, output.getvalue())

    def test_missing_audible_frames_fail_evaluation(self):
        case = object_case()
        result = trajectory_check.evaluate(
            case,
            case["objects"][0],
            [None, None],
            frame_len=2048,
            frames=2,
        )

        self.assertIsNotNone(result)
        self.assertFalse(result["ok"])
        self.assertGreater(result["missing"], 0)

    def test_media_selection_includes_ajoc_and_excludes_ims(self):
        case = {
            **object_case(),
            "dme_ac4": [
                {"level": 3, "bitrate": 768},
                {"level": 4, "bitrate": 768, "mode": "3dof"},
            ],
            "dee_ims": [
                {
                    "bitrate": 256,
                    "encoding_profile": "ims",
                    "legacy_presentation": False,
                }
            ],
        }
        with tempfile.TemporaryDirectory() as directory:
            case_dir = Path(directory)
            encoded = case_dir / "encoded"
            encoded.mkdir()
            names = [
                "master_ac4_768K.m4a",
                "master_ac4_dme_l3_768K.m4a",
                "master_ac4_dme_l4_768K_3dof.m4a",
                "master_ac4_ims_256K.m4a",
            ]
            for name in names:
                (encoded / name).touch()

            media, skipped, errors = trajectory_check.select_trajectory_media(
                case_dir, case
            )

        self.assertEqual([item.name for item in media], names[:3])
        self.assertEqual([item.name for item in skipped], names[3:])
        self.assertEqual(errors, [])

    def test_unclassified_media_fails_closed(self):
        with tempfile.TemporaryDirectory() as directory:
            case_dir = Path(directory)
            encoded = case_dir / "encoded"
            encoded.mkdir()
            (encoded / "master_ac4_768K.m4a").touch()
            (encoded / "mystery.m4a").touch()

            _, _, errors = trajectory_check.select_trajectory_media(
                case_dir, object_case()
            )

        self.assertEqual(errors, ["存在未分类的编码产物：mystery.m4a"])

    def test_declared_ajoc_media_must_exist(self):
        with tempfile.TemporaryDirectory() as directory:
            case_dir = Path(directory)
            (case_dir / "encoded").mkdir()

            media, _, errors = trajectory_check.select_trajectory_media(
                case_dir, object_case()
            )

        self.assertEqual(media, [])
        self.assertEqual(
            errors,
            ["缺少已声明的 A-JOC 产物：master_ac4_768K.m4a"],
        )


if __name__ == "__main__":
    unittest.main()
