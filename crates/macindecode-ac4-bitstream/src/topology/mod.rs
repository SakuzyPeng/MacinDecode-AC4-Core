//! TOC 层的 presentation 与 substream 拓扑。
//!
//! 对应 `TS103190-2:v1.3.1:6.2.1`（bitstream_version = 2 的 TOC）与
//! `TS103190-1:v1.4.1:4.2.3.11`（`substream_index_table`，表 14）。
//!
//! 本模块回答的是“这一帧里有哪些 presentation、它们引用了哪些 substream
//! group、每个 group 走哪条编码路径”，不解码音频，也不解析 OAMD 有效载荷。
//!
//! 拓扑是判定编码路径的唯一依据：channel-based、A-JOC 与 direct-object 三
//! 者的区别只体现在 `ac4_substream_group_info()` 的 `b_channel_coded` 与
//! `b_ajoc` 上，容器层与渲染输出都不构成证据。

use crate::oamd::OamdError;
use crate::presentation::{Ac4PresentationV1Info, MAX_GROUPS_PER_PRESENTATION};
use crate::presentation_substream::{
    PresentationSubstreamContext, PresentationSubstreamSelectionContext,
};
use crate::reader::{BitReader, ReadError};
use crate::substream::{Ac4SubstreamGroupInfo, SubstreamInfo};
use crate::toc::{Ac4Toc, SequenceTransition};
use core::fmt;

/// 单帧内可保存的 presentation 数上限。
pub const MAX_PRESENTATIONS: usize = 8;
/// 单帧内可保存的 substream group 数上限。
pub const MAX_SUBSTREAM_GROUPS: usize = 8;
/// `substream_index_table()` 中可保存的 substream 数上限。
pub const MAX_SUBSTREAMS: usize = 32;

/// 超出固定容量的结构种类。
///
/// 本 crate 不分配内存，所有集合都有编译期上限；超限是明确的错误而不是
/// 静默截断，否则拓扑会在下游被当作完整的来使用。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capacity {
    /// presentation 数。
    Presentations,
    /// TOC 级的 substream group 数。
    SubstreamGroups,
    /// 单个 presentation 引用的 group 数。
    GroupsPerPresentation,
    /// 单个 group 内的低频 substream 数。
    LfSubstreams,
    /// `substream_index_table()` 中的 substream 数。
    Substreams,
    /// 附加 EMDF substream 数。
    AddEmdfSubstreams,
}

impl fmt::Display for Capacity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match *self {
            Capacity::Presentations => "presentation",
            Capacity::SubstreamGroups => "substream group",
            Capacity::GroupsPerPresentation => "groups referenced by a presentation",
            Capacity::LfSubstreams => "substreams in a group",
            Capacity::Substreams => "substream index-table entries",
            Capacity::AddEmdfSubstreams => "additional EMDF substreams",
        };
        f.write_str(text)
    }
}

/// 本实现尚未覆盖的语法分支。
///
/// 包含已由规范定义但尚未实现的旧语法，以及当前规范尚未定义的
/// 未来版本。遇到时明确报错，不做猜测性解析。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unsupported {
    /// `bitstream_version` ≤ 1，TOC 使用 `ac4_presentation_info()`。
    LegacyPresentationInfo {
        /// 实际的 bitstream_version。
        bitstream_version: u32,
    },
    /// 当前规范尚未定义的 `bitstream_version` 扩展。
    FutureBitstreamVersion {
        /// 实际的 bitstream_version。
        bitstream_version: u32,
    },
    /// `channel_mode` 落在保留取值 `0b111111111` 之后的扩展区间。
    ReservedChannelMode {
        /// 扩展后的 ch_mode 值。
        ch_mode: u32,
    },
}

impl fmt::Display for Unsupported {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Unsupported::LegacyPresentationInfo { bitstream_version } => write!(
                f,
                "bitstream_version {bitstream_version} uses ac4_presentation_info(); only version 2 is supported"
            ),
            Unsupported::FutureBitstreamVersion { bitstream_version } => write!(
                f,
                "bitstream_version {bitstream_version} is not defined by the current specification; only version 2 is supported"
            ),
            Unsupported::ReservedChannelMode { ch_mode } => {
                write!(f, "ch_mode {ch_mode} is reserved")
            }
        }
    }
}

