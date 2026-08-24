#!/usr/bin/env python3
"""decode_check 的 fail-closed、分段隔离与原子更新回归测试。

真实语料跑不到这里的多数分支：那两条 channel-based 向量本来就不在基线里，
所以「已冻结条目也能跳过」之类的注入在门禁上完全沉默。fail-closed 的守卫只
能由构造输入来验，否则它守的是一个到不了的分支。"""

from __future__ import annotations

import contextlib
import io
import json
import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from scripts import decode_check


def shape(digest: str) -> dict:
    return {
        "sha256": digest * 64,
        "sample_rate": 48_000,
        "channels": 2,
        "frames": 4,
        "tracks": ["2:0:0", "2:0:1"],
    }


def core_stage() -> decode_check.Stage:
    return decode_check.stages()["core"]


class DecodeCheckTests(unittest.TestCase):
    def test_default_inputs_include_missing_baseline_media_and_local_extras(self):
        with tempfile.TemporaryDirectory() as directory:
            vectors = Path(directory) / "vectors"
            present = vectors / "present" / "encoded" / "present.m4a"
            extra = vectors / "extra" / "encoded" / "extra.m4a"
            present.parent.mkdir(parents=True)
            extra.parent.mkdir(parents=True)
            present.touch()
            extra.touch()
            entries = {
                "missing/missing.m4a": shape("a"),
                "present/present.m4a": shape("b"),
            }

            with mock.patch.object(decode_check, "VECTORS", vectors):
                inputs = decode_check.default_inputs(entries)

            self.assertEqual(
                {path.relative_to(vectors).as_posix() for path in inputs},
                {
                    "extra/encoded/extra.m4a",
                    "missing/encoded/missing.m4a",
                    "present/encoded/present.m4a",
                },
            )

    def test_missing_default_input_fails_without_decoding_a_subset(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            vectors = root / "vectors"
            baseline = vectors / "decode_baseline.json"
            vectors.mkdir()
            baseline.write_text(
                json.dumps({"comment": [], "entries": {"missing/a.m4a": shape("a")}}),
                encoding="utf-8",
            )
            stderr = io.StringIO()
            with (
                mock.patch.object(decode_check, "REPO_ROOT", root),
                mock.patch.object(decode_check, "VECTORS", vectors),
                mock.patch.object(sys, "argv", ["decode_check.py", "--stage", "core"]),
                mock.patch.object(decode_check, "decode") as decode,
                contextlib.redirect_stderr(stderr),
            ):
                result = decode_check.main()

            self.assertEqual(result, 1)
            self.assertIn("找不到输入", stderr.getvalue())
            decode.assert_not_called()

    def test_failed_update_keeps_the_old_baseline_byte_for_byte(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            vectors = root / "vectors"
            baseline = vectors / "decode_baseline.json"
            first = vectors / "a" / "encoded" / "a.m4a"
            second = vectors / "b" / "encoded" / "b.m4a"
            first.parent.mkdir(parents=True)
            second.parent.mkdir(parents=True)
            first.touch()
            second.touch()
            original = json.dumps(
                {
                    "comment": ["old"],
                    "entries": {"a/a.m4a": shape("a"), "b/b.m4a": shape("b")},
                },
                ensure_ascii=False,
                indent=2,
            )
            baseline.write_text(original, encoding="utf-8")

            def fake_decode(path: Path, stage) -> dict:
                if path.name == "a.m4a":
                    return shape("c")
                raise RuntimeError("注入失败")

            with (
                mock.patch.object(decode_check, "REPO_ROOT", root),
                mock.patch.object(decode_check, "VECTORS", vectors),
                mock.patch.object(
                    sys, "argv", ["decode_check.py", "--stage", "core", "--update"]
                ),
                mock.patch.object(decode_check, "decode", side_effect=fake_decode),
                contextlib.redirect_stdout(io.StringIO()),
                contextlib.redirect_stderr(io.StringIO()),
            ):
                result = decode_check.main()

            self.assertEqual(result, 1)
            self.assertEqual(baseline.read_text(encoding="utf-8"), original)

    @unittest.skipUnless(os.name == "posix", "POSIX 文件权限断言")
    def test_atomic_write_preserves_the_baseline_permissions(self):
        with tempfile.TemporaryDirectory() as directory:
            vectors = Path(directory)
            baseline = vectors / "decode_baseline.json"
            baseline.write_text("old\n", encoding="utf-8")
            baseline.chmod(0o640)
            replacement = {"comment": ["new"], "entries": {}}

            with mock.patch.object(decode_check, "VECTORS", vectors):
                decode_check.write_baseline(core_stage(), replacement)

            self.assertEqual(
                json.loads(baseline.read_text(encoding="utf-8")), replacement
            )
            self.assertEqual(baseline.stat().st_mode & 0o777, 0o640)
            self.assertEqual(list(baseline.parent.glob(".decode_baseline.json.tmp-*")), [])

    def test_baseline_keys_cannot_escape_the_encoded_directory(self):
        for name in ["../outside.m4a", "case/../outside.m4a", "case/..\\outside.m4a"]:
            with self.subTest(name=name):
                with self.assertRaises(ValueError):
                    decode_check.path_for_key(name)

    def test_scene_pcm_presentation_overrides_are_explicit_and_u32(self):
        override = {"case/input.m4a": 1}
        for name in ("core", "aspx", "objects"):
            stage = decode_check.stages()[name]
            with self.subTest(stage=name):
                self.assertEqual(
                    decode_check.presentation_overrides(
                        stage, {"entries": {}, "presentation_overrides": override}
                    ),
                    override,
                )
                for invalid in [True, -1, 1 << 32, "0"]:
                    with self.subTest(invalid=invalid), self.assertRaises(ValueError):
                        decode_check.presentation_overrides(
                            stage,
                            {
                                "entries": {},
                                "presentation_overrides": {"case/input.m4a": invalid},
                            },
                        )
    def test_scene_pcm_gates_forward_the_declared_presentation(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            vectors = root / "vectors"
            media = vectors / "case" / "encoded" / "input.m4a"
            media.parent.mkdir(parents=True)
            media.touch()
            for name, baseline_name in [
                ("core", "decode_baseline.json"),
                ("aspx", "aspx_baseline.json"),
                ("objects", "objects_baseline.json"),
            ]:
                with self.subTest(stage=name):
                    (vectors / baseline_name).write_text(
                        json.dumps(
                            {
                                "comment": [],
                                "presentation_overrides": {"case/input.m4a": 1},
                                "entries": {"case/input.m4a": shape("a")},
                            }
                        ),
                        encoding="utf-8",
                    )
                    with (
                        mock.patch.object(decode_check, "REPO_ROOT", root),
                        mock.patch.object(decode_check, "VECTORS", vectors),
                        mock.patch.object(
                            decode_check, "decode", return_value=shape("a")
                        ) as decode,
                        contextlib.redirect_stdout(io.StringIO()),
                        contextlib.redirect_stderr(io.StringIO()),
                    ):
                        stage = decode_check.stages()[name]
                        failed = decode_check.run_stage(stage, [], update=False)

                    self.assertFalse(failed)
                    decode.assert_called_once_with(media, stage, 1)

    def run_gate(self, entries: dict, decoder, media: list[str]) -> tuple[int, str, str]:
        """在临时向量树上跑一次门禁，返回 (退出码, stdout, stderr)。"""
        with tempfile.TemporaryDirectory() as directory:
            # `key_for` 用 `resolve()` 后再取相对路径；临时目录在 macOS 上会
            # 被解析成 `/private/...`，不先归一化会让键退化成裸文件名。
            root = Path(directory).resolve()
            vectors = root / "vectors"
            baseline = vectors / "decode_baseline.json"
            vectors.mkdir()
            for name in media:
                case, _, leaf = name.partition("/")
                path = vectors / case / "encoded" / leaf
                path.parent.mkdir(parents=True, exist_ok=True)
                path.touch()
            baseline.write_text(
                json.dumps({"comment": [], "entries": entries}, ensure_ascii=False),
                encoding="utf-8",
            )
            out, err = io.StringIO(), io.StringIO()
            with (
                mock.patch.object(decode_check, "REPO_ROOT", root),
                mock.patch.object(decode_check, "VECTORS", vectors),
                mock.patch.object(sys, "argv", ["decode_check.py", "--stage", "core"]),
                mock.patch.object(decode_check, "decode", side_effect=decoder),
                contextlib.redirect_stdout(out),
                contextlib.redirect_stderr(err),
            ):
                code = decode_check.main()
            return code, out.getvalue(), err.getvalue()

    def test_an_unimplemented_coding_path_is_skipped_only_outside_the_baseline(self):
        """跳过是有条件的放行：**已冻结的条目永远不许跳过。**

        真实语料里跳不到这条——那两个 channel-based 向量本来就不在基线里，
        所以「已进基线的也能跳」这类注入在门禁上完全沉默。fail-closed 的守卫
        必须由构造输入来验，否则它守的是一个到不了的分支。
        """
        unsupported = decode_check.DecodeFailed("尚未实现", "channel_based")

        def decoder(path: Path, stage) -> dict:
            if path.name == "new.m4a":
                raise unsupported
            return shape("a")

        code, out, _ = self.run_gate(
            {"old/old.m4a": shape("a")}, decoder, ["old/old.m4a", "new/new.m4a"]
        )
        self.assertEqual(code, 0)
        self.assertIn("跳过，编码路径 channel_based", out)
        self.assertIn("已跳过 1 个", out)

        # 同一个失败落在已冻结的条目上，必须失败而不是跳过。
        code, _, err = self.run_gate(
            {"old/old.m4a": shape("a")},
            lambda path, stage: (_ for _ in ()).throw(unsupported),
            ["old/old.m4a"],
        )
        self.assertEqual(code, 1)
        self.assertIn("解码失败", err)

    def test_other_failures_are_never_treated_as_unimplemented(self):
        """只有 `unsupported.coding_path` 能放行，别的失败一律照旧。"""

        def decoder(path: Path, stage) -> dict:
            raise decode_check.DecodeFailed("解码器崩了")

        code, _, err = self.run_gate({}, decoder, ["new/new.m4a"])
        self.assertEqual(code, 1)
        self.assertIn("解码失败", err)

    def test_skipping_everything_is_not_a_pass(self):
        """一条都没解出来时必须失败，否则跳过就成了免检。"""

        def decoder(path: Path, stage) -> dict:
            raise decode_check.DecodeFailed("尚未实现", "channel_based")

        code, _, err = self.run_gate({}, decoder, ["a/a.m4a", "b/b.m4a"])
        self.assertEqual(code, 1)
        self.assertIn("没有任何输入被解码", err)

    def test_the_band_extended_track_source_pins_the_lfe_role(self):
        """LFE 被错标成一个 A-JOC 输入下标时，来源串必须变。

        带宽扩展那段的下标语义是 `Pseudocode 14a` 的 A-JOC 输入顺序，LFE 不
        进入 A-JOC。只记整数下标的话，错标既不改变声道数也不改变摘要，基线
        会静默接受一份语义已经错了的导出——这条接缝在 Rust 单元测试里够不到，
        只有真实码流上的逐路自述能钉住。
        """
        aspx = decode_check.stages()["aspx"]
        self.assertEqual(
            aspx.track_of({"substream": 2, "role": "ajoc_input", "ajoc_input": 4}),
            "2:ajoc_input:4",
        )
        self.assertEqual(aspx.track_of({"substream": 2, "role": "lfe"}), "2:lfe")
        # 错标后的自述会带上下标，与真正的 LFE 串不同。
        self.assertNotEqual(
            aspx.track_of({"substream": 2, "role": "ajoc_input", "ajoc_input": 5}),
            aspx.track_of({"substream": 2, "role": "lfe"}),
        )
        # 核心带那段仍按传输侧编号，两段不共用写法。
        core = core_stage()
        self.assertEqual(
            core.track_of({"substream": 2, "element": 0, "channel": 1}), "2:0:1"
        )
        self.assertNotEqual(core.baseline, aspx.baseline)

        objects = decode_check.stages()["objects"]
        self.assertEqual(
            objects.track_of(
                {
                    "substream": 2,
                    "role": "ajoc_object",
                    "ajoc_object": 4,
                    "output_channel": 5,
                }
            ),
            "2:ajoc_object:4:5",
        )
        self.assertEqual(
            objects.track_of(
                {"substream": 2, "role": "lfe", "output_channel": 0}
            ),
            "2:lfe:0",
        )
        self.assertNotEqual(objects.baseline, core.baseline)
        self.assertNotEqual(objects.baseline, aspx.baseline)

    def test_updating_one_stage_never_rewrites_the_other_baseline(self):
        """三份基线各管各的，尤其 objects 更新不得重冻旧两层。

        核心带基线的价值正在于「不因上层改动而变」；共用一个文件迟早会因为
        一次 `--update` 把三层一起重冻，那时逐位基线就不再证明任何事。
        """
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            vectors = root / "vectors"
            media = vectors / "a" / "encoded" / "a.m4a"
            media.parent.mkdir(parents=True)
            media.touch()
            core = vectors / "decode_baseline.json"
            aspx = vectors / "aspx_baseline.json"
            objects = vectors / "objects_baseline.json"
            frozen_core = json.dumps(
                {"comment": ["core"], "entries": {"a/a.m4a": shape("a")}},
                ensure_ascii=False,
                indent=2,
            )
            frozen_aspx = json.dumps(
                {"comment": ["aspx"], "entries": {"a/a.m4a": shape("b")}},
                ensure_ascii=False,
                indent=2,
            )
            core.write_text(frozen_core, encoding="utf-8")
            aspx.write_text(frozen_aspx, encoding="utf-8")
            objects.write_text(
                json.dumps({"comment": ["objects"], "entries": {}}, ensure_ascii=False),
                encoding="utf-8",
            )

            with (
                mock.patch.object(decode_check, "REPO_ROOT", root),
                mock.patch.object(decode_check, "VECTORS", vectors),
                mock.patch.object(
                    sys,
                    "argv",
                    ["decode_check.py", "--stage", "objects", "--update"],
                ),
                mock.patch.object(
                    decode_check, "decode", side_effect=lambda path, stage: shape("f")
                ),
                contextlib.redirect_stdout(io.StringIO()),
                contextlib.redirect_stderr(io.StringIO()),
            ):
                code = decode_check.main()

            self.assertEqual(code, 0)
            self.assertEqual(core.read_text(encoding="utf-8"), frozen_core)
            self.assertEqual(aspx.read_text(encoding="utf-8"), frozen_aspx)
            self.assertEqual(
                json.loads(objects.read_text(encoding="utf-8"))["entries"],
                {"a/a.m4a": shape("f")},
            )

    def test_a_failing_stage_does_not_hide_the_other(self):
        """一段失败不跳过后一段，否则先失败的那层会盖住后一层的回归。"""
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            vectors = root / "vectors"
            media = vectors / "a" / "encoded" / "a.m4a"
            media.parent.mkdir(parents=True)
            media.touch()
            for leaf, digest in (
                ("decode_baseline.json", "a"),
                ("aspx_baseline.json", "b"),
                ("objects_baseline.json", "a"),
            ):
                (vectors / leaf).write_text(
                    json.dumps(
                        {"comment": [], "entries": {"a/a.m4a": shape(digest)}},
                        ensure_ascii=False,
                    ),
                    encoding="utf-8",
                )

            # 三段都返回同一个摘要：core/objects 匹配，aspx 不匹配；第三段仍须执行。
            out, err = io.StringIO(), io.StringIO()
            with (
                mock.patch.object(decode_check, "REPO_ROOT", root),
                mock.patch.object(decode_check, "VECTORS", vectors),
                mock.patch.object(sys, "argv", ["decode_check.py"]),
                mock.patch.object(
                    decode_check, "decode", side_effect=lambda path, stage: shape("a")
                ),
                contextlib.redirect_stdout(out),
                contextlib.redirect_stderr(err),
            ):
                code = decode_check.main()

            self.assertEqual(code, 1)
            self.assertIn("核心带 PCM 基线通过", out.getvalue())
            self.assertIn("A-JOC 对象 PCM 基线通过", out.getvalue())
            self.assertIn("带宽扩展 PCM 基线未通过", err.getvalue())


    def test_each_committed_baseline_matches_the_comment_the_script_would_write(self):
        """入库的基线注释必须与脚本现在会写出的一致。

        两者漂移不会有任何东西报警，直到某次 `--update` 顺手把注释改掉——那次
        改动会混在真正的数值变动里一起提交。曾经就这样把一句陈旧的「参考解码器
        尚不可得」写进 SHARED_COMMENT，而磁盘上那份早已修正过。
        """
        for name, stage in decode_check.stages().items():
            with self.subTest(stage=name):
                self.assertTrue(stage.baseline.is_file(), f"{name} 基线应已入库")
                on_disk = json.loads(stage.baseline.read_text(encoding="utf-8"))
                self.assertEqual(on_disk["comment"], list(stage.comment))


    def test_unsupported_path_is_read_from_the_code_not_the_message(self):
        """按 code 认，不按文本认——文本会变，而按文本匹配会放行别的失败。"""
        diagnostic = {
            "schema": "macinac4.cli-diagnostic",
            "code": "unsupported.coding_path",
            "context": {"scene_path": "channel_based"},
        }
        self.assertEqual(
            decode_check.unsupported_path(json.dumps(diagnostic)), "channel_based"
        )

        other = dict(diagnostic, code="internal.invariant_failed")
        self.assertIsNone(decode_check.unsupported_path(json.dumps(other)))

        # 消息文本里出现同样的字样，但 code 不同，不得放行。
        self.assertIsNone(
            decode_check.unsupported_path(
                json.dumps(dict(other, message="unsupported.coding_path channel_based"))
            )
        )
        self.assertIsNone(decode_check.unsupported_path("不是 JSON 的一行"))


if __name__ == "__main__":
    unittest.main()
