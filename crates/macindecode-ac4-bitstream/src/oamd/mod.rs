//! Object Audio Metadata（OAMD）。
//!
//! 对应 `TS103190-2:v1.3.1:6.2.8`（语法）与 `6.3.9`（语义）。
//!
//! # OAMD 不在单一位置
//!
//! `4.8.3.4.2` 表 7 规定 OAMD 分散在多个比特流层，位置取决于 `b_ajoc`：
//!
//! | 部分 | `b_ajoc` 为假 | `b_ajoc` 为真 |
//! |---|---|---|
//! | 配置数据 | `ac4_substream_info_obj` | `ac4_substream_info_ajoc` |
//! | 公共数据 | `oamd_substream` | `oamd_substream` 或 `ac4_substream_info_ajoc` |
//! | 时间数据 | `oamd_substream` | `oamd_substream` 或 `audio_data_ajoc` |
//! | 动态数据 | 通常为 `oamd_dyndata_multi`，alternative 时为 `metadata()` 内的 `oamd_dyndata_single` | `oamd_dyndata_single`，在 `audio_data_ajoc` |
//!
//! 本模块同时提供 `oamd_substream` 载荷解析和共享的 `oamd_dyndata_single` 语法模型。
//! non-A-JOC alternative 路径由 `audio_substream::metadata()` 调用后者；A-JOC 路径的
//! 同类数据位于 `audio_data_ajoc`，在 `var_channel_element()` 与 `ajoc()` 之后。
//!
//! # 原始量化值
//!
//! 逐对象的 `object_basic_info()`、`object_render_info()` 与
//! `add_per_object_md()` 保留码流中的量化码值，不在解析层换算为增益或坐标。
//! 公共数据中的 `trim()` 与 `bed_render_info()` 当前只公开结构性字段，其余码值
//! 仍按语法消费，以维持精确的载荷边界。

use crate::reader::{BitReader, ReadError};
use core::fmt;

/// 一个 `oamd_timing_data()` 中可携带的 `object_info_block` 上限。
///
/// `num_obj_info_blocks` 为 3 比特字段，取值不超过 7。
pub const MAX_OBJ_INFO_BLOCKS: usize = 8;

/// 单个 substream group 中可描述的对象数上限。
pub const MAX_OAMD_OBJECTS: usize = 32;

/// 一个 `oamd_substream()` 中可返回的逐对象更新上限。
pub const MAX_OAMD_METADATA_BLOCKS: usize = MAX_OAMD_OBJECTS * MAX_OBJ_INFO_BLOCKS;

/// `trim()` 的配置数，见 `6.3.9.10.4`。
pub const NUM_TRIM_CONFIGS: usize = 9;

/// OAMD 解析失败的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OamdError {
    /// 底层读取失败。
    Read(ReadError),
    /// `oamd_substream()` 以 `byte_align` 结尾，残余比特必须少于 8。
    ///
    /// 残余过多说明前面某个可变长字段解析错位，而不是码流有额外数据。
    Misaligned {
        /// 解析结束后剩余的比特数。
        remaining_bits: u64,
    },
    /// `add_data_bytes` 声明的字节数不足以容纳已解析的子元素。
    AdditionalDataUnderflow {
        /// 声明的字节数。
        declared_bytes: u64,
        /// 子元素实际消耗的比特数。
        used_bits: u64,
    },
    /// `num_obj_info_blocks` 超出本实现的固定容量。
    TooManyBlocks {
        /// 声明的块数。
        declared: u32,
    },
    /// 对象数超出本实现的固定容量。
    TooManyObjects {
        /// 本实现的上限。
        limit: usize,
    },
    /// 单个载荷的逐对象更新总数超过固定容量。
    TooManyMetadataBlocks {
        /// 本实现的上限。
        limit: usize,
    },
    /// 时间数据缺失，且没有可延续的前序状态。
    ///
    /// `b_oamd_timing_present` 为假时 `num_obj_info_blocks` 不在本帧传输，
    /// 只能取自前一帧。随机访问点之后若仍缺失，说明码流不自洽。
    TimingUnavailable,
    /// I-frame 的 `oamd_dyndata_single()` 没有任何对象信息块。
    ZeroBlocksInIframe,
    /// alternative dataset 存在，但该 substream 的对象描述为空。
    AlternativeDataWithoutObjects {
        /// 声明的 alternative dataset 数。
        data_sets: u32,
        /// 检测到矛盾时的比特偏移。
        bit_position: u64,
    },
}

