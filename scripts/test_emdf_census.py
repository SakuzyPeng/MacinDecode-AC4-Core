#!/usr/bin/env python3
"""emdf_census 的 fail-closed、零路由与原子更新回归测试。"""

from __future__ import annotations

import contextlib
import copy
import io
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from scripts import emdf_census


def sample(frames: int = 4, routed: int = 2) -> dict:
    return {
        "codec_frames": frames,
        "census": {
            "infos": frames,
            "routed_infos": routed,
            "routed_frames": routed,
            "located_substreams": routed,
            "parsed_substreams": routed,
            "nonempty_substreams": routed,
            "empty_substreams": 0,
            "payloads": routed,
            "payload_bytes": routed,
            "max_payload_bytes": 1,
            "failures": 0,
            "first_error": None,
            "routes": [
                {
                    "kind": "primary",
                    "emdf_version": 0,
                    "key_id": 0,
                    "substream_index": 3,
                    "count": routed,
                }
            ],
            "signatures": [
                {
                    "id": 20,
                    "count": routed,
                    "size_bytes": 1,
                    "fnv1a64": "af63bd4c8601b7df",
                    "opaque_prefix_hex": "00",
                    "opaque_prefix_truncated": False,
                    "config": {"discard_unknown_payload": True},
                }
            ],
            "first_detail": {
                "frame": 0,
                "substream_index": 3,
                "substream_bytes": 4,
                "payload_count": 1,
                "payload_bytes": 1,
            },
        },
    }


