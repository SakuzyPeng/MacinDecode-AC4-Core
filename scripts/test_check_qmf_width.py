import copy
import unittest

import numpy as np

from scripts import check_qmf_width as check


def cases():
    return [dict(id=f'c{owner+1}', scope='s', owner=owner,
                 transport_track=0 if owner == -1 else (owner+6) % 9+1,
                 width=8 if owner == -1 else 48, last_au=3,
                 layouts=[[2048]]*4, targets=[36, 44, 62], sources=[28, 18])
            for owner in range(-1, 9)]


def grids():
    return [dict(id=f'n{n}p{p}c{int(c)}', n=n, pattern=p, centered=c)
            for n, patterns, centers in [(8, range(2), (False, True)),
                                          (48, range(4), (False, True)),
                                          (64, range(4), (False,))]
            for p in patterns for c in centers]


class KernelTests(unittest.TestCase):
    def test_dense_kernel_matches_fft_factorization(self):
        rng = np.random.default_rng(818)
        for n in (8, 48, 64):
            x = rng.normal(size=24*n)
            w = rng.normal(size=10*n)
            h = rng.normal(size=(9, n))
            pre, post = check.ideal_phases(n)
            expected = check.fft_analysis(x.reshape(-1, n), h, w, pre, post)['qmf']
            actual = check.apply_kernel(x, check.ideal_kernel(n, w), h)
            np.testing.assert_allclose(actual, expected, atol=4e-14)

    def test_all_impulse_positions_reconstruct_the_operator(self):
        for n in (8, 48):
            w = np.sin(np.arange(10*n)*.023)
            pre, post = check.ideal_phases(n)
            observations = {}
            for phase in range(n):
                x = np.zeros((24, n))
                x[2, phase] = 1
                observations[phase] = check.fft_analysis(x, np.zeros((9, n)), w, pre, post)['qmf']
            actual = check.impulse_kernel(n, observations)
            np.testing.assert_allclose(actual, check.ideal_kernel(n, w), atol=5e-16)

    def test_missing_position_and_tail_are_rejected(self):
        observations = {i: np.zeros((24, 8), complex) for i in range(8)}
        with self.assertRaises(ValueError):
            check.impulse_kernel(8, {k: v for k, v in observations.items() if k != 7})
        observations[1][12, 0] = 1
        with self.assertRaisesRegex(ValueError, '支持区间'):
            check.impulse_kernel(8, observations)

    def test_history_is_newest_block_first_and_split_is_continuous(self):
        n = 8
        x = np.random.default_rng(128).normal(size=(32, n))
        kernel = check.ideal_kernel(n, np.ones(10*n))
        whole = check.apply_kernel(x.reshape(-1), kernel)
        latter = check.apply_kernel(x[16:].reshape(-1), kernel, x[7:16][::-1])
        np.testing.assert_allclose(latter, whole[16:], atol=1e-15)

    def test_symmetric_prototype_gives_the_predicted_phase(self):
        def kernel(n):
            m = np.arange(10*n)
            h = np.exp(-((m-5*n)/(1.2*n))**2)
            h[0] = 0
            return check.ideal_kernel(n, h*np.where((m//(2*n)) % 2, -1, 1))
        full = kernel(64)
        for n in (8, 48):
            offsets = np.array([-187.5, -93.75, 0, 93.75, 187.5])
            frequencies = ((np.arange(n)+.5)*375)[:, None]+offsets
            freq = frequencies.reshape(-1)
            index = np.arange(len(freq)), np.repeat(np.arange(n), len(offsets))
            predicted = check.frequency_response(full, freq)[:, :n]*check.phase_factor(n, freq)
            actual = check.frequency_response(kernel(n), freq)
            self.assertLess(np.max(abs(np.angle(actual[index]/predicted[index]))), 3e-12)

    def test_geometry_does_not_change_the_64_band_control(self):
        self.assertEqual(check.center_offset(8), 3.5)
        self.assertAlmostEqual(check.center_offset(48), 1/6)
        self.assertEqual(check.center_offset(64), 0)
        np.testing.assert_array_equal(check.phase_factor(64, [10, 10000]), np.ones((2, 64)))


class SamplingTests(unittest.TestCase):
    def test_fractional_delay_has_known_sign_and_unity_dc(self):
        t = np.arange(4096)
        for delta in (1/6, 3.5, -5/6):
            x = np.cos(2*np.pi*13000*t/48000+.31)
            actual = check.fractional_delay(x, delta, 256)
            expected = np.cos(2*np.pi*13000*(t-delta)/48000+.31)
            self.assertLess(np.max(abs(actual[512:-512]-expected[512:-512])), 2e-7)
            self.assertAlmostEqual(check.delay_kernel(delta, 256).sum(), 1, places=14)

    def test_centered_sampling_matches_a_known_continuous_wave(self):
        t = np.arange(4096)
        for n in (8, 48):
            x = np.cos(2*np.pi*1200*t/48000+.31)
            actual = check.sample_centered(x, n)
            position = (np.arange(len(actual))+.5)*64/n-.5
            expected = np.cos(2*np.pi*1200*position/48000+.31)
            valid = (position >= 512) & (position < 3584)
            self.assertLess(np.max(abs(actual[valid]-expected[valid])), 2e-7)
            wrong = np.cos(2*np.pi*1200*np.arange(len(actual))*64/n/48000+.31)
            self.assertGreater(np.max(abs(actual[valid]-wrong[valid])), .01)

    def test_nonfinite_input_bad_history_and_unsupported_geometry_fail(self):
        with self.assertRaises(ValueError):
            check.fractional_delay([0, np.nan], 0, 256)
        with self.assertRaises(ValueError):
            check.sample_centered(np.zeros(65), 48)
        with self.assertRaises(ValueError):
            check.center_offset(32)
        with self.assertRaises(ValueError):
            check.apply_kernel(np.zeros(8), np.zeros((80, 8)), np.zeros((8, 8)))

    def test_power_measure_does_not_confuse_phase_with_gain(self):
        x = np.array([[1+2j, 3-1j]])
        ref = check.components(x)
        self.assertAlmostEqual(check.source_power(ref, x*1j)['relative_error_norm'], 0)
        self.assertAlmostEqual(check.source_power(ref, x*2)['aggregate_relative_bias'], 3)


class ManifestTests(unittest.TestCase):
    def test_complete_coverage_accepted(self):
        check.validate_cases(cases())
        check.validate_grids(grids())

    def test_missing_or_duplicate_owner_rejected(self):
        for rows in (cases()[:-1], cases()+cases()[:1]):
            with self.assertRaises(ValueError):
                check.validate_cases(rows)

    def test_fitted_transport_permutation_rejected(self):
        rows = cases()
        rows[1]['transport_track'], rows[2]['transport_track'] = rows[2]['transport_track'], rows[1]['transport_track']
        with self.assertRaisesRegex(ValueError, 'P14'):
            check.validate_cases(rows)

    def test_missing_future_guard_or_invalid_layout_rejected(self):
        rows = cases()
        rows[0]['layouts'] = rows[0]['layouts'][:-1]
        with self.assertRaisesRegex(ValueError, '保护帧'):
            check.validate_cases(rows)
        rows = cases()
        rows[0]['layouts'] = [[512]]*4
        with self.assertRaises(ValueError):
            check.validate_cases(rows)

    def test_patch_cannot_read_outside_low_band(self):
        rows = cases()
        rows[1]['sources'] = [35, 18]
        with self.assertRaisesRegex(ValueError, '越界'):
            check.validate_cases(rows)

    def test_grid_omission_duplicate_and_wrong_width_rejected(self):
        rows = grids()
        for candidate in (rows[:-1], rows+rows[:1]):
            with self.assertRaises(ValueError):
                check.validate_grids(candidate)
        rows = copy.deepcopy(rows)
        rows[0]['n'] = 32
        with self.assertRaises(ValueError):
            check.validate_grids(rows)


if __name__ == '__main__':
    unittest.main()
