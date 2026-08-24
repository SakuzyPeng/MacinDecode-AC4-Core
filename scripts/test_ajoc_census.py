#!/usr/bin/env python3
"""ajoc_census 的 fail-closed、具名跳过与原子更新回归测试。"""

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

from scripts import ajoc_census


def sample(frames: int = 4) -> dict:
    return {
        "codec_frames": frames,
        "substreams": frames,
        "census": {
            "substreams": frames,
            "full_support": {
                "supported": frames,
                "unsupported": 0,
                "first_unsupported": None,
            },
            "raw": {"min": -2, "max": 7},
        },
    }


class AjocCensusTests(unittest.TestCase):
    def test_baseline_keys_cannot_escape_the_encoded_directory(self):
        for name in ["../outside.m4a", "case/../outside.m4a", "case/..\\outside.m4a"]:
            with self.subTest(name=name):
                with self.assertRaises(ValueError):
                    ajoc_census.path_for_key(name)

    def _run_gate(
        self,
        entries: dict,
        skips: dict,
        media: list[str],
        inspector,
        *,
        update: bool = False,
    ) -> tuple[int, str, str, int]:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            vectors = root / "vectors"
            baseline = vectors / "ajoc_syntax_baseline.json"
            vectors.mkdir()
            for name in media:
                case, _, leaf = name.partition("/")
                path = vectors / case / "encoded" / leaf
                path.parent.mkdir(parents=True, exist_ok=True)
                path.touch()
            baseline.write_text(
                json.dumps(
                    {
                        "comment": ajoc_census.COMMENT,
                        "entries": entries,
                        "skips": skips,
                    },
                    ensure_ascii=False,
                ),
                encoding="utf-8",
            )
            out, err = io.StringIO(), io.StringIO()
            argv = ["ajoc_census.py"] + (["--update"] if update else [])
            with (
                mock.patch.object(ajoc_census, "REPO_ROOT", root),
                mock.patch.object(ajoc_census, "VECTORS", vectors),
                mock.patch.object(ajoc_census, "BASELINE", baseline),
                mock.patch.object(sys, "argv", argv),
                mock.patch.object(ajoc_census, "inspect", side_effect=inspector) as inspect,
                contextlib.redirect_stdout(out),
                contextlib.redirect_stderr(err),
            ):
                code = ajoc_census.main()
            return code, out.getvalue(), err.getvalue(), inspect.call_count

    def test_missing_media_fails_before_inspecting_a_subset(self):
        code, _, err, calls = self._run_gate(
            {"missing/a.m4a": sample()}, {}, [], lambda path: ("ajoc", sample())
        )
        self.assertEqual(code, 1)
        self.assertIn("找不到输入", err)
        self.assertEqual(calls, 0)

    def test_unregistered_local_ajoc_media_fails_closed(self):
        def inspector(path: Path):
            return "ajoc", sample()

        code, _, err, calls = self._run_gate(
            {"old/old.m4a": sample()},
            {},
            ["old/old.m4a", "new/new.m4a"],
            inspector,
        )
        self.assertEqual(code, 1)
        self.assertIn("基线中没有该输入", err)
        self.assertEqual(calls, 2)

    def test_channel_based_skip_requires_exact_name_and_scene_path(self):
        def inspector(path: Path):
            if path.name == "ims.m4a":
                return "channel_based", None
            return "ajoc", sample()

        code, out, err, _ = self._run_gate(
            {"a/a.m4a": sample()},
            {"ims/ims.m4a": "channel_based"},
            ["a/a.m4a", "ims/ims.m4a"],
            inspector,
        )
        self.assertEqual(code, 0, err)
        self.assertIn("按名称跳过", out)

        code, _, err, _ = self._run_gate(
            {"a/a.m4a": sample()},
            {"ims/ims.m4a": "channel_based"},
            ["a/a.m4a", "ims/ims.m4a"],
            lambda path: ("direct_object", None)
            if path.name == "ims.m4a"
            else ("ajoc", sample()),
        )
        self.assertEqual(code, 1)
        self.assertIn("不允许作为 M6 具名跳过", err)

    def test_direct_object_can_never_be_registered_as_a_skip(self):
        code, _, err, _ = self._run_gate(
            {"a/a.m4a": sample()},
            {},
            ["a/a.m4a", "direct/direct.m4a"],
            lambda path: ("direct_object", None)
            if path.name == "direct.m4a"
            else ("ajoc", sample()),
            update=True,
        )
        self.assertEqual(code, 1)
        self.assertIn("不允许作为 M6 具名跳过", err)

    def test_any_census_difference_fails(self):
        changed = copy.deepcopy(sample())
        changed["census"]["raw"]["max"] = 8
        code, _, err, _ = self._run_gate(
            {"a/a.m4a": sample()},
            {},
            ["a/a.m4a"],
            lambda path: ("ajoc", changed),
        )
        self.assertEqual(code, 1)
        self.assertIn("census 与基线不一致", err)

    def test_failed_update_preserves_the_old_baseline_byte_for_byte(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            vectors = root / "vectors"
            baseline = vectors / "ajoc_syntax_baseline.json"
            for name in ("a", "b"):
                path = vectors / name / "encoded" / f"{name}.m4a"
                path.parent.mkdir(parents=True, exist_ok=True)
                path.touch()
            original = json.dumps(
                {
                    "comment": ["old"],
                    "entries": {"a/a.m4a": sample(), "b/b.m4a": sample()},
                    "skips": {},
                },
                ensure_ascii=False,
                indent=2,
            )
            baseline.write_text(original, encoding="utf-8")

            def inspector(path: Path):
                if path.name == "a.m4a":
                    return "ajoc", sample()
                raise RuntimeError("注入失败")

            with (
                mock.patch.object(ajoc_census, "REPO_ROOT", root),
                mock.patch.object(ajoc_census, "VECTORS", vectors),
                mock.patch.object(ajoc_census, "BASELINE", baseline),
                mock.patch.object(sys, "argv", ["ajoc_census.py", "--update"]),
                mock.patch.object(ajoc_census, "inspect", side_effect=inspector),
                contextlib.redirect_stdout(io.StringIO()),
                contextlib.redirect_stderr(io.StringIO()),
            ):
                code = ajoc_census.main()

            self.assertEqual(code, 1)
            self.assertEqual(baseline.read_text(encoding="utf-8"), original)

    @unittest.skipUnless(os.name == "posix", "POSIX 文件权限断言")
    def test_atomic_write_preserves_baseline_permissions(self):
        with tempfile.TemporaryDirectory() as directory:
            baseline = Path(directory) / "ajoc_syntax_baseline.json"
            baseline.write_text("old\n", encoding="utf-8")
            baseline.chmod(0o640)
            replacement = {
                "comment": ajoc_census.COMMENT,
                "entries": {},
                "skips": {},
            }
            with mock.patch.object(ajoc_census, "BASELINE", baseline):
                ajoc_census.write_baseline(replacement)
            self.assertEqual(
                json.loads(baseline.read_text(encoding="utf-8")), replacement
            )
            self.assertEqual(baseline.stat().st_mode & 0o777, 0o640)
            self.assertEqual(
                list(baseline.parent.glob(".ajoc_syntax_baseline.json.tmp-*")), []
            )

    def test_update_rebuilds_entries_and_named_skips_together(self):
        def inspector(path: Path):
            if path.name == "ims.m4a":
                return "channel_based", None
            return "ajoc", sample(7)

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            vectors = root / "vectors"
            baseline = vectors / "ajoc_syntax_baseline.json"
            for name in ("a/a.m4a", "ims/ims.m4a"):
                case, _, leaf = name.partition("/")
                path = vectors / case / "encoded" / leaf
                path.parent.mkdir(parents=True, exist_ok=True)
                path.touch()
            baseline.write_text(
                json.dumps({"comment": ["old"], "entries": {}, "skips": {}}),
                encoding="utf-8",
            )
            with (
                mock.patch.object(ajoc_census, "REPO_ROOT", root),
                mock.patch.object(ajoc_census, "VECTORS", vectors),
                mock.patch.object(ajoc_census, "BASELINE", baseline),
                mock.patch.object(sys, "argv", ["ajoc_census.py", "--update"]),
                mock.patch.object(ajoc_census, "inspect", side_effect=inspector),
                contextlib.redirect_stdout(io.StringIO()),
                contextlib.redirect_stderr(io.StringIO()),
            ):
                code = ajoc_census.main()
            written = json.loads(baseline.read_text(encoding="utf-8"))
            self.assertEqual(code, 0)
            self.assertEqual(written["comment"], ajoc_census.COMMENT)
            self.assertEqual(written["entries"], {"a/a.m4a": sample(7)})
            self.assertEqual(written["skips"], {"ims/ims.m4a": "channel_based"})

    def test_partial_update_is_rejected_without_inspection(self):
        with (
            mock.patch.object(
                sys, "argv", ["ajoc_census.py", "--update", "case/a.m4a"]
            ),
            mock.patch.object(ajoc_census, "inspect") as inspect,
            contextlib.redirect_stderr(io.StringIO()) as err,
        ):
            code = ajoc_census.main()
        self.assertEqual(code, 1)
        self.assertIn("不接受部分输入", err.getvalue())
        inspect.assert_not_called()

    def test_inspect_rejects_a_missing_full_support_credential(self):
        envelope = {
            "schema": "macinac4.cli-result",
            "result": {
                "validation": {
                    "topology": {"configuration": {"scene_path": "ajoc"}},
                    "ajoc": {
                        "coverage": {
                            "frames": 1,
                            "parsed": 1,
                            "substreams": 1,
                            "parsed_substreams": 1,
                            "failures": 0,
                        },
                        "observations": {
                            "ajoc_matrix": {
                                "substreams": 1,
                                "full_support": {
                                    "supported": 0,
                                    "unsupported": 1,
                                    "first_unsupported": "SIMPLE",
                                },
                            }
                        },
                    },
                }
            },
        }
        completed = subprocess.CompletedProcess(
            args=[], returncode=0, stdout=json.dumps(envelope), stderr=""
        )
        with mock.patch.object(
            ajoc_census.subprocess, "run", return_value=completed
        ):
            with self.assertRaisesRegex(RuntimeError, "full 支持凭证"):
                ajoc_census.inspect(Path("input.m4a"))

    def test_committed_baseline_comment_matches_the_checker(self):
        self.assertTrue(ajoc_census.BASELINE.is_file(), "census 基线应已入库")
        baseline = json.loads(ajoc_census.BASELINE.read_text(encoding="utf-8"))
        self.assertEqual(baseline["comment"], ajoc_census.COMMENT)


if __name__ == "__main__":
    unittest.main()
