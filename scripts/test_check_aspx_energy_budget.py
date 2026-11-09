"""不用媒体／参考库，检查功率账目和被篡改的实际链观察。"""
import unittest

import numpy as np

from scripts.check_aspx_energy_budget import audit, checked_metadata, close


def fixture(mode='off', tone=False, onset=False):
    # 两时隙功率分别为 1、5，平均 E=3；S=260、Q=64，故 N0=16、T0=2。
    # 普通信号 G0=1，音调组 G0=8，均可直接由整数功率核对。
    meta = dict(au=1, source=1, sbx=32, sbz=34, factor=1, borders=[0, 2],
                noise_cursors=[0, 4], noise_enabled=[0 if onset else 1, 0 if tone or onset else 1])
    a = dict(interpolation=True, signal_borders=[[32, 33, 34]],
             limiter_borders=[] if mode == 'off' else [32, 34],
             estimated=[3., 3.], target=[260., 260.], ratio=[64., 64.],
             sine_index=[False, tone], sine_area=[False, tone], onset=[onset],
             initial_noise=[16., 16.], initial_sine=[0., 2. if tone else 0.])
    # sqrt(260/259) 用独立的 80 位 Decimal 求值后舍入到 f32。
    boost = 1.001928687095642 if mode == 'on' else 1.
    gains = [8.062257766723633]*2 if onset else [1., 8. if tone else 1.]
    a.update(signal=[float(np.float32(v*boost)) for v in gains],
             noise=[float(np.float32(16*boost))]*2,
             sine=[0., 2. if tone else 0.], boost=[boost]*2)
    data = {k: np.zeros((2, 64, 2), '<f4') for k in ('high', 'noise', 'sine', 'assembled')}
    data['high'][0, 32:34, 0] = 1
    data['high'][1, 32:34] = [1, 2]
    if mode == 'capped':
        # E=0、S=65、Q=1/64：G0=8、N0=1，所需 boost=sqrt(65)，触发上限。
        data['high'].fill(0)
        a.update(estimated=[0., 0.], target=[65., 65.], ratio=[1/64]*2,
                 initial_noise=[1., 1.], signal=[float(np.float32(8*1.584893192))]*2,
                 noise=[float(np.float32(1.584893192))]*2,
                 boost=[float(np.float32(1.584893192))]*2)
    meta['gain_audit'] = a
    meta['signal_gains'] = a['signal'].copy()
    table = np.tile(np.array([[1, 0], [-1, 0], [0, 1], [0, -1]], '<f4'), (128, 1))
    data['noise'][:, 32:34] = table[[[1, 2], [3, 4]]]*np.array(a['noise'], '<f4')[None, :, None]
    if tone:
        data['sine'][0, 33, 0] = 2
        data['sine'][1, 33, 1] = -2
    for t in range(2):
        for b in range(2):
            y = data['high'][t, b+32]*np.float32(a['signal'][b])
            if meta['noise_enabled'][b]:
                y += data['noise'][t, b+32]
            data['assembled'][t, b+32] = y+data['sine'][t, b+32]
    return meta, data, table


