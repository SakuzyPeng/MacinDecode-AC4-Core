//! Presentation 信息元素。
//!
//! 对应 `TS103190-2:v1.3.1:6.2.1.3` 至 `6.2.1.5` 与 `6.2.1.12`，其中
//! `presentation_version()` 与 `frame_rate_multiply_info()` 见
//! `TS103190-1:v1.4.1:4.2.3.3`、`4.2.3.4`。
//!
//! 一个 presentation 本身不携带音频，它只声明“这一路输出由哪些 substream
//! group 组成”。group 存放在 TOC 级别，presentation 通过 `group_index` 引用。

use crate::emdf::EmdfInfo;
use crate::reader::BitReader;
use crate::topology::{Capacity, MAX_PRESENTATION_VERSION, TopologyError, read_substream_index};

/// 单个 presentation 可引用的 group 数上限。
pub const MAX_GROUPS_PER_PRESENTATION: usize = 8;
/// 单个 presentation 可携带的附加 EMDF 信息上限。
pub const MAX_ADD_EMDF_SUBSTREAMS: usize = 32;

/// `presentation_config` 的可读名称，见 `TS103190-2:v1.3.1:表 53`。
#[must_use]
pub const fn presentation_config_label(config: u32) -> &'static str {
    match config {
        0 => "music_and_effects+dialogue",
        1 => "main+dialogue_enhancement",
        2 => "main+associated_audio",
        3 => "music_and_effects+dialogue+associated_audio",
        4 => "main+dialogue_enhancement+associated_audio",
        5 => "arbitrary_roles",
        6 => "emdf_and_other_data",
        _ => "extended",
    }
}

/// `ac4_presentation_substream_info()`，见 `6.2.1.12`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PresentationSubstreamInfo {
    /// 是否为替代呈现。
    pub alternative: bool,
    /// presentation substream 是否无时间依赖，见 `4.5.2` 的 `b_pres_ndot`。
    pub ndot: bool,
    /// substream 索引表中的下标。
    pub substream_index: u32,
}

impl PresentationSubstreamInfo {
    fn parse(reader: &mut BitReader<'_>) -> Result<Self, TopologyError> {
        let alternative = reader.read_flag()?;
        let ndot = reader.read_flag()?;
        let substream_index = read_substream_index(reader)?;
        Ok(Self {
            alternative,
            ndot,
            substream_index,
        })
    }
}

/// `ac4_presentation_v1_info()`，见 `6.2.1.3`。
///
/// 本实现只覆盖 `bitstream_version == 2` 的形态；版本 0 与 1 在
/// [`crate::topology::Ac4Topology::parse`] 处提前拒绝。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ac4PresentationV1Info {
    /// 是否只引用一个 substream group。
    pub single_substream_group: bool,
    /// presentation 配置；单 group 时不传输。
    pub presentation_config: Option<u32>,
    /// presentation 语法版本。
    pub presentation_version: u32,
    /// 元数据兼容级别。
    pub md_compat: Option<u8>,
    /// presentation 标识。
    pub presentation_id: Option<u32>,
    /// 帧率因子，决定 `b_audio_ndot` 的重复次数。
    pub frame_rate_factor: u32,
    /// 帧率分数，1、2 或 4。
    pub frame_rate_fraction: u32,
    /// presentation 级 EMDF 信息。
    pub emdf: Option<EmdfInfo>,
    /// 是否声明了 presentation 过滤。
    pub presentation_filter: Option<bool>,
    /// 是否跨多个传输流。
    pub multi_pid: Option<bool>,
    /// 是否已做虚拟化预处理。
    pub pre_virtualized: Option<bool>,
    /// 声明的 substream group 数。
    ///
    /// 注意它与 `group_indices()` 的长度可以不同：`presentation_config`
    /// 为 1 或 4 时读入两三个 `group_index`，但只计作一或两个角色。
    pub n_substream_groups: u32,
    /// presentation 自身的 substream。
    pub substream: Option<PresentationSubstreamInfo>,
    /// 附加 EMDF substream 数。
    pub n_add_emdf_substreams: u32,
    group_indices: [u32; MAX_GROUPS_PER_PRESENTATION],
    written: usize,
    additional_emdf: [EmdfInfo; MAX_ADD_EMDF_SUBSTREAMS],
    additional_emdf_written: usize,
}

