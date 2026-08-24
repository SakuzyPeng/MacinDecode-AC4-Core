//! 对象 assignment、A-JOC 与 direct-object substream info。

use super::common::SubstreamTail;
use super::*;

/// A-JOC 完整重建中位于 LFE 之前的床声道。
///
/// P2 `5.7.2.3` 的 `Pseudocode 15` 把它们写作
/// `b_reconstruction_contains_Left/Right/Centre_channel`。规范没有定义同名的
/// 独立码流字段；这些值由完整解码侧的 `bed_dyn_obj_assignment()` 派生，映射见
/// `TS103190-2:v1.3.1:6.3.2.10.8.2` 的表 63 至表 66。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AjocReconstructionChannels {
    /// 是否含 Left 床声道。
    pub left: bool,
    /// 是否含 Right 床声道。
    pub right: bool,
    /// 是否含 Centre 床声道。
    pub centre: bool,
}

impl AjocReconstructionChannels {
    /// `Pseudocode 15` 中 LFE 的插回位置 `pos_lfe`。
    #[must_use]
    pub const fn lfe_reinsertion_position(self) -> u32 {
        (self.left as u32)
            .saturating_add(self.right as u32)
            .saturating_add(self.centre as u32)
    }

    fn from_assignment_code(code: usize) -> Self {
        // 表 63：八码都以 L、R 开头；除 2.0.0 的 code 0 外都继续含 C。
        Self {
            left: code < 8,
            right: code < 8,
            centre: code > 0 && code < 8,
        }
    }

    fn from_nonstd_flags(flags: u64) -> Self {
        // 表 64 的数组位置 16、15、14 分别是 L、R、C。
        Self {
            left: flags & (1u64 << 16) != 0,
            right: flags & (1u64 << 15) != 0,
            centre: flags & (1u64 << 14) != 0,
        }
    }

    fn from_std_flags(flags: u64) -> Self {
        // 表 65 的数组位置 9 同时分配 L/R，位置 8 分配 C。
        let left_right = flags & (1u64 << 9) != 0;
        Self {
            left: left_right,
            right: left_right,
            centre: flags & (1u64 << 8) != 0,
        }
    }

    fn include_individual(&mut self, assignment: u64) {
        // 表 66 的 0、1、2 分别是 L、R、C；其余位置不影响 pos_lfe。
        match assignment {
            0 => self.left = true,
            1 => self.right = true,
            2 => self.centre = true,
            _ => {}
        }
    }
}

/// 一组信号中各类对象的数量及 A-JOC 重建所需的床声道摘要。
///
/// 规范以数组形式给出逐个对象的类型与位置；此处保留 TOC 层所需的分类计数，
/// 以及 `Pseudocode 15` 插回 LFE 所需的 L/R/C 存在标志。其余逐对象位置仍留给
/// A-JOC 重建阶段。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ObjectAssignment {
    /// 参与分配的信号数。
    pub n_signals: u32,
    /// 床对象数。
    pub n_bed: u32,
    /// ISF 对象数。
    pub n_isf: u32,
    /// 是否只含动态对象。
    pub dynamic_only: bool,
    /// 从床声道分配派生的 L/R/C 存在标志；不是独立传输字段。
    pub reconstruction_channels: AjocReconstructionChannels,
}

impl ObjectAssignment {
    /// 动态对象数，即信号数减去静态分配掉的部分。
    #[must_use]
    pub const fn n_dynamic(&self) -> u32 {
        self.n_signals
            .saturating_sub(self.n_bed)
            .saturating_sub(self.n_isf)
    }

