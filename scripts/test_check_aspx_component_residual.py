"""无媒体的源绑定、分量闭合与参考重放反例。"""
import copy
import unittest

import numpy as np

from scripts.check_aspx_component_residual import decompose, validate_layout
from scripts.check_aspx_noise_mask import render


def fixture(onset=False):
    row = dict(first=0, last=4, sbx=32, sbz=34, onset=onset, noise_cursor=9, tone_cursor=1)
    table = np.arange(1024, dtype='<f4').reshape(512, 2)/np.float32(1024)
    high = np.arange(16, dtype='<f4').reshape(4, 2, 2)/np.float32(16)
    gain = np.array([.5, 2.], '<f4')
    ng = np.array([[.5, 1., 0.], [2., .25, 0.]], '<f4')
    noise = render(np.zeros_like(high), ng, table, 32, 9, 1, False, 'both')
    mask = np.full(2, 0 if onset else 1, '<f4')
    assembled = high*gain[None, :, None]
    if not onset:
        assembled += noise
    data = dict(local_high=high, local_noise=noise, local_sine=np.zeros_like(high),
                local_gain=gain, local_noise_enabled=mask, local_assembled=assembled,
                native_high=high.copy(), native_gain=ng,
                native_assembled=render(high, ng, table, 32, 9, 1, onset, 'both'))
    return row, data, table


class ComponentResidualTests(unittest.TestCase):
    def test_fixed_basis_difference_is_reported_without_parameter_fit(self):
        result = decompose(*fixture())
        self.assertEqual(result['native_assembly_max_error_over_bound'], 0)
        self.assertEqual(result['noise_table_coordinates']['error_energy'], 0)
        self.assertGreater(result['noise']['error_energy'], 0)
        totals = result['totals']
        self.assertAlmostEqual((totals['signal_error']+totals['noise_error']+totals['cross'])/totals['error'], 1., places=6)

    def test_generated_but_suppressed_noise_is_not_attributed_to_output(self):
        row, data, table = fixture(onset=True)
        data['local_noise'].fill(100)
        result = decompose(row, data, table)
        self.assertEqual(result['totals']['noise_error'], 0)

    def test_wrong_local_gain_mask_or_clock_fails_closure(self):
        for field in ('local_gain', 'local_noise_enabled', 'local_noise', 'local_assembled'):
            row, data, table = fixture()
            data[field].flat[0] += np.float32(.25)
            with self.assertRaises(ValueError):
                decompose(row, data, table)

    def test_reference_wrong_cursor_and_output_are_rejected(self):
        row, data, table = fixture()
        with self.assertRaises(ValueError):
            decompose(dict(row, noise_cursor=10), data, table)
        data['native_assembled'][0, 0, 0] += np.float32(.1)
        with self.assertRaises(ValueError):
            decompose(row, data, table)

    def test_shape_nan_dtype_and_unsupported_tone_are_rejected(self):
        for value in (np.zeros((2, 2, 2), '<f4'), np.full((4, 2, 2), np.nan, '<f4'),
                      np.zeros((4, 2, 2), '<f8')):
            row, data, table = fixture();data['local_high'] = value
            with self.assertRaises(ValueError):
                decompose(row, data, table)
        for field in ('local_sine', 'native_gain'):
            row, data, table = fixture()
            if field == 'native_gain':data[field][0, 2] = 1
            else:data[field][0, 0, 0] = 1
            with self.assertRaises(ValueError):
                decompose(row, data, table)

    def test_invalid_geometry_and_noninteger_clock_are_rejected(self):
        for field, value in [('first', -1), ('last', 0), ('sbx', 48), ('sbz', 32),
                             ('noise_cursor', True), ('tone_cursor', 4), ('onset', 0)]:
            row, data, table = fixture();row[field] = value
            with self.assertRaises(ValueError):
                decompose(row, data, table)

    def test_finite_inputs_that_overflow_are_rejected(self):
        row, data, table = fixture()
        data['local_high'].fill(np.finfo(np.float32).max)
        data['local_gain'].fill(2)
        with np.errstate(over='ignore'), self.assertRaises(ValueError):
            decompose(row, data, table)


class LayoutTests(unittest.TestCase):
    def fixture(self):
        case = dict(id='case', fullband=5, au=4)
        order = [3, 4, 5, 1, 2]
        rows = [dict(case='case', call=i, env=e, source=source, au=4,
                     first=e*2, last=(e+1)*2, sbx=32, sbz=34,
                     producer_tokens=list(range(i*32, (i+1)*32)),
                     consumer_tokens=list(range(i*32, (i+1)*32)))
                for i, source in enumerate(order) for e in range(2)]
        return rows, [case]

    def test_complete_producer_consumer_bindings(self):
        validate_layout(*self.fixture())

    def test_wrong_identity_duplicate_and_missing_source_fail(self):
        rows, cases = self.fixture()
        for bad in (rows+rows[:1], rows[:-2], rows[1:]):
            with self.assertRaises(ValueError):validate_layout(bad, cases)
        for field, value in [('source', 1), ('source', True), ('au', 3), ('au', 4.0),
                             ('consumer_tokens', [0]*32),
                             ('consumer_tokens', [False, True]+list(range(2, 32)))]:
            bad = copy.deepcopy(rows);bad[0][field] = value
            with self.assertRaises(ValueError):validate_layout(bad, cases)

    def test_interrupted_envelope_or_changed_band_layout_fails(self):
        rows, cases = self.fixture()
        for field, value in [('first', 3), ('sbx', 33), ('env', 2)]:
            bad = copy.deepcopy(rows);bad[1][field] = value
            with self.assertRaises(ValueError):validate_layout(bad, cases)


if __name__ == '__main__':
    unittest.main()
