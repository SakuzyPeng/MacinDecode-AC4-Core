//! 反量化谱线的 MDCT 声道矩阵。仅 audio-decode 可用。
use super::*;

impl ChannelElement {
    /// 对已经反量化、仍按窗口组排列的谱线应用 MDCT 域声道矩阵。
    ///
    /// 两声道元素按 `5.3.3.2` 使用 `chparam_info()` 的逐频带系数；三声道元素按
    /// `5.3.3.3` 表 178 组合两份系数。`b_enable_mdct_stereo_proc == false` 与单声道
    /// 元素保持原样。矩阵必须发生在 [`crate::asf::reconstruct::ungroup_spectrum`]
    /// 和 IMDCT 之前。
    ///
    /// `spectra` 的前 [`Self::channels`] 项分别对应 `sf_data()` 的输入次序，每项
    /// 至少包含该声道布局的 [`AsfWindowLayout::total_lines`] 条谱线。
    ///
    /// # Errors
    ///
    /// 元素状态不完整、SAP 差值缺失、三声道选择码为保留值，或任一缓冲不足时
    /// 返回 [`ChannelMatrixError`]。所有可由输入判断的错误都在改写谱线前返回。
    pub fn apply_channel_matrix(
        &self,
        spectra: &mut [&mut [f32]; MAX_ELEMENT_CHANNELS],
    ) -> Result<(), ChannelMatrixError> {
        let channels = usize::from(self.channels);
        if !(1..=MAX_ELEMENT_CHANNELS).contains(&channels) {
            return Err(ChannelMatrixError::UnsupportedChannelCount {
                channels: self.channels,
            });
        }
        for channel in 0..channels {
            let layout = self
                .layout(channel)
                .ok_or(ChannelMatrixError::MissingLayout { channel })?;
            let needed = usize::try_from(layout.total_lines()).unwrap_or(usize::MAX);
            let provided = spectra.get(channel).map(|values| values.len()).unwrap_or(0);
            if provided < needed {
                return Err(ChannelMatrixError::SpectrumTooSmall {
                    channel,
                    needed,
                    provided,
                });
            }
        }

        let [first, second, third] = spectra;
        match channels {
            1 => Ok(()),
            2 => {
                let enabled = self
                    .mdct_stereo_proc
                    .ok_or(ChannelMatrixError::MissingMdctStereoFlag)?;
                if !enabled {
                    return Ok(());
                }
                let layout = self
                    .layout(0)
                    .ok_or(ChannelMatrixError::MissingLayout { channel: 0 })?;
                validate_band_ranges(layout)?;
                let parameters =
                    self.stereo_params(0)
                        .ok_or(ChannelMatrixError::MissingStereoParameters {
                            needed: 1,
                            provided: usize::from(self.stereo_count),
                        })?;
                let alpha = reconstruct_sap_alpha(parameters, 0, layout)?;
                apply_two_channel_matrix(layout, parameters, &alpha, first, second)
            }
            3 => {
                let selector = self.chel_matsel.unwrap_or(u8::MAX);
                if selector > 11 {
                    return Err(ChannelMatrixError::ReservedMatrixSelector { selector });
                }
                let layout = self
                    .layout(0)
                    .ok_or(ChannelMatrixError::MissingLayout { channel: 0 })?;
                validate_band_ranges(layout)?;
                let first_parameters =
                    self.stereo_params(0)
                        .ok_or(ChannelMatrixError::MissingStereoParameters {
                            needed: 2,
                            provided: usize::from(self.stereo_count),
                        })?;
                let second_parameters =
                    self.stereo_params(1)
                        .ok_or(ChannelMatrixError::MissingStereoParameters {
                            needed: 2,
                            provided: usize::from(self.stereo_count),
                        })?;
                let first_alpha = reconstruct_sap_alpha(first_parameters, 0, layout)?;
                let second_alpha = reconstruct_sap_alpha(second_parameters, 1, layout)?;
                apply_three_channel_matrix(
                    layout,
                    selector,
                    (first_parameters, &first_alpha),
                    (second_parameters, &second_alpha),
                    first,
                    second,
                    third,
                )
            }
            _ => unreachable!("channel count was restricted to 1..3 at entry"),
        }
    }
}

