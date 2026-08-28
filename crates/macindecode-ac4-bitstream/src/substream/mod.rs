//! Substream group 与 substream 信息元素。
//!
//! 对应 `TS103190-2:v1.3.1:6.2.1.6` 至 `6.2.1.14`。
//!
//! 编码路径的判定全部落在本模块：`b_channel_coded` 区分声道编码与对象编码，
//! 对象编码下再由 `b_ajoc` 区分 A-JOC 与 direct-coded object。这三条路径在
//! 后续阶段对应完全不同的重建流程。

use crate::oamd::OamdCommonData;
use crate::reader::BitReader;
use crate::topology::{Capacity, TopologyError, Unsupported, read_substream_index};

/// 单个 group 内可保存的低频 substream 数上限。
pub const MAX_LF_SUBSTREAMS: usize = 8;

mod channel;
mod common;
mod group;
mod object;

pub use channel::*;
pub use group::*;
pub use object::*;

/// 一个低频 substream 的类型化信息。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubstreamInfo {
    /// 声道编码。
    Chan(SubstreamInfoChan),
    /// A-JOC 编码的对象。
    Ajoc(SubstreamInfoAjoc),
    /// direct-coded 对象。
    Obj(SubstreamInfoObj),
}

impl Default for SubstreamInfo {
    fn default() -> Self {
        SubstreamInfo::Chan(SubstreamInfoChan::default())
    }
}

impl SubstreamInfo {
    /// 本 substream 携带的对象数；声道编码返回 0。
    #[must_use]
    pub const fn n_objects(&self) -> u32 {
        match *self {
            SubstreamInfo::Chan(_) => 0,
            SubstreamInfo::Ajoc(ref info) => info.n_upmix_signals.saturating_add(info.b_lfe as u32),
            SubstreamInfo::Obj(ref info) => info.n_objects,
        }
    }

    /// substream 索引表中的下标。
    #[must_use]
    pub const fn substream_index(&self) -> Option<u32> {
        match *self {
            SubstreamInfo::Chan(ref info) => info.substream_index(),
            SubstreamInfo::Ajoc(ref info) => info.substream_index(),
            SubstreamInfo::Obj(ref info) => info.substream_index(),
        }
    }

    /// 该 substream 是否无时间依赖，可独立于前序帧解码。
    #[must_use]
    pub const fn audio_ndot(&self) -> bool {
        match *self {
            SubstreamInfo::Chan(ref info) => info.audio_ndot(),
            SubstreamInfo::Ajoc(ref info) => info.audio_ndot(),
            SubstreamInfo::Obj(ref info) => info.audio_ndot(),
        }
    }

    /// 路径名称，用于序列化。
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match *self {
            SubstreamInfo::Chan(_) => "channel",
            SubstreamInfo::Ajoc(_) => "ajoc",
            SubstreamInfo::Obj(_) => "direct_object",
        }
    }

    pub(crate) const fn configuration_copy(&self) -> Self {
        match *self {
            SubstreamInfo::Chan(mut info) => {
                info.tail = info.tail.configuration_copy();
                SubstreamInfo::Chan(info)
            }
            SubstreamInfo::Ajoc(mut info) => {
                info.tail = info.tail.configuration_copy();
                // OAMD common 是随机访问点携带的状态刷新，不是音频解码配置；
                // 否则 present flag 在相邻帧切换会被误判为 topology 重配。
                info.oamd_common_data_present = false;
                info.oamd_common_data = None;
                SubstreamInfo::Ajoc(info)
            }
            SubstreamInfo::Obj(mut info) => {
                info.tail = info.tail.configuration_copy();
                SubstreamInfo::Obj(info)
            }
        }
    }
}

