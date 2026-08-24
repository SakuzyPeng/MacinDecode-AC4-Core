//! 两种导出格式共享的确定性粉红噪声与整数时间缩放。

use crate::metadata_batch::MetadataElement;
use crate::scene_export::scene_selector;

pub(crate) const OUTPUT_SAMPLE_RATE: u32 = 48_000;
pub(crate) const BYTES_PER_SAMPLE: u64 = 3;
pub(crate) const SAMPLE_MAX: f64 = 8_388_607.0;
pub(crate) const MAX_PROBE_OBJECTS: usize = 118;

#[derive(Debug, Clone)]
pub(crate) struct PinkNoise {
    rng: u32,
    rows: [i32; 16],
    counter: u32,
}

impl PinkNoise {
    pub(crate) fn new(seed: u32) -> Self {
        let mut out = Self {
            rng: seed.max(1),
            rows: [0; 16],
            counter: 0,
        };
        for index in 0..out.rows.len() {
            let value = out.random_i16();
            if let Some(slot) = out.rows.get_mut(index) {
                *slot = value;
            }
        }
        out
    }

    pub(crate) fn next(&mut self) -> f64 {
        self.counter = self.counter.wrapping_add(1);
        let row = self.counter.trailing_zeros().min(15) as usize;
        let value = self.random_i16();
        if let Some(slot) = self.rows.get_mut(row) {
            *slot = value;
        }
        let white = self.random_i16();
        let sum = self.rows.iter().fold(i64::from(white), |acc, value| {
            acc.saturating_add(i64::from(*value))
        });
        sum as f64 / (17.0 * 32_768.0)
    }

    fn random_i16(&mut self) -> i32 {
        let mut value = self.rng;
        value ^= value << 13;
        value ^= value >> 17;
        value ^= value << 5;
        self.rng = value.max(1);
        i32::from((value >> 16) as u16).saturating_sub(32_768)
    }
}

pub(crate) fn selector_seed(scene: &MetadataElement) -> u32 {
    let mut hash = 2_166_136_261u32;
    for byte in scene_selector(scene).bytes() {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(16_777_619);
    }
    hash.max(1)
}

pub(crate) fn rescale_u64(value: u64, source_rate: u32, target_rate: u32) -> Result<u64, String> {
    if source_rate == 0 {
        return Err("源采样率为零".to_owned());
    }
    let numerator = u128::from(value)
        .checked_mul(u128::from(target_rate))
        .ok_or("时间缩放乘法溢出")?
        .checked_add(u128::from(source_rate / 2))
        .ok_or("时间缩放舍入溢出")?;
    let scaled = numerator
        .checked_div(u128::from(source_rate))
        .ok_or("源采样率为零")?;
    u64::try_from(scaled).map_err(|_| "时间缩放结果溢出".to_owned())
}
