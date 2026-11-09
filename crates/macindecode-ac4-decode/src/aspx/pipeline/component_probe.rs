//! 只读观察实际 HF 生成、增益、噪声、音调与组装出口；仅显式启用的测试线程收集。
extern crate std;

use crate::aspx::{
    bands::AspxBandTables, frames::AspxInterval, hfgain::AdjustedGains, qmf::QmfSlot,
};
use std::{cell::RefCell, collections::VecDeque, vec::Vec};

#[derive(Debug)]
pub(crate) struct Components {
    pub sbx: u8,
    pub sbz: u8,
    pub factor: u8,
    pub borders: Vec<i16>,
    pub signal_gains: Vec<f32>,
    pub noise_enabled: Vec<u8>,
    pub high: Vec<QmfSlot>,
    pub noise: Vec<QmfSlot>,
    pub sine: Vec<QmfSlot>,
    pub assembled: Vec<QmfSlot>,
}

std::thread_local! {
    static ACTIVE: RefCell<Option<VecDeque<Components>>> = const { RefCell::new(None) };
}

pub(crate) fn begin() {
    ACTIVE.with(|cell| {
        assert!(cell.borrow().is_none());
        *cell.borrow_mut() = Some(VecDeque::new());
    });
}

pub(crate) fn end() {
    ACTIVE.with(|cell| {
        if let Some(pending) = cell.borrow_mut().take() {
            assert!(pending.is_empty(), "每次 HF 组装必须绑定到实际输出源");
        }
    });
}

#[expect(
    clippy::too_many_arguments,
    reason = "只读记录同一次组装的真实输入和输出"
)]
pub(super) fn observe(
    gains: &AdjustedGains,
    bands: &AspxBandTables,
    interval: &AspxInterval,
    factor: u8,
    high: &[QmfSlot],
    noise: &[QmfSlot],
    sine: &[QmfSlot],
    assembled: &[QmfSlot],
) {
    ACTIVE.with(|cell| {
        if let Some(pending) = cell.borrow_mut().as_mut() {
            let envelopes = usize::from(gains.envelopes());
            let subbands = usize::from(gains.subbands());
            pending.push_back(Components {
                sbx: bands.sbx(),
                sbz: bands.sbz(),
                factor,
                borders: (0..=envelopes)
                    .map(|e| interval.sig_border(e).unwrap())
                    .collect(),
                signal_gains: (0..envelopes)
                    .flat_map(|e| (0..subbands).map(move |b| gains.signal_gain(b, e).unwrap()))
                    .collect(),
                noise_enabled: (0..envelopes)
                    .flat_map(|e| {
                        (0..subbands).map(move |b| u8::from(gains.noise_enabled(b, e).unwrap()))
                    })
                    .collect(),
                high: high.to_vec(),
                noise: noise.to_vec(),
                sine: sine.to_vec(),
                assembled: assembled.to_vec(),
            });
        }
    });
}

pub(crate) fn bind_output(produced: &[QmfSlot]) -> Option<Components> {
    ACTIVE.with(|cell| {
        let component = cell.borrow_mut().as_mut()?.pop_front()?;
        let range = usize::from(component.sbx)..usize::from(component.sbz);
        assert!(component.assembled.len() >= produced.len());
        for (actual, assembled) in produced.iter().zip(&component.assembled) {
            for band in range.clone() {
                assert_eq!(
                    actual.re.get(band).unwrap().to_bits(),
                    assembled.re.get(band).unwrap().to_bits()
                );
                assert_eq!(
                    actual.im.get(band).unwrap().to_bits(),
                    assembled.im.get(band).unwrap().to_bits()
                );
            }
        }
        Some(component)
    })
}