    /// 解析 `bed_dyn_obj_assignment(n_signals)`，见 `6.2.1.10`。
    ///
    /// 该元素只在 A-JOC 编码的 substream 中出现。按 `6.3.2.10.8.2` 的 NOTE 2，
    /// A-JOC 的床对象不含 LFE，因此规范在此处跳过 LFE 位置。
    ///
    /// # Errors
    ///
    /// 读取越界时返回错误。
    pub fn parse(reader: &mut BitReader<'_>, n_signals: u32) -> Result<Self, TopologyError> {
        let mut out = Self {
            n_signals,
            ..Self::default()
        };

        if reader.read_flag()? {
            out.dynamic_only = true;
            return Ok(out);
        }

        if reader.read_flag()? {
            let config = usize::try_from(reader.read_bits(3)?).unwrap_or(usize::MAX);
            out.n_isf = isf_object_count(config);
            return Ok(out);
        }

        if reader.read_flag()? {
            let code = usize::try_from(reader.read_bits(3)?).unwrap_or(usize::MAX);
            // A-JOC 的床不含 LFE，计数表与 direct-coded 的不同
            out.n_bed = [2u32, 3, 5, 7, 9, 7, 9, 11].get(code).copied().unwrap_or(0);
            out.reconstruction_channels = AjocReconstructionChannels::from_assignment_code(code);
            return Ok(out);
        }

        if reader.read_flag()? {
            if reader.read_flag()? {
                let flags = reader.read_bits(17)?;
                out.n_bed = count_nonstd_bed(flags, true);
                out.reconstruction_channels = AjocReconstructionChannels::from_nonstd_flags(flags);
            } else {
                let flags = reader.read_bits(10)?;
                out.n_bed = count_std_bed(flags, true);
                out.reconstruction_channels = AjocReconstructionChannels::from_std_flags(flags);
            }
            return Ok(out);
        }

        // 逐信号给出非标准床位置：每个信号 4 比特，取值 3 表示该信号不是床
        let n_bed_signals = if n_signals > 1 {
            let bits = ceil_log2(n_signals);
            u32::try_from(reader.read_bits(bits)?)
                .unwrap_or(0)
                .saturating_add(1)
        } else {
            1
        };
        for _ in 0..n_bed_signals {
            let assignment = reader.read_bits(4)?;
            if assignment != 3 {
                out.n_bed = out.n_bed.saturating_add(1);
            }
            out.reconstruction_channels.include_individual(assignment);
        }
        Ok(out)
    }
}

/// `isf_config` 对应的对象数，见 `TS103190-2:v1.3.1:表 61`。
pub(super) fn isf_object_count(config: usize) -> u32 {
    [4u32, 8, 10, 14, 15, 30].get(config).copied().unwrap_or(0)
}

/// 统计 17 比特非标准床位置标志。
///
/// 位按读取顺序对应规范循环中的 `i`；`skip_lfe` 为真时跳过 LFE 所在的
/// `i == 3` 与 `i == 16`。
pub(super) fn count_nonstd_bed(flags: u64, skip_lfe: bool) -> u32 {
    let mut count = 0u32;
    for i in 0..17u32 {
        let shift = 16u32.saturating_sub(i);
        if flags.checked_shr(shift).unwrap_or(0) & 1 == 0 {
            continue;
        }
        if skip_lfe && (i == 3 || i == 16) {
            continue;
        }
        count = count.saturating_add(1);
    }
    count
}

/// 统计 10 比特标准床位置标志，每个位置对应 1 或 2 个声道。
pub(super) fn count_std_bed(flags: u64, skip_lfe: bool) -> u32 {
    const WIDTH: [u32; 10] = [2, 1, 1, 2, 2, 2, 2, 2, 2, 1];
    let mut count = 0u32;
    for i in 0..10u32 {
        let shift = 9u32.saturating_sub(i);
        if flags.checked_shr(shift).unwrap_or(0) & 1 == 0 {
            continue;
        }
        if skip_lfe && (i == 2 || i == 9) {
            continue;
        }
        let width = WIDTH
            .get(usize::try_from(i).unwrap_or(usize::MAX))
            .copied()
            .unwrap_or(0);
        count = count.saturating_add(width);
    }
    count
}

/// `ceil(log2(n))`，用于 `bed_ch_bits`。
pub(super) fn ceil_log2(n: u32) -> u32 {
    n.checked_sub(1)
        .map_or(0, |less| u32::BITS.saturating_sub(less.leading_zeros()))
}