impl Ac4PresentationV1Info {
    /// 未解析状态的占位值，用于初始化固定容量数组。
    pub const EMPTY: Self = Self {
        single_substream_group: false,
        presentation_config: None,
        presentation_version: 0,
        md_compat: None,
        presentation_id: None,
        frame_rate_factor: 1,
        frame_rate_fraction: 1,
        emdf: None,
        presentation_filter: None,
        multi_pid: None,
        pre_virtualized: None,
        n_substream_groups: 0,
        substream: None,
        n_add_emdf_substreams: 0,
        group_indices: [0; MAX_GROUPS_PER_PRESENTATION],
        written: 0,
        additional_emdf: [EmdfInfo::EMPTY; MAX_ADD_EMDF_SUBSTREAMS],
        additional_emdf_written: 0,
    };

    /// 本 presentation 引用的全部 `group_index`，按读取顺序排列。
    #[must_use]
    pub fn group_indices(&self) -> &[u32] {
        self.group_indices.get(..self.written).unwrap_or(&[])
    }

    /// presentation 末尾的附加 `emdf_info()`，按码流顺序排列。
    #[must_use]
    pub fn additional_emdf(&self) -> &[EmdfInfo] {
        self.additional_emdf
            .get(..self.additional_emdf_written)
            .unwrap_or(&[])
    }

    /// 帧率因子，供 TOC 级的 substream group 解析使用。
    #[must_use]
    pub const fn frame_rate_factor(&self) -> u32 {
        self.frame_rate_factor
    }

    /// 解析 `ac4_presentation_v1_info()`。
    ///
    /// # Errors
    ///
    /// 读取越界、引用的 group 数超过 [`MAX_GROUPS_PER_PRESENTATION`]，或
    /// `presentation_version` 的一元编码过长时返回错误。
    pub fn parse(
        reader: &mut BitReader<'_>,
        bitstream_version: u32,
        frame_rate_index: u8,
    ) -> Result<Self, TopologyError> {
        let single_substream_group = reader.read_flag()?;
        let presentation_config = if single_substream_group {
            None
        } else {
            let mut config = u32::try_from(reader.read_bits(3)?).unwrap_or(0);
            if config == 7 {
                config = reader.variable_bits_scaled_u32(2, config, 0)?;
            }
            Some(config)
        };

        let presentation_version = if bitstream_version == 1 {
            0
        } else {
            read_presentation_version(reader)?
        };

        let mut out = Self {
            single_substream_group,
            presentation_config,
            presentation_version,
            ..Self::EMPTY
        };

        // config 6 表示这一路只承载 EMDF 与其他数据，没有音频拓扑
        if presentation_config == Some(6) {
            out.parse_additional_emdf(reader)?;
            return Ok(out);
        }

        if bitstream_version != 1 {
            out.md_compat = Some(u8::try_from(reader.read_bits(3)?).unwrap_or(0));
        }
        if reader.read_flag()? {
            out.presentation_id = Some(reader.variable_bits_u32(2)?);
        }

        out.frame_rate_factor = read_frame_rate_multiply_info(reader, frame_rate_index)?;
        out.frame_rate_fraction =
            read_frame_rate_fractions_info(reader, frame_rate_index, out.frame_rate_factor)?;
        out.emdf = Some(EmdfInfo::parse(reader)?);

        out.presentation_filter = if reader.read_flag()? {
            Some(reader.read_flag()?)
        } else {
            None
        };

        if single_substream_group {
            out.push_group(read_sgi_specifier(reader, bitstream_version)?)?;
            out.n_substream_groups = 1;
        } else {
            out.multi_pid = Some(reader.read_flag()?);
            // 每个 case 的 ac4_sgi_specifier() 次数与 n_substream_groups 未必
            // 相等：config 1 与 4 中，对话增强与其主 group 共用一个角色。
            let (specifiers, groups) = match presentation_config {
                Some(0) => (2u32, 2u32),
                Some(1) => (2, 1),
                Some(2) => (2, 2),
                Some(3) => (3, 3),
                Some(4) => (3, 2),
                Some(5) => {
                    let mut count = u32::try_from(reader.read_bits(2)?)
                        .unwrap_or(0)
                        .saturating_add(2);
                    if count == 5 {
                        count = reader.variable_bits_scaled_u32(2, count, 0)?;
                    }
                    (count, count)
                }
                _ => {
                    skip_presentation_config_ext_info(reader)?;
                    (0, 0)
                }
            };
            for _ in 0..specifiers {
                out.push_group(read_sgi_specifier(reader, bitstream_version)?)?;
            }
            out.n_substream_groups = groups;
        }

        out.pre_virtualized = Some(reader.read_flag()?);
        let add_emdf = reader.read_flag()?;
        out.substream = Some(PresentationSubstreamInfo::parse(reader)?);

        if add_emdf {
            out.parse_additional_emdf(reader)?;
        }
        Ok(out)
    }

