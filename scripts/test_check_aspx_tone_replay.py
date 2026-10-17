"""构造小矩阵验证检查器的身份、包络、状态和数值检测，不加载参考库。"""
import json
from pathlib import Path
import tempfile
import unittest
import numpy as np
from scripts.check_aspx_tone_replay import MODES, add_stats, compare, load_cases, stats


class ToneReplayTests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        self.ref, self.local = self.root / "ref", self.root / "local"
        self.ref.mkdir()
        self.local.mkdir()
        self.path = self.root / "cases.txt"
        self.lines = [
            "a 0 0 2 1 16 1 1 6 0 3 0 36 2 0 0 0 1 0 0",
            "b 1 1 1 0 16 4 1 6 0 3 0 36 2 0 1 0 1 1 0",
        ]
        self.path.write_text("\n".join(self.lines)+"\n")
        self.refs, self.outputs = [], []
        phases = np.array([[[1, 0], [1, 0]], [[0, 1], [0, -1]], [[-1, 0], [-1, 0]], [[0, -1], [0, 1]]], dtype="<f4")
        for c in load_cases(self.path):
            a, b = c["left_ats"]*c["factor"], c["right_ats"]*c["factor"]
            length = b-a
            amplitude = np.ones(length, dtype="<f4")
            if c["profile"]:
                amplitude[:4] = 0
                amplitude[8:12] = 0
            x = phases[np.arange(length) % 4] * amplitude[:, None, None]
            raw = np.zeros((40, 64, 2), dtype="<f4")
            raw[a:b, 36:38] = x / 2**23
            raw.tofile(self.ref / f"{c['id']}.qmf.f32")
            checks = []
            for env in range(c["envelopes"]):
                start = a+env*length//c["envelopes"]
                stop = a+(env+1)*length//c["envelopes"]
                checks.append(dict(env=env, first=start, last=stop, present=int(amplitude[start-a] > 0), before=(start-a) % 4, after=(stop-a) % 4))
            self.refs.append(dict(c, sine_before=0, sine_after=length % 4, noise_before=0, noise_after=length*2 % 512, envelope_checks=checks, finite=True, guards=True, save=0))
            for mode in MODES:
                origin, zero = "qmf_origin" in mode, "initial_zero" in mode
                base = (3 if zero else 0) if c["first_frame"] else 3
                if origin and not zero:
                    base = 3
                injected = (base + (0 if origin else c["left_ats"]*(c["factor"]-1))) % 4
                first = int(c["first_frame"] and origin and not zero)
                offset = 1 if first else injected+1
                y = phases[(np.arange(length)+offset) % 4] * amplitude[:, None, None]
                y.tofile(self.local / f"{c['id']}_{mode}.f32")
                end = (offset+length-1) % 4
                self.outputs.append(f"{c['id']} {mode} 3 {injected} {end} {(end+1)%4} 2 {length} {first}")
        self.save()

    def save(self):
        self.path.write_text("\n".join(self.lines)+"\n")
        (self.ref / "tone_probes.jsonl").write_text("\n".join(json.dumps(x) for x in [*self.refs, dict(complete=True, cases=len(self.refs))])+"\n")
        (self.local / "results.txt").write_text("\n".join(self.outputs)+"\n")

    def compare(self):
        return compare(self.path, self.ref, self.local)

    def test_separates_origin_initialization_and_cursor_encoding(self):
        r = self.compare()
        self.assertEqual(r["reference_model_differing_components"], 0)
        self.assertEqual(r["reference_clock_mismatch_envelopes"], 0)
        self.assertEqual([r["total"][m]["clock_mismatch_cases"] for m in MODES], [2, 1, 1, 0])
        self.assertEqual(r["total"][MODES[-1]]["differing_components"], 0)
        self.assertGreater(r["total"]["literal"]["max_abs_error"], 0)
        self.assertEqual(r["direct_production_mode"], "qmf_origin")
        self.assertEqual(r["envelopes"], 5)

    def test_missing_duplicate_and_reordered_reference_calls_are_rejected(self):
        original = self.refs
        for rows in [original[:-1], [*original, original[0]], original[::-1]]:
            self.refs = rows
            self.save()
            with self.assertRaises(ValueError):
                self.compare()

    def test_missing_duplicate_or_shifted_envelope_is_rejected(self):
        original = self.refs[1]["envelope_checks"]
        for rows in [original[:-1], [original[0], *original[:-1]], [dict(original[0], first=1), *original[1:]]]:
            self.refs[1]["envelope_checks"] = rows
            self.save()
            with self.assertRaises(ValueError):
                self.compare()

    def test_missing_mode_and_bad_state_conversion_are_rejected(self):
        original = self.outputs
        bad = original[0].split()
        bad[5] = str((int(bad[5])+1) % 4)
        for rows in [original[:-1], [*original, original[0]], [" ".join(bad), *original[1:]]]:
            self.outputs = rows
            self.save()
            with self.assertRaises(ValueError):
                self.compare()

    def test_rejects_nonfinite_truncated_and_outside_samples(self):
        path = self.ref / "a.qmf.f32"
        original = path.read_bytes()
        for bad in ("nan", "outside", "truncated"):
            data = np.frombuffer(original, dtype="<f4").copy()
            if bad == "truncated":
                path.write_bytes(original[:-4])
            else:
                data[0] = np.nan if bad == "nan" else 1
                data.tofile(path)
            with self.assertRaises(ValueError):
                self.compare()

    def test_detects_numeric_difference_without_fitting_it_away(self):
        path = self.local / "a_qmf_origin_initial_zero.f32"
        data = np.fromfile(path, dtype="<f4")
        data[0] = 2
        data.tofile(path)
        r = self.compare()["total"][MODES[-1]]
        self.assertEqual(r["differing_components"], 1)
        self.assertEqual(r["max_abs_error"], 1)
        self.assertEqual(r["max_relative_error_nonzero_reference"], 1)
        self.assertEqual(r["clock_mismatch_cases"], 0)

    def test_signed_zero_is_not_counted_as_numeric_difference(self):
        s = stats()
        add_stats(s, np.array([0, 1], dtype="<f4"), np.array([-0., 1], dtype="<f4"), False)
        self.assertEqual(s["signed_zero_only_differences"], 1)
        self.assertEqual(s["differing_components"], 0)
        self.assertEqual(s["max_ulp"], 0)

    def test_bad_manifest_and_noncontiguous_group_are_rejected(self):
        original = self.lines
        for lines in [[], [original[0], original[0]], [original[0].replace(" 2 0 0 0 1 0 0", " 2 4 0 0 1 0 0")], [original[0].replace(" 0 1 0 0", " 0 NaN 0 0")], [*original, original[0].replace("a ", "c ")]]:
            self.lines = lines
            self.save()
            with self.assertRaises(ValueError):
                self.compare()

    def test_reference_clock_error_is_reported(self):
        self.refs[0]["envelope_checks"][0]["after"] = 1
        self.refs[0]["sine_after"] = 1
        self.save()
        self.assertEqual(self.compare()["reference_clock_mismatch_envelopes"], 1)

    def test_reads_historical_replay_without_mislabeling_its_production_path(self):
        cases = {c["id"]: c for c in load_cases(self.path)}
        aliases = {"literal": "production", "literal_initial_zero": "initial_zero"}
        output = []
        for line in self.outputs:
            parts = line.split()
            name, mode = parts[:2]
            c = cases[name]
            origin, zero = "qmf_origin" in mode, "initial_zero" in mode
            base = (3 if zero else 0) if c["first_frame"] else 3
            parts[3] = str((base-(c["left_ats"]*(c["factor"]-1) if origin else 0)) % 4)
            parts[8] = str(int(c["first_frame"] and not origin and not zero))
            parts[1] = aliases.get(mode, mode)
            (self.local / f"{name}_{parts[1]}.f32").write_bytes((self.local / f"{name}_{mode}.f32").read_bytes())
            output.append(" ".join(parts))
        self.outputs = output
        self.save()
        r = self.compare()
        self.assertEqual(r["local_production_origin"], "literal")
        self.assertEqual(r["direct_production_mode"], "literal")
        self.assertEqual([r["total"][m]["clock_mismatch_cases"] for m in MODES], [2, 1, 1, 0])


if __name__ == "__main__":
    unittest.main()