/// `ac4_substream_info_ajoc()`，见 `6.2.1.9`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SubstreamInfoAjoc {
    /// 是否含 LFE。
    pub b_lfe: bool,
    /// 下混信号数是否固定为 5。
    pub static_dmx: bool,
    /// 全带下混信号数。
    pub n_dmx_signals: u32,
    /// 下混侧的对象分配；`static_dmx` 为真时不传输。
    pub dmx_assignment: Option<ObjectAssignment>,
    /// 全带上混信号数，即完整解码模式下的对象数。
    pub n_upmix_signals: u32,
    /// 上混侧的对象分配。
    pub upmix_assignment: ObjectAssignment,
    /// 是否携带 OAMD 公共数据。
    pub oamd_common_data_present: bool,
    /// 本元素内嵌的 OAMD 公共数据；未携带时为 `None`。
    pub oamd_common_data: Option<OamdCommonData>,
    pub(super) tail: SubstreamTail,
}

impl SubstreamInfoAjoc {
    /// 两份引用是否给同一物理 substream 建立了相同的音频解析上下文。
    ///
    /// group 级 OAMD common/timing 可以按场景引用分别保留，因此不参与本比较；
    /// 其余会改变 `audio_data_ajoc()` 语法、对象布局或时间轴的字段必须一致。
    #[must_use]
    pub fn has_same_audio_context(&self, other: &Self) -> bool {
        self.b_lfe == other.b_lfe
            && self.static_dmx == other.static_dmx
            && self.n_dmx_signals == other.n_dmx_signals
            && self.dmx_assignment == other.dmx_assignment
            && self.n_upmix_signals == other.n_upmix_signals
            && self.upmix_assignment == other.upmix_assignment
            && self.sampling_frequency_multiplier() == other.sampling_frequency_multiplier()
            && self.audio_ndot() == other.audio_ndot()
            && self.substream_index() == other.substream_index()
    }

    /// 按 `Pseudocode 15` 得到 LFE 在 A-JOC 输出中的插回位置。
    ///
    /// 位置必须从**完整解码侧**的 [`Self::upmix_assignment`] 派生：
    /// `Qout_AJOC` 含 `num_umx_signals` 个重建对象，对应 `6.2.1.9` 第二次、以
    /// `n_fullband_upmix_signals` 为参数的 `bed_dyn_obj_assignment()`。下混侧分配
    /// 只描述 core decode，不能用于这里。无 LFE 时不执行插回，返回 `None`。
    #[must_use]
    pub const fn lfe_reinsertion_position(&self) -> Option<u32> {
        if self.b_lfe {
            Some(
                self.upmix_assignment
                    .reconstruction_channels
                    .lfe_reinsertion_position(),
            )
        } else {
            None
        }
    }

    /// substream 索引表中的下标。
    #[must_use]
    pub const fn substream_index(&self) -> Option<u32> {
        self.tail.substream_index
    }

    /// 该 substream 是否无时间依赖，可独立于前序帧解码。
    #[must_use]
    pub const fn audio_ndot(&self) -> bool {
        self.tail.audio_ndot
    }

    /// 相对 TOC 基准采样频率的乘子，见表 89。
    ///
    /// `1`、`2`、`4` 分别表示基准采样率、96 kHz 与 192 kHz（后两者只在
    /// `fs_index == 1` 时可传输）。
    #[must_use]
    pub const fn sampling_frequency_multiplier(&self) -> u32 {
        self.tail.sampling_frequency_multiplier()
    }

    pub(super) fn parse(
        reader: &mut BitReader<'_>,
        fs_index: u8,
        frame_rate_factor: u32,
        substreams_present: bool,
    ) -> Result<Self, TopologyError> {
        let b_lfe = reader.read_flag()?;
        let static_dmx = reader.read_flag()?;
        let (n_dmx_signals, dmx_assignment) = if static_dmx {
            (5, None)
        } else {
            let count = u32::try_from(reader.read_bits(4)?)
                .unwrap_or(0)
                .saturating_add(1);
            (count, Some(ObjectAssignment::parse(reader, count)?))
        };

        let oamd_common_data_present = reader.read_flag()?;
        let oamd_common_data = if oamd_common_data_present {
            Some(OamdCommonData::parse(reader)?)
        } else {
            None
        };

        let mut n_upmix_signals = u32::try_from(reader.read_bits(4)?)
            .unwrap_or(0)
            .saturating_add(1);
        if n_upmix_signals == 16 {
            n_upmix_signals = reader.variable_bits_scaled_u32(3, n_upmix_signals, 0)?;
        }
        let upmix_assignment = ObjectAssignment::parse(reader, n_upmix_signals)?;

        let tail = SubstreamTail::parse(reader, fs_index, frame_rate_factor, substreams_present)?;

        Ok(Self {
            b_lfe,
            static_dmx,
            n_dmx_signals,
            dmx_assignment,
            n_upmix_signals,
            upmix_assignment,
            oamd_common_data_present,
            oamd_common_data,
            tail,
        })
    }
}