#[cfg(test)]
use common::read_bitrate_indicator;
#[cfg(test)]
use object::{ceil_log2, count_nonstd_bed, count_std_bed};

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use std::format;

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
    fn group_preserves_hsf_substream_indices() {
        // 显式 substream、HSF、单个声道编码 info：mono，无码率信息，
        // audio_ndot=1，基础下标=0，HSF 下标=1，无 content_type。
        let data = pack("1 1 1 1 0 0 1 00 01 0");
        let group = Ac4SubstreamGroupInfo::parse(&mut BitReader::new(&data), 2, 0, 1).unwrap();

        assert_eq!(group.frame_rate_factor, 1);
        assert_eq!(group.hsf_substream_indices(), &[Some(1)]);
    }

    fn channel_mode_of(bits: &str) -> ChannelMode {
        let data = pack(bits);
        let mut reader = BitReader::new(&data);
        ChannelMode::parse(&mut reader).unwrap()
    }

    /// 表 56 的每个码长都要正确终止，否则后续字段整体错位。
    #[test]
    fn decodes_channel_mode_prefix_code() {
        assert_eq!(channel_mode_of("0").ch_mode, 0);
        assert_eq!(channel_mode_of("10").ch_mode, 1);
        assert_eq!(channel_mode_of("1100").ch_mode, 2);
        assert_eq!(channel_mode_of("1110").ch_mode, 4);
        assert_eq!(channel_mode_of("1111000").ch_mode, 5);
        assert_eq!(channel_mode_of("1111101").ch_mode, 10);
        assert_eq!(channel_mode_of("11111100").ch_mode, 11);
        assert_eq!(channel_mode_of("11111101").ch_mode, 12);
        assert_eq!(channel_mode_of("111111100").ch_mode, 13);
        assert_eq!(channel_mode_of("111111110").ch_mode, 15);
    }

    #[test]
    fn channel_mode_consumes_exact_bit_count() {
        for (bits, expected) in [
            ("0", 1u64),
            ("10", 2),
            ("1101", 4),
            ("1111011", 7),
            ("11111100", 8),
            ("111111101", 9),
        ] {
            let data = pack(bits);
            let mut reader = BitReader::new(&data);
            ChannelMode::parse(&mut reader).unwrap();
            assert_eq!(reader.bit_position(), expected, "码字 {bits}");
        }
    }

    #[test]
    fn rejects_reserved_channel_mode() {
        let data = pack("111111111 01 0");
        let mut reader = BitReader::new(&data);
        assert!(matches!(
            ChannelMode::parse(&mut reader).unwrap_err(),
            TopologyError::Unsupported {
                what: Unsupported::ReservedChannelMode { ch_mode: 17 },
                ..
            }
        ));
    }

    #[test]
    fn channel_mode_labels_cover_table_56() {
        assert_eq!(channel_mode_of("1110").label(), Some("5.1"));
        assert_eq!(channel_mode_of("11111101").label(), Some("7.1.4"));
    }

    /// 3 比特码的最低位为 0；为 1 时补读 2 比特。
    #[test]
    fn bitrate_indicator_selects_width() {
        let data = pack("010");
        let mut reader = BitReader::new(&data);
        assert_eq!(read_bitrate_indicator(&mut reader).unwrap(), 0b010);
        assert_eq!(reader.bit_position(), 3);

        let data = pack("00111");
        let mut reader = BitReader::new(&data);
        assert_eq!(read_bitrate_indicator(&mut reader).unwrap(), 0b00111);
        assert_eq!(reader.bit_position(), 5);
    }

    #[test]
    fn ceil_log2_matches_definition() {
        assert_eq!(ceil_log2(1), 0);
        assert_eq!(ceil_log2(2), 1);
        assert_eq!(ceil_log2(3), 2);
        assert_eq!(ceil_log2(4), 2);
        assert_eq!(ceil_log2(5), 3);
        assert_eq!(ceil_log2(16), 4);
    }

    /// A-JOC 的床跳过 LFE 位置，direct-coded 的不跳过。
    #[test]
    fn std_bed_count_differs_by_lfe_handling() {
        let all = 0b11_1111_1111u64;
        assert_eq!(
            count_std_bed(all, false),
            2 + 1 + 1 + 2 + 2 + 2 + 2 + 2 + 2 + 1
        );
        assert_eq!(count_std_bed(all, true), 17 - 1 - 1);
    }

    #[test]
    fn nonstd_bed_count_skips_two_lfe_positions() {
        let all = (1u64 << 17) - 1;
        assert_eq!(count_nonstd_bed(all, false), 17);
        assert_eq!(count_nonstd_bed(all, true), 15);
    }

    #[test]
    fn direct_bed_layout_preserves_lfe_object_ordinals() {
        for (bits, expected_count, expected_lfe_mask) in [
            // 固定 5.1 配置：LFE 是第 4 个床对象。
            ("000 0 1 1 1 010 0 1", 6u32, 0b1000u32),
            // 非标准标志的位置 3 与 16 都是 LFE；输出序位分别为 1、2。
            ("000 0 1 1 0 1 10010000000000001 0 1", 3, 0b110),
            // 标准位置 0 先展开为两个对象，位置 2 与 9 的 LFE 随后位于 2、3。
            ("000 0 1 1 0 0 1010000001 0 1", 4, 0b1100),
        ] {
            let data = pack(bits);
            let info = SubstreamInfoObj::parse(&mut BitReader::new(&data), 0, 1, false).unwrap();
            assert_eq!(info.n_bed, expected_count, "{bits}");

            let descriptors = crate::oamd::ObjectDescriptors::from_object_substream(&info).unwrap();
            assert_eq!(descriptors.as_slice().len(), expected_count as usize);
            for (index, descriptor) in descriptors.as_slice().iter().enumerate() {
                let index = u32::try_from(index).unwrap_or(u32::MAX);
                let expected = expected_lfe_mask.checked_shr(index).unwrap_or(0) & 1 != 0;
                assert_eq!(info.bed_object_is_lfe(index), expected, "{bits}: {index}");
                assert_eq!(descriptor.b_lfe, expected, "{bits}: {index}");
            }
        }
    }

    #[test]
    fn dynamic_only_assignment_reads_single_bit() {
        let data = pack("1");
        let mut reader = BitReader::new(&data);
        let assignment = ObjectAssignment::parse(&mut reader, 8).unwrap();
        assert!(assignment.dynamic_only);
        assert_eq!(assignment.n_dynamic(), 8);
        assert_eq!(
            assignment.reconstruction_channels,
            AjocReconstructionChannels::default()
        );
        assert_eq!(reader.bit_position(), 1);
    }

    /// 表 63 的 2.0.0 只有 L/R，其余七个固定配置都以 L/R/C 开头。
    #[test]
    fn ajoc_assignment_codes_derive_reconstruction_channels() {
        for code in 0..8usize {
            let data = pack(&format!("0 0 1 {code:03b}"));
            let assignment = ObjectAssignment::parse(&mut BitReader::new(&data), 12).unwrap();
            assert_eq!(
                assignment.reconstruction_channels,
                AjocReconstructionChannels {
                    left: true,
                    right: true,
                    centre: code != 0,
                },
                "bed_chan_assign_code={code}"
            );
        }
    }

    /// 表 64 的数组位置 16/15/14 必须分别映射到 L/R/C，不能按循环下标反向。
    #[test]
    fn nonstd_flags_derive_each_reconstruction_channel() {
        for (flags, expected) in [
            (
                1u64 << 16,
                AjocReconstructionChannels {
                    left: true,
                    right: false,
                    centre: false,
                },
            ),
            (
                1u64 << 15,
                AjocReconstructionChannels {
                    left: false,
                    right: true,
                    centre: false,
                },
            ),
            (
                1u64 << 14,
                AjocReconstructionChannels {
                    left: false,
                    right: false,
                    centre: true,
                },
            ),
            // Ls 与 Pseudocode 15 的三个标志无关。
            (1u64 << 12, AjocReconstructionChannels::default()),
        ] {
            let data = pack(&format!("0 0 0 1 1 {flags:017b}"));
            let assignment = ObjectAssignment::parse(&mut BitReader::new(&data), 17).unwrap();
            assert_eq!(assignment.reconstruction_channels, expected, "{flags:017b}");
        }
    }

    /// 表 65 的最高位成对分配 L/R，次高位单独分配 C。
    #[test]
    fn std_flags_derive_paired_left_and_right() {
        for (flags, expected) in [
            (
                1u64 << 9,
                AjocReconstructionChannels {
                    left: true,
                    right: true,
                    centre: false,
                },
            ),
            (
                1u64 << 8,
                AjocReconstructionChannels {
                    left: false,
                    right: false,
                    centre: true,
                },
            ),
            // Ls/Rs 与 Pseudocode 15 的三个标志无关。
            (1u64 << 6, AjocReconstructionChannels::default()),
        ] {
            let data = pack(&format!("0 0 0 1 0 {flags:010b}"));
            let assignment = ObjectAssignment::parse(&mut BitReader::new(&data), 10).unwrap();
            assert_eq!(assignment.reconstruction_channels, expected, "{flags:010b}");
        }
    }

    /// 表 66 的逐对象位置 0/1/2 分别是 L/R/C；其他床位置不能改变插回点。
    #[test]
    fn individual_assignments_derive_reconstruction_channels() {
        for (channel, expected) in [
            (
                0u64,
                AjocReconstructionChannels {
                    left: true,
                    right: false,
                    centre: false,
                },
            ),
            (
                1,
                AjocReconstructionChannels {
                    left: false,
                    right: true,
                    centre: false,
                },
            ),
            (
                2,
                AjocReconstructionChannels {
                    left: false,
                    right: false,
                    centre: true,
                },
            ),
            (4, AjocReconstructionChannels::default()),
        ] {
            // n_signals=1 时不传 n_bed_signals_minus1。
            let data = pack(&format!("0 0 0 0 {channel:04b}"));
            let assignment = ObjectAssignment::parse(&mut BitReader::new(&data), 1).unwrap();
            assert_eq!(
                assignment.reconstruction_channels, expected,
                "channel={channel}"
            );
        }

        // n_signals=3：两比特的 n_bed_signals_minus1=2，随后依次给 L/R/C。
        // 这条钉住逐条赋值是在同一摘要里累积，而不是只留下最后一个位置。
        let data = pack("0 0 0 0 10 0000 0001 0010");
        let assignment = ObjectAssignment::parse(&mut BitReader::new(&data), 3).unwrap();
        assert_eq!(
            assignment.reconstruction_channels,
            AjocReconstructionChannels {
                left: true,
                right: true,
                centre: true,
            }
        );
        assert_eq!(
            assignment
                .reconstruction_channels
                .lfe_reinsertion_position(),
            3
        );
    }

    /// Pseudocode 15 后处理的是 full decode 输出，不能误取形状相同的 core 分配。
    #[test]
    fn lfe_position_uses_the_upmix_assignment_only() {
        let info = SubstreamInfoAjoc {
            b_lfe: true,
            dmx_assignment: Some(ObjectAssignment {
                reconstruction_channels: AjocReconstructionChannels {
                    left: true,
                    right: true,
                    centre: true,
                },
                ..ObjectAssignment::default()
            }),
            upmix_assignment: ObjectAssignment {
                reconstruction_channels: AjocReconstructionChannels {
                    left: false,
                    right: false,
                    centre: true,
                },
                ..ObjectAssignment::default()
            },
            ..SubstreamInfoAjoc::default()
        };

        assert_eq!(info.lfe_reinsertion_position(), Some(1));
        assert_eq!(
            SubstreamInfoAjoc {
                b_lfe: false,
                ..info
            }
            .lfe_reinsertion_position(),
            None
        );
    }

    /// 内嵌 `oamd_common_data()` 改为完整解析后，比特消耗必须与只跳过时一致。
    ///
    /// 这条契约**没有实测材料兜底**：八条流全部 568 帧的
    /// `b_oamd_common_data_present` 恒为假，该路径一次也没有执行过，因此落点
    /// 判据不覆盖它。位置一旦错开，遇到真正携带内嵌 common 的码流会从此处
    /// 开始整段错位，而现有向量发现不了。
    ///
    /// oracle 是改动前的实现，只按 `add_data_bytes` 跳过而不解析内部结构；
    /// 保留它的理由与 IFFT 保留 `O(M²)` DFT 相同——参考实现足够简单到可以
    /// 直接对着语法表核对。
    #[test]
    fn oamd_common_data_consumes_the_same_bits_as_the_skip_oracle() {
        fn skip_oracle(reader: &mut BitReader<'_>) -> Result<(), TopologyError> {
            if !reader.read_flag()? {
                reader.read_bits(5)?;
            }
            reader.read_flag()?;
            if !reader.read_flag()? {
                return Ok(());
            }
            let mut add_data_bytes = u32::try_from(reader.read_bits(1)?)
                .unwrap_or(0)
                .saturating_add(1);
            if add_data_bytes == 2 {
                add_data_bytes = reader.variable_bits_scaled_u32(2, add_data_bytes, 0)?;
            }
            reader.skip_bits(u64::from(add_data_bytes).saturating_mul(8))?;
            Ok(())
        }

        let mut state = 0x1234_5678_9abc_def0_u64;
        let (mut agree, mut stricter) = (0u32, 0u32);
        for _ in 0..20_000 {
            let mut bytes = [0u8; 512];
            for byte in bytes.iter_mut() {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                *byte = (state >> 56) as u8;
            }
            let mut actual = BitReader::new(&bytes);
            let mut oracle = BitReader::new(&bytes);
            match (OamdCommonData::parse(&mut actual), skip_oracle(&mut oracle)) {
                (Ok(_), Ok(())) => {
                    assert_eq!(
                        actual.bit_position(),
                        oracle.bit_position(),
                        "解析与跳过的比特消耗应完全一致"
                    );
                    agree = agree.saturating_add(1);
                }
                // 子元素消耗超过 `add_data_bytes` 声明：旧实现静默跳到边界，
                // 位置正确但内容是错的；新实现按 `AdditionalDataUnderflow` 拒绝。
                (Err(_), Ok(())) => stricter = stricter.saturating_add(1),
                (Err(_), Err(_)) => {}
                (Ok(_), Err(_)) => panic!("解析不应接受 oracle 拒绝的输入"),
            }
        }
        assert!(agree > 10_000, "应有足量两侧都接受的用例，实得 {agree}");
        // 若 underflow 检查被删除，这一项会归零：新实现必须严格强于 oracle。
        assert!(stricter > 0, "应存在 oracle 静默跳过而解析拒绝的用例");
    }

    #[test]
    fn ajoc_info_retains_inline_oamd_common_data() {
        // static dmx；内嵌 common 使用非默认屏幕比例 17；单个动态上混对象；
        // 无码率信息，ndot=1，substream_index=2。
        let data = pack("0 1 1 0 10001 1 0 0000 1 0 1 10");
        let mut reader = BitReader::new(&data);
        let info = SubstreamInfoAjoc::parse(&mut reader, 0, 1, true).unwrap();

        assert!(info.oamd_common_data_present);
        assert_eq!(
            info.oamd_common_data,
            Some(OamdCommonData {
                default_screen_size_ratio: false,
                master_screen_size_ratio_code: Some(17),
                bed_object_chan_distribute: true,
                add_data_bytes: None,
                trim: crate::oamd::Trim::default(),
                bed_render_info: crate::oamd::BedRenderInfo::default(),
                headphone: crate::oamd::Headphone::default(),
            })
        );
        assert_eq!(info.substream_index(), Some(2));
        assert_eq!(reader.bit_position(), 20);

        let SubstreamInfo::Ajoc(config) = SubstreamInfo::Ajoc(info).configuration_copy() else {
            panic!("配置副本应保持 A-JOC 类型");
        };
        assert!(!config.oamd_common_data_present);
        assert_eq!(config.oamd_common_data, None);
    }

    /// b_dyn_objects_only=0, b_isf=1, isf_config=010 → 10 个 ISF 对象
    #[test]
    fn isf_assignment_uses_table_61() {
        let data = pack("0 1 010");
        let mut reader = BitReader::new(&data);
        let assignment = ObjectAssignment::parse(&mut reader, 10).unwrap();
        assert_eq!(assignment.n_isf, 10);
        assert_eq!(assignment.n_dynamic(), 0);
        assert_eq!(
            assignment.reconstruction_channels,
            AjocReconstructionChannels::default()
        );
    }

    /// b_dyn_objects_only=0, b_isf=0, b_ch_assign_code=1, code=100 → 9 个床对象
    #[test]
    fn ajoc_bed_assign_code_excludes_lfe() {
        let data = pack("0 0 1 100");
        let mut reader = BitReader::new(&data);
        let assignment = ObjectAssignment::parse(&mut reader, 12).unwrap();
        assert_eq!(assignment.n_bed, 9);
        assert_eq!(assignment.n_dynamic(), 3);
        assert_eq!(
            assignment.reconstruction_channels,
            AjocReconstructionChannels {
                left: true,
                right: true,
                centre: true,
            }
        );
    }
}