/// 拓扑解析失败的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TopologyError {
    /// 读取越界或变长字段溢出。
    Read(ReadError),
    /// A-JOC substream info 中的内嵌 OAMD 公共数据解析失败。
    OamdCommon(OamdError),
    /// 结构规模超出固定容量。
    CapacityExceeded {
        /// 超限的结构种类。
        what: Capacity,
        /// 码流声明的数量。
        declared: u32,
        /// 本实现的上限。
        limit: usize,
    },
    /// 语法分支未覆盖。
    Unsupported {
        /// 具体分支。
        what: Unsupported,
        /// 遇到该分支时的比特偏移。
        bit_position: u64,
    },
    /// `presentation_version()` 的一元编码长度不合理。
    ///
    /// 该字段以连续的 1 计数，损坏的码流会让它一直读到帧尾。
    PresentationVersionTooLong {
        /// 遇到该字段时的比特偏移。
        bit_position: u64,
    },
    /// presentation 引用了 TOC 中不存在的 substream group。
    GroupIndexOutOfRange {
        /// 被引用的下标。
        group_index: u32,
        /// TOC 中实际存在的 group 数。
        total: usize,
    },
    /// 某个元素引用了索引表以外的 substream。
    SubstreamIndexOutOfRange {
        /// 被引用的下标。
        index: u32,
        /// 索引表声明的 substream 数。
        total: u32,
    },
    /// 索引表中的 substream 没有被任何 TOC 元素引用。
    UnreferencedSubstream {
        /// 未被引用的下标。
        index: u32,
    },
    /// 索引表未传输尺寸，无法定位单个 substream 的载荷。
    SubstreamSizesAbsent,
    /// 按索引表算出的载荷区间超出帧长度。
    SubstreamPayloadOutOfFrame {
        /// 被定位的下标。
        index: u32,
        /// 载荷相对帧首的起始字节。
        start: u64,
        /// 载荷结束字节。
        end: u64,
        /// 帧的实际字节数。
        frame_len: u64,
    },
}

impl fmt::Display for TopologyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            TopologyError::Read(error) => write!(f, "Failed to read topology: {error}"),
            TopologyError::OamdCommon(error) => {
                write!(
                    f,
                    "Failed to parse embedded A-JOC OAMD common data: {error}"
                )
            }
            TopologyError::CapacityExceeded {
                what,
                declared,
                limit,
            } => write!(
                f,
                "{what} count {declared} exceeds implementation limit {limit}"
            ),
            TopologyError::Unsupported { what, bit_position } => {
                write!(f, "At bit offset {bit_position}: {what}")
            }
            TopologyError::PresentationVersionTooLong { bit_position } => write!(
                f,
                "Unary presentation_version at bit offset {bit_position} exceeds {MAX_PRESENTATION_VERSION} bits"
            ),
            TopologyError::GroupIndexOutOfRange { group_index, total } => {
                write!(
                    f,
                    "Presentation references group {group_index}, but the TOC contains only {total} groups"
                )
            }
            TopologyError::SubstreamIndexOutOfRange { index, total } => {
                write!(
                    f,
                    "TOC references substream {index}, but the index table contains only {total} entries"
                )
            }
            TopologyError::UnreferencedSubstream { index } => {
                write!(
                    f,
                    "Substream {index} in the index table is not referenced by the TOC"
                )
            }
            TopologyError::SubstreamSizesAbsent => {
                write!(
                    f,
                    "Index table does not carry substream sizes, so payloads cannot be located"
                )
            }
            TopologyError::SubstreamPayloadOutOfFrame {
                index,
                start,
                end,
                frame_len,
            } => write!(
                f,
                "Payload range [{start}, {end}) for substream {index} exceeds frame length {frame_len}"
            ),
        }
    }
}

impl core::error::Error for TopologyError {}

impl From<ReadError> for TopologyError {
    fn from(error: ReadError) -> Self {
        TopologyError::Read(error)
    }
}

impl From<OamdError> for TopologyError {
    fn from(error: OamdError) -> Self {
        TopologyError::OamdCommon(error)
    }
}

/// `presentation_version()` 一元编码的长度上限。
///
/// 规范目前只定义到版本 2；给出上限是为了让损坏码流立刻报错，而不是把整帧
/// 当作一个字段读完。
pub(crate) const MAX_PRESENTATION_VERSION: u32 = 31;

/// 读取 `substream_index`：2 比特，取值 3 时以 `variable_bits(2)` 扩展。
///
/// 该模式在 `ac4_substream_info_*`、`oamd_substream_info`、
/// `ac4_hsf_ext_substream_info` 与 `emdf_payloads_substream_info` 中重复出现。
pub(crate) fn read_substream_index(reader: &mut BitReader<'_>) -> Result<u32, TopologyError> {
    let base = u32::try_from(reader.read_bits(2)?).unwrap_or(u32::MAX);
    if base == 3 {
        return Ok(reader.variable_bits_scaled_u32(2, base, 0)?);
    }
    Ok(base)
}

mod parse;
mod state;
mod validate;

