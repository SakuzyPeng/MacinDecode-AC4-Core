"""离线验证差分判据能抓住错位、错轨、增益错误及无效输入。"""

from __future__ import annotations

import contextlib
import copy
import hashlib
import io
import json
import struct
import tempfile
import unittest
from pathlib import Path

import numpy as np

from scripts import compare_object_pcm as checker


def write_wave(path, samples, *, code=3):
    samples = np.asarray(samples, dtype="<f4")
    channels = samples.shape[1]
    fmt = struct.pack("<HHIIHH", code, channels, 48000, 48000 * channels * 4, channels * 4, 32)
    data = samples.tobytes()
    body = b"WAVEfmt " + struct.pack("<I", len(fmt)) + fmt + b"data" + struct.pack("<I", len(data)) + data
    path.write_bytes(b"RIFF" + struct.pack("<I", len(body)) + body)


def file_spec(path):
    return {"path": path.name, "sha256": hashlib.sha256(path.read_bytes()).hexdigest()}


def tracks():
    return [{"output_channel": 0, "role": "lfe", "substream": 2},
            {"output_channel": 1, "role": "ajoc_object", "ajoc_object": 0, "substream": 2}]


def fixture(root, local, reference, *, lag=0, scale=1 / 32768):
    local = np.asarray(local, dtype="<f4")
    reference = np.asarray(reference, dtype="<f4")
    source = root / "input.ac4"
    source.write_bytes(b"controlled fixture, not real AC-4")
    wave = root / "local.wav"
    write_wave(wave, local)
    raw = root / "reference.raw"
    raw.write_bytes(reference.tobytes())
    export = root / "local.json"
    export.write_text(json.dumps({
        "schema": "macinac4.cli-result", "version": 1, "command": "export-objects-pcm",
        "result": {"scale": "±32768", "channel_order": "ajoc_objects_with_lfe_reinserted",
                   "audio": {"sample_rate_hz": 48000, "channels": 2, "frames": len(local),
                             "bit_depth": 32, "format": "wave_extensible_ieee_float32"},
                   "tracks": tracks()},
    }))
    return {
        "version": 1, "case": "controlled_test", "input": file_spec(source),
        "local_pcm": file_spec(wave), "local_export": file_spec(export),
        "reference_pcm": {**file_spec(raw), "format": "interleaved_f32le", "sample_rate_hz": 48000,
                          "channels": 2, "tracks": tracks(), "runtime": "independent synthetic fixture",
                          "identity_evidence": "fixture 的轨道定义与两路不同脉冲",
                          "processing": {"gain": 1}},
        "alignment": {"reference_lag_samples": lag, "evidence": "fixture 的显式零填充"},
        "local_scale": scale, "scale_evidence": "fixture 使用整数幅度 32768 对应 1.0",
    }


def signal():
    # 不同位置、不同幅度和非对称脉冲使错轨、错位、极性错误都可观察。
    x = np.zeros((48, 2), dtype="<f4")
    x[[3, 11, 18], 0] = [1 / 8, -1 / 4, 3 / 8]
    x[[7, 14, 25], 1] = [-1 / 2, 1 / 16, 1 / 4]
    return x


class ObjectPcmCompareTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)

    def compare(self, manifest, tolerance=0):
        return checker.compare_manifest(manifest, self.root, tolerance)

    def test_exact_match_passes_without_fitting(self):
        x = signal()
        report = self.compare(fixture(self.root, x * 32768, x))
        self.assertEqual(report["verdict"], "pass")
        self.assertFalse(report["scale"]["gain_fit_applied"])
        self.assertEqual(report["max_abs_error"], 0)
        self.assertEqual(report["excited_tracks"], 2)

    def test_gain_error_fails_even_when_fitted_residual_is_zero(self):
        x = signal()
        report = self.compare(fixture(self.root, x * 65536, x))
        self.assertEqual(report["verdict"], "fail")
        self.assertEqual(report["tracks"][1]["max_abs_error"], 0.5)
        fitted = report["tracks"][1]["gain_fit_diagnostic_only"]
        self.assertEqual(fitted["reference_over_local"], 0.5)
        self.assertTrue(fitted["zero_residual"])

    def test_polarity_inversion_is_visible_and_not_corrected(self):
        x = signal()
        report = self.compare(fixture(self.root, x * 32768, -x))
        self.assertEqual(report["verdict"], "fail")
        self.assertEqual(report["tracks"][1]["correlation"], -1)
        self.assertTrue(report["tracks"][1]["gain_fit_diagnostic_only"]["inverted_polarity"])

    def test_threshold_is_inclusive_and_has_two_sided_boundary(self):
        x = signal()
        manifest = fixture(self.root, x * 65536, x)
        self.assertEqual(self.compare(manifest, 0.5)["verdict"], "pass")
        self.assertEqual(self.compare(manifest, np.nextafter(0.5, 0).item())["verdict"], "fail")

    def test_signed_delay_and_uncompared_ranges_are_explicit(self):
        x = signal()
        delayed = np.pad(x, ((5, 0), (0, 0)))
        for lag, a, b in [(5, x, delayed), (-5, delayed, x)]:
            with self.subTest(lag=lag):
                manifest = fixture(self.root, a * 32768, b, lag=lag)
                report = self.compare(manifest)
                self.assertEqual(report["verdict"], "pass")
                self.assertEqual(report["alignment"]["compared_frames"], 48)
                self.assertEqual(report["alignment"]["local_start"], max(0, -lag))
                self.assertEqual(report["alignment"]["reference_start"], max(0, lag))
                manifest["alignment"]["reference_lag_samples"] = lag + 1
                self.assertEqual(self.compare(manifest)["verdict"], "fail")

    def test_permuted_pcm_needs_matching_identity_map(self):
        x = signal()
        manifest = fixture(self.root, x * 32768, x[:, ::-1])
        self.assertEqual(self.compare(manifest)["verdict"], "fail")
        for track in manifest["reference_pcm"]["tracks"]:
            track["output_channel"] = 1 - track["output_channel"]
        report = self.compare(manifest)
        self.assertEqual(report["verdict"], "pass")
        self.assertEqual([t["reference_channel"] for t in report["tracks"]], [1, 0])

    def test_duplicate_or_missing_identity_is_not_silently_paired(self):
        x = signal()
        original = fixture(self.root, x * 32768, x)
        for mutation in ["duplicate_output", "duplicate_identity", "missing_identity"]:
            manifest = copy.deepcopy(original)
            track = manifest["reference_pcm"]["tracks"][1]
            if mutation == "duplicate_output":
                track["output_channel"] = 0
            elif mutation == "duplicate_identity":
                track["role"] = "lfe"
            else:
                track["ajoc_object"] = 1
            with self.subTest(mutation=mutation), self.assertRaises(ValueError):
                self.compare(manifest)

    def test_all_silence_is_insufficient_and_one_sided_signal_fails(self):
        x = np.zeros_like(signal())
        report = self.compare(fixture(self.root, x, x))
        self.assertEqual(report["verdict"], "insufficient_excitation")
        self.assertEqual(report["tracks"][0]["activity"], "both_zero")
        for a, b in [(x, signal()), (signal() * 32768, x)]:
            report = self.compare(fixture(self.root, a, b))
            self.assertEqual(report["verdict"], "fail")

    def test_nonfinite_samples_outside_overlap_are_rejected(self):
        x = signal()
        for value in [float("nan"), float("inf"), -float("inf")]:
            y = np.pad(x, ((5, 0), (0, 0)))
            y[0, 0] = value
            manifest = fixture(self.root, x * 32768, y, lag=5)
            with self.subTest(value=value), self.assertRaisesRegex(ValueError, "非有限"):
                self.compare(manifest)

    def test_hash_and_export_shape_are_checked(self):
        x = signal()
        manifest = fixture(self.root, x * 32768, x)
        (self.root / "input.ac4").write_bytes(b"different input")
        with self.assertRaisesRegex(ValueError, "SHA-256"):
            self.compare(manifest)
        manifest = fixture(self.root, x * 32768, x)
        p = self.root / "local.json"
        export = json.loads(p.read_text())
        export["result"]["audio"]["frames"] += 1
        p.write_text(json.dumps(export))
        manifest["local_export"] = file_spec(p)
        with self.assertRaisesRegex(ValueError, "形状"):
            self.compare(manifest)

    def test_no_overlap_or_invalid_calibration_fails(self):
        x = signal()
        original = fixture(self.root, x * 32768, x)
        for lag in [48, -48, True, 1.5]:
            manifest = copy.deepcopy(original)
            manifest["alignment"]["reference_lag_samples"] = lag
            with self.subTest(lag=lag), self.assertRaises(ValueError):
                self.compare(manifest)
        for scale in [0, -1, float("nan"), float("inf"), True]:
            manifest = copy.deepcopy(original)
            manifest["local_scale"] = scale
            with self.subTest(scale=scale), self.assertRaises(ValueError):
                self.compare(manifest)
        for tolerance in [-1, float("nan"), float("inf")]:
            with self.subTest(tolerance=tolerance), self.assertRaises(ValueError):
                self.compare(original, tolerance)

    def test_reference_truncation_and_integer_wave_are_rejected(self):
        x = signal()
        manifest = fixture(self.root, x * 32768, x)
        p = self.root / "reference.raw"
        p.write_bytes(p.read_bytes()[:-1])
        manifest["reference_pcm"].update(file_spec(p))
        with self.assertRaisesRegex(ValueError, "完整采样帧"):
            self.compare(manifest)
        manifest = fixture(self.root, x * 32768, x)
        p = self.root / "local.wav"
        write_wave(p, x, code=1)
        manifest["local_pcm"] = file_spec(p)
        with self.assertRaisesRegex(ValueError, "IEEE Float32"):
            self.compare(manifest)

    def test_extensible_wave_and_bounded_chunks(self):
        x = signal()
        p = self.root / "extended.wav"
        # 标准 IEEE Float 子格式 GUID，非从被测实现常量复制。
        fmt = struct.pack("<HHIIHHHHI", 0xFFFE, 2, 48000, 384000, 8, 32, 22, 32, 0)
        fmt += bytes.fromhex("0300000000001000800000aa00389b71")
        pcm = x.tobytes()
        body = b"WAVEfmt " + struct.pack("<I", 40) + fmt + b"data" + struct.pack("<I", len(pcm)) + pcm
        valid = b"RIFF" + struct.pack("<I", len(body)) + body
        p.write_bytes(valid)
        actual, rate = checker.float_wave(p)
        np.testing.assert_array_equal(actual, x)
        self.assertEqual(rate, 48000)
        del actual
        variants = [valid[:-1], valid[:64] + struct.pack("<I", len(pcm) + 8) + valid[68:],
                    valid[:44] + b"\x01" + valid[45:]]
        for index, invalid in enumerate(variants):
            p.write_bytes(invalid)
            with self.subTest(index=index), self.assertRaises(ValueError):
                checker.float_wave(p)

    def test_invalid_json_types_fail_with_structured_exit(self):
        p = self.root / "manifest.json"
        for value in [[], None, {"version": True}, {"version": 1.0}]:
            p.write_text(json.dumps(value))
            with self.subTest(value=value), contextlib.redirect_stderr(io.StringIO()):
                self.assertEqual(checker.main([str(p)]), 2)

    def test_band_exclusion_never_hides_time_domain_error(self):
        n = np.arange(8192)
        x = np.column_stack([0.25 * np.sin(2 * np.pi * 1000 * n / 48000), np.zeros(len(n))])
        y = x.copy()
        y[:, 0] += 0.125 * np.sin(2 * np.pi * 21000 * n / 48000)
        manifest = fixture(self.root, x * 32768, y)
        report = self.compare(manifest, 0.01)
        self.assertEqual(report["verdict"], "fail")
        bands = report["tracks"][0]["bands"]
        self.assertLess(bands[0]["rmse"], 1e-6)
        self.assertGreater(bands[2]["rmse"], 0.08)
        self.assertFalse(bands[2]["usable_as_drp_reference"])

    def test_observation_is_not_a_pass_and_cli_never_overwrites(self):
        x = signal()
        manifest = fixture(self.root, x * 65536, x)
        p = self.root / "manifest.json"
        p.write_text(json.dumps(manifest))
        output = self.root / "report.json"
        self.assertEqual(checker.main([str(p), "--output", str(output)]), 0)
        report = json.loads(output.read_text())
        self.assertEqual(report["verdict"], "observation_only")
        original = output.read_bytes()
        with contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(checker.main([str(p), "--output", str(output)]), 2)
        self.assertEqual(output.read_bytes(), original)
        with contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(checker.main([str(p), "--max-abs-error", "0"]), 1)


if __name__ == "__main__":
    unittest.main()
