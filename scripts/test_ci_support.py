"""CI 必需依赖、MSRV 和非空测试集的失败路径。"""
from pathlib import Path
import tempfile
import unittest

from scripts import ci_support as ci


class GateTests(unittest.TestCase):
    def test_all_dependencies_must_succeed(self):
        jobs = ["audio_checks", "spec_audit", "consistency"]
        needs = {job: {"result": "success"} for job in jobs}
        ci.require_success(needs, jobs)
        for status in ("failure", "cancelled", "skipped", None):
            with self.subTest(status=status):
                failed = {**needs, "consistency": {"result": status}}
                with self.assertRaises(ValueError):
                    ci.require_success(failed, jobs)

    def test_missing_or_empty_dependencies_fail(self):
        for needs, jobs in [({}, []), ({}, ["audio_checks"]),
                            ({"unexpected": {"result": "success"}}, ["audio_checks"])]:
            with self.subTest(needs=needs), self.assertRaises(ValueError):
                ci.require_success(needs, jobs)

    def test_missing_empty_and_duplicate_test_inventory_fail(self):
        lines = [f"{name}: test" for name in sorted(ci.CONSISTENCY_TESTS)]
        ci.check_test_inventory("\n".join(lines))
        for lines in ([], lines[:-1], lines + [lines[0]]):
            with self.assertRaises(ValueError):
                ci.check_test_inventory("\n".join(lines))

    def test_success_exit_with_ignored_or_filtered_tests_fails(self):
        count = len(ci.CONSISTENCY_TESTS)
        ci.check_test_result(f"test result: ok. {count} passed; 0 failed; 0 ignored; 0 measured; 0 filtered out;")
        for output in ("", "running 0 tests", f"test result: ok. {count-1} passed; 0 failed; 1 ignored; 0 measured; 0 filtered out;"):
            with self.assertRaises(ValueError):
                ci.check_test_result(output)


class ToolchainTests(unittest.TestCase):
    def test_pinned_msrv_actual_compiler_and_member_inheritance(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            (root / "member").mkdir()
            (root / "Cargo.toml").write_text('[workspace]\nmembers=["member"]\n[workspace.package]\nrust-version="1.98"\n')
            (root / "rust-toolchain.toml").write_text('[toolchain]\nchannel="1.98.0"\n')
            package = root / "member/Cargo.toml"
            package.write_text('[package]\nrust-version.workspace=true\n')
            ci.check_toolchain(root, "rustc 1.98.0 (test)")
            with self.assertRaises(ValueError):
                ci.check_toolchain(root, "rustc 1.99.0 (test)")
            for declaration in ('rust-version="1.97"', ''):
                package.write_text('[package]\n' + declaration)
                with self.assertRaises(ValueError):
                    ci.check_toolchain(root, "rustc 1.98.0 (test)")
            (root / "rust-toolchain.toml").write_text('[toolchain]\nchannel="stable"\n')
            with self.assertRaises(ValueError):
                ci.check_toolchain(root, "rustc 1.98.0 (test)")


class ReleaseReuseTests(unittest.TestCase):
    def test_missing_or_rebuilt_production_artifacts_fail(self):
        artifacts = [{"reason": "compiler-artifact", "target": {"name": name}, "fresh": True}
                     for name in ("macindecode_ac4_decode", "macinac4")]
        ci.check_reused_artifacts(artifacts)
        for missing in ([], artifacts[:1], artifacts[1:]):
            with self.assertRaises(ValueError):
                ci.check_reused_artifacts(missing)
        for artifact in artifacts:
            artifact["fresh"] = False
            with self.assertRaises(ValueError):
                ci.check_reused_artifacts(artifacts)
            artifact["fresh"] = True


if __name__ == "__main__":
    unittest.main()