    /// 用于配置代次比较的规范化副本；逐帧 ndot 与保留填充不参与配置。
    pub(crate) fn configuration_copy(&self) -> Self {
        let mut out = *self;
        if let Some(mut substream) = out.substream {
            substream.ndot = false;
            out.substream = Some(substream);
        }
        out.emdf = out.emdf.map(|info| info.configuration_copy());
        for info in out
            .additional_emdf
            .iter_mut()
            .take(out.additional_emdf_written)
        {
            *info = info.configuration_copy();
        }
        out
    }

    fn parse_additional_emdf(&mut self, reader: &mut BitReader<'_>) -> Result<(), TopologyError> {
        let mut count = u32::try_from(reader.read_bits(2)?).unwrap_or(u32::MAX);
        if count == 0 {
            count = reader.variable_bits_scaled_u32(2, 4, 0)?;
        }
        if count > u32::try_from(MAX_ADD_EMDF_SUBSTREAMS).unwrap_or(u32::MAX) {
            return Err(TopologyError::CapacityExceeded {
                what: Capacity::AddEmdfSubstreams,
                declared: count,
                limit: MAX_ADD_EMDF_SUBSTREAMS,
            });
        }

        for _ in 0..count {
            let info = EmdfInfo::parse(reader)?;
            let slot = self
                .additional_emdf
                .get_mut(self.additional_emdf_written)
                .ok_or(TopologyError::CapacityExceeded {
                    what: Capacity::AddEmdfSubstreams,
                    declared: count,
                    limit: MAX_ADD_EMDF_SUBSTREAMS,
                })?;
            *slot = info;
            self.additional_emdf_written = self.additional_emdf_written.saturating_add(1);
        }
        self.n_add_emdf_substreams = count;
        Ok(())
    }

    fn push_group(&mut self, index: u32) -> Result<(), TopologyError> {
        let slot =
            self.group_indices
                .get_mut(self.written)
                .ok_or(TopologyError::CapacityExceeded {
                    what: Capacity::GroupsPerPresentation,
                    declared: u32::try_from(self.written)
                        .unwrap_or(u32::MAX)
                        .saturating_add(1),
                    limit: MAX_GROUPS_PER_PRESENTATION,
                })?;
        *slot = index;
        self.written = self.written.saturating_add(1);
        Ok(())
    }
}

