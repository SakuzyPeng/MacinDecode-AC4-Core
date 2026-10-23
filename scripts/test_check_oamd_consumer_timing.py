"""自有包络、时间字段与消费者记录；不需要媒体、PDF 或原生库。"""
import copy
import json
import struct
import tempfile
import unittest
from pathlib import Path

from scripts import check_oamd_consumer_timing as check
from scripts.test_check_ajoc_timing_stream import owned_frame


def owned_stream(crc=False):
    frame, fields = owned_frame(crc)
    all_fields = []
    for i in range(100):
        for field in fields:
            shift = i * len(frame) * 8
            all_fields.append(dict(field, au=i + 1, bit=field['bit'] + shift,
                                   region=[v + shift for v in field['region']]))
    return frame * 100, all_fields


def envelope(call):
    return struct.pack('<IIQIiQQ', 0x444d414f, 2, call, 0, 352, 2048, 3) + b'abc'


def native_fixture():
    payloads, external, rows = [], [], []
    for call in range(1, 401):
        blocks = [[31, 1001]] if call == 100 else [[0, 2048]]
        objects = 10 if call == 1 else 21
        payloads.append(dict(call_index=call, pcm_start=(call - 1) * 2048, api_offset=352, payload_sha256='owned'))
        external.append(dict(call_index=call, internal_offset=0, blocks=blocks, objects=objects, payload_sha256='owned'))
        rows.extend([
            dict(kind='parse', au=call, rc=0, objects=objects, internal_offset=0, blocks=blocks),
            dict(kind='enqueue', au=call, api_offset=352, queue_origin=10240, queue_time=10591,
                 count=1, internal_offset=0, blocks=blocks),
            dict(kind='acceptance', call_index=call, parse_rc=0, parse_calls=1, accepted_payloads=1,
                 objects=objects, render_completed=True, errors=0),
        ])
    for au in range(99, 103):
        for centre in range(16, 2048, 32):
            source = au if centre >= 352 else au - 1
            while True:
                query = (au - source) * 2048 + centre - 352
                factor, duration = (31, 1001) if source == 100 else (0, 2048)
                active = query >= factor * 32
                row = dict(kind='query', function=2, au=au, source_call=source, query=query,
                           parent_query=centre, rc=21 if active else 0, internal_offset=0, blocks=[[factor, duration]])
                if active:
                    row.update(value=max(0, duration - query + factor * 32), duration=duration)
                rows.append(row)
                if active:
                    break
                source -= 1
    return rows, payloads, external