class EmdfCensusTests(unittest.TestCase):
    def test_baseline_keys_cannot_escape_the_encoded_directory(self):
        for name in [
            "/outside.m4a",
            "../outside.m4a",
            "case/../outside.m4a",
            "case/..\\outside.m4a",
        ]:
            with self.subTest(name=name):
                with self.assertRaises(ValueError):
                    emdf_census.path_for_key(name)

    def _run_gate(
        self,
        entries: dict,
        media: list[str],
        inspector,
        *,
        update: bool = False,
    ) -> tuple[int, str, str, int]:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            vectors = root / "vectors"
            baseline = vectors / "emdf_baseline.json"
            vectors.mkdir()
            for name in media:
                case, _, leaf = name.partition("/")
                path = vectors / case / "encoded" / leaf
                path.parent.mkdir(parents=True, exist_ok=True)
                path.touch()
            baseline.write_text(
                json.dumps(
                    {"comment": emdf_census.COMMENT, "entries": entries},
                    ensure_ascii=False,
                ),
                encoding="utf-8",
            )
            out, err = io.StringIO(), io.StringIO()
            argv = ["emdf_census.py"] + (["--update"] if update else [])
            with (
                mock.patch.object(emdf_census, "REPO_ROOT", root),
                mock.patch.object(emdf_census, "VECTORS", vectors),
                mock.patch.object(emdf_census, "BASELINE", baseline),
                mock.patch.object(sys, "argv", argv),
                mock.patch.object(emdf_census, "inspect", side_effect=inspector) as inspect,
                contextlib.redirect_stdout(out),
                contextlib.redirect_stderr(err),
            ):
                code = emdf_census.main()
            return code, out.getvalue(), err.getvalue(), inspect.call_count

    def test_missing_media_fails_before_inspecting_a_subset(self):
        code, _, err, calls = self._run_gate(
            {"missing/a.m4a": sample()}, [], lambda path: sample()
        )
        self.assertEqual(code, 1)
        self.assertIn("找不到输入", err)
        self.assertEqual(calls, 0)

    def test_unregistered_local_nonzero_media_fails_closed(self):
        code, _, err, calls = self._run_gate(
            {"old/old.m4a": sample()},
            ["old/old.m4a", "new/new.m4a"],
            lambda path: sample(),
        )
        self.assertEqual(code, 1)
        self.assertIn("基线中没有该非零输入", err)
        self.assertEqual(calls, 2)

    def test_unregistered_zero_route_media_is_checked_without_being_registered(self):
        def inspector(path: Path):
            return None if path.name == "zero.m4a" else sample()

        code, out, err, calls = self._run_gate(
            {"active/active.m4a": sample()},
            ["active/active.m4a", "zero/zero.m4a"],
            inspector,
        )
        self.assertEqual(code, 0, err)
        self.assertEqual(calls, 2)
        self.assertIn("zero/zero.m4a：零 EMDF 路由", out)

    def test_registered_media_cannot_silently_become_zero_route(self):
        code, _, err, _ = self._run_gate(
            {"a/a.m4a": sample()}, ["a/a.m4a"], lambda path: None
        )
        self.assertEqual(code, 1)
        self.assertIn("变为零路由", err)

    def test_any_census_difference_fails(self):
        changed = copy.deepcopy(sample())
        changed["census"]["signatures"][0]["fnv1a64"] = "0000000000000000"
        code, _, err, _ = self._run_gate(
            {"a/a.m4a": sample()}, ["a/a.m4a"], lambda path: changed
        )
        self.assertEqual(code, 1)
        self.assertIn("census 与基线不一致", err)

    def test_failed_update_preserves_the_old_baseline_byte_for_byte(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            vectors = root / "vectors"
            baseline = vectors / "emdf_baseline.json"
            for name in ("a", "b"):
                path = vectors / name / "encoded" / f"{name}.m4a"
                path.parent.mkdir(parents=True, exist_ok=True)
                path.touch()
            original = json.dumps(
                {
                    "comment": ["old"],
                    "entries": {"a/a.m4a": sample(), "b/b.m4a": sample()},
                },
                ensure_ascii=False,
                indent=2,
            )
            baseline.write_text(original, encoding="utf-8")

            def inspector(path: Path):
                if path.name == "a.m4a":
                    return sample()
                raise RuntimeError("注入失败")

            with (
                mock.patch.object(emdf_census, "REPO_ROOT", root),
                mock.patch.object(emdf_census, "VECTORS", vectors),
                mock.patch.object(emdf_census, "BASELINE", baseline),
                mock.patch.object(sys, "argv", ["emdf_census.py", "--update"]),
                mock.patch.object(emdf_census, "inspect", side_effect=inspector),
                contextlib.redirect_stdout(io.StringIO()),
                contextlib.redirect_stderr(io.StringIO()),
            ):
                code = emdf_census.main()

            self.assertEqual(code, 1)
            self.assertEqual(baseline.read_text(encoding="utf-8"), original)

    @unittest.skipUnless(os.name == "posix", "POSIX 文件权限断言")
    def test_atomic_write_preserves_baseline_permissions(self):
        with tempfile.TemporaryDirectory() as directory:
            baseline = Path(directory) / "emdf_baseline.json"
            baseline.write_text("old\n", encoding="utf-8")
            baseline.chmod(0o640)
            replacement = {"comment": emdf_census.COMMENT, "entries": {}}
            with mock.patch.object(emdf_census, "BASELINE", baseline):
                emdf_census.write_baseline(replacement)
            self.assertEqual(
                json.loads(baseline.read_text(encoding="utf-8")), replacement
            )
            self.assertEqual(baseline.stat().st_mode & 0o777, 0o640)
            self.assertEqual(
                list(baseline.parent.glob(".emdf_baseline.json.tmp-*")), []
            )

    def test_update_writes_only_routed_media(self):
        def inspector(path: Path):
            return None if path.name == "zero.m4a" else sample(7, 3)

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            vectors = root / "vectors"
            baseline = vectors / "emdf_baseline.json"
            for name in ("active/active.m4a", "zero/zero.m4a"):
                case, _, leaf = name.partition("/")
                path = vectors / case / "encoded" / leaf
                path.parent.mkdir(parents=True, exist_ok=True)
                path.touch()
            baseline.write_text(
                json.dumps({"comment": ["old"], "entries": {}}),
                encoding="utf-8",
            )
            with (
                mock.patch.object(emdf_census, "REPO_ROOT", root),
                mock.patch.object(emdf_census, "VECTORS", vectors),
                mock.patch.object(emdf_census, "BASELINE", baseline),
                mock.patch.object(sys, "argv", ["emdf_census.py", "--update"]),
                mock.patch.object(emdf_census, "inspect", side_effect=inspector),
                contextlib.redirect_stdout(io.StringIO()),
                contextlib.redirect_stderr(io.StringIO()),
            ):
                code = emdf_census.main()
            written = json.loads(baseline.read_text(encoding="utf-8"))
            self.assertEqual(code, 0)
            self.assertEqual(written["comment"], emdf_census.COMMENT)
            self.assertEqual(
                written["entries"], {"active/active.m4a": sample(7, 3)}
            )

    def test_partial_update_is_rejected_without_inspection(self):
        with (
            mock.patch.object(
                sys, "argv", ["emdf_census.py", "--update", "case/a.m4a"]
            ),
            mock.patch.object(emdf_census, "inspect") as inspect,
            contextlib.redirect_stderr(io.StringIO()) as err,
        ):
            code = emdf_census.main()
        self.assertEqual(code, 1)
        self.assertIn("不接受部分输入", err.getvalue())
        inspect.assert_not_called()

    def test_inspect_rejects_inconsistent_route_counts(self):
        census = sample()["census"]
        census["routes"][0]["count"] = 1
        envelope = {
            "schema": "macinac4.cli-result",
            "result": {
                "validation": {
                    "topology": {
                        "coverage": {"frames_parsed": 4, "parse_failures": 0},
                        "observations": {"emdf": census},
                    }
                }
            },
        }
        completed = subprocess.CompletedProcess(
            args=[], returncode=0, stdout=json.dumps(envelope), stderr=""
        )
        with mock.patch.object(
            emdf_census.subprocess, "run", return_value=completed
        ):
            with self.assertRaisesRegex(RuntimeError, "route count"):
                emdf_census.inspect(Path("input.m4a"))

    def test_inspect_accepts_a_consistent_zero_route_media(self):
        census = sample()["census"]
        census.update(
            {
                "routed_infos": 0,
                "routed_frames": 0,
                "located_substreams": 0,
                "parsed_substreams": 0,
                "nonempty_substreams": 0,
                "payloads": 0,
                "payload_bytes": 0,
                "max_payload_bytes": 0,
                "routes": [],
                "signatures": [],
                "first_detail": None,
            }
        )
        envelope = {
            "schema": "macinac4.cli-result",
            "result": {
                "validation": {
                    "topology": {
                        "coverage": {"frames_parsed": 4, "parse_failures": 0},
                        "observations": {"emdf": census},
                    }
                }
            },
        }
        completed = subprocess.CompletedProcess(
            args=[], returncode=0, stdout=json.dumps(envelope), stderr=""
        )
        with mock.patch.object(
            emdf_census.subprocess, "run", return_value=completed
        ):
            self.assertIsNone(emdf_census.inspect(Path("input.m4a")))

    def test_committed_baseline_comment_matches_the_checker(self):
        self.assertTrue(emdf_census.BASELINE.is_file(), "EMDF census 基线应已入库")
        baseline = json.loads(emdf_census.BASELINE.read_text(encoding="utf-8"))
        self.assertEqual(baseline["comment"], emdf_census.COMMENT)


if __name__ == "__main__":
    unittest.main()