/// 一份 `chparam_info()` 在一个时频 tile 上展开出的 2×2 系数。
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct StereoMatrix {
    pub(super) a: f32,
    pub(super) b: f32,
    pub(super) c: f32,
    pub(super) d: f32,
}

impl StereoMatrix {
    pub(super) const IDENTITY: Self = Self {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
    };
    pub(super) const MID_SIDE: Self = Self {
        a: 1.0,
        b: 1.0,
        c: 1.0,
        d: -1.0,
    };
}

type SapAlpha = [[i16; MAX_SFB]; MAX_WINDOWS];

/// `5.3.2` `Pseudocode 59`：把 SAP 的频率/时间差分还原为逐带 alpha。
pub(super) fn reconstruct_sap_alpha(
    parameters: &ChparamInfo,
    parameter: usize,
    layout: &AsfWindowLayout,
) -> Result<SapAlpha, ChannelMatrixError> {
    let mut alpha = [[0i16; MAX_SFB]; MAX_WINDOWS];
    if parameters.sap_mode != 3 {
        return Ok(alpha);
    }
    let sap = parameters
        .sap()
        .ok_or(ChannelMatrixError::MissingSapData { parameter })?;
    let mut max_sfb_previous = layout.max_sfb(0).unwrap_or(0);

    for group in 0..usize::from(layout.num_window_groups()) {
        let max_sfb = layout.max_sfb(group).unwrap_or(0);
        for sfb in 0..usize::from(max_sfb) {
            if sap.coeff_used(group, sfb) != Some(true) {
                continue;
            }
            let value = if sfb % 2 == 1 {
                alpha
                    .get(group)
                    .and_then(|row| row.get(sfb.saturating_sub(1)))
                    .copied()
                    .unwrap_or(0)
            } else {
                let symbol = sap.dpcm_alpha_q(group, sfb).ok_or(
                    ChannelMatrixError::MissingSapAlphaDelta {
                        parameter,
                        group: u8::try_from(group).unwrap_or(u8::MAX),
                        sfb: u8::try_from(sfb).unwrap_or(u8::MAX),
                    },
                )?;
                let delta = i16::from(symbol).saturating_sub(60);
                let code_in_time = group > 0
                    && max_sfb == max_sfb_previous
                    && sap.delta_code_time.unwrap_or(false);
                if code_in_time {
                    alpha
                        .get(group.saturating_sub(1))
                        .and_then(|row| row.get(sfb))
                        .copied()
                        .unwrap_or(0)
                        .saturating_add(delta)
                } else if sfb == 0 {
                    delta
                } else {
                    alpha
                        .get(group)
                        .and_then(|row| row.get(sfb.saturating_sub(2)))
                        .copied()
                        .unwrap_or(0)
                        .saturating_add(delta)
                }
            };
            if let Some(slot) = alpha.get_mut(group).and_then(|row| row.get_mut(sfb)) {
                *slot = value;
            }
        }
        max_sfb_previous = max_sfb;
    }
    Ok(alpha)
}

