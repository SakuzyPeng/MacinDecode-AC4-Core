"""自有 sync-frame／时间字段和事件模型，不需要媒体、PDF 或参考库。"""
import copy
import hashlib
import unittest
import struct
import tempfile
from pathlib import Path
import numpy as np
from scripts.check_ajoc_timing_stream import configurations,crc16,sync_frames,validate_fields,make_variant,verify_variant,NativeTimeline
from scripts.check_ajoc_timing_stream import prefix_digest,read_scene_export


def owned_frame(crc=False):
    payload=bytearray(32);data=bytearray((b'\xac\x41' if crc else b'\xac\x40')+len(payload).to_bytes(2,'big')+payload)
    def bits(at,width,value):
        for i in range(width):
            b=at+i;data[b//8]|=((value>>(width-1-i))&1)<<(7-b%8)
    bits(33,1,1);bits(35,3,1);bits(44,3,7);bits(47,4,15)
    bits(64,2,1);bits(71,6,31)
    if crc:data+=crc16(data[2:]).to_bytes(2,'big')
    fields=[dict(au=1,field=k,point=0,bit=bit,width=w,value=value,region=[32,288]) for k,bit,w,value in [
        ('block_offset_factor',38,6,0),('ramp_duration_table_index',47,4,15),('ajoc_start_pos',66,5,0),('ajoc_ramp_len_minus1',71,6,31)]]
    return bytes(data),fields


def config(mode='ajoc',start=1,ramp=2):
    return dict(id='owned',source='owned',raw_au=1,consumer_au=2,mode=mode,start=start,ramp=ramp)


class TimingStreamTests(unittest.TestCase):
    def test_unchanged_prefix_excludes_unrelated_runtime_output(self):
        with tempfile.TemporaryDirectory() as directory:
            p=Path(directory)/'pcm';p.write_bytes(b'prefixsuffix')
            expected=prefix_digest(p,6);p.write_bytes(b'prefixchanged')
            self.assertEqual(prefix_digest(p,6),expected)
            p.write_bytes(b'brokenchanged');self.assertNotEqual(prefix_digest(p,6),expected)
            for size in [-1,100]:
                with self.assertRaises(ValueError):prefix_digest(p,size)

    def test_scene_coordinate_is_retained_without_assuming_sample_units(self):
        data=struct.pack('<4sIIIQqIHH',b'MASC',1,48000,21,1,42,64,1,0)+bytes(16)
        with tempfile.TemporaryDirectory() as directory:
            p=Path(directory)/'scene';p.write_bytes(data)
            self.assertEqual(read_scene_export(p)[0]['coordinate'],42)
            self.assertEqual(read_scene_export(p)[0]['ramp'],64)
            for invalid in [data[:10],data[:-1],data+b'0']:
                p.write_bytes(invalid)
                with self.assertRaises(ValueError):read_scene_export(p)
    def test_fixed_configuration_coverage(self):
        cases=configurations();self.assertEqual(len(cases),38);self.assertEqual(len({c['id'] for c in cases}),38)
        self.assertEqual(sum(c['mode']=='both' for c in cases),3)
        self.assertEqual(sum(c['mode']=='oamd' for c in cases),3)

    def test_crc_known_answer_and_round_trip(self):
        self.assertEqual(crc16(b'123456789'),0xfee8)
        a,fields=owned_frame(True);b,proof=make_variant(a,fields,config('both'))
        self.assertEqual(len(sync_frames(b)),1);self.assertEqual(proof['crc_aus'],[1])
        self.assertTrue(verify_variant(a,b,proof)['protected_bits_unchanged'])

    def test_noop_is_byte_exact(self):
        for crc in (False,True):
            a,fields=owned_frame(crc);b,proof=make_variant(a,fields,config(start=0,ramp=32))
            self.assertEqual(a,b);self.assertEqual(verify_variant(a,b,proof)['changed_bits'],0)

    def test_separate_ajoc_metadata_and_paired_changes(self):
        a,fields=owned_frame()
        for mode,names in [('ajoc',{'ajoc_start_pos','ajoc_ramp_len_minus1'}),('oamd',{'block_offset_factor','ramp_duration_table_index'}),('both',{f['field'] for f in fields})]:
            _,p=make_variant(a,fields,config(mode));self.assertEqual({x['field'] for x in p['patches']},names)

    def test_rejects_truncated_header_payload_and_crc(self):
        a,_=owned_frame(True)
        for data in [b'',a[:2],a[:-1],a[:-2],a[:-1]+bytes([a[-1]^1])]:
            with self.assertRaises(ValueError):sync_frames(data)

    def test_rejects_wrong_source_and_old_value(self):
        a,f=owned_frame();b,p=make_variant(a,f,config());p['source_sha256']='0'*64
        with self.assertRaises(ValueError):verify_variant(a,b,p)
        f[0]['value']=1
        with self.assertRaises(ValueError):validate_fields(a,f)

    def test_rejects_wrong_bit_missing_and_duplicate_fields(self):
        a,f=owned_frame()
        bad=copy.deepcopy(f);bad[2]['bit']+=1
        for fields in [f[:-1],f+[f[0]],bad]:
            with self.assertRaises(ValueError):validate_fields(a,fields)

    def test_rejects_field_region_escape_and_nonwhitelisted_bits(self):
        a,f=owned_frame();bad=copy.deepcopy(f);bad[0]['region']=[0,288]
        with self.assertRaises(ValueError):validate_fields(a,bad)
        b,p=make_variant(a,f,config());b=bytearray(b);b[20]^=1;p['variant_sha256']=hashlib.sha256(b).hexdigest()
        with self.assertRaises(ValueError):verify_variant(a,bytes(b),p)

    def test_rejects_invalid_semantics_and_unrepresentable_metadata(self):
        a,f=owned_frame()
        for c in [config(start=32),config(ramp=0),config(ramp=65),config('oamd',ramp=31),config('unknown')]:
            with self.assertRaises(ValueError):make_variant(a,f,c)

    def test_prefix_configuration_cannot_rebind_raw_au(self):
        a,f=owned_frame();b,p=make_variant(a,f,config());p['configuration']['raw_au']=2
        with self.assertRaises(ValueError):verify_variant(a,b,p)

    def test_immediate_first_increment_and_exact_count(self):
        m=NativeTimeline([0],[0],0,0)
        self.assertEqual(m.slot([2],2).tolist(),[1]);self.assertEqual(m.slot().tolist(),[2])
        self.assertEqual(m.slot().tolist(),[2]);self.assertEqual(m.completed,2)

    def test_cross_frame_holds_and_interrupted_ramp(self):
        m=NativeTimeline([0],[0],0,0);m.slot([64],64)
        for _ in range(31):m.slot()
        self.assertEqual(m.current.tolist(),[32]);self.assertEqual(m.completed,32)
        self.assertEqual(m.slot([0],2).tolist(),[16]);self.assertEqual(m.slot().tolist(),[0])

    def test_reference_endpoint_does_not_snap(self):
        m=NativeTimeline([0],[0],0,0);target=np.array([1],'<f4');m.slot(target,10)
        for _ in range(9):m.slot()
        self.assertNotEqual(m.current.tobytes(),target.tobytes());before=m.current.copy();m.slot()
        self.assertEqual(m.current.tobytes(),before.tobytes())

    def test_bad_reference_cursor_and_target_fail(self):
        with self.assertRaises(ValueError):NativeTimeline([0],[0],33,32)
        m=NativeTimeline([0],[0],0,0)
        for target,length in [([np.nan],1),([1,2],1),([1],0),([1],65)]:
            with self.assertRaises(ValueError):m.slot(target,length)


if __name__=='__main__':unittest.main()