/// `presentation_version()`：连续 1 的个数即版本号。
fn read_presentation_version(reader: &mut BitReader<'_>) -> Result<u32, TopologyError> {
    let start = reader.bit_position();
    let mut value = 0u32;
    while reader.read_flag()? {
        value = value.saturating_add(1);
        if value > MAX_PRESENTATION_VERSION {
            return Err(TopologyError::PresentationVersionTooLong {
                bit_position: start,
            });
        }
    }
    Ok(value)
}

/// `frame_rate_multiply_info()`，返回 `frame_rate_factor`，见表 87。
fn read_frame_rate_multiply_info(
    reader: &mut BitReader<'_>,
    frame_rate_index: u8,
) -> Result<u32, TopologyError> {
    match frame_rate_index {
        2..=4 => {
            if !reader.read_flag()? {
                return Ok(1);
            }
            Ok(if reader.read_flag()? { 4 } else { 2 })
        }
        0 | 1 | 7..=9 => Ok(if reader.read_flag()? { 2 } else { 1 }),
        // 索引 5、6、10…13 不传输任何比特，因子恒为 1
        _ => Ok(1),
    }
}

/// `frame_rate_fractions_info()`，见 `6.2.1.4`。
fn read_frame_rate_fractions_info(
    reader: &mut BitReader<'_>,
    frame_rate_index: u8,
    frame_rate_factor: u32,
) -> Result<u32, TopologyError> {
    if matches!(frame_rate_index, 5..=9) {
        if frame_rate_factor == 1 && reader.read_flag()? {
            return Ok(2);
        }
        return Ok(1);
    }
    if matches!(frame_rate_index, 10..=12) {
        if !reader.read_flag()? {
            return Ok(1);
        }
        return Ok(if reader.read_flag()? { 4 } else { 2 });
    }
    Ok(1)
}

/// `ac4_sgi_specifier()`，见 `6.2.1.7`。
///
/// `bitstream_version` 为 1 时该元素直接内联整个 group；版本 2 起改为引用
/// TOC 级的 group 下标。版本 1 已在上游拒绝。
fn read_sgi_specifier(
    reader: &mut BitReader<'_>,
    _bitstream_version: u32,
) -> Result<u32, TopologyError> {
    let mut index = u32::try_from(reader.read_bits(3)?).unwrap_or(0);
    if index == 7 {
        index = reader.variable_bits_scaled_u32(2, index, 0)?;
    }
    Ok(index)
}

