//! 只读观察实际 HF 生成、增益、噪声、音调与组装出口；仅显式启用的测试线程收集。
extern crate std;

use crate::aspx::{
    bands::AspxBandTables, frames::AspxInterval, hfadjust::EnvelopeEstimate, hfgain::AdjustedGains,
    limiter::LimiterTable, qmf::QmfSlot,
};
use std::{cell::RefCell, collections::VecDeque, format, string::String, vec::Vec};

#[derive(Debug)]
pub(crate) struct GainAudit {
    interpolation: bool,
    signal_borders: Vec<Vec<u8>>,
    limiter_borders: Vec<u8>,
    estimated: Vec<f32>,
    target: Vec<f32>,
    ratio: Vec<f32>,
    sine_index: Vec<bool>,
    sine_area: Vec<bool>,
    onset: Vec<bool>,
    initial_noise: Vec<f32>,
    initial_sine: Vec<f32>,
    signal: Vec<f32>,
    noise: Vec<f32>,
    sine: Vec<f32>,
    boost: Vec<f32>,
}

impl GainAudit {
    pub(crate) fn json(&self) -> String {
        format!(
            "{{\"interpolation\":{},\"signal_borders\":{:?},\"limiter_borders\":{:?},\"estimated\":{:?},\"target\":{:?},\"ratio\":{:?},\"sine_index\":{:?},\"sine_area\":{:?},\"onset\":{:?},\"initial_noise\":{:?},\"initial_sine\":{:?},\"signal\":{:?},\"noise\":{:?},\"sine\":{:?},\"boost\":{:?}}}",
            self.interpolation,
            self.signal_borders,
            self.limiter_borders,
            self.estimated,
            self.target,
            self.ratio,
            self.sine_index,
            self.sine_area,
            self.onset,
            self.initial_noise,
            self.initial_sine,
            self.signal,
            self.noise,
            self.sine,
            self.boost,
        )
    }
}

#[derive(Default)]
struct Capture {
    gains: VecDeque<GainAudit>,
    components: VecDeque<Components>,
}

#[derive(Debug)]
pub(crate) struct Components {
    pub sbx: u8,
    pub sbz: u8,
    pub factor: u8,
    pub borders: Vec<i16>,
    pub signal_gains: Vec<f32>,
    pub noise_enabled: Vec<u8>,
    pub noise_cursors: [u16; 2],
    pub high: Vec<QmfSlot>,
    pub noise: Vec<QmfSlot>,
    pub sine: Vec<QmfSlot>,
    pub assembled: Vec<QmfSlot>,
    pub gain_audit: GainAudit,
}

std::thread_local! {
    static ACTIVE: RefCell<Option<Capture>> = const { RefCell::new(None) };
}

pub(crate) fn begin() {
    ACTIVE.with(|cell| {
        assert!(cell.borrow().is_none());
        *cell.borrow_mut() = Some(Capture::default());
    });
}

pub(crate) fn end() {
    ACTIVE.with(|cell| {
        if let Some(pending) = cell.borrow_mut().take() {
            assert!(pending.gains.is_empty(), "每份实际增益必须由组装消费");
            assert!(
                pending.components.is_empty(),
                "每次 HF 组装必须绑定到实际输出源"
            );
        }
    });
}

pub(super) fn gain(
    bands: &AspxBandTables,
    interval: &AspxInterval,
    interpolation: bool,
    limiter: Option<&LimiterTable>,
    estimate: &EnvelopeEstimate,
    gains: &AdjustedGains,
) {
    ACTIVE.with(|cell| {
        if let Some(pending) = cell.borrow_mut().as_mut() {
            let envelopes = usize::from(gains.envelopes());
            let subbands = usize::from(gains.subbands());
            let indices = || (0..envelopes).flat_map(|e| (0..subbands).map(move |b| (b, e)));
            pending.gains.push_back(GainAudit {
                interpolation,
                signal_borders: (0..envelopes)
                    .map(|e| {
                        if interval.freq_res(e).unwrap() {
                            (0..=usize::from(bands.num_sbg_sig_highres()))
                                .map(|i| bands.sig_highres_border(i).unwrap())
                                .collect()
                        } else {
                            (0..=usize::from(bands.num_sbg_sig_lowres()))
                                .map(|i| bands.sig_lowres_border(i).unwrap())
                                .collect()
                        }
                    })
                    .collect(),
                limiter_borders: limiter.map_or_else(Vec::new, |table| {
                    (0..=usize::from(table.count()))
                        .map(|i| table.border(i).unwrap())
                        .collect()
                }),
                estimated: indices()
                    .map(|(b, e)| estimate.estimated(b, e).unwrap())
                    .collect(),
                target: indices()
                    .map(|(b, e)| estimate.signal_scale(b, e).unwrap())
                    .collect(),
                ratio: indices()
                    .map(|(b, e)| estimate.noise_scale(b, e).unwrap())
                    .collect(),
                sine_index: indices()
                    .map(|(b, e)| estimate.sine_index(b, e).unwrap())
                    .collect(),
                sine_area: indices()
                    .map(|(b, e)| estimate.sine_area(b, e).unwrap())
                    .collect(),
                onset: (0..envelopes)
                    .map(|e| estimate.sine_onset(e).unwrap())
                    .collect(),
                initial_noise: indices()
                    .map(|(b, e)| estimate.noise_level(b, e).unwrap())
                    .collect(),
                initial_sine: indices()
                    .map(|(b, e)| estimate.sine_level(b, e).unwrap())
                    .collect(),
                signal: indices()
                    .map(|(b, e)| gains.signal_gain(b, e).unwrap())
                    .collect(),
                noise: indices()
                    .map(|(b, e)| gains.noise_level(b, e).unwrap())
                    .collect(),
                sine: indices()
                    .map(|(b, e)| gains.sine_level(b, e).unwrap())
                    .collect(),
                boost: indices()
                    .map(|(b, e)| gains.boost_factor(b, e).unwrap())
                    .collect(),
            });
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
    noise_cursors: [u16; 2],
    high: &[QmfSlot],
    noise: &[QmfSlot],
    sine: &[QmfSlot],
    assembled: &[QmfSlot],
) {
    ACTIVE.with(|cell| {
        if let Some(pending) = cell.borrow_mut().as_mut() {
            let envelopes = usize::from(gains.envelopes());
            let subbands = usize::from(gains.subbands());
            let audit = pending
                .gains
                .pop_front()
                .expect("组装必须消费此前的实际增益");
            let signal_gains: Vec<_> = (0..envelopes)
                .flat_map(|e| (0..subbands).map(move |b| gains.signal_gain(b, e).unwrap()))
                .collect();
            assert_eq!(audit.signal, signal_gains);
            pending.components.push_back(Components {
                sbx: bands.sbx(),
                sbz: bands.sbz(),
                factor,
                borders: (0..=envelopes)
                    .map(|e| interval.sig_border(e).unwrap())
                    .collect(),
                signal_gains,
                noise_cursors,
                noise_enabled: (0..envelopes)
                    .flat_map(|e| {
                        (0..subbands).map(move |b| u8::from(gains.noise_enabled(b, e).unwrap()))
                    })
                    .collect(),
                high: high.to_vec(),
                noise: noise.to_vec(),
                sine: sine.to_vec(),
                assembled: assembled.to_vec(),
                gain_audit: audit,
            });
        }
    });
}

pub(crate) fn bind_output(produced: &[QmfSlot]) -> Option<Components> {
    ACTIVE.with(|cell| {
        let component = cell.borrow_mut().as_mut()?.components.pop_front()?;
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
