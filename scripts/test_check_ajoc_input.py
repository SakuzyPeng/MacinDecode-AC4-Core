"""无媒体边界夹具：先确认正向，再分别破坏身份、时间和数值。"""
import copy
import hashlib
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import numpy as np

try:
    from . import check_ajoc_input as check
    from .check_aspx_basis_replay import components
except ImportError:
    import check_ajoc_input as check
    from check_aspx_basis_replay import components


class InputBoundaryTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.addCleanup(self.temp.cleanup)
        self.rows = [dict(au=au, source_au=au-1 if au > 1 else None, samples=1536,
                          channels=5, fullband=4, lfe=True, substream=2, pcm_delay=96,
                          control_delay=1, source_order=[3, 4, 1, 2],
                          crossover=[64, 16, 20, 24, 28] if au > 1 else None,
                          selected=au > 1, reconstructed=au > 1, wet=False) for au in range(1, 4)]
        self.save_rows()
        # 自有非规范 FIR 系数只验证检查器，不冒充许可 QMF 表或生产 DSP。
        window = (np.sin(np.arange(640)*.013)/64).astype('<f4')
        window.tofile(self.root/'window.f32')
        self.window_hash = hashlib.sha256(window.tobytes()).hexdigest()
        self.core = np.random.default_rng(193).normal(size=(3, 5, 1536)).astype('<f4')
        self.core.tofile(self.root/'core.f32')
        for au in (2, 3):
            model, _ = check.expected_low(self.core, au, 96, window)
            for source in range(5):
                value = components(model[source]).astype('<f4')
                value.tofile(self.root/f'au{au:04}.source{source}.f32')
                if source == 0:
                    value.tofile(self.root/f'au{au:04}.lfe.f32')
                else:
                    target = [3, 4, 1, 2].index(source)
                    value.tofile(self.root/f'au{au:04}.input{target}.f32')
            for kind, role in [('aspx', 'applied'), ('ajoc', 'consumed')]:
                for prefix in ['raw', role]:
                    (self.root/f'au{au:04}.{prefix}_{kind}.txt').write_text(f'{kind}: source AU {au-1}')
            (self.root/f'au{au:04}.state_before.txt').write_text(f'state {au-1}')
            (self.root/f'au{au:04}.state_after.txt').write_text(f'state {au}')

    def save_rows(self):
        (self.root/'frames.jsonl').write_text(''.join(json.dumps(row)+'\n' for row in self.rows))

    def run_check(self):
        with patch.object(check, 'WINDOW_SHA256', self.window_hash):
            return check.check_local(self.root, [2, 3])

    def test_complete_positive_and_reordered_json_fields(self):
        self.assertEqual(self.run_check()['frames'], 3)
        self.rows = [dict(reversed(list(row.items()))) for row in self.rows]
        self.save_rows()
        self.assertEqual(self.run_check()['reconstructed_frames'], 2)

    def test_current_control_cannot_replace_due_control(self):
        self.rows[1]['source_au'] = 2
        self.save_rows()
        with self.assertRaisesRegex(ValueError, '到期 AU'):
            self.run_check()

    def test_missing_duplicate_and_extra_au(self):
        for rows in [self.rows[:2], [self.rows[0], self.rows[1], self.rows[1]], self.rows+self.rows[-1:]]:
            with self.subTest(rows=len(rows)), self.assertRaises(ValueError):
                check.validate_frames(rows, [2, 3])

    def test_all_long_frame_delays_have_independent_values(self):
        for length, delay in [(1536, 96), (1920, 288), (2048, 352)]:
            rows = copy.deepcopy(self.rows)
            for row in rows:
                row.update(samples=length, pcm_delay=delay)
            self.assertEqual(check.validate_frames(rows, [2, 3])[2], delay)
            rows[1]['pcm_delay'] += 64
            with self.assertRaises(ValueError):
                check.validate_frames(rows, [2, 3])

    def test_routing_cannot_be_transport_order(self):
        self.rows[1]['source_order'] = [1, 2, 3, 4]
        self.save_rows()
        with self.assertRaisesRegex(ValueError, '路由'):
            self.run_check()

    def test_actual_input_swap_and_scale_are_detected(self):
        target = self.root/'au0002.input0.f32'
        original = target.read_bytes()
        target.write_bytes((self.root/'au0002.input1.f32').read_bytes())
        with self.assertRaisesRegex(ValueError, '逐位'):
            self.run_check()
        target.write_bytes(original)
        x = np.fromfile(target, '<f4')*2
        x.tofile(target)
        with self.assertRaisesRegex(ValueError, '逐位'):
            self.run_check()

    def test_delay_error_on_both_sides_is_caught_by_fir(self):
        source = self.root/'au0002.source3.f32'
        x = np.fromfile(source, '<f4').reshape(24, 64, 2)
        x = np.roll(x, 1, axis=0)
        x.tofile(source)
        x.tofile(self.root/'au0002.input0.f32')
        with self.assertRaisesRegex(ValueError, '前向界'):
            self.run_check()

    def test_lfe_delay_error_is_caught_independently(self):
        source = self.root/'au0002.source0.f32'
        x = np.fromfile(source, '<f4').reshape(24, 64, 2)
        x = np.roll(x, 6, axis=0)
        x.tofile(source)
        x.tofile(self.root/'au0002.lfe.f32')
        with self.assertRaisesRegex(ValueError, '前向界'):
            self.run_check()

    def test_wrong_consumed_matrix_and_applied_aspx_are_detected(self):
        for kind, role in [('aspx', 'applied'), ('ajoc', 'consumed')]:
            path = self.root/f'au0002.{role}_{kind}.txt'
            original = path.read_bytes()
            path.write_text('wrong AU content')
            with self.assertRaisesRegex(ValueError, '完整控制'):
                self.run_check()
            path.write_bytes(original)

    def test_reset_history_between_adjacent_real_frames_is_detected(self):
        (self.root/'au0003.state_before.txt').write_text('fresh')
        with self.assertRaisesRegex(ValueError, '历史'):
            self.run_check()

    def test_nonfinite_and_truncated_inputs_are_rejected(self):
        path = self.root/'au0002.source1.f32'
        original = path.read_bytes()
        x = np.frombuffer(original, '<f4').copy()
        x[7] = np.nan
        x.tofile(path)
        with self.assertRaises(ValueError):
            self.run_check()
        path.write_bytes(original[:-4])
        with self.assertRaises(ValueError):
            self.run_check()

    def test_protocol_digest_and_exact_file_inventory(self):
        media = self.root/'input.ac4'
        media.write_bytes(b'checker fixture, not licensed media')
        files = {p.name: check.digest(p) for p in self.root.iterdir()}
        manifest = dict(schema='macinac4.ajoc-input-manifest', version=1, fit_parameters=False,
                        cases=[dict(id='own', input=str(media), input_sha256=check.digest(media),
                                    directory=str(self.root), files=files,
                                    selected_aus=[2, 3], expected_frames=3)])
        # manifest 本身在另一目录，不属于检查点清单。
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp)/'manifest.json'
            path.write_text(json.dumps(manifest))
            with patch.object(check, 'WINDOW_SHA256', self.window_hash):
                self.assertEqual(check.compare(path)['status'], 'local_boundary_pass')
                (self.root/'extra.txt').write_text('extra')
                with self.assertRaisesRegex(ValueError, '遗漏、多余'):
                    check.compare(path)
                (self.root/'extra.txt').unlink()
                media.write_bytes(b'changed')
                with self.assertRaisesRegex(ValueError, '媒体'):
                    check.compare(path)

    def test_reference_identity_is_not_inferred_from_silent_samples(self):
        entry = np.zeros((10, 32, 64, 2), '<f4')
        tokens = np.arange(320).reshape(10, 32).tolist()
        check.verify_reference_binding(tokens[1:], tokens, entry[1:], entry, entry.copy())
        wrong = copy.deepcopy(tokens[1:])
        wrong[0], wrong[1] = wrong[1], wrong[0]
        with self.assertRaisesRegex(ValueError, '身份'):
            check.verify_reference_binding(wrong, tokens, entry[1:], entry, entry.copy())

    def test_reference_handoff_and_independent_repeat_must_be_exact(self):
        entry = np.arange(10*32*64*2, dtype='<f4').reshape(10, 32, 64, 2)
        tokens = np.arange(320).reshape(10, 32).tolist()
        for which in ('producer', 'repeat'):
            produced, repeated = entry[1:].copy(), entry.copy()
            values = produced if which == 'producer' else repeated
            values[0, 0, 0, 1] += 1
            with self.assertRaisesRegex(ValueError, '逐位'):
                check.verify_reference_binding(tokens[1:], tokens, produced, entry, repeated)


if __name__ == '__main__':
    unittest.main()