class OamdConsumerTests(unittest.TestCase):
    def test_fixed_sixteen_configurations_preserve_old_protocol(self):
        rows = check.configurations()
        self.assertEqual(len(rows), 16)
        self.assertEqual(len({r['id'] for r in rows}), 16)
        self.assertEqual(sum(r['reused'] for r in rows), 10)
        self.assertEqual({(r['factor'], r['duration']) for r in rows[10:]},
                         {(f, d) for f in (1, 31, 63) for d in (32, 1001)})

    def test_all_variants_and_crc_round_trip(self):
        for crc in (False, True):
            source, fields = owned_stream(crc)
            for cfg in check.configurations():
                variant, proof = check.make_variant(source, fields, cfg)
                check.verify_variant(source, variant, fields, proof)
                if not cfg['changes']:
                    self.assertEqual(source, variant)

    def test_rejects_bad_position_bounds_old_value_and_source(self):
        source, fields = owned_stream()
        variant, proof = check.make_variant(source, fields, check.configurations()[-1])
        for key, value in [('bit', -1), ('width', 7), ('old', 1), ('au', 98), ('region', [0, 100000])]:
            bad = copy.deepcopy(proof)
            bad['patches'][0][key] = value
            with self.assertRaises(ValueError):
                check.verify_variant(source, variant, fields, bad)

    def test_rejects_changes_outside_whitelist_even_with_new_hash(self):
        source, fields = owned_stream()
        variant, proof = check.make_variant(source, fields, check.configurations()[-1])
        bad = bytearray(variant)
        bad[20] ^= 1
        proof['variant_sha256'] = check.sha_bytes(bad)
        with self.assertRaises(ValueError):
            check.verify_variant(source, bad, fields, proof)

    def test_rejects_crc_damage_truncation_and_missing_field(self):
        source, fields = owned_stream(True)
        variant, proof = check.make_variant(source, fields, check.configurations()[-1])
        for bad in (variant[:-1], variant[:-1] + bytes([variant[-1] ^ 1])):
            with self.assertRaises(ValueError):
                check.verify_variant(source, bad, fields, proof)
        with self.assertRaises(ValueError):
            check.make_variant(source, fields[:-1], check.configurations()[-1])

    def test_sample_offset_encoding_paths(self):
        self.assertEqual(check.sample_offset('Implicit', 0), 0)
        for value in (8, 16, 24):
            self.assertEqual(check.sample_offset('Code', value), value)
        for value in range(32):
            self.assertEqual(check.sample_offset('Explicit', value), value)
        for source, value in [('Implicit', 1), ('Code', 32), ('Explicit', -1), ('Explicit', 32)]:
            with self.assertRaises(ValueError):
                check.sample_offset(source, value)

    def test_ramp_table_and_explicit_paths(self):
        self.assertEqual([check.ramp_samples('Table', i) for i in range(16)],
                         [32, 64, 128, 256, 320, 480, 1000, 1001, 1024, 1600, 1601, 1602, 1920, 2000, 2002, 2048])
        for kind, result in [('Zero', 0), ('Fixed512', 512), ('Fixed1536', 1536)]:
            self.assertEqual(check.ramp_samples(kind), result)
        for value in (0, 1, 1001, 2047):
            self.assertEqual(check.ramp_samples('Explicit', value), value)
        with self.assertRaises(ValueError):
            check.ramp_samples('Explicit', 2048)

    def test_integer_coordinates_and_overflow(self):
        for factor in (0, 1, 2, 31, 62, 63):
            self.assertEqual(check.event_sample(-2048, 31, factor), -2017 + 32 * factor)
        for values in [(check.I64_MAX, 0, 1), (0, 32, 0), (0, 0, 64), (0, 0, True)]:
            with self.assertRaises(ValueError):
                check.event_sample(*values)

    def test_payload_complete_sequence(self):
        with tempfile.TemporaryDirectory() as directory:
            p = Path(directory) / 'payload'
            p.write_bytes(envelope(1) + envelope(2))
            rows = check.read_payloads(p, 2)
            self.assertEqual([r['pcm_start'] for r in rows], [0, 2048])

    def test_payload_missing_duplicate_reordered_trailing_and_wrong_version(self):
        data = envelope(1) + envelope(2)
        bad_version = bytearray(data)
        bad_version[4] = 1
        with tempfile.TemporaryDirectory() as directory:
            p = Path(directory) / 'payload'
            for bad in [data[:-1], data[:20], envelope(1), envelope(1) * 2,
                        envelope(2) + envelope(1), data + b'0', bad_version]:
                p.write_bytes(bad)
                with self.assertRaises(ValueError):
                    check.read_payloads(p, 2)

    def test_queue_boundary_cross_frame_and_reset(self):
        q = check.EventQueue()
        events = [dict(object=0, sample=t, order=i) for i, t in enumerate((0, 2047, 2048, 4096))]
        self.assertEqual([e['sample'] for e in q.frame(2048, events)], [0, 2047])
        self.assertEqual([e['sample'] for e in q.frame(2048, [])], [2048])
        q.reset()
        self.assertEqual(q.frame(2048, []), [])

    def test_queue_split_processing_is_equivalent(self):
        events = [dict(object=0, sample=t, order=i) for i, t in enumerate((1, 32, 2016, 2048, 2049))]
        whole, split = check.EventQueue(), check.EventQueue()
        expected = whole.frame(4096, events)
        actual = split.frame(2048, events) + split.frame(2048, [])
        self.assertEqual(actual, expected)

    def test_queue_rejects_per_object_reordering(self):
        q = check.EventQueue()
        with self.assertRaises(ValueError):
            q.frame(2048, [dict(object=0, sample=t, order=i) for i, t in enumerate((10, 9))])

    def test_native_queries_cover_first_effective_endpoint_and_carry(self):
        rows, payloads, external = native_fixture()
        result = check.validate_native(rows, payloads, external)
        self.assertEqual(result['first_active_query']['relative_query'], 1008)
        self.assertEqual(result['first_active_query']['remaining'], 985)
        self.assertEqual(result['last_active_query']['remaining'], 0)
        self.assertEqual(result['last_active_query']['au'], 101)

    def test_native_unit_origin_rounding_and_source_faults_are_detected(self):
        rows, payloads, external = native_fixture()
        for change in ('missing_scale', 'double_scale', 'round64', 'missing_api', 'double_api', 'wrong_source', 'ramp'):
            bad = copy.deepcopy(rows)
            q = next(r for r in bad if r['kind'] == 'query' and r['source_call'] == 100 and r['query'] == 1008)
            if change in ('missing_scale', 'double_scale', 'round64'):
                false_start = {'missing_scale': 31, 'double_scale': 31 * 1024, 'round64': 1024}[change]
                q['value'] = max(0, 1001 - (q['query'] - false_start))
            elif change == 'wrong_source':
                q['source_call'] -= 1
            elif change == 'ramp':
                q['duration'] += 1
            else:
                q['query'] += 352 if change == 'missing_api' else -352
            with self.assertRaises(ValueError, msg=change):
                check.validate_native(bad, payloads, external)

    def test_native_outer_completion_does_not_mask_parse_or_queue_failure(self):
        rows, payloads, external = native_fixture()
        for kind, key, value in [('parse', 'rc', -1), ('acceptance', 'parse_rc', -1),
                                 ('acceptance', 'accepted_payloads', 0), ('enqueue', 'queue_time', 0)]:
            bad = copy.deepcopy(rows)
            next(r for r in bad if r['kind'] == kind)[key] = value
            with self.assertRaises(ValueError):
                check.validate_native(bad, payloads, external)

    def test_native_missing_capture_is_not_coverage(self):
        rows, payloads, external = native_fixture()
        with self.assertRaises(ValueError):
            check.validate_native(rows[1:], payloads, external)

    def test_native_dropped_duplicate_and_reordered_queries_fail(self):
        rows, payloads, external = native_fixture()
        for bad in (rows[:-1], rows + [rows[-1]], rows[:-2] + list(reversed(rows[-2:]))):
            with self.assertRaises(ValueError):
                check.validate_native(bad, payloads, external)

    def test_legacy_bridge_missing_scale_is_a_retained_counterexample(self):
        payloads = [dict(pcm_start=202752, api_offset=352)]
        external = [dict(internal_offset=0, blocks=[[2, 64]], objects=21)]
        expected = check.canonical_events(payloads, external)
        self.assertEqual(expected, [dict(coordinate=203168, ramp=64, objects=21)])
        with self.assertRaisesRegex(ValueError, '桥接反例'):
            check.validate_bridge([dict(coordinate=203106, ramp=64, objects=21)], expected)
        check.validate_bridge(expected, expected)

    def test_scene_missing_eof_wrong_source_and_offset_fail(self):
        cfg = check.configurations()[0]
        rows = [dict(kind='frame', au=1, source_start=0, codec_start=0, duration=2048, objects=20, updates=0),
                dict(kind='frame', au=2, source_start=2048, codec_start=2048, duration=2048, objects=20, updates=21)]
        rows += [dict(kind='event', au=2, source_au=1, object=i, block=0, offset=0, ramp=2048,
                      sample_offset=0, offset_source='Implicit', factor=0, ramp_code=3,
                      ramp_encoding='Table { index: 15 }', timing_updated=True, order=i) for i in range(21)]
        rows.append(dict(kind='eof', aus=2, source_samples=4096))
        check.validate_scene(rows, cfg, 2)
        for key, value in [('source_au', 2), ('offset', 32), ('object', 1), ('order', 1)]:
            bad = copy.deepcopy(rows)
            bad[2][key] = value
            with self.assertRaises(ValueError):
                check.validate_scene(bad, cfg, 2)
        with self.assertRaises(ValueError):
            check.validate_scene(rows[:-1], cfg, 2)


if __name__ == '__main__':
    unittest.main()