impl fmt::Display for OamdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            OamdError::Read(error) => write!(f, "Failed to read OAMD: {error}"),
            OamdError::Misaligned { remaining_bits } => write!(
                f,
                "{remaining_bits} bits remain after parsing oamd_substream, exceeding byte_align range 0..7"
            ),
            OamdError::AdditionalDataUnderflow {
                declared_bytes,
                used_bits,
            } => write!(
                f,
                "add_data declares {declared_bytes} bytes, but child elements already consumed {used_bits} bits"
            ),
            OamdError::TooManyBlocks { declared } => write!(
                f,
                "num_obj_info_blocks is {declared}, exceeding implementation limit {MAX_OBJ_INFO_BLOCKS}"
            ),
            OamdError::TooManyObjects { limit } => {
                write!(
                    f,
                    "Substream-group object count exceeds implementation limit {limit}"
                )
            }
            OamdError::TooManyMetadataBlocks { limit } => {
                write!(
                    f,
                    "Per-object OAMD update count exceeds implementation limit {limit}"
                )
            }
            OamdError::TimingUnavailable => {
                write!(
                    f,
                    "Frame carries no oamd_timing_data and has no prior state to continue"
                )
            }
            OamdError::ZeroBlocksInIframe => {
                write!(
                    f,
                    "I-frame oamd_dyndata_single contains zero object-info blocks"
                )
            }
            OamdError::AlternativeDataWithoutObjects {
                data_sets,
                bit_position,
            } => write!(
                f,
                "Alternative OAMD declares {data_sets} datasets for an empty object list at bit offset {bit_position}"
            ),
        }
    }
}

impl core::error::Error for OamdError {}

impl From<ReadError> for OamdError {
    fn from(error: ReadError) -> Self {
        OamdError::Read(error)
    }
}

mod alternative;
mod common;
mod descriptors;
mod object;
mod payload;
mod state;

pub use alternative::*;
pub use common::*;
pub use descriptors::*;
pub use object::*;
pub use payload::*;
pub use state::*;

#[cfg(test)]
#[expect(
    clippy::indexing_slicing,
    reason = "测试内的位串切片，长度由 pack 的返回值决定"
)]
mod tests {
    use super::*;

