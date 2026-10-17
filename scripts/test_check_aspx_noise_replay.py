"""只用构造矩阵检查报告器，不加载许可表或参考库。"""
import json
from pathlib import Path
import tempfile
import unittest
import numpy as np
from scripts.check_aspx_noise_replay import accumulate, compare, empty_stats, MODES


class NoiseReplayTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.ref = self.root / "reference"
        self.local = self.root / "local"
        self.ref.mkdir()
        self.local.mkdir()
        self.path = self.root / "cases.json"
        table = np.stack([np.arange(1, 513), -np.arange(1, 513)], axis=1).astype("<f4")
        table.tofile(self.local / "table.f32")
        cases, refs, output = [], [], []
        for group, onset in enumerate([False, True]):
            c = dict(id=f"x{group}", group=group, factor=2, left_ats=1, right_ats=16, envelopes=1,
                     sbx=36, subbands=2, seed=511, level=1.0, onset=onset, reset=False, kind="origin")
            cases.append(c)
            index = (511 + np.arange(60) + 1) % 512
            samples = table[index].reshape(30, 2, 2)
            raw = np.zeros((40, 64, 2), dtype="<f4")
            raw[2:32, 36:38] = samples / 2**23 if not onset else 0
            raw.tofile(self.ref / f"x{group}.qmf.f32")
            refs.append(dict(c, noise_before=511, noise_after=59, finite=True, guards=True, save=0))
            for mode in MODES:
                extra = 2 if mode == "literal" else 0
                table[(index + extra) % 512].astype("<f4").tofile(self.local / f"x{group}_{mode}.f32")
                injected = 511 if mode == "literal" else 509
                output.append(f"x{group} {mode} 511 {injected} {59 + extra} 2 30")
        self.cases, self.refs, self.output = cases, refs, output
        self.save()

    def save(self):
        self.path.write_text(json.dumps(self.cases))
        (self.ref / "noise_probes.jsonl").write_text("\n".join(json.dumps(r) for r in [*self.refs, dict(complete=True, cases=len(self.refs))]) + "\n")
        (self.local / "results.txt").write_text("\n".join(self.output) + "\n")

    def test_finds_clock_shift_and_does_not_count_muting_as_table_evidence(self):
        r = compare(self.path, self.ref, self.local)
        self.assertEqual(r["informative_cases"], 1)
        self.assertEqual(r["reference_integer_model_mismatch_cases"], 0)
        self.assertEqual(r["reference_table_model_differing_components"], 0)
        self.assertEqual(r["total"]["literal"]["clock_mismatch_cases"], 2)
        self.assertEqual(r["total"]["literal"]["differing_components"], 120)
        self.assertEqual(r["total"]["qmf_origin"]["differing_components"], 0)

    def test_rejects_missing_or_duplicate_local_results(self):
        original = self.output
        for altered in [original[:-1], [*original, original[0]]]:
            self.output = altered
            self.save()
            with self.assertRaises(ValueError):
                compare(self.path, self.ref, self.local)

    def test_rejects_nonfinite_or_truncated_reference_matrix(self):
        f = self.ref / "x0.qmf.f32"
        original = f.read_bytes()
        a = np.frombuffer(original, "<f4").copy()
        a[0] = np.nan
        for data in [a.tobytes(), original[:-4]]:
            f.write_bytes(data)
            with self.assertRaises(ValueError):
                compare(self.path, self.ref, self.local)

    def test_rejects_unrelated_cursor_or_failed_guard(self):
        self.refs[0]["noise_before"] = 0
        self.save()
        with self.assertRaises(ValueError):
            compare(self.path, self.ref, self.local)
        self.refs[0]["noise_before"] = 511
        self.refs[0]["guards"] = False
        self.save()
        with self.assertRaises(ValueError):
            compare(self.path, self.ref, self.local)

    def test_rejects_identity_geometry_or_level_errors(self):
        original = dict(self.cases[0])
        for key, value in [("id", "../x"), ("factor", 3), ("subbands", 44), ("envelopes", True), ("level", float("inf"))]:
            self.cases[0] = dict(original, **{key: value})
            self.save()
            with self.subTest(key=key), self.assertRaises(ValueError):
                compare(self.path, self.ref, self.local)

    def test_rejects_reference_write_outside_the_active_region(self):
        f = self.ref / "x0.qmf.f32"
        a = np.fromfile(f, "<f4")
        a[0] = 1
        a.tofile(f)
        with self.assertRaises(ValueError):
            compare(self.path, self.ref, self.local)

    def test_reports_signed_zero_bits_separately_from_numeric_equality(self):
        stats = empty_stats()
        x = np.array([[0.0, -0.0]], dtype="<f4")
        y = np.array([[-0.0, 0.0]], dtype="<f4")
        accumulate(stats, x, y, False)
        self.assertEqual(stats["differing_components"], 0)
        self.assertEqual(stats["bit_differing_components"], 2)
        self.assertEqual(stats["signed_zero_only_differences"], 2)
        self.assertEqual(stats["max_ulp"], 0)


if __name__ == "__main__":
    unittest.main()