/// 跳过 `presentation_config_ext_info()`，见 `6.2.1.5`。
fn skip_presentation_config_ext_info(reader: &mut BitReader<'_>) -> Result<(), TopologyError> {
    let mut n_skip_bytes = u32::try_from(reader.read_bits(5)?).unwrap_or(0);
    if reader.read_flag()? {
        n_skip_bytes = reader.variable_bits_scaled_u32(2, n_skip_bytes, 5)?;
    }
    reader.skip_bits(u64::from(n_skip_bytes).saturating_mul(8))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[expect(
        clippy::arithmetic_side_effects,
        clippy::indexing_slicing,
        reason = "测试内的位串打包，索引受输入长度约束"
    )]
    fn pack(bits: &str) -> [u8; 32] {
        let mut out = [0u8; 32];
        let mut index = 0usize;
        for ch in bits.chars() {
            if ch == '0' || ch == '1' {
                if ch == '1' {
                    out[index / 8] |= 1 << (7 - index % 8);
                }
                index += 1;
            }
        }
        out
    }

    #[test]
    fn presentation_version_counts_leading_ones() {
        let data = pack("0");
        assert_eq!(
            read_presentation_version(&mut BitReader::new(&data)).unwrap(),
            0
        );
        let data = pack("110");
        assert_eq!(
            read_presentation_version(&mut BitReader::new(&data)).unwrap(),
            2
        );
    }

    /// 全 1 的输入会一直读到帧尾，必须在耗尽前给出明确错误。
    #[test]
    fn presentation_version_rejects_runaway_unary_code() {
        let data = [0xFFu8; 32];
        assert!(matches!(
            read_presentation_version(&mut BitReader::new(&data)).unwrap_err(),
            TopologyError::PresentationVersionTooLong { .. }
        ));
    }

    /// 索引 13 不传输 frame_rate_multiply_info 的任何比特。
    #[test]
    fn frame_rate_factor_is_one_for_index_13() {
        let data = pack("1111");
        let mut reader = BitReader::new(&data);
        assert_eq!(read_frame_rate_multiply_info(&mut reader, 13).unwrap(), 1);
        assert_eq!(reader.bit_position(), 0, "不得消耗比特");
    }

    #[test]
    fn frame_rate_factor_reads_multiplier_bit() {
        let data = pack("11");
        let mut reader = BitReader::new(&data);
        assert_eq!(read_frame_rate_multiply_info(&mut reader, 3).unwrap(), 4);
        assert_eq!(reader.bit_position(), 2);

        let data = pack("10");
        let mut reader = BitReader::new(&data);
        assert_eq!(read_frame_rate_multiply_info(&mut reader, 3).unwrap(), 2);

        let data = pack("0");
        let mut reader = BitReader::new(&data);
        assert_eq!(read_frame_rate_multiply_info(&mut reader, 3).unwrap(), 1);
        assert_eq!(reader.bit_position(), 1);
    }

    /// 索引 0、1、7、8、9 只有一位，没有 multiplier_bit。
    #[test]
    fn frame_rate_factor_single_bit_form() {
        let data = pack("11");
        let mut reader = BitReader::new(&data);
        assert_eq!(read_frame_rate_multiply_info(&mut reader, 8).unwrap(), 2);
        assert_eq!(reader.bit_position(), 1);
    }

    #[test]
    fn frame_rate_fraction_only_applies_to_some_indices() {
        let data = pack("1");
        let mut reader = BitReader::new(&data);
        assert_eq!(
            read_frame_rate_fractions_info(&mut reader, 13, 1).unwrap(),
            1
        );
        assert_eq!(reader.bit_position(), 0);

        let data = pack("1");
        let mut reader = BitReader::new(&data);
        assert_eq!(
            read_frame_rate_fractions_info(&mut reader, 6, 1).unwrap(),
            2
        );

        // 因子不为 1 时索引 5…9 不传输该位
        let data = pack("1");
        let mut reader = BitReader::new(&data);
        assert_eq!(
            read_frame_rate_fractions_info(&mut reader, 6, 2).unwrap(),
            1
        );
        assert_eq!(reader.bit_position(), 0);

        // 索引 10…12 用两位表示 4
        let data = pack("11");
        let mut reader = BitReader::new(&data);
        assert_eq!(
            read_frame_rate_fractions_info(&mut reader, 11, 1).unwrap(),
            4
        );
    }

    #[test]
    fn sgi_specifier_extends_at_seven() {
        let data = pack("111 01 0");
        let mut reader = BitReader::new(&data);
        assert_eq!(read_sgi_specifier(&mut reader, 2).unwrap(), 8);
    }

    #[test]
    fn config_labels_cover_table_53() {
        assert_eq!(presentation_config_label(1), "main+dialogue_enhancement");
        assert_eq!(presentation_config_label(9), "extended");
    }

    #[test]
    fn preserves_additional_emdf_substream_references() {
        // count=1；emdf_version=0，key_id=0，b_payloads=1，index=2，无保留字节。
        let data = pack("01 00 000 1 10 00 00");
        let mut reader = BitReader::new(&data);
        let mut presentation = Ac4PresentationV1Info::EMPTY;
        presentation.parse_additional_emdf(&mut reader).unwrap();

        assert_eq!(presentation.n_add_emdf_substreams, 1);
        assert_eq!(presentation.additional_emdf().len(), 1);
        assert_eq!(
            presentation
                .additional_emdf()
                .first()
                .unwrap()
                .payloads_substream_index,
            Some(2)
        );
    }
}