/// `5.3.2` `Pseudocode 59`：为一个时频 tile 选择四个矩阵系数。
pub(super) fn stereo_matrix(
    parameters: &ChparamInfo,
    alpha: &SapAlpha,
    group: usize,
    sfb: usize,
) -> StereoMatrix {
    match parameters.sap_mode {
        0 => StereoMatrix::IDENTITY,
        1 if parameters.ms_used(group, sfb) == Some(true) => StereoMatrix::MID_SIDE,
        1 => StereoMatrix::IDENTITY,
        2 => StereoMatrix::MID_SIDE,
        3 if parameters.sap().and_then(|sap| sap.coeff_used(group, sfb)) == Some(true) => {
            let quantized = alpha
                .get(group)
                .and_then(|row| row.get(sfb))
                .copied()
                .unwrap_or(0);
            let gain = f32::from(quantized) * 0.1;
            StereoMatrix {
                a: 1.0 + gain,
                b: 1.0,
                c: 1.0 - gain,
                d: -1.0,
            }
        }
        3 => StereoMatrix::IDENTITY,
        _ => StereoMatrix::IDENTITY,
    }
}

pub(super) fn band_range(
    layout: &AsfWindowLayout,
    group: usize,
    sfb: usize,
) -> Result<(usize, usize), ChannelMatrixError> {
    let invalid = || ChannelMatrixError::InvalidBandRange {
        group: u8::try_from(group).unwrap_or(u8::MAX),
        sfb: u8::try_from(sfb).unwrap_or(u8::MAX),
    };
    let start = usize::from(layout.sect_sfb_offset(group, sfb).ok_or_else(invalid)?);
    let end = usize::from(
        layout
            .sect_sfb_offset(group, sfb.saturating_add(1))
            .ok_or_else(invalid)?,
    );
    let total = usize::try_from(layout.total_lines()).unwrap_or(usize::MAX);
    if start > end || end > total {
        return Err(invalid());
    }
    Ok((start, end))
}

/// 在任何谱线被改写之前遍历一次全部范围，使矩阵处理具备事务性。
pub(super) fn validate_band_ranges(layout: &AsfWindowLayout) -> Result<(), ChannelMatrixError> {
    for group in 0..usize::from(layout.num_window_groups()) {
        for sfb in 0..usize::from(layout.max_sfb(group).unwrap_or(0)) {
            band_range(layout, group, sfb)?;
        }
    }
    Ok(())
}

pub(super) fn apply_two_channel_matrix(
    layout: &AsfWindowLayout,
    parameters: &ChparamInfo,
    alpha: &SapAlpha,
    first: &mut [f32],
    second: &mut [f32],
) -> Result<(), ChannelMatrixError> {
    for group in 0..usize::from(layout.num_window_groups()) {
        for sfb in 0..usize::from(layout.max_sfb(group).unwrap_or(0)) {
            let coefficients = stereo_matrix(parameters, alpha, group, sfb);
            let (start, end) = band_range(layout, group, sfb)?;
            let invalid = || ChannelMatrixError::InvalidBandRange {
                group: u8::try_from(group).unwrap_or(u8::MAX),
                sfb: u8::try_from(sfb).unwrap_or(u8::MAX),
            };
            let first_band = first.get_mut(start..end).ok_or_else(invalid)?;
            let second_band = second.get_mut(start..end).ok_or_else(invalid)?;
            for (output0, output1) in first_band.iter_mut().zip(second_band.iter_mut()) {
                let (input0, input1) = (*output0, *output1);
                *output0 = coefficients.a * input0 + coefficients.b * input1;
                *output1 = coefficients.c * input0 + coefficients.d * input1;
            }
        }
    }
    Ok(())
}

