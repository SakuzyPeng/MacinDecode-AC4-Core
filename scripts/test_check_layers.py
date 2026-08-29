#!/usr/bin/env python3
"""层依赖门禁的注入回归测试。

期望值一律在本文件内写死，**不引用 `check_layers` 的 `LAYERS` / `ALLOWED`**。
判据引用实现自己的常量会随实现一起漂移，改实现时判据跟着改，永远成立。
"""

from __future__ import annotations

import tempfile
import unittest
from pathlib import Path
from unittest import mock

from scripts import check_layers


# 与被测实现无共同来源的一份层映射：语法层一个模块、解码层一个模块、基础层一个。
TEST_LAYERS = {
    "meta": "syntax",
    "dsp": "decode",
    "bits": "primitive",
}


class StripTestItemsTests(unittest.TestCase):
    def test_multiline_attribute_between_marker_and_item_is_skipped(self):
        """`#[cfg(test)]` 与条目之间夹一个跨行属性时，整个测试模块仍须剥掉。

        这是实测踩到的缺陷：逐行匹配 `^#\\[` 会停在属性的续行上，实现随后走进
        「扫到下一个 `;` 为止」的单行条目分支，于是该 `;` **之后**的测试代码被
        整段当成生产代码留下来（`oamd/mod.rs` 就是这个形状）。

        夹缝在这里：越界引用必须排在模块内第一个 `;` 之后。放在它前面时，缺陷实现
        会顺手把它一起吞掉，得到与正确实现相同的输出，判据就恒过——这条测试第一版
        正是这样写的，退回缺陷实现照样全绿。
        """
        source = "\n".join(
            [
                "pub fn production() {}",
                "",
                "#[cfg(test)]",
                "#[expect(",
                "    clippy::indexing_slicing,",
                '    reason = "测试内的位串切片"',
                ")]",
                "mod tests {",
                "    let _first = 1;",
                "    use crate::dsp::Leaked;",
                "}",
                "",
                "pub fn after() {}",
            ]
        )
        stripped = check_layers.strip_test_items(source)
        self.assertNotIn("crate::dsp", stripped)
        self.assertIn("pub fn production", stripped)
        self.assertIn("pub fn after", stripped)

    def test_production_code_after_a_test_module_is_retained(self):
        source = "\n".join(
            [
                "#[cfg(test)]",
                "mod tests {",
                "    fn helper() {}",
                "}",
                "",
                "pub fn still_production() {}",
            ]
        )
        stripped = check_layers.strip_test_items(source)
        self.assertIn("still_production", stripped)
        self.assertNotIn("helper", stripped)

    def test_single_line_cfg_test_use_is_skipped(self):
        source = "\n".join(
            [
                "#[cfg(test)]",
                "use crate::dsp::Only;",
                "pub fn production() {}",
            ]
        )
        stripped = check_layers.strip_test_items(source)
        self.assertNotIn("crate::dsp", stripped)
        self.assertIn("production", stripped)


class ReferencedModulesTests(unittest.TestCase):
    def test_every_supported_spelling_is_seen(self):
        """五种写法都要被看见——任何一种漏掉都是可被绕过的盲区。"""
        cases = {
            "use crate::dsp::Thing;": "单行 use",
            "use crate::{bits::Reader, dsp::Thing};": "brace group",
            "pub type Alias = crate::dsp::Thing;": "内联类型别名",
            "fn f(e: crate::dsp::Thing) {}": "内联函数签名",
            "use crate::dsp::Thing as Renamed;": "use ... as",
        }
        for source, label in cases.items():
            with self.subTest(label):
                self.assertIn("dsp", check_layers.referenced_modules(source))

    def test_super_paths_are_ignored(self):
        self.assertEqual(check_layers.referenced_modules("use super::dsp::Thing;"), set())

    def test_multiline_brace_group_heads(self):
        source = "use crate::{\n    bits::Reader,\n    dsp::{Thing, Other},\n};"
        self.assertEqual(check_layers.referenced_modules(source), {"bits", "dsp"})


class GateTests(unittest.TestCase):
    def build_tree(self, files: dict[str, str]) -> Path:
        root = Path(self.enterContext(tempfile.TemporaryDirectory()))
        for name, body in files.items():
            path = root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(body, encoding="utf-8")
        return root

    def run_gate(self, files: dict[str, str], layers=None):
        root = self.build_tree(files)
        with (
            mock.patch.object(check_layers, "CRATE_SRC", root),
            mock.patch.object(check_layers, "LAYERS", layers or TEST_LAYERS),
        ):
            edges = check_layers.collect_edges()
            return check_layers.violations(edges)

    def test_clean_tree_reports_nothing(self):
        """不注入的基线：什么都不破坏时判据必须通过。"""
        bad = self.run_gate(
            {
                "meta.rs": "use crate::bits::Reader;\npub fn parse() {}",
                "dsp.rs": "use crate::meta::Parsed;\npub fn decode() {}",
                "bits.rs": "pub struct Reader;",
            }
        )
        self.assertEqual(bad, [])

    def test_syntax_referencing_decode_is_reported(self):
        bad = self.run_gate(
            {
                "meta.rs": "use crate::dsp::Thing;",
                "dsp.rs": "pub struct Thing;",
                "bits.rs": "pub struct Reader;",
            }
        )
        self.assertEqual([(s, t) for s, t, _ in bad], [("meta", "dsp")])

    def test_decode_referencing_syntax_stays_silent(self):
        """等价变体：合法方向的边不得报——报了说明锁死的是实现细节。"""
        bad = self.run_gate(
            {
                "meta.rs": "pub struct Parsed;",
                "dsp.rs": "use crate::meta::Parsed;",
                "bits.rs": "pub struct Reader;",
            }
        )
        self.assertEqual(bad, [])

    def test_primitive_referencing_syntax_is_reported(self):
        bad = self.run_gate(
            {
                "meta.rs": "pub struct Parsed;",
                "dsp.rs": "pub struct Thing;",
                "bits.rs": "use crate::meta::Parsed;",
            }
        )
        self.assertEqual([(s, t) for s, t, _ in bad], [("bits", "meta")])

    def test_violation_inside_test_module_is_ignored(self):
        bad = self.run_gate(
            {
                "meta.rs": "#[cfg(test)]\nmod tests {\n    use crate::dsp::Thing;\n}",
                "dsp.rs": "pub struct Thing;",
                "bits.rs": "pub struct Reader;",
            }
        )
        self.assertEqual(bad, [])

    def test_undeclared_module_fails_closed(self):
        with self.assertRaises(check_layers.LayerError):
            self.run_gate(
                {
                    "meta.rs": "pub struct Parsed;",
                    "dsp.rs": "pub struct Thing;",
                    "bits.rs": "pub struct Reader;",
                    "stray.rs": "pub struct New;",
                }
            )

    def test_empty_tree_fails_closed(self):
        """空扫描必须失败，否则源码树布局一变审计就恒过。"""
        with self.assertRaises(check_layers.LayerError):
            self.run_gate({})


class RealTreeTests(unittest.TestCase):
    def test_repository_currently_passes(self):
        """真实源码树上的基线。CI 的 quality 检查跑的是同一条路径。"""
        edges = check_layers.collect_edges()
        self.assertEqual(check_layers.violations(edges), [])
        self.assertGreater(len(edges), 50)


if __name__ == "__main__":
    unittest.main()
