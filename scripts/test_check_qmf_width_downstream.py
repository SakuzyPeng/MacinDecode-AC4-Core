"""无媒体检查器回归：先通过完整边界，再逐个破坏实际消费关系。"""
from copy import deepcopy
from pathlib import Path
import tempfile
import unittest

import numpy as np

from scripts import check_qmf_width_downstream as check


def case():
    return dict(id="music100c0i1", scope="music100", call=0, interpolation=1,
                master=1, start=6, stop=1, noise=3, xover=0,
                sbx=36, sbz=56, sources=[28, 24], targets=[36, 44, 56],
                signal_borders=[0, 6, 8, 12, 18], first=0, last=36,
                envelopes=[[0, 12], [12, 16], [16, 24], [24, 36]],
                freq_res=[1, 0, 0, 1], high=[36, 38, 40, 42, 44, 47, 50, 53, 56],
                low=[36, 40, 44, 50, 56], chirp=[.75, .75],
                controls=[dict(onset=e == 1, target=[9.0]*20, noise_ratio=[.5]*20,
                               sine_index=[False]*20, sine_area=[False]*20,
                               noise_level=[np.sqrt(3)]*20, sine_level=[0.0]*20,
                               limiter=[36, 44, 56]) for e in range(4)])


def fixture(root, c, data, table, step=0, previous=None, equivalent=False):
    stream = np.concatenate((data, data[32:64]))
    pos = 26+32*step
    first, last = (c["last"]-32, 32) if step else (c["first"], c["last"])
    controls = [dict(c["controls"][-1], onset=False)] if step else c["controls"]
    borders = [[first, last]] if step else c["envelopes"]
    freq = [c["freq_res"][-1]] if step else c["freq_res"]
    low = stream[pos:pos+38]
    pre = np.ones(64, "<f4")
    pre[:36] = check.preflatten_model(low, first, last, 36)
    alpha = np.zeros((64, 4), "<f4")
    alpha[:36] = check.prediction_model(stream[pos-4:pos+38, :36].copy().tobytes(), 36)
    hf = np.zeros((last-first, 64, 2), "<f4")
    hf[:, 36:56] = check.hf_model(stream[pos-4:pos+38], first, last, c, alpha, pre, True)
    if equivalent:
        # 物理等价的 f64 中间舍入；不是把不正确输入放宽为任意值。
        hf = hf.astype(float).astype("<f4")
    levels, noise, y = [], np.zeros_like(hf), np.zeros((last, 64, 2), "<f4")
    before = [0, 0] if previous is None else previous["state"][2:4]
    if previous is not None:
        y[:first] = previous["y"][32:]
    for e, (a, b) in enumerate(borders):
        s = slice(a-first, b-first)
        groups = c["high"] if freq[e] else c["low"]
        energy = check.estimate_power(hf[s, 36:56], [k-36 for k in groups], c["interpolation"])
        g = check.gain_model(energy, controls[e], 2)[0]
        levels.append(np.column_stack((energy, g)))
        raster = (int(before[0])+np.arange((a-first)*20, (b-first)*20)+1) % 512
        noise[s, 36:56] = table[raster].reshape(b-a, 20, 2)*g[:, 1][None, :, None]
        y[a:b, 36:56] = hf[s, 36:56]*g[:, 0][None, :, None] + noise[s, 36:56]*g[:, 4][None, :, None]
    state = np.array([*before, (before[0]+(last-first)*20) % 512,
                      (before[1]+last-first) % 4, last-32], "<f4")
    outputs = dict(alpha=alpha, pre=pre, hf=hf, levels=np.asarray(levels),
                   history=low[28:32], noise=noise, tone=np.zeros_like(hf), y=y,
                   state=state, merged=low[:32]+y[:32], pcm=np.zeros(2048, "<f4"))
    for name, value in outputs.items():
        np.asarray(value, "<f4").tofile(root/f"fixtures{step}.{name}.f32")
    return outputs


class WidthChainTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.c = case()
        t, b = np.arange(64)[:, None], np.arange(36)[None, :]
        z = (3+.02*b)*np.exp(1j*(.27*t+.01*b)) + (.15+.001*b)*np.exp(.91j*t)
        self.data = np.zeros((64, 64, 2), "<f4")
        self.data[:, :36] = check.components(z)
        self.table = np.column_stack((np.cos(np.arange(512)), np.sin(np.arange(512)))).astype("<f4")
        self.outputs = fixture(self.root, self.c, self.data, self.table)

    def inspect(self, step=0, previous=None):
        return check.inspect_step(self.c, self.data, self.root, "fixture", step,
                                  True, 2, self.table, previous)

    def corrupt(self, name, mutate):
        x = self.outputs[name].copy()
        mutate(x)
        np.asarray(x, "<f4").tofile(self.root/f"fixtures0.{name}.f32")
        with self.assertRaises(ValueError):
            self.inspect()

    def test_baseline_and_constructed_successor(self):
        one = self.inspect()
        fixture(self.root, self.c, self.data, self.table, 1, one)
        two = self.inspect(1, one)
        np.testing.assert_array_equal(one["y"][32:], two["y"][:4])

    def test_equivalent_lossless_intermediate(self):
        baseline = self.inspect()
        fixture(self.root, self.c, self.data, self.table, equivalent=True)
        np.testing.assert_array_equal(baseline["active_y"], self.inspect()["active_y"])

    def test_swapped_source_band_is_detected(self):
        self.corrupt("hf", lambda x: x.__setitem__((slice(None), 36), x[:, 38]))

    def test_stale_estimate_is_detected(self):
        self.corrupt("levels", lambda x: x.__setitem__((1, slice(None), 0), x[0, :, 0]*2))

    def test_wrong_noise_clock_is_detected(self):
        self.corrupt("noise", lambda x: x.__setitem__((0, 36, 0), x[1, 36, 0]+1))

    def test_reset_history_is_detected(self):
        self.corrupt("history", lambda x: x.fill(0))

    def test_wrong_filter_is_detected(self):
        self.corrupt("alpha", lambda x: x.fill(0))

    def test_old_preflatten_sign_is_detected(self):
        self.corrupt("pre", lambda x: x.__setitem__(slice(0, 36), 1/x[:36]))

    def test_nonfinite_is_detected(self):
        self.corrupt("levels", lambda x: x.__setitem__((0, 0, 0), np.nan))

    def test_missing_envelope_is_detected(self):
        self.outputs["levels"][:-1].astype("<f4").tofile(self.root/"fixtures0.levels.f32")
        with self.assertRaises(ValueError):
            self.inspect()

    def test_lost_tail_is_detected(self):
        one = self.inspect()
        output = fixture(self.root, self.c, self.data, self.table, 1, one)
        self.assertTrue(np.any(output["y"][:4]))
        output["y"][:4] = 0
        output["y"].tofile(self.root/"fixtures1.y.f32")
        with self.assertRaises(ValueError):
            self.inspect(1, one)

    def test_zero_and_floor_are_not_silently_dropped(self):
        for e in [0, 2**-30, 1, 1e6]:
            for mode in range(3):
                g, branches, budgets = check.gain_model(np.full(20, e), self.c["controls"][0], mode)
                self.assertTrue(np.isfinite(g).all())
                self.assertEqual(bool(branches["below_unit_floor"].all()), e < 1)
                self.assertEqual(len(budgets), 2)

    def test_identical_comparisons_report_zero_error(self):
        x = self.outputs["hf"]
        self.assertEqual(check.metrics(x, x.copy())["error_energy"], 0)

    def test_manifest_rejects_missing_cases(self):
        m = dict(schema="macinac4.qmf-width-downstream", version=1, variants=check.VARIANTS,
                 common_to_local="Q_common / R64 * 2^23", initial_cursors=[0, 0], cases=[self.c])
        with self.assertRaises(ValueError):
            check.validate_manifest(m)
        m = deepcopy(m)
        m["cases"][0]["envelopes"] = [[0, 36]]
        with self.assertRaises(ValueError):
            check.validate_manifest(m)

    def test_complete_manifest_and_order_equivalence(self):
        cases = []
        for scope in ("tone", "music100", "music33"):
            for call in range(9):
                for interp in range(2):
                    c = case()
                    c.update(scope=scope, call=call, interpolation=interp,
                             id=f"{scope}c{call}i{interp}")
                    if scope == "tone":
                        c.update(stop=0, sbz=62, sources=[28, 18], targets=[36, 44, 62],
                                 low=[36, 44, 62], high=[36, 40, 44, 50, 56, 62])
                        for r in c["controls"]:
                            r["limiter"] = [36, 44, 62]
                            for k in ("target", "noise_ratio", "sine_index", "sine_area", "noise_level", "sine_level"):
                                r[k] = [r[k][0]]*26
                    cases.append(c)
        m = dict(schema="macinac4.qmf-width-downstream", version=1, variants=check.VARIANTS,
                 common_to_local="Q_common / R64 * 2^23", initial_cursors=[0, 0], cases=cases)
        check.validate_manifest(m)
        m["cases"].reverse()
        check.validate_manifest(m)
        m["cases"].append(deepcopy(m["cases"][0]))
        with self.assertRaises(ValueError):
            check.validate_manifest(m)


if __name__ == "__main__":
    unittest.main()
