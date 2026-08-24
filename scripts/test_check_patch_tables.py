#!/usr/bin/env python3
"""patch 表核对脚本的 fail-closed 回归测试。"""

from __future__ import annotations

import contextlib
import io
import sys
import unittest
from unittest import mock

from scripts import check_patch_tables


class CheckPatchTablesTests(unittest.TestCase):
    def run_empty_sweep(
        self,
        expected_count: int,
        expected_patch_digest: int,
        expected_limiter_digest: int,
        derive,
    ) -> int:
        with (
            mock.patch.object(sys, "argv", ["check_patch_tables.py", "--sweep"]),
            mock.patch.object(check_patch_tables, "read_anchors", return_value=[]),
            mock.patch.object(
                check_patch_tables,
                "read_sweep_digest",
                return_value=expected_patch_digest,
            ),
            mock.patch.object(
                check_patch_tables,
                "read_limiter_digest",
                return_value=expected_limiter_digest,
            ),
            mock.patch.object(
                check_patch_tables,
                "SPEC_CONFIGURATION_COUNT",
                expected_count,
            ),
            mock.patch.object(check_patch_tables, "derive", side_effect=derive),
            contextlib.redirect_stdout(io.StringIO()),
            contextlib.redirect_stderr(io.StringIO()),
        ):
            return check_patch_tables.main()

    def test_unsatisfiable_configuration_fails_the_sweep(self):
        def derive(*args):
            if args == (False, 0, 0, 0, False):
                raise check_patch_tables.Unsatisfiable("注入")
            return None

        result = self.run_empty_sweep(
            expected_count=0,
            expected_patch_digest=check_patch_tables.FNV64_OFFSET_BASIS,
            expected_limiter_digest=check_patch_tables.FNV64_OFFSET_BASIS,
            derive=derive,
        )
        self.assertEqual(result, 1)

    def test_configuration_count_reduction_fails_the_sweep(self):
        result = self.run_empty_sweep(
            expected_count=1,
            expected_patch_digest=check_patch_tables.FNV64_OFFSET_BASIS,
            expected_limiter_digest=check_patch_tables.FNV64_OFFSET_BASIS,
            derive=lambda *_args: None,
        )
        self.assertEqual(result, 1)

    def test_patch_digest_mismatch_fails_the_sweep(self):
        result = self.run_empty_sweep(
            expected_count=0,
            expected_patch_digest=check_patch_tables.FNV64_OFFSET_BASIS ^ 1,
            expected_limiter_digest=check_patch_tables.FNV64_OFFSET_BASIS,
            derive=lambda *_args: None,
        )
        self.assertEqual(result, 1)

    def test_limiter_digest_mismatch_fails_the_sweep(self):
        result = self.run_empty_sweep(
            expected_count=0,
            expected_patch_digest=check_patch_tables.FNV64_OFFSET_BASIS,
            expected_limiter_digest=check_patch_tables.FNV64_OFFSET_BASIS ^ 1,
            derive=lambda *_args: None,
        )
        self.assertEqual(result, 1)

    def test_empty_sweep_without_an_injected_failure_passes(self):
        result = self.run_empty_sweep(
            expected_count=0,
            expected_patch_digest=check_patch_tables.FNV64_OFFSET_BASIS,
            expected_limiter_digest=check_patch_tables.FNV64_OFFSET_BASIS,
            derive=lambda *_args: None,
        )
        self.assertEqual(result, 0)


if __name__ == "__main__":
    unittest.main()
