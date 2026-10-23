//! 受控构造流的有界字段位置，仅在显式测试驱动中启用。
extern crate std;
use macindecode_ac4_bitstream::{
    oamd::{OamdCommonData, OamdTimingData},
    reader::BitReader,
};
use std::{
    cell::RefCell,
    fs::OpenOptions,
    io::{BufWriter, Write},
    path::Path,
};

struct Location {
    writer: BufWriter<std::fs::File>,
    au: usize,
    base: usize,
    size: usize,
    audio: Option<(u64, u64)>,
}
std::thread_local! { static LOCATION: RefCell<Option<Location>> = const { RefCell::new(None) }; }

pub(crate) fn select(root: &Path, au: usize, data: &[u8]) {
    finish();
    LOCATION.with(|c| {
        *c.borrow_mut() = Some(Location {
            writer: BufWriter::new(
                OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(root.join("fields.jsonl"))
                    .unwrap(),
            ),
            au,
            base: data.as_ptr() as usize,
            size: data.len(),
            audio: None,
        })
    });
}
pub(crate) fn finish() {
    LOCATION.with(|c| {
        if let Some(mut l) = c.borrow_mut().take() {
            l.writer.flush().unwrap();
        }
    });
}
fn region(l: &Location, payload: &[u8]) -> (u64, u64) {
    let byte = (payload.as_ptr() as usize).checked_sub(l.base).unwrap();
    let end = byte.checked_add(payload.len()).unwrap();
    assert!(end <= l.size);
    (
        u64::try_from(byte).unwrap().checked_mul(8).unwrap(),
        u64::try_from(end).unwrap().checked_mul(8).unwrap(),
    )
}
pub(crate) fn audio_region(payload: &[u8]) {
    LOCATION.with(|c| {
        if let Some(l) = c.borrow_mut().as_mut() {
            l.audio = Some(region(l, payload));
        }
    });
}
fn field(
    l: &mut Location,
    name: &str,
    point: usize,
    position: u64,
    width: u32,
    value: u8,
    bounds: (u64, u64),
) {
    assert!(position >= bounds.0 && position.checked_add(u64::from(width)).unwrap() <= bounds.1);
    writeln!(l.writer,"{{\"au\":{},\"field\":\"{name}\",\"point\":{point},\"bit\":{position},\"width\":{width},\"value\":{value},\"region\":[{},{}]}}",l.au,bounds.0,bounds.1).unwrap();
}
pub(crate) fn ajoc(position: u64, point: usize, start: u8, ramp: u8) {
    LOCATION.with(|c| {
        if let Some(l) = c.borrow_mut().as_mut() {
            let bounds = l.audio.unwrap();
            let bit = bounds.0.checked_add(position).unwrap();
            field(l, "ajoc_start_pos", point, bit, 5, start, bounds);
            field(
                l,
                "ajoc_ramp_len_minus1",
                point,
                bit.checked_add(5).unwrap(),
                6,
                ramp,
                bounds,
            );
        }
    });
}
pub(crate) fn group(payload: &[u8], expected: Option<OamdTimingData>) {
    LOCATION.with(|c| {
        let mut binding = c.borrow_mut();
        let Some(l) = binding.as_mut() else { return };
        let bounds = region(l, payload);
        let mut reader = BitReader::new(payload);
        if reader.read_flag().unwrap() {
            OamdCommonData::parse(&mut reader).unwrap();
        }
        if !reader.read_flag().unwrap() {
            assert!(expected.is_none());
            return;
        }
        let mut verify = reader.clone();
        assert_eq!(Some(OamdTimingData::parse(&mut verify).unwrap()), expected);
        assert!(
            !reader.read_flag().unwrap(),
            "此等长协议仅接受 implicit sample_offset"
        );
        let count = reader.read_bits(3).unwrap();
        assert_eq!(count, 1);
        for point in 0..usize::try_from(count).unwrap() {
            let bit = bounds.0.checked_add(reader.bit_position()).unwrap();
            let factor = u8::try_from(reader.read_bits(6).unwrap()).unwrap();
            field(l, "block_offset_factor", point, bit, 6, factor, bounds);
            assert_eq!(reader.read_bits(2).unwrap(), 3);
            assert!(reader.read_flag().unwrap(), "保持现有 table 编码路径");
            let bit = bounds.0.checked_add(reader.bit_position()).unwrap();
            let index = u8::try_from(reader.read_bits(4).unwrap()).unwrap();
            field(l, "ramp_duration_table_index", point, bit, 4, index, bounds);
        }
        assert_eq!(reader.bit_position(), verify.bit_position());
    });
}