class EnergyBudgetTests(unittest.TestCase):
    def test_off_keeps_the_epsilon_deficit_and_finite_window_cross_term(self):
        result = audit(*fixture())
        totals = result['envelopes'][0]['totals']
        self.assertEqual(totals['target'], 520)
        self.assertEqual(totals['modeled_signal']+totals['modeled_noise'], 518)
        self.assertNotEqual(totals['cross'], 0)
        self.assertNotEqual(totals['assembled'], totals['target'])
        self.assertTrue(result['assembly_bit_exact'])

    def test_on_restores_group_budget_but_not_sample_energy(self):
        result = audit(*fixture('on'))
        group = result['limiter_groups'][0]
        self.assertEqual(group['limited_subbands'], 0)
        self.assertAlmostEqual(group['after_boost']/520, 1, places=6)
        self.assertNotEqual(result['envelopes'][0]['totals']['assembled'], 520)

    def test_float_error_bound_cannot_hide_a_representable_noise_amplitude(self):
        with self.assertRaises(ValueError):
            close(0., 2.**-75, 'P94')

    def test_complete_cancellation_still_has_a_closed_power_ledger(self):
        meta, data, table = fixture()
        a = meta['gain_audit']
        a.update(estimated=[1., 1.], target=[3., 3.], ratio=[.5, .5],
                 initial_noise=[1., 1.], signal=[1., 1.], noise=[1., 1.])
        meta['signal_gains'] = [1., 1.]
        data['high'].fill(0)
        data['high'][:, 32:34, 0] = 1
        data['noise'].fill(0)
        data['noise'][:, 32:34, 0] = -1
        table[:] = [-1, 0]
        data['assembled'].fill(0)
        totals = audit(meta, data, table)['envelopes'][0]['totals']
        self.assertEqual(totals['assembled'], 0)
        self.assertEqual(totals['signal']+totals['noise']+totals['cross'], 0)

    def test_boost_cap_leaves_a_reported_deficit(self):
        result = audit(*fixture('capped'))
        group = result['limiter_groups'][0]
        self.assertTrue(group['boost_capped'])
        self.assertLess(group['after_boost'], group['target']/10)
        self.assertAlmostEqual(group['after_boost'], 2*10**.4, places=6)

    def test_harmonic_and_onset_consume_only_budgeted_noise(self):
        for tone, onset, expected in [(True, False, 1), (False, True, 0)]:
            result = audit(*fixture(tone=tone, onset=onset))
            self.assertEqual(result['envelopes'][0]['noise_enabled_subbands'], expected)
            self.assertTrue(result['noise_bit_exact'])

    def test_wrong_estimate_target_gain_noise_or_boost_is_rejected(self):
        for field in ('estimated', 'target', 'ratio', 'initial_noise', 'signal', 'noise', 'boost'):
            meta, data, table = fixture('on')
            meta['gain_audit'][field][0] *= 2
            if field == 'signal':
                meta['signal_gains'][0] *= 2
            with self.subTest(field=field), self.assertRaises(ValueError):
                audit(meta, data, table)

    def test_noninterpolated_estimate_must_use_the_frequency_group(self):
        meta, data, table = fixture()
        meta['gain_audit']['interpolation'] = False
        meta['gain_audit']['signal_borders'] = [[32, 34]]
        audit(meta, data, table)
        data['high'][:, 32] *= 2
        with self.assertRaisesRegex(ValueError, 'P90'):
            audit(meta, data, table)

    def test_wrong_noise_mask_cursors_table_or_samples_are_rejected(self):
        for field in ('mask', 'before', 'after', 'table', 'noise', 'sine', 'assembled'):
            meta, data, table = fixture()
            if field == 'mask':
                meta['noise_enabled'][0] = 0
            elif field in ('before', 'after'):
                meta['noise_cursors'][field == 'after'] += 1
            elif field == 'table':
                table[1, 0] += 1
            else:
                data[field][0, 32, 0] += 1
            with self.subTest(field=field), self.assertRaises(ValueError):
                audit(meta, data, table)

    def test_changing_both_cursors_cannot_hide_a_noise_index_shift(self):
        meta, data, table = fixture()
        meta['noise_cursors'] = [1, 5]
        with self.assertRaisesRegex(ValueError, 'P102'):
            audit(meta, data, table)

    def test_missing_and_invalid_group_fields_are_rejected(self):
        for field, value in [('signal_borders', [[32, 32, 34]]), ('signal_borders', [[32, 33]]),
                             ('limiter_borders', [32, 33]), ('onset', [0]),
                             ('sine_area', [True, False])]:
            meta, _, _ = fixture()
            meta['gain_audit'][field] = value
            with self.subTest(field=field), self.assertRaises(ValueError):
                checked_metadata(meta)
        meta, _, _ = fixture()
        del meta['gain_audit']['estimated']
        with self.assertRaises(KeyError):
            checked_metadata(meta)

    def test_nonfinite_wrong_dtype_truncation_and_extra_slots_are_rejected(self):
        for value in (np.full((2, 64, 2), np.nan, '<f4'), np.zeros((2, 64, 2), '<f8'),
                      np.zeros((1, 64, 2), '<f4'), np.zeros((3, 64, 2), '<f4')):
            meta, data, table = fixture()
            data['high'] = value
            with self.assertRaises(ValueError):
                audit(meta, data, table)
        for value in (float('nan'), float('inf'), -1, True):
            meta, data, table = fixture()
            meta['gain_audit']['target'][0] = value
            with self.assertRaises(ValueError):
                audit(meta, data, table)

    def test_bad_geometry_and_fractional_identity_are_rejected(self):
        for field, value in [('au', 0), ('source', .5), ('factor', 3), ('sbz', 32),
                             ('borders', [0, 2, 2]), ('noise_cursors', [0, 512])]:
            meta, data, table = fixture()
            meta[field] = value
            with self.subTest(field=field), self.assertRaises(ValueError):
                audit(meta, data, table)

    def test_prefix_carry_is_excluded_and_interval_clock_stays_relative(self):
        meta, data, table = fixture()
        meta['borders'] = [1, 3]
        data['assembled'] = np.concatenate((np.ones((1, 64, 2), '<f4'), data['assembled']))
        result = audit(meta, data, table)
        self.assertEqual(result['envelopes'][0]['first'], 1)
        self.assertTrue(result['assembly_bit_exact'])


if __name__ == '__main__':
    unittest.main()