pub use parse::*;
pub use state::*;
pub use validate::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scene_path_labels_are_stable() {
        assert_eq!(ScenePath::Ajoc.label(), "ajoc");
        assert_eq!(ScenePath::DirectObject.label(), "direct_object");
    }

    #[test]
    fn index_table_omits_sizes_for_single_substream() {
        // n_substreams=01(=1)，b_size_present=0
        let data = [0b0100_0000u8];
        let mut reader = BitReader::new(&data);
        let table = SubstreamIndexTable::parse(&mut reader).unwrap();
        assert_eq!(table.n_substreams, 1);
        assert!(!table.size_present);
        assert!(table.sizes().is_empty());
    }

    #[test]
    fn index_table_reads_sizes_when_multiple() {
        // n_substreams=10(=2)；两条 (b_more_bits=0, size=1) 与 (0, size=2)
        let bits = "10 0 0000000001 0 0000000010";
        let data = pack(bits);
        let mut reader = BitReader::new(&data);
        let table = SubstreamIndexTable::parse(&mut reader).unwrap();
        assert_eq!(table.n_substreams, 2);
        assert!(table.size_present);
        assert_eq!(table.sizes().len(), 2);
        assert_eq!(table.sizes().first().unwrap().bytes, 1);
        assert_eq!(table.sizes().get(1).unwrap().bytes, 2);
    }

    /// `b_more_bits` 让 10 比特的尺寸再加上左移 10 位的扩展量。
    #[test]
    fn index_table_extends_size() {
        // n_substreams=01(=1)，b_size_present=1，b_more_bits=1，size=1，
        // variable_bits(2) = 01 后接停止位 0 得 1，故 1 + (1 << 10)
        let data = pack("01 1 1 0000000001 01 0");
        let mut reader = BitReader::new(&data);
        let table = SubstreamIndexTable::parse(&mut reader).unwrap();
        assert_eq!(table.sizes().first().unwrap().bytes, 1 + (1 << 10));
    }

    /// TOC 前置字段：版本 2、序号 0、48 kHz、帧率索引 13、I-frame、
    /// 单 presentation、payload_base 为 0，随后 b_program_id 为 0。
    const TOC_PREFIX: &str = "10 0000000000 0 1 1101 1 1 0 0";

    /// 与 [`TOC_PREFIX`] 相同，但帧率索引为 3，可传输 2×/4× 帧率因子。
    const TOC_PREFIX_MULTIPLIED: &str = "10 0000000000 0 1 0011 1 1 0 0";

    /// 一个最简 presentation：单 group、引用 group 0、无 EMDF 负载。
    ///
    /// b_single_substream_group=1, presentation_version=0(一元), md_compat=000,
    /// b_presentation_id=0, emdf_info(00,000,0,00,00), b_presentation_filter=0,
    /// sgi_specifier group_index=000, b_pre_virtualized=0,
    /// b_add_emdf_substreams=0, presentation_substream(0,0,index=00)。
    /// 帧率索引 13 下 frame_rate_multiply_info 与 fractions_info 均不占比特。
    const PRESENTATION_SINGLE_GROUP: &str = "1 0 000 0 00 000 0 00 00 0 000 0 0 0 0 00";

    /// 与 [`PRESENTATION_SINGLE_GROUP`] 相同，但 presentation 级 EMDF 在
    /// substream 1 携带 payload。该路由允许逐帧出现或消失。
    const PRESENTATION_WITH_EMDF_PAYLOAD: &str = "1 0 000 0 00 000 1 01 00 00 0 000 0 0 0 0 00";

    /// 构造完整帧并解析。
    ///
    /// 帧按位串长度裁剪：多余的零字节会被解析器当作可读负载，让本该触发
    /// 截断错误的用例反而成功。
    #[expect(
        clippy::arithmetic_side_effects,
        reason = "位串打包的索引与移位受输入长度约束"
    )]
    fn parse_frame(parts: &[&str]) -> Ac4Topology {
        let mut joined = [0u8; 64];
        let mut bits = 0usize;
        for part in parts {
            for ch in part.chars() {
                if ch == '0' || ch == '1' {
                    if ch == '1'
                        && let Some(slot) = joined.get_mut(bits / 8)
                    {
                        *slot |= 1 << (7 - bits % 8);
                    }
                    bits = bits.saturating_add(1);
                }
            }
        }
        let length = bits.div_ceil(8);
        Ac4Topology::parse(joined.get(..length).unwrap_or(&joined)).unwrap()
    }

    /// direct-coded object 路径在当前编码链下无法产生，只能由构造的码流覆盖。
    #[test]
    fn recognises_direct_object_path() {
        // group: substreams_present=1, hsf_ext=0, single_substream=1,
        // channel_coded=0, oamd_substream=0, b_ajoc=0 → ac4_substream_info_obj
        //   n_objects_code=010(=2), b_dynamic_objects=1, b_lfe=0,
        //   b_sf_multiplier=0, b_bitrate_info=0, b_audio_ndot=0,
        //   substream_index=01, b_content_type=0
        let group = "1 0 1 0 0 0 010 1 0 0 0 0 01 0";
        // index table: n_substreams=10(=2)，两条尺寸 16 与 32
        let table = "10 0 0000010000 0 0000100000";
        let topology = parse_frame(&[TOC_PREFIX, PRESENTATION_SINGLE_GROUP, group, table]);

        assert_eq!(topology.scene_path(), ScenePath::DirectObject);
        assert_eq!(topology.presentations().len(), 1);
        assert_eq!(topology.groups().len(), 1);
        assert_eq!(topology.total_objects(), 2);
        assert_eq!(
            topology.presentation_substream_selection_context(0),
            Some(PresentationSubstreamSelectionContext::new(false, 1))
        );
        assert_eq!(
            topology.presentation_substream_context(0),
            Some(PresentationSubstreamContext::new(false, 1, 1, true))
        );
        assert_eq!(topology.presentation_substream_selection_context(1), None);
        assert_eq!(topology.presentation_substream_context(1), None);

        let substream = topology
            .groups()
            .first()
            .unwrap()
            .substreams()
            .first()
            .unwrap();
        assert_eq!(substream.kind(), "direct_object");
        assert_eq!(substream.substream_index(), Some(1));
        assert_eq!(topology.index_table.n_substreams, 2);
        assert_eq!(topology.index_table.sizes().first().unwrap().bytes, 16);
        assert_eq!(topology.index_table.sizes().get(1).unwrap().bytes, 32);
        validate_group_references(&topology).unwrap();
        validate_substream_references(&topology).unwrap();

        // Pseudocode 1：偏移 = payload_base 加上所有更小下标的尺寸累加，
        // 全部相对字节对齐的 ac4_toc 末尾。定位只用尺寸表，不看载荷内容。
        let base = usize::try_from(
            topology
                .toc_bytes()
                .saturating_add(u64::from(topology.toc.payload_base)),
        )
        .unwrap();
        let mut frame = [0u8; 128];
        *frame.get_mut(base).unwrap() = 0xAA;
        *frame.get_mut(base.saturating_add(16)).unwrap() = 0xBB;
        assert_eq!(
            topology.substream_payload(&frame, 0).unwrap().first(),
            Some(&0xAA),
            "substream 0 起点即 payload_base"
        );
        assert_eq!(
            topology.substream_payload(&frame, 1).unwrap().first(),
            Some(&0xBB),
            "substream 1 紧接 substream 0 的 16 字节之后"
        );
        assert_eq!(topology.substream_payload(&frame, 0).unwrap().len(), 16);
        assert_eq!(topology.substream_payload(&frame, 1).unwrap().len(), 32);
        assert!(matches!(
            topology.substream_payload(&frame, 2),
            Err(TopologyError::SubstreamIndexOutOfRange { index: 2, total: 2 })
        ));
        // 帧不够长时必须报错，而不是截断出短切片。
        assert!(matches!(
            topology.substream_payload(frame.get(..base.saturating_add(20)).unwrap(), 1),
            Err(TopologyError::SubstreamPayloadOutOfFrame { index: 1, .. })
        ));
    }

    /// config 1 的第二个 SGI 是 dialogue-enhancement group，但规范仍要求它进入
    /// `n_substreams_in_presentation` 的 outer loop；不能因 `n_substream_groups == 1`
    /// 而只数 main group。
    #[test]
    fn presentation_selection_context_counts_every_sgi_group() {
        // config 1: main group 0 + DE group 1；alternative presentation substream 0。
        let presentation = "0 001 0 000 0 00 000 0 00 00 0 0 000 001 0 0 1 0 00";
        // group 0：一个 direct-object substream，物理 index 1。
        let main_group = "1 0 1 0 0 0 010 1 0 0 0 0 01 0";
        // group 1：两个 direct-object substream，物理 index 2、3。
        let de_group = "1 0 0 00 0 0 \
                        0 000 1 0 0 0 0 10 \
                        0 000 1 0 0 0 0 11 \
                        0";
        // presentation 0 加三条 audio substream，共四个物理 payload。
        let table = "00 00 0 \
                     0 0000000001 0 0000000001 \
                     0 0000000001 0 0000000001";
        let topology = parse_frame(&[
            TOC_PREFIX,
            presentation,
            main_group,
            de_group,
            table,
            "00000000",
        ]);

        assert_eq!(
            topology.presentations().first().unwrap().n_substream_groups,
            1
        );
        assert_eq!(
            topology.presentations().first().unwrap().group_indices(),
            &[0, 1]
        );
        assert_eq!(
            topology.presentation_substream_selection_context(0),
            Some(PresentationSubstreamSelectionContext::new(true, 3))
        );
        assert_eq!(
            topology.presentation_substream_context(0),
            Some(PresentationSubstreamContext::new(true, 3, 1, true))
        );
    }

    /// 扩展 presentation config 按当前规范不携带 SGI，但普通 presentation 没有
    /// alternative selection 前缀，零音频 substream 上下文仍足以精确解析该前缀。
    #[test]
    fn ordinary_extended_config_keeps_selection_context() {
        // config 7 + zero-length presentation_config_ext_info；普通 presentation substream 0。
        let presentation = "0 111 00 0 0 000 0 00 000 0 00 00 0 0 00000 0 0 0 0 0 00";
        let topology = parse_frame(&[TOC_PREFIX, presentation, "01 0"]);

        assert_eq!(
            topology
                .presentations()
                .first()
                .unwrap()
                .presentation_config,
            Some(7)
        );
        assert!(
            topology
                .presentations()
                .first()
                .unwrap()
                .group_indices()
                .is_empty()
        );
        assert!(topology.groups().is_empty());
        assert_eq!(
            topology.presentation_substream_selection_context(0),
            Some(PresentationSubstreamSelectionContext::new(false, 0))
        );
        assert_eq!(
            topology.presentation_substream_context(0),
            Some(PresentationSubstreamContext::new(false, 0, 0, true))
        );
    }

    /// b_lfe 参与对象计数，见表 60。
    #[test]
    fn direct_object_counts_lfe() {
        // n_objects_code=100(=5)，b_dynamic_objects=1，b_lfe=1 → 5+1=6
        let group = "1 0 1 0 0 0 100 1 1 0 0 0 00 0";
        let table = "01 0";
        let topology = parse_frame(&[TOC_PREFIX, PRESENTATION_SINGLE_GROUP, group, table]);
        assert_eq!(topology.total_objects(), 6);

        let descriptors =
            crate::oamd::ObjectDescriptors::from_group(topology.groups().first().unwrap()).unwrap();
        assert!(descriptors.as_slice().first().unwrap().b_lfe);
        assert!(matches!(
            descriptors.as_slice().get(1).unwrap().obj_type,
            crate::oamd::ObjectType::Dynamic
        ));

        // 单 substream 省略尺寸时，唯一载荷从 payload_base 延伸到帧尾。
        let base = usize::try_from(
            topology
                .toc_bytes()
                .saturating_add(u64::from(topology.toc.payload_base)),
        )
        .unwrap();
        let mut frame = [0u8; 128];
        let payload_end = base.saturating_add(3);
        frame
            .get_mut(base..payload_end)
            .unwrap()
            .copy_from_slice(&[1, 2, 3]);
        assert_eq!(
            topology
                .substream_payload(frame.get(..payload_end).unwrap(), 0)
                .unwrap(),
            &[1, 2, 3]
        );
    }

    /// A-JOC 路径：core 与 full 两种解码模式各自声明信号数。
    #[test]
    fn recognises_ajoc_path() {
        // group 头同上，但 b_ajoc=1 → ac4_substream_info_ajoc
        //   b_lfe=1, b_static_dmx=0, n_fullband_dmx_signals_minus1=1000(=8) → 9,
        //   bed_dyn_obj_assignment(9): b_dyn_objects_only=1,
        //   b_oamd_common_data_present=0,
        //   n_fullband_upmix_signals_minus1=0011(=3) → 4,
        //   bed_dyn_obj_assignment(4): b_dyn_objects_only=1,
        //   b_sf_multiplier=0, b_bitrate_info=0, b_audio_ndot=0,
        //   substream_index=10, b_content_type=0
        let group = "1 0 1 0 0 1 1 0 1000 1 0 0011 1 0 0 0 10 0";
        let table = "01 0";
        let topology = parse_frame(&[TOC_PREFIX, PRESENTATION_SINGLE_GROUP, group, table]);

        assert_eq!(topology.scene_path(), ScenePath::Ajoc);
        assert_eq!(
            topology.presentation_substream_context(0),
            Some(PresentationSubstreamContext::new(false, 1, 1, true))
        );
        assert_eq!(
            topology.total_objects(),
            5,
            "对象数取 full 模式的上混信号数并计入 LFE"
        );

        let group = topology.groups().first().unwrap();
        let SubstreamInfo::Ajoc(ajoc) = group.substreams().first().unwrap() else {
            panic!("应识别为 A-JOC");
        };
        assert!(ajoc.b_lfe);
        assert!(!ajoc.static_dmx);
        assert_eq!(ajoc.n_dmx_signals, 9);
        assert_eq!(ajoc.n_upmix_signals, 4);
        assert_eq!(ajoc.substream_index(), Some(2));

        // audio_data_ajoc 的两种模式使用各自的 assignment；LFE 都固定在信号 0。
        let dmx_descriptors = crate::oamd::ObjectDescriptors::from_ajoc_assignment(
            ajoc.dmx_assignment.unwrap(),
            ajoc.b_lfe,
        )
        .unwrap();
        let umx_descriptors =
            crate::oamd::ObjectDescriptors::from_ajoc_assignment(ajoc.upmix_assignment, ajoc.b_lfe)
                .unwrap();
        assert_eq!(dmx_descriptors.as_slice().len(), 10);
        assert_eq!(umx_descriptors.as_slice().len(), 5);
        assert!(dmx_descriptors.as_slice().first().unwrap().b_lfe);
        assert!(umx_descriptors.as_slice().first().unwrap().b_lfe);
        assert!(
            dmx_descriptors
                .as_slice()
                .iter()
                .skip(1)
                .all(|object| !object.b_lfe)
        );
        assert!(
            umx_descriptors
                .as_slice()
                .iter()
                .skip(1)
                .all(|object| !object.b_lfe)
        );

        let group_descriptors = crate::oamd::ObjectDescriptors::from_group(group).unwrap();
        assert_eq!(group_descriptors.as_slice().len(), 5);
        assert!(group_descriptors.as_slice().first().unwrap().b_lfe);
    }

    /// 声道编码路径不产生对象。
    #[test]
    fn recognises_channel_based_path() {
        // channel_coded=1；bitstream_version=2 时不读 sus_ver
        //   channel_mode=1110(=5.1), b_sf_multiplier=0, b_bitrate_info=0,
        //   b_audio_ndot=0, substream_index=00, b_content_type=0
        let group = "1 0 1 1 1110 0 0 0 00 0";
        let table = "01 0";
        let topology = parse_frame(&[TOC_PREFIX, PRESENTATION_SINGLE_GROUP, group, table]);

        assert_eq!(topology.scene_path(), ScenePath::ChannelBased);
        assert_eq!(topology.total_objects(), 0);
        assert_eq!(
            topology.presentation_substream_context(0),
            Some(PresentationSubstreamContext::new(false, 1, 1, false))
        );
        let SubstreamInfo::Chan(chan) = topology
            .groups()
            .first()
            .unwrap()
            .substreams()
            .first()
            .unwrap()
        else {
            panic!("应识别为声道编码");
        };
        assert_eq!(chan.channel_mode.label(), Some("5.1"));
    }

    /// TOC 前置字段中把 b_iframe_global 置 0 的变体。
    const TOC_PREFIX_NOT_IFRAME: &str = "10 0000000000 0 1 1101 0 1 0 0";

    /// 与 [`PRESENTATION_SINGLE_GROUP`] 相同，但 b_pres_ndot 置 1。
    ///
    /// 基础常量把该位置为 0，即 presentation substream 依赖前序帧；完整
    /// 随机访问的用例需要它为 1。
    const PRESENTATION_NDOT: &str = "1 0 000 0 00 000 0 00 00 0 000 0 0 0 1 00";

    /// b_iframe_global 只覆盖每个 presentation 的首个 substream，
    /// 完整随机访问还需要 OAMD 与 presentation substream 也无时间依赖。
    #[test]
    fn full_random_access_requires_every_ndot() {
        // group：对象编码，带 OAMD substream（b_oamd_ndot=1），A-JOC
        // b_audio_ndot=1
        let group_all_ndot = "1 0 1 0 1 1 01 1 1 0 1000 1 0 0011 1 0 0 1 10 0";
        let table = "01 0";
        let topology = parse_frame(&[TOC_PREFIX, PRESENTATION_NDOT, group_all_ndot, table]);
        assert_eq!(topology.random_access(), RandomAccess::Full);

        // 同样的 group，但 b_oamd_ndot=0
        let group_oamd_dependent = "1 0 1 0 1 0 01 1 1 0 1000 1 0 0011 1 0 0 1 10 0";
        let topology = parse_frame(&[TOC_PREFIX, PRESENTATION_NDOT, group_oamd_dependent, table]);
        assert_eq!(
            topology.random_access(),
            RandomAccess::AudioOnly,
            "OAMD 依赖前序帧时不是完整随机访问点"
        );

        // b_audio_ndot=0
        let group_audio_dependent = "1 0 1 0 1 1 01 1 1 0 1000 1 0 0011 1 0 0 0 10 0";
        let topology = parse_frame(&[TOC_PREFIX, PRESENTATION_NDOT, group_audio_dependent, table]);
        assert_eq!(topology.random_access(), RandomAccess::AudioOnly);

        // presentation substream 的 b_pres_ndot=0
        let topology = parse_frame(&[TOC_PREFIX, PRESENTATION_SINGLE_GROUP, group_all_ndot, table]);
        assert_eq!(topology.random_access(), RandomAccess::AudioOnly);
    }

    /// b_iframe_global 为假时一律不可作为起解点。
    #[test]
    fn no_random_access_without_iframe_global() {
        let group = "1 0 1 0 1 1 01 1 1 0 1000 1 0 0011 1 0 0 1 10 0";
        let topology = parse_frame(&[TOC_PREFIX_NOT_IFRAME, PRESENTATION_NDOT, group, "01 0"]);
        assert_eq!(topology.random_access(), RandomAccess::None);
    }

    /// 配置指纹应忽略逐帧变化的字段，只在真正需要重配时改变。
    #[test]
    fn config_fingerprint_ignores_per_frame_fields() {
        let group = "1 0 1 1 1110 0 0 1 00 0";
        let base = parse_frame(&[TOC_PREFIX, PRESENTATION_SINGLE_GROUP, group, "01 0"]);
        // 仅 sequence_counter 不同
        let other_sequence = parse_frame(&[
            "10 1000000001 0 1 1101 1 1 0 0",
            PRESENTATION_SINGLE_GROUP,
            group,
            "01 0",
        ]);
        assert_eq!(
            base.config_fingerprint(),
            other_sequence.config_fingerprint(),
            "sequence_counter 变化不构成重配置"
        );

        // 编码路径不同则必须重配置
        let ajoc_group = "1 0 1 0 0 1 1 0 1000 1 0 0011 1 0 0 0 10 0";
        let ajoc = parse_frame(&[TOC_PREFIX, PRESENTATION_SINGLE_GROUP, ajoc_group, "01 0"]);
        assert_ne!(base.config_fingerprint(), ajoc.config_fingerprint());
        assert_eq!(
            base.config_fingerprint().scene_path,
            ScenePath::ChannelBased
        );
        assert_eq!(ajoc.config_fingerprint().scene_path, ScenePath::Ajoc);

        // A-JOC core 信号数不同，即使 full 对象数相同也需重配。
        let other_ajoc_group = "1 0 1 0 0 1 1 0 0111 1 0 0011 1 0 0 0 10 0";
        let other_ajoc = parse_frame(&[
            TOC_PREFIX,
            PRESENTATION_SINGLE_GROUP,
            other_ajoc_group,
            "01 0",
        ]);
        assert_eq!(ajoc.scene_path(), other_ajoc.scene_path());
        assert_eq!(ajoc.total_objects(), other_ajoc.total_objects());
        assert_ne!(ajoc.config_fingerprint(), other_ajoc.config_fingerprint());

        // 相同路径与数量下，声道模式不同仍需重配。
        let stereo_group = "1 0 1 1 10 0 0 1 00 0";
        let stereo = parse_frame(&[TOC_PREFIX, PRESENTATION_SINGLE_GROUP, stereo_group, "01 0"]);
        assert_eq!(base.scene_path(), stereo.scene_path());
        assert_eq!(base.total_objects(), stereo.total_objects());
        assert_ne!(base.config_fingerprint(), stereo.config_fingerprint());

        // audio/presentation ndot 是逐帧随机访问信息，不开启新代次。
        let dependent_group = "1 0 1 1 1110 0 0 0 00 0";
        let dependent = parse_frame(&[TOC_PREFIX, PRESENTATION_NDOT, dependent_group, "01 0"]);
        assert_eq!(base.config_fingerprint(), dependent.config_fingerprint());

        // EMDF payload substream 只在实际携带负载的帧出现；总 substream 数和
        // payload 路由随之变化，但固定的 presentation/audio 映射没有改变。
        let with_emdf = parse_frame(&[
            TOC_PREFIX,
            PRESENTATION_WITH_EMDF_PAYLOAD,
            group,
            "10 0 0000000001 0 0000000001",
        ]);
        assert_eq!(with_emdf.index_table.n_substreams, 2);
        assert_eq!(base.index_table.n_substreams, 1);
        assert_eq!(base.config_fingerprint(), with_emdf.config_fingerprint());
    }

    #[test]
    fn config_fingerprint_span_includes_every_multiplied_audio_substream() {
        let cases = [
            (
                // frame_rate_multiply_info=10 => factor 2
                "1 0 000 0 10 00 000 0 00 00 0 000 0 0 0 0 00",
                // 两个 b_audio_ndot，音频首下标为 0。
                "1 0 1 1 1110 0 0 11 00 0",
                "10 0 0000000001 0 0000000001",
                2,
            ),
            (
                // frame_rate_multiply_info=11 => factor 4
                "1 0 000 0 11 00 000 0 00 00 0 000 0 0 0 0 00",
                // 四个 b_audio_ndot，音频首下标为 0。
                "1 0 1 1 1110 0 0 1111 00 0",
                // n_substreams=0 + variable_bits(2)=0 表示 4 项。
                "00 00 0 0 0000000001 0 0000000001 0 0000000001 0 0000000001",
                4,
            ),
        ];

        for (presentation, group, table, factor) in cases {
            let topology = parse_frame(&[TOC_PREFIX_MULTIPLIED, presentation, group, table]);
            assert_eq!(topology.groups().first().unwrap().frame_rate_factor, factor);
            assert_eq!(topology.config_fingerprint().n_substreams, factor);
            validate_substream_references(&topology).unwrap();
        }
    }

    #[test]
    fn substream_reference_validation_rejects_gaps_and_overruns() {
        // presentation 引用 0，direct-object 音频引用 1，第 2 条无主。
        let direct_group = "1 0 1 0 0 0 010 1 0 0 0 0 01 0";
        let with_gap = parse_frame(&[
            TOC_PREFIX,
            PRESENTATION_SINGLE_GROUP,
            direct_group,
            "11 0 0000000001 0 0000000001 0 0000000001",
        ]);
        assert_eq!(
            validate_substream_references(&with_gap),
            Err(TopologyError::UnreferencedSubstream { index: 2 })
        );

        // A-JOC 音频引用 2，而表中只有 0。
        let ajoc_group = "1 0 1 0 0 1 1 0 1000 1 0 0011 1 0 0 0 10 0";
        let overrun = parse_frame(&[TOC_PREFIX, PRESENTATION_SINGLE_GROUP, ajoc_group, "01 0"]);
        assert_eq!(
            validate_substream_references(&overrun),
            Err(TopologyError::SubstreamIndexOutOfRange { index: 2, total: 1 })
        );
    }

    #[test]
    fn state_machine_gates_resets_on_full_random_access() {
        let full_group = "1 0 1 1 1110 0 0 1 00 0";
        let first = parse_frame(&[TOC_PREFIX, PRESENTATION_NDOT, full_group, "01 0"]);
        let sequential = parse_frame(&[
            "10 0000000001 0 1 1101 1 1 0 0",
            PRESENTATION_NDOT,
            full_group,
            "01 0",
        ]);
        let source_change = parse_frame(&[
            "10 0000000011 0 1 1101 1 1 0 0",
            PRESENTATION_NDOT,
            full_group,
            "01 0",
        ]);

        let mut state = TopologyStateMachine::new();
        let transition = state.observe(&first);
        assert_eq!(transition.generation, 1);
        assert!(transition.config_changed);
        assert_eq!(
            transition.action,
            DecoderAction::Reset {
                reason: ResetReason::Initial
            }
        );

        let transition = state.observe(&sequential);
        assert_eq!(transition.sequence, SequenceTransition::Continuous);
        assert_eq!(transition.action, DecoderAction::Continue);

        let transition = state.observe(&source_change);
        assert_eq!(transition.sequence, SequenceTransition::SourceChange);
        assert_eq!(transition.generation, 1);
        assert_eq!(
            transition.action,
            DecoderAction::Reset {
                reason: ResetReason::SourceChange
            }
        );

        state.mark_discontinuity(ResetReason::ExternalDiscontinuity);
        let dependent =
            parse_frame(&[TOC_PREFIX_NOT_IFRAME, PRESENTATION_NDOT, full_group, "01 0"]);
        let transition = state.observe(&dependent);
        assert_eq!(
            transition.action,
            DecoderAction::WaitForRandomAccess {
                reason: ResetReason::ExternalDiscontinuity
            }
        );
        assert!(state.is_waiting_for_random_access());

        let transition = state.observe(&sequential);
        assert_eq!(
            transition.action,
            DecoderAction::Reset {
                reason: ResetReason::ExternalDiscontinuity
            }
        );
        assert!(!state.is_waiting_for_random_access());
    }

    #[test]
    fn state_machine_starts_new_generation_for_topology_change() {
        let surround_group = "1 0 1 1 1110 0 0 1 00 0";
        let stereo_group = "1 0 1 1 10 0 0 1 00 0";
        let surround = parse_frame(&[TOC_PREFIX, PRESENTATION_NDOT, surround_group, "01 0"]);
        let stereo = parse_frame(&[
            "10 0000000001 0 1 1101 1 1 0 0",
            PRESENTATION_NDOT,
            stereo_group,
            "01 0",
        ]);

        let mut state = TopologyStateMachine::new();
        let _ = state.observe(&surround);
        let transition = state.observe(&stereo);
        assert_eq!(transition.generation, 2);
        assert!(transition.config_changed);
        assert_eq!(
            transition.action,
            DecoderAction::Reset {
                reason: ResetReason::ConfigurationChange
            }
        );
    }

    /// bitstream_version ≤ 1 使用 ac4_presentation_info()，本实现明确拒绝。
    #[test]
    fn rejects_legacy_bitstream_version() {
        // bitstream_version=01(=1)
        let (data, length) = pack_frame("01 0000000000 0 1 1101 1 1 0");
        assert!(matches!(
            Ac4Topology::parse(data.get(..length).unwrap()).unwrap_err(),
            TopologyError::Unsupported {
                what: Unsupported::LegacyPresentationInfo {
                    bitstream_version: 1
                },
                ..
            }
        ));
    }

    /// 当前规范只定义到版本 2，不得将未来版本当作 v2 解析。
    #[test]
    fn rejects_future_bitstream_version() {
        // bitstream_version=3 + variable_bits(2)的 1 = 4
        let (data, length) = pack_frame("11 01 0 0000000000 0 1 1101 1 1 0");
        assert!(matches!(
            Ac4Topology::parse(data.get(..length).unwrap()).unwrap_err(),
            TopologyError::Unsupported {
                what: Unsupported::FutureBitstreamVersion {
                    bitstream_version: 4
                },
                ..
            }
        ));
    }

    /// 截断的帧必须报错，不得返回部分拓扑。
    #[test]
    fn reports_truncated_frame() {
        let (data, length) = pack_frame(TOC_PREFIX);
        assert!(matches!(
            Ac4Topology::parse(data.get(..length).unwrap()).unwrap_err(),
            TopologyError::Read(_)
        ));
    }

    /// 位串打包为帧字节与有效长度。
    ///
    /// 必须按位串长度裁剪：多余的零字节会被解析器当作可读负载，让本该
    /// 触发截断错误的用例反而成功。
    fn pack_frame(bits: &str) -> ([u8; 16], usize) {
        let count = bits.chars().filter(|ch| *ch == '0' || *ch == '1').count();
        (pack(bits), count.div_ceil(8))
    }

    /// 位串打包 helper，与 `toc.rs` 中的同名函数一致。
    #[expect(
        clippy::arithmetic_side_effects,
        clippy::indexing_slicing,
        reason = "测试内的位串打包，索引受输入长度约束"
    )]
    fn pack(bits: &str) -> [u8; 16] {
        let mut out = [0u8; 16];
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
}