/// `ac4_substream_info_obj()`，见 `6.2.1.11`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SubstreamInfoObj {
    /// `n_objects_code` 原始码字。
    pub n_objects_code: u32,
    /// 本 substream 中的对象数。
    pub n_objects: u32,
    /// 是否为动态对象。
    pub dynamic_objects: bool,
    /// 是否含 LFE。
    pub b_lfe: bool,
    /// 床对象数。
    pub n_bed: u32,
    /// ISF 对象数。
    pub n_isf: u32,
    pub(super) tail: SubstreamTail,
}

impl SubstreamInfoObj {
    /// substream 索引表中的下标。
    #[must_use]
    pub const fn substream_index(&self) -> Option<u32> {
        self.tail.substream_index
    }

    /// 该 substream 是否无时间依赖，可独立于前序帧解码。
    #[must_use]
    pub const fn audio_ndot(&self) -> bool {
        self.tail.audio_ndot
    }

    pub(super) fn parse(
        reader: &mut BitReader<'_>,
        fs_index: u8,
        frame_rate_factor: u32,
        substreams_present: bool,
    ) -> Result<Self, TopologyError> {
        let n_objects_code = u32::try_from(reader.read_bits(3)?).unwrap_or(0);
        let dynamic_objects = reader.read_flag()?;

        let mut out = Self {
            n_objects_code,
            dynamic_objects,
            ..Self::default()
        };

        if dynamic_objects {
            out.b_lfe = reader.read_flag()?;
            // 表 60：n_objects = [0,1,2,3,5][code] + b_lfe。规范的语法表在此
            // 处给出的数组是 [0,1,2,3,5,7] 且不加 b_lfe，与表 60 不一致；
            // 该差异不影响比特消耗，此处采用表 60 的解释。
            let base = [0u32, 1, 2, 3, 5]
                .get(usize::try_from(n_objects_code).unwrap_or(usize::MAX))
                .copied()
                .unwrap_or(0);
            out.n_objects = base.saturating_add(u32::from(out.b_lfe));
            return finish_obj(reader, out, fs_index, frame_rate_factor, substreams_present);
        }

        if reader.read_flag()? {
            // 床对象；b_bed_start 为假时本 substream 只是前一段床的延续
            if reader.read_flag()? {
                if reader.read_flag()? {
                    let code = usize::try_from(reader.read_bits(3)?).unwrap_or(usize::MAX);
                    // direct-coded 的床含 LFE，计数表与 A-JOC 的不同
                    out.n_bed = [2u32, 3, 6, 8, 10, 8, 10, 12]
                        .get(code)
                        .copied()
                        .unwrap_or(0);
                } else if reader.read_flag()? {
                    let flags = reader.read_bits(17)?;
                    out.n_bed = count_nonstd_bed(flags, false);
                } else {
                    let flags = reader.read_bits(10)?;
                    out.n_bed = count_std_bed(flags, false);
                }
            }
            out.n_objects = out.n_bed;
        } else if reader.read_flag()? {
            // ISF 对象
            if reader.read_flag()? {
                let config = usize::try_from(reader.read_bits(3)?).unwrap_or(usize::MAX);
                out.n_isf = isf_object_count(config);
            }
            out.n_objects = out.n_isf;
        } else {
            // 既非床也非 ISF：本 substream 只携带保留数据
            let res_bytes = reader.read_bits(4)?;
            reader.skip_bits(res_bytes.saturating_mul(8))?;
        }

        finish_obj(reader, out, fs_index, frame_rate_factor, substreams_present)
    }
}

pub(super) fn finish_obj(
    reader: &mut BitReader<'_>,
    mut info: SubstreamInfoObj,
    fs_index: u8,
    frame_rate_factor: u32,
    substreams_present: bool,
) -> Result<SubstreamInfoObj, TopologyError> {
    info.tail = SubstreamTail::parse(reader, fs_index, frame_rate_factor, substreams_present)?;
    Ok(info)
}
