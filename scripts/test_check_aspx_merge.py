from copy import deepcopy
import unittest
import numpy as np
from scripts import check_aspx_merge as check


def fixture(phase=0):
    q = np.zeros((96, 64, 2), '<f4')
    q[:, 35, 0] = np.arange(96)+1
    z = [1, 1j, -1, -1j][phase]
    q[:, 36] = [z.real, z.imag]
    y = np.zeros_like(q)
    y[:64, 36, 0] = 1
    return q, y


def cases():
    rows = []
    def add(**c):
        rows.append(dict(id=f"r{len(rows)}", **c))
    for scope in ('tone', 'music100', 'music33'):
        for call in range(9):
            for v in ('a', 'b', 'b512', 'c', 'c512', 'd', 'e'):
                for i in range(2):
                    for p in range(2):
                        for m in range(3):
                            stem = f'{scope}c{call}i{i}{v}p{p}m{m}'
                            add(kind='local', scope=scope, call=call, variant=v, interpolation=i,
                                preflat=p, mode=m, sbx=36, sbz=62 if scope=='tone' else 56,
                                previous_chain=stem, input=f'{scope}c{call}{v}q', sec=stem+'y')
            add(kind='reference_fixed', scope=scope, call=call, variant='a', sbx=36,
                sbz=62 if scope=='tone' else 56, input=f'{scope}c{call}aq',
                sec=f'{scope}c{call}referencey', reference=f'{scope}c{call}reference.f32')
    for b in (4, 36, 47, 63):
        for p in range(4):
            add(kind='synthetic', sbx=b, sbz=64, phase_quarters=p, input=f's{b}p{p}q', sec=f's{b}p{p}y')
    return rows


class MergeTests(unittest.TestCase):
    def test_complete_positive_and_reordered_manifest(self):
        c = cases()
        check.validate_cases(c)
        check.validate_cases(c[::-1])

    def test_missing_duplicate_and_wrong_source(self):
        c = cases()
        for broken in (c[:-1], c+[deepcopy(c[0])]):
            with self.assertRaises(ValueError): check.validate_cases(broken)
        c[0]['input'] = 'wrongOwner'
        with self.assertRaises(ValueError): check.validate_cases(c)

    def test_declared_add_and_select_with_positive_low_band(self):
        q, y = fixture()
        d, a, s = check.merge_model(q, y, 36)
        self.assertEqual(a[0, 36, 0], 2)
        self.assertEqual(s[0, 36, 0], 1)
        self.assertEqual(s[0, 35, 0], 27)
        check.verify_merge(a, d+y)
        np.testing.assert_array_equal(s[:, 36:], y[:, 36:])

    def test_equivalent_addition_order(self):
        q, y = fixture()
        d, a, _ = check.merge_model(q, y, 36)
        check.verify_merge(a, y+d)

    def test_wrong_delay_is_detected(self):
        q, y = fixture()
        expected = check.merge_model(q, y, 36)[1]
        wrong = check.merge_model(q, y, 36, 3)[1]
        with self.assertRaises(ValueError): check.verify_merge(wrong, expected)

    def test_dropped_low_band_is_detected(self):
        q, y = fixture()
        expected = check.merge_model(q, y, 36)[2]
        with self.assertRaises(ValueError): check.verify_merge(y, expected)

    def test_unselected_leakage_and_wrong_crossover_are_detected(self):
        q, y = fixture()
        _, a, s = check.merge_model(q, y, 36)
        with self.assertRaises(ValueError): check.verify_merge(a, s)
        wrong = s.copy(); wrong[:, 35] = y[:, 35]
        with self.assertRaises(ValueError): check.verify_merge(wrong, s)

    def test_cross_term_has_independent_quarter_phase_anchors(self):
        for phase, expected_energy in enumerate((128, 64, 0, 64)):
            q, y = fixture(phase)
            d, a, _ = check.merge_model(q, y, 36)
            result = check.energy_terms(d[:32, 36:], y[:32, 36:], a[:32, 36:])
            self.assertEqual(result['leak'], 32)
            self.assertEqual(result['sec'], 32)
            self.assertEqual(result['combined'], expected_energy)

    def test_omitted_or_wrong_cross_term_is_detected(self):
        q, y = fixture(2)
        d, a, _ = check.merge_model(q, y, 36)
        with self.assertRaises(ValueError):
            check.energy_terms(d[:32, 36:], y[:32, 36:], a[:32, 36:]+1)

    def test_zero_components_and_exact_identity(self):
        q, y = fixture()
        q[:, 36:] = 0
        _, a, s = check.merge_model(q, y, 36)
        check.verify_merge(a, s)
        self.assertEqual(check.metrics(a, s)['error_energy'], 0)

    def test_nonfinite_and_truncated_input_fail_closed(self):
        q, y = fixture()
        with self.assertRaises(ValueError): check.merge_model(q[:-1], y, 36)
        q[0, 0, 0] = np.inf
        with self.assertRaises(ValueError): check.merge_model(q, y, 36)

    def test_out_of_scope_sec_is_rejected(self):
        q, y = fixture()
        for t, b in ((0, 35), (95, 36)):
            bad = y.copy(); bad[t, b, 0] = 1
            with self.assertRaises(ValueError): check.merge_model(q, bad, 36)

    def test_identical_pcm_has_no_frequency_band_error(self):
        t = np.arange(6144)
        x = np.cos(2*np.pi*15000*t/48000).astype('<f4')
        r = check.pcm_stats(x, x.copy())
        self.assertEqual(r['full']['error_energy'], 0)
        self.assertEqual(r['band_12_18khz']['error_energy'], 0)
        self.assertGreater(r['band_12_18khz']['signal_energy'], 1000)


if __name__ == '__main__': unittest.main()
