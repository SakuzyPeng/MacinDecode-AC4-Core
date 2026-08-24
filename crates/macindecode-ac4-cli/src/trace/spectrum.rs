//! 统一 Full engine 的 ASF trace observation 累计值。
//!
//! CLI 不再持有第二套缩放、解组、IMDCT 工作区或 overlap；这里仅保存稳定
//! `trace` JSON 契约需要的计数与首个错误。

#[cfg(feature = "audio-decode")]
#[derive(Debug, Default)]
pub(crate) struct ScaledStats {
    /// 写出的谱线总数与绝对值峰值。
    pub(super) lines: u64,
    pub(super) peak: f32,
    /// 缩放后非有限值个数。
    pub(super) nonfinite: u64,
    /// 缩放返回错误的声道次数及首个错误。
    pub(super) scale_failures: u64,
    pub(super) scale_first_error: Option<String>,
    /// 解组后写出的谱线总数。
    pub(super) ungrouped_lines: u64,
    /// 解组返回错误的声道次数及首个错误。
    pub(super) ungroup_failures: u64,
    pub(super) ungroup_first_error: Option<String>,
    /// 解组前后非零谱线数不等的次数。
    pub(super) ungroup_count_mismatch: u64,
    /// 解组前后平方和的最大相对偏差。
    pub(super) ungroup_energy_drift: f64,
    /// 合成出的 PCM 样本总数与声道帧数。
    pub(super) pcm_samples: u64,
    pub(super) pcm_frames: u64,
    /// PCM 绝对值峰值与非有限值个数。
    pub(super) pcm_peak: f32,
    pub(super) pcm_nonfinite: u64,
    /// 合成返回错误的次数及首个错误。
    pub(super) synthesis_failures: u64,
    pub(super) synthesis_first_error: Option<String>,
    /// 谱线全零的声道帧数。
    pub(super) silent_input_frames: u64,
    /// 当前谱线非零、当前 PCM 输出全零的声道帧数。
    pub(super) zero_output_with_nonzero_input_frames: u64,
}
