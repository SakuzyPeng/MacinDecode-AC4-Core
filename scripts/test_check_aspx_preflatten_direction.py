import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import numpy as np

from scripts import check_aspx_preflatten_direction as check


class FormulaTests(unittest.TestCase):
    def test_reversing_the_gain_makes_division_flatten_a_cubic_envelope(self):
        case = dict(sbx=36, first=2, last=38)
        x = np.linspace(-1, 1, 36)
        db = 90 + 16*x - 4*x**2 + 3*x**3
        low = np.zeros((40, 64, 2), dtype="<f4")
        low[:, :36, 0] = np.sqrt(10**(db/10)) * .6
        low[:, :36, 1] = np.sqrt(10**(db/10)) * .8
        original = low[2:38, :36].copy()
        source = np.arange(36)
        old = check.gain_model(low, case, 1)
        reverse = check.gain_model(low, case, 1, reverse=True)
        spans = {}
        for mode, gain in [("literal", old), ("multiply", old), ("reverse_gain", reverse), ("bypass", old)]:
            out = check.apply_gain(original, gain, source, mode)
            power = np.mean(np.sum(out.astype(float)**2, axis=-1), axis=0)
            spans[mode] = float(np.ptp(10*np.log10(power)))
        self.assertLess(spans["reverse_gain"], 1e-3)
        self.assertLess(spans["multiply"], 1e-3)
        self.assertGreater(spans["literal"], 1.99 * spans["bypass"])

    def test_source_map_keeps_duplicate_source_bands_in_each_patch(self):
        c = dict(sbx=6, sbz=10, targets=[6, 8, 10], sources=[2, 3])
        np.testing.assert_array_equal(check.source_map(c), [2, 3, 3, 4])
        c["sources"] = [2, 5]
        with self.assertRaises(ValueError):
            check.source_map(c)

    def test_zero_or_nonfinite_gain_and_unknown_mode_are_rejected(self):
        off = np.ones((2, 3, 2), dtype="<f4")
        for value in [0, -1, np.nan, np.inf]:
            with self.assertRaises(ValueError):
                check.apply_gain(off, [1, value, 1], np.arange(3), "multiply")
        with self.assertRaises(ValueError):
            check.apply_gain(off, [1, 1, 1], np.arange(3), "unknown")

    def test_equal_unit_gain_modes_and_signed_zero_are_distinguished(self):
        off = np.zeros((2, 2, 2), dtype="<f4")
        off[0, 0] = [3, -4]
        for mode in check.MODES:
            check.validate_output(off, np.ones(2, dtype="<f4"), np.arange(2), off, mode)
        wrong = off.copy()
        wrong[1, 1, 0] = -0.0
        with self.assertRaises(ValueError):
            check.validate_output(off, np.ones(2, dtype="<f4"), np.arange(2), wrong, "bypass")


class CandidateBoundaryTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.data = self.root / "data"
        self.refs = self.root / "refs"
        self.data.mkdir()
        (self.refs / "r0").mkdir(parents=True)
        self.case = dict(id="a", kind="live", scope="unit", first=0, last=4,
                         sbx=8, sbz=12, targets=[8, 10, 12], sources=[2, 4])
        self.manifest = self.root / "manifest.json"
        self.manifest.write_text(json.dumps(dict(cases=[self.case], reference_runs=["r0"])))
        low = np.zeros((40, 64, 2), dtype="<f4")
        low[:, :8, 0] = [1, 2, 4, 8, 16, 32, 64, 128]
        low[:, :8, 1] = low[:, :8, 0] * .5
        low.tofile(self.data / "a.qmf.f32")
        mapping = check.source_map(self.case)
        off = np.zeros((4, 64, 2), dtype="<f4")
        off[:, 8:12] = low[:4, mapping]
        reference = np.zeros_like(low)
        reference[:4] = off * .25
        reference.tofile(self.refs / "r0/a.reference0.f32")
        for mode in check.MODES:
            directory = self.root / f"hf_{mode}"
            directory.mkdir()
            (directory / "state.bin").write_bytes(b"fixed state")
            for unit, scale in check.SCALES:
                gain = np.ones(64, dtype="<f4")
                gain[:8] = check.gain_model(low, self.case, scale, mode == "reverse_gain")
                out = np.zeros_like(off)
                out[:, 8:12] = check.apply_gain(off[:, 8:12], gain, mapping, mode)
                gain.tofile(directory / f"a.{unit}.gain.f32")
                out.tofile(directory / f"a.{unit}.preflat.f32")
                off.tofile(directory / f"a.{unit}.off.f32")
        # 只隔离新增候选边界；完整输入／参考门禁由既有 HF 检查器及其测试负责。
        self.gate = patch.object(check.hf, "compare", return_value=None)
        self.gate.start()
        self.addCleanup(self.gate.stop)

    def compare(self):
        return check.compare(self.manifest, self.data, self.refs, self.root)

    def test_valid_candidate_set_passes_and_calls_the_existing_gate(self):
        self.assertEqual(len(self.compare()["cases"]), 1)
        check.hf.compare.assert_called_once()

    def test_existing_gate_failure_is_not_suppressed(self):
        check.hf.compare.side_effect = ValueError("missing reference")
        with self.assertRaisesRegex(ValueError, "missing reference"):
            self.compare()

    def test_missing_candidate_output_is_rejected(self):
        (self.root / "hf_multiply/a.scaled.preflat.f32").unlink()
        with self.assertRaisesRegex(ValueError, "缺失"):
            self.compare()

    def test_changing_predictor_or_history_is_rejected(self):
        (self.root / "hf_bypass/state.bin").write_bytes(b"changed state")
        with self.assertRaisesRegex(ValueError, "状态"):
            self.compare()

    def test_divide_cannot_be_relabelled_multiply(self):
        old = self.root / "hf_literal/a.scaled.preflat.f32"
        (self.root / "hf_multiply/a.scaled.preflat.f32").write_bytes(old.read_bytes())
        with self.assertRaisesRegex(ValueError, "运算方向"):
            self.compare()

    def test_reverse_gain_cannot_keep_the_old_exponent(self):
        old = self.root / "hf_literal/a.raw.gain.f32"
        (self.root / "hf_reverse_gain/a.raw.gain.f32").write_bytes(old.read_bytes())
        with self.assertRaisesRegex(ValueError, "P85 增益"):
            self.compare()

    def test_output_outside_target_bands_is_rejected(self):
        path = self.root / "hf_bypass/a.raw.preflat.f32"
        values = np.fromfile(path, dtype="<f4").reshape(4, 64, 2)
        values[1, 1, 0] = 1
        values.tofile(path)
        with self.assertRaisesRegex(ValueError, "目标子带"):
            self.compare()

    def test_nonfinite_output_is_rejected(self):
        path = self.root / "hf_bypass/a.raw.preflat.f32"
        values = np.fromfile(path, dtype="<f4")
        values[16] = np.nan
        values.tofile(path)
        with self.assertRaises(ValueError):
            self.compare()


if __name__ == "__main__":
    unittest.main()