    /// 位串打包 helper，与 `topology.rs` 中的同名函数一致。
    #[expect(
        clippy::arithmetic_side_effects,
        clippy::indexing_slicing,
        reason = "测试内的位串打包，索引受输入长度约束"
    )]
    fn pack(bits: &str) -> ([u8; 32], usize) {
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
        (out, index.div_ceil(8))
    }

    fn payload(bits: &str) -> ([u8; 32], usize) {
        pack(bits)
    }

    const NO_OBJECTS: &[ObjectDescriptor] = &[];

    fn context() -> OamdContext<'static> {
        OamdContext {
            objects: NO_OBJECTS,
            b_alternative: false,
            b_oamd_ndot: true,
            previous_num_obj_info_blocks: None,
        }
    }

    /// 表 92 与表 93 的三条 `sample_offset` 路径。
    #[test]
    fn sample_offset_follows_prefix_tables() {
        // 0b0 -> 0；num_obj_info_blocks = 0。
        let (bits, len) = payload("0 000");
        let mut reader = BitReader::new(&bits[..len]);
        let timing = OamdTimingData::parse(&mut reader).unwrap();
        assert_eq!(timing.offset_source, SampleOffsetSource::Implicit);
        assert_eq!(timing.sample_offset, 0);

        // 0b10 + 0b0 -> 16
        let (bits, len) = payload("10 0 000");
        let mut reader = BitReader::new(&bits[..len]);
        assert_eq!(
            OamdTimingData::parse(&mut reader).unwrap().sample_offset,
            16
        );

        // 0b10 + 0b10 -> 8
        let (bits, len) = payload("10 10 000");
        let mut reader = BitReader::new(&bits[..len]);
        assert_eq!(OamdTimingData::parse(&mut reader).unwrap().sample_offset, 8);

        // 0b10 + 0b11 -> 24
        let (bits, len) = payload("10 11 000");
        let mut reader = BitReader::new(&bits[..len]);
        assert_eq!(
            OamdTimingData::parse(&mut reader).unwrap().sample_offset,
            24
        );

        // 0b11 + 5 比特直接给出
        let (bits, len) = payload("11 10101 000");
        let mut reader = BitReader::new(&bits[..len]);
        let timing = OamdTimingData::parse(&mut reader).unwrap();
        assert_eq!(timing.offset_source, SampleOffsetSource::Explicit);
        assert_eq!(timing.sample_offset, 21);
    }

    /// 表 94 的四个 `ramp_duration_code` 分支。
    #[test]
    fn ramp_duration_follows_code_table() {
        // 四个块：码 0b00/0b01/0b10，以及 0b11 走 ramp_duration_table[15]。
        let (bits, len) = payload(
            "0 100 \
             000001 00 \
             000010 01 \
             000011 10 \
             000100 11 1 1111",
        );
        let mut reader = BitReader::new(&bits[..len]);
        let timing = OamdTimingData::parse(&mut reader).unwrap();
        let blocks = timing.blocks();
        assert_eq!(blocks.len(), 4);
        assert_eq!(blocks[0].ramp_duration, 0);
        assert_eq!(blocks[0].ramp_duration_encoding, RampDurationEncoding::Zero);
        assert_eq!(blocks[1].ramp_duration, 512);
        assert_eq!(
            blocks[1].ramp_duration_encoding,
            RampDurationEncoding::Fixed512
        );
        assert_eq!(blocks[2].ramp_duration, 1_536);
        assert_eq!(
            blocks[2].ramp_duration_encoding,
            RampDurationEncoding::Fixed1536
        );
        assert_eq!(
            blocks[3].ramp_duration, 2_048,
            "表 95 的末项是 2 048，不是更大的 2 的幂"
        );
        assert_eq!(
            blocks[3].ramp_duration_encoding,
            RampDurationEncoding::Table { index: 15 }
        );
        // block_offset_factor 以 32 采样为单位。
        assert_eq!(blocks[0].offset_samples(), 32);
        assert_eq!(blocks[3].offset_samples(), 128);
    }

    /// `ramp_duration_code == 0b11` 且不使用表时，为 11 比特直接编码。
    #[test]
    fn ramp_duration_supports_explicit_eleven_bits() {
        let (bits, len) = payload("0 001 000000 11 0 11111111111");
        let mut reader = BitReader::new(&bits[..len]);
        let timing = OamdTimingData::parse(&mut reader).unwrap();
        assert_eq!(
            timing.blocks()[0].ramp_duration,
            2_047,
            "11 比特元素的上界即 6.3.9.3.8 声明的范围"
        );
        assert_eq!(
            timing.blocks()[0].ramp_duration_encoding,
            RampDurationEncoding::Explicit { value: 2_047 }
        );
    }

    #[test]
    fn equal_ramp_durations_keep_distinct_wire_encodings() {
        // 固定码 0b01 与 11 比特显式值都可表示 512，原始来源仍必须可区分。
        let (bits, len) = payload(
            "0 010 \
             000000 01 \
             000001 11 0 01000000000",
        );
        let mut reader = BitReader::new(&bits[..len]);
        let timing = OamdTimingData::parse(&mut reader).unwrap();
        let blocks = timing.blocks();

        assert_eq!(blocks[0].ramp_duration, blocks[1].ramp_duration);
        assert_eq!(
            blocks[0].ramp_duration_encoding,
            RampDurationEncoding::Fixed512
        );
        assert_eq!(
            blocks[1].ramp_duration_encoding,
            RampDurationEncoding::Explicit { value: 512 }
        );
    }

    /// `oamd_substream()` 以 byte_align 结尾，残余必须少于 8 比特。
    #[test]
    fn detects_misalignment() {
        // 两个前导 flag 均为 0，随后整字节的多余数据无法由 byte_align 吸收。
        let (bits, len) = payload("0 0 00000000 00000000");
        let error = OamdSubstreamPayload::parse(&bits[..len], context()).unwrap_err();
        assert!(
            matches!(error, OamdError::Misaligned { remaining_bits } if remaining_bits >= 8),
            "实际为 {error:?}"
        );
    }

    /// 无公共数据、无时间数据时只消耗两个 flag。
    #[test]
    fn parses_minimal_substream() {
        let (bits, len) = payload("0 0 000000");
        let parsed = OamdSubstreamPayload::parse(&bits[..len], context()).unwrap();
        assert!(parsed.common.is_none());
        assert!(parsed.timing.is_none());
        assert_eq!(parsed.align_bits, 6);
        assert_eq!(parsed.dyndata_blocks, 0);
    }

    /// `add_data_bytes` 覆盖 trim、bed_render_info 与 headphone。
    #[test]
    fn common_data_consumes_declared_additional_bytes() {
        // b_default_screen_size_ratio=1, b_bed_object_chan_distribute=0,
        // b_additional_data=1, add_data_bytes_minus1=0 -> 1 字节 = 8 比特。
        // 其中 trim(1) + bed_render_info(1) + headphone(1) = 3 比特，余 5 比特跳过。
        let (bits, len) = payload("1 0 1 0 0 0 0 00000");
        let mut reader = BitReader::new(&bits[..len]);
        let common = OamdCommonData::parse(&mut reader).unwrap();
        assert_eq!(common.add_data_bytes, Some(1));
        assert!(!common.trim.present);
        assert!(!common.bed_render_info.present);
        assert!(!common.headphone.present);
        assert_eq!(
            reader.bit_position(),
            12,
            "3 个前导 flag + add_data_bytes_minus1 + 8 比特 add_data"
        );
    }

    /// 超出 `u32` 的变长声明不得窄化为 0 后作为两字节附加数据接受。
    #[test]
    fn common_data_rejects_length_extension_above_u32() {
        // 15 个 continuation group `11 + more` 后以 `00 + stop` 终止，
        // variable_bits(2) = 5_726_623_056；末尾 16 个 0 足以让错误实现成功。
        let (bits, len) = payload(
            "1 0 1 1 \
             111 111 111 111 111 111 111 111 111 111 111 111 111 111 111 000 \
             0000000000000000",
        );
        let mut reader = BitReader::new(&bits[..len]);

        assert!(matches!(
            OamdCommonData::parse(&mut reader),
            Err(OamdError::Read(ReadError::ValueOverflow { .. }))
        ));
    }

    /// `headphone()` 只在模式 0b001/0b010 下附带头部跟踪开关。
    #[test]
    fn headphone_reads_head_tracking_only_for_two_modes() {
        let (bits, len) = payload("1 001 1");
        let mut reader = BitReader::new(&bits[..len]);
        let (headphone, used) = Headphone::parse(&mut reader).unwrap();
        assert_eq!(headphone.head_track_disable_all, Some(true));
        assert_eq!(used, 5);

        let (bits, len) = payload("1 100");
        let mut reader = BitReader::new(&bits[..len]);
        let (headphone, used) = Headphone::parse(&mut reader).unwrap();
        assert_eq!(headphone.head_track_disable_all, None);
        assert_eq!(used, 4);
    }

    /// I-frame 的首块不得引用前序状态。
    #[test]
    fn no_delta_block_forces_all_new() {
        // b_object_not_active=0；b_no_delta 为真，因此不读 b_basic_info_reuse。
        // object_basic_info: b_default_basic_info_md=1。
        // object_render_info(ALL_NEW)：位置 6+6+1+4，zone 与 otherprops 各取默认。
        // 末尾 b_add_table_data=0。
        let (bits, len) = payload("0 1 000000 000000 0 0000 1 1 0");
        let mut reader = BitReader::new(&bits[..len]);
        let block = ObjectInfoBlock::parse(&mut reader, true, true).unwrap();
        assert_eq!(block.basic_info_status, InfoStatus::AllNew);
        assert_eq!(block.render_info_status, InfoStatus::AllNew);
        assert!(!block.diff_pos_coding);
        assert!(block.position_present);
        assert!(!block.depends_on_history(), "随机访问点的首块必须自足");
    }

    /// basic、render 与附加对象字段都保留原始量化码值。
    #[test]
    fn object_block_preserves_all_defined_codes() {
        let (bits, len) = payload(
            "0 \
             0 10 0 101010 10001 \
             000001 000010 0 0100 \
             0 100 101 \
             0 1111 \
             1 00011 00100 00101 \
             110 10 \
             0 1001 \
             10 100001 \
             1 0001 \
             1 1 111 01 10 11 1 10 0 0",
        );
        let mut reader = BitReader::new(&bits[..len]);
        let block = ObjectInfoBlock::parse(&mut reader, true, true).unwrap();

        let basic = block.basic_info.unwrap();
        assert_eq!(basic.basic_info_md, Some(0b10));
        assert_eq!(basic.object_gain_code, Some(0b0));
        assert_eq!(basic.object_gain_value, Some(42));
        assert_eq!(basic.object_priority_code, Some(17));

        let render = block.render_info.unwrap();
        assert_eq!(
            render.position,
            Some(PositionUpdate::Absolute(AbsolutePosition {
                x: 1,
                y: 2,
                z_sign: false,
                z: 4,
            }))
        );
        assert_eq!(render.zone.unwrap().group_zone_flag, Some(0b100));
        assert_eq!(render.zone.unwrap().zone_mask, Some(0b101));
        let other = render.other_properties.unwrap();
        assert_eq!(
            other.width,
            Some(WidthUpdate::Cartesian { x: 3, y: 4, z: 5 })
        );
        assert_eq!(other.screen_factor_code, Some(6));
        assert_eq!(other.depth_factor, Some(2));
        assert_eq!(other.object_at_infinity, Some(false));
        assert_eq!(other.distance_factor_code, Some(9));
        assert_eq!(other.divergence_mode, Some(0b10));
        assert_eq!(other.divergence_code, Some(33));

        assert_eq!(block.additional_data_bytes, Some(2));
        let additional = block.additional_metadata.unwrap();
        assert!(additional.trim_disabled);
        assert_eq!(
            additional.extended_position,
            Some(ExtendedPrecisionPosition {
                presence: 0b111,
                x: Some(1),
                y: Some(2),
                z: Some(3),
            })
        );
        assert_eq!(
            additional.headphone,
            Some(ObjectHeadphone {
                render_mode: 0b10,
                head_tracking_disabled: false,
            })
        );
    }

    /// 非 I-frame 可以完全沿用前序状态。
    #[test]
    fn reuse_status_marks_history_dependency() {
        // b_object_not_active=0, b_basic_info_reuse=1, b_render_info_reuse=1,
        // b_add_table_data=0。
        let (bits, len) = payload("0 1 1 0");
        let mut reader = BitReader::new(&bits[..len]);
        let block = ObjectInfoBlock::parse(&mut reader, false, true).unwrap();
        assert_eq!(block.basic_info_status, InfoStatus::Reuse);
        assert_eq!(block.render_info_status, InfoStatus::Reuse);
        assert!(block.depends_on_history());
    }

    /// 部分沿用只出现在 render info，且掩码决定后续字段。
    #[test]
    fn part_reuse_reads_presence_mask() {
        // b_object_not_active=0, b_basic_info_reuse=1,
        // b_render_info_reuse=0, b_render_info_partial_reuse=1,
        // mask: otherprops=0, zone=0, position=1
        // b_diff_pos_coding=1 -> 3+3+3
        // b_add_table_data=0
        let (bits, len) = payload("0 1 0 1 0 0 1 1 000 000 000 0");
        let mut reader = BitReader::new(&bits[..len]);
        let block = ObjectInfoBlock::parse(&mut reader, false, true).unwrap();
        assert_eq!(block.render_info_status, InfoStatus::PartReuse);
        assert!(block.diff_pos_coding);
        assert!(block.depends_on_history(), "差分位置编码同样依赖前一块");
    }

    /// 不活动的对象使用默认值，不依赖前序帧。
    #[test]
    fn inactive_object_uses_defaults() {
        let (bits, len) = payload("1 0");
        let mut reader = BitReader::new(&bits[..len]);
        let block = ObjectInfoBlock::parse(&mut reader, false, true).unwrap();
        assert!(block.object_not_active);
        assert_eq!(block.basic_info_status, InfoStatus::Default);
        assert_eq!(block.render_info_status, InfoStatus::Default);
        assert!(!block.depends_on_history());
    }

    /// 非动态对象不携带 render info，见 6.2.8.5。
    #[test]
    fn non_dynamic_object_skips_render_info() {
        // b_object_not_active=0, b_basic_info_reuse=1, b_add_table_data=0
        let (bits, len) = payload("0 1 0");
        let mut reader = BitReader::new(&bits[..len]);
        let block = ObjectInfoBlock::parse(&mut reader, false, false).unwrap();
        assert_eq!(block.render_info_status, InfoStatus::Default);
        assert_eq!(reader.bit_position(), 3);
    }

    /// A-JOC 编码的对象不在 oamd_dyndata_multi 中出现。
    #[test]
    fn ajoc_coded_objects_contribute_no_dyndata() {
        let objects = [ObjectDescriptor {
            obj_type: ObjectType::Dynamic,
            b_lfe: false,
            b_ajoc_coded: true,
        }; 4];
        // 时间数据声明 2 个块；若错误地为 A-JOC 对象解析块，会读到越界。
        let (bits, len) = payload("0 1 0 010 000000 00 000001 00 00");
        let parsed = OamdSubstreamPayload::parse(
            &bits[..len],
            OamdContext {
                objects: &objects,
                b_alternative: false,
                b_oamd_ndot: true,
                previous_num_obj_info_blocks: None,
            },
        )
        .unwrap();
        assert_eq!(parsed.dyndata_blocks, 0);
        assert_eq!(parsed.timing.unwrap().num_obj_info_blocks, 2);
    }

    /// 直接编码的对象逐个携带 object_info_block。
    #[test]
    fn direct_coded_objects_contribute_dyndata() {
        let objects = [
            ObjectDescriptor {
                obj_type: ObjectType::Bed,
                b_lfe: true,
                b_ajoc_coded: false,
            },
            ObjectDescriptor {
                obj_type: ObjectType::Dynamic,
                b_lfe: false,
                b_ajoc_coded: false,
            },
        ];
        // 时间数据声明 1 个块；随后每个对象一个块。
        // 规范顺序中对象 0 是 LFE 床对象：非动态，只有 basic info。
        // 对象 1 是动态对象：b_no_delta 为真 -> 不活动标志 + ALL_NEW render info。
        let (bits, len) = payload(
            "0 1 0 001 000000 00 \
             1 0 \
             0 1 000000 000000 0 0000 1 1 0",
        );
        let parsed = OamdSubstreamPayload::parse(
            &bits[..len],
            OamdContext {
                objects: &objects,
                b_alternative: false,
                b_oamd_ndot: true,
                previous_num_obj_info_blocks: None,
            },
        )
        .unwrap();
        assert_eq!(parsed.dyndata_blocks, 2);
        assert_eq!(parsed.history_dependent_blocks, 0);
    }

    /// b_alternative 为真时动态数据不在本 substream，见表 7。
    #[test]
    fn alternative_presentation_moves_dyndata_elsewhere() {
        let objects = [ObjectDescriptor {
            obj_type: ObjectType::Dynamic,
            b_lfe: false,
            b_ajoc_coded: false,
        }];
        let (bits, len) = payload("0 1 0 001 000000 00 00");
        let parsed = OamdSubstreamPayload::parse(
            &bits[..len],
            OamdContext {
                objects: &objects,
                b_alternative: true,
                b_oamd_ndot: true,
                previous_num_obj_info_blocks: None,
            },
        )
        .unwrap();
        assert_eq!(parsed.dyndata_blocks, 0);
    }

    /// 缺少时间数据且无前序状态时必须报错，而不是假定 0 块。
    #[test]
    fn missing_timing_without_history_is_an_error() {
        let objects = [ObjectDescriptor {
            obj_type: ObjectType::Dynamic,
            b_lfe: false,
            b_ajoc_coded: false,
        }];
        let (bits, len) = payload("0 0 000000");
        let error = OamdSubstreamPayload::parse(
            &bits[..len],
            OamdContext {
                objects: &objects,
                b_alternative: false,
                b_oamd_ndot: false,
                previous_num_obj_info_blocks: None,
            },
        )
        .unwrap_err();
        assert_eq!(error, OamdError::TimingUnavailable);
    }

    /// 有前序状态时沿用其块数。
    #[test]
    fn missing_timing_continues_previous_block_count() {
        let objects = [ObjectDescriptor {
            obj_type: ObjectType::Bed,
            b_lfe: false,
            b_ajoc_coded: false,
        }];
        // 沿用 1 个块：床对象非动态，块内为 b_object_not_active=1 + b_add_table_data=0。
        let (bits, len) = payload("0 0 1 0 0000");
        let parsed = OamdSubstreamPayload::parse(
            &bits[..len],
            OamdContext {
                objects: &objects,
                b_alternative: false,
                b_oamd_ndot: false,
                previous_num_obj_info_blocks: Some(1),
            },
        )
        .unwrap();
        assert_eq!(parsed.dyndata_blocks, 1);
    }

    /// 原始码值必须可见，状态层按对象合并帧内差分并在 reset 后拒绝复用。
    #[test]
    fn preserves_quantized_values_and_applies_history() {
        let objects = [ObjectDescriptor {
            obj_type: ObjectType::Dynamic,
            b_lfe: false,
            b_ajoc_coded: false,
        }];

        // I-frame：一个块，显式增益 42、默认优先级，绝对位置 (10, 20, +3)。
        let (bits, len) = payload(
            "0 1 0 001 000000 00 \
             0 0 0 0 101010 001010 010100 1 0011 1 1 0",
        );
        let first = OamdSubstreamPayload::parse(
            &bits[..len],
            OamdContext {
                objects: &objects,
                b_alternative: false,
                b_oamd_ndot: true,
                previous_num_obj_info_blocks: None,
            },
        )
        .unwrap();
        let first_update = first.metadata_blocks().first().unwrap();
        assert_eq!(
            first_update.info.basic_info.unwrap().object_gain_value,
            Some(42)
        );
        assert_eq!(
            first_update.info.render_info.unwrap().position,
            Some(PositionUpdate::Absolute(AbsolutePosition {
                x: 10,
                y: 20,
                z_sign: true,
                z: 3,
            }))
        );

        let mut state = OamdState::new();
        state.apply(&first).unwrap();
        assert_eq!(
            state.object(0).unwrap().basic.unwrap().gain,
            ObjectGainState::Quantized(42)
        );
        assert_eq!(
            state.object(0).unwrap().render.unwrap().position,
            QuantizedPosition {
                x: 10,
                y: 20,
                z: 3,
                coding: PositionCoding::AbsolutePositive,
            }
        );

        // 下一帧沿用 basic，部分更新位置：(+1, -1, +2)。
        let (bits, len) = payload("0 0 0 1 0 1 0 0 1 1 001 111 010 0");
        let second = OamdSubstreamPayload::parse(
            &bits[..len],
            OamdContext {
                objects: &objects,
                b_alternative: false,
                b_oamd_ndot: false,
                previous_num_obj_info_blocks: state.previous_num_obj_info_blocks(),
            },
        )
        .unwrap();
        assert_eq!(second.history_dependent_blocks, 1);
        assert_eq!(
            second
                .metadata_blocks()
                .first()
                .unwrap()
                .info
                .render_info
                .unwrap()
                .position,
            Some(PositionUpdate::Differential(DifferentialPosition {
                x: 0b001,
                y: 0b111,
                z: 0b010,
            }))
        );
        state.apply(&second).unwrap();
        assert_eq!(
            state.object(0).unwrap().render.unwrap().position,
            QuantizedPosition {
                x: 11,
                y: 19,
                z: 5,
                coding: PositionCoding::Differential,
            }
        );
        assert_eq!(
            state.object(0).unwrap().basic.unwrap().gain,
            ObjectGainState::Quantized(42),
            "basic REUSE 必须保留显式增益"
        );

        // 再下一帧传输 ALL_NEW basic，但 gain code=0b11 沿用前值；render 完全复用。
        let (bits, len) = payload("0 0 0 0 0 0 11 1 0");
        let third = OamdSubstreamPayload::parse(
            &bits[..len],
            OamdContext {
                objects: &objects,
                b_alternative: false,
                b_oamd_ndot: false,
                previous_num_obj_info_blocks: state.previous_num_obj_info_blocks(),
            },
        )
        .unwrap();
        state.apply(&third).unwrap();
        let current = state.object(0).unwrap();
        assert_eq!(current.basic.unwrap().gain, ObjectGainState::Quantized(42));
        assert_eq!(
            current.render.unwrap().position,
            QuantizedPosition {
                x: 11,
                y: 19,
                z: 5,
                coding: PositionCoding::Differential,
            }
        );

        state.reset();
        assert!(matches!(
            state.apply(&third),
            Err(OamdStateError::HistoryUnavailable {
                object_index: 0,
                ..
            })
        ));
    }

    /// 整批更新是事务性的：后一块失败时前一块也不得留下。
    ///
    /// A-JOC 路径一次交进来的是**整帧所有对象所有块**，中途失败若已经写下
    /// 前几块，下一帧会在一份半新半旧的状态上继续推进差分位置。
    #[test]
    fn apply_blocks_commits_all_or_nothing() {
        let mut state = OamdState::new();
        let all_new = ObjectInfoBlock {
            basic_info_status: InfoStatus::AllNew,
            basic_info: Some(ObjectBasicInfo {
                default_metadata: true,
                ..ObjectBasicInfo::default()
            }),
            ..ObjectInfoBlock::default()
        };
        let reuse = ObjectInfoBlock {
            basic_info_status: InfoStatus::Reuse,
            ..ObjectInfoBlock::default()
        };

        // 对象 0 自足，对象 1 引用不存在的历史。
        let batch = [
            OamdMetadataBlock {
                object_index: 0,
                block_index: 0,
                info: all_new,
            },
            OamdMetadataBlock {
                object_index: 1,
                block_index: 0,
                info: reuse,
            },
        ];

        assert!(matches!(
            state.apply_blocks(&batch, Some(1)),
            Err(OamdStateError::HistoryUnavailable {
                object_index: 1,
                field: OamdStateField::Basic,
            })
        ));
        assert_eq!(
            state.previous_num_obj_info_blocks(),
            None,
            "失败的一批不得记下块数"
        );
        assert!(
            state.object(0).is_some_and(|item| item.basic.is_none()),
            "对象 0 已成功的那一块也不得提交"
        );

        // 只有对象 0 的那一块时整批成功。
        state
            .apply_blocks(batch.get(..1).unwrap_or(&[]), Some(1))
            .expect("自足的一批应能提交");
        assert!(state.object(0).is_some_and(|item| item.basic.is_some()));
    }

    #[test]
    fn apply_frame_retains_common_timing_but_resets_block_additional_metadata() {
        let (timing_bits, timing_len) = payload("0 001 000011 01");
        let mut reader = BitReader::new(&timing_bits[..timing_len]);
        let timing = OamdTimingData::parse(&mut reader).expect("timing 应可解析");
        let common = OamdCommonData {
            default_screen_size_ratio: false,
            master_screen_size_ratio_code: Some(17),
            bed_object_chan_distribute: true,
            add_data_bytes: None,
            trim: Trim::default(),
            bed_render_info: BedRenderInfo::default(),
            headphone: Headphone::default(),
        };
        let additional = AdditionalObjectMetadata {
            trim_disabled: true,
            extended_position: Some(ExtendedPrecisionPosition {
                presence: 0b100,
                x: Some(1),
                y: None,
                z: None,
            }),
            headphone: Some(ObjectHeadphone {
                render_mode: 2,
                head_tracking_disabled: true,
            }),
        };
        let block = OamdMetadataBlock {
            object_index: 0,
            block_index: 0,
            info: ObjectInfoBlock {
                additional_metadata: Some(additional),
                ..ObjectInfoBlock::default()
            },
        };

        let mut state = OamdState::new();
        state
            .apply_frame(&[block], Some(common), Some(timing))
            .expect("完整帧应可提交");
        assert_eq!(state.effective_common(), Some(common));
        assert_eq!(state.effective_timing(), Some(timing));
        assert_eq!(state.object_additional(0), Some(&additional));

        state
            .apply_frame(&[], None, None)
            .expect("空更新应沿用 common/timing");
        assert_eq!(state.effective_common(), Some(common));
        assert_eq!(state.effective_timing(), Some(timing));

        state
            .apply_frame(
                &[OamdMetadataBlock {
                    object_index: 0,
                    block_index: 0,
                    info: ObjectInfoBlock::default(),
                }],
                None,
                None,
            )
            .expect("未携带 additional table 的对象更新应恢复默认值");
        assert_eq!(
            state.object_additional(0),
            Some(&AdditionalObjectMetadata::default())
        );
    }
}