/// 表 178。两份 2×2 系数按 `chel_matsel` 组合为一个 3×3 矩阵。
pub(super) fn three_channel_matrix(
    selector: u8,
    first: StereoMatrix,
    second: StereoMatrix,
) -> Option<[[f32; 3]; 3]> {
    let StereoMatrix {
        a: a0,
        b: b0,
        c: c0,
        d: d0,
    } = first;
    let StereoMatrix {
        a: a1,
        b: b1,
        c: c1,
        d: d1,
    } = second;
    match selector {
        0 => Some([
            [a0 * a1, b0 * a1, b1],
            [c0, d0, 0.0],
            [a0 * c1, b0 * c1, d1],
        ]),
        1 => Some([
            [d0, c0, 0.0],
            [b0 * a1, a0 * a1, b1],
            [b0 * c1, a0 * c1, d1],
        ]),
        2 => Some([
            [a0 * a1, b1, b0 * a1],
            [a0 * c1, d1, b0 * c1],
            [c0, 0.0, d0],
        ]),
        3 => Some([
            [a1, c0 * b1, d0 * b1],
            [0.0, a0, b0],
            [c1, c0 * d1, d0 * d1],
        ]),
        4 => Some([
            [a0, 0.0, b0],
            [c0 * b1, a1, d0 * b1],
            [c0 * d1, c1, d0 * d1],
        ]),
        5 => Some([
            [a1, d0 * b1, c0 * b1],
            [c1, d0 * d1, c0 * d1],
            [0.0, b0, a0],
        ]),
        6 => Some([
            [d0 * d1, c0 * d1, c1],
            [b0, a0, 0.0],
            [d0 * b1, c0 * b1, a1],
        ]),
        7 => Some([
            [a0, b0, 0.0],
            [c0 * d1, d0 * d1, c1],
            [c0 * b1, d0 * b1, a1],
        ]),
        8 => Some([
            [d0 * d1, c1, c0 * d1],
            [d0 * b1, a1, c0 * b1],
            [b0, 0.0, a0],
        ]),
        9 => Some([
            [d1, b0 * c1, a0 * c1],
            [0.0, d0, c0],
            [b1, b0 * a1, a0 * a1],
        ]),
        10 => Some([
            [d0, 0.0, c0],
            [b0 * c1, d1, a0 * c1],
            [b0 * a1, b1, a0 * a1],
        ]),
        11 => Some([
            [d1, a0 * c1, b0 * c1],
            [b1, a0 * a1, b0 * a1],
            [0.0, c0, d0],
        ]),
        _ => None,
    }
}

pub(super) fn apply_three_channel_matrix(
    layout: &AsfWindowLayout,
    selector: u8,
    first_parameters: (&ChparamInfo, &SapAlpha),
    second_parameters: (&ChparamInfo, &SapAlpha),
    first: &mut [f32],
    second: &mut [f32],
    third: &mut [f32],
) -> Result<(), ChannelMatrixError> {
    for group in 0..usize::from(layout.num_window_groups()) {
        for sfb in 0..usize::from(layout.max_sfb(group).unwrap_or(0)) {
            let first_coefficients =
                stereo_matrix(first_parameters.0, first_parameters.1, group, sfb);
            let second_coefficients =
                stereo_matrix(second_parameters.0, second_parameters.1, group, sfb);
            let coefficients =
                three_channel_matrix(selector, first_coefficients, second_coefficients)
                    .ok_or(ChannelMatrixError::ReservedMatrixSelector { selector })?;
            let [[m00, m01, m02], [m10, m11, m12], [m20, m21, m22]] = coefficients;
            let (start, end) = band_range(layout, group, sfb)?;
            let invalid = || ChannelMatrixError::InvalidBandRange {
                group: u8::try_from(group).unwrap_or(u8::MAX),
                sfb: u8::try_from(sfb).unwrap_or(u8::MAX),
            };
            let first_band = first.get_mut(start..end).ok_or_else(invalid)?;
            let second_band = second.get_mut(start..end).ok_or_else(invalid)?;
            let third_band = third.get_mut(start..end).ok_or_else(invalid)?;
            for ((output0, output1), output2) in first_band
                .iter_mut()
                .zip(second_band.iter_mut())
                .zip(third_band.iter_mut())
            {
                let (input0, input1, input2) = (*output0, *output1, *output2);
                *output0 = m00 * input0 + m01 * input1 + m02 * input2;
                *output1 = m10 * input0 + m11 * input1 + m12 * input2;
                *output2 = m20 * input0 + m21 * input1 + m22 * input2;
            }
        }
    }
    Ok(())
}
