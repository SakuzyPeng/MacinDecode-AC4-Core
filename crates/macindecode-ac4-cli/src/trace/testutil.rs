//! trace 测试共用的构造工具。
//!
//! 打包极小 TOC、构造合成的 substream 与 OAMD 块。集中在一处是因为多个子模块的
//! 判据都要用同一份构造——散开会让「同一个 fixture」变成几份各自漂移的副本。

#![cfg(test)]
#![expect(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    reason = "测试按固定语法打包极小 TOC，下标与算术越界即是构造本身写错"
)]

pub(crate) use super::*;
#[cfg(feature = "audio-decode")]
pub(crate) use crate::metadata_batch::{
    MetadataElement, MetadataElementId, MetadataElementKind, MetadataEvent,
    default_output_metadata_event,
};
#[cfg(feature = "audio-decode")]
pub(crate) use macindecode_ac4_bitstream::aspx::{
    AspxBandTables, AspxConfig, AspxData, AspxState, EnvelopeKind, HcbType, StereoMode,
    codebooks::table_for, get_aspx_hcb,
};
#[cfg(feature = "audio-decode")]
pub(crate) use macindecode_ac4_bitstream::huffman::HuffmanTable;
#[cfg(feature = "audio-decode")]
pub(crate) use macindecode_ac4_bitstream::oamd::{
    AbsolutePosition, AdditionalObjectMetadata, DifferentialPosition, InfoStatus, ObjectBasicInfo,
    ObjectInfoBlock, ObjectRenderInfo, OtherPropertiesUpdate, PositionUpdate, ZoneUpdate,
};
pub(crate) use macindecode_ac4_bitstream::substream::Ac4SubstreamGroupInfo;
pub(crate) use macindecode_ac4_bitstream::{Ac4PresentationV1Info, BitReader};

pub(crate) fn pack_bits(source: &str) -> (Vec<u8>, usize) {
    let bits: Vec<u8> = source
        .bytes()
        .filter(|bit| matches!(*bit, b'0' | b'1'))
        .collect();
    let mut data = vec![0u8; bits.len().div_ceil(8)];
    for (index, bit) in bits.iter().enumerate() {
        if *bit == b'1' {
            data[index / 8] |= 1 << (7 - index % 8);
        }
    }
    (data, bits.len())
}
/// 搜索解码为 `symbol` 的最短码字，返回二进制字符串。
///
/// 码本由构建脚本从附录 A 生成，码字不在源码里；穷举比手抄可靠，也不会在
/// 表更新后悄悄失配。
#[cfg(feature = "audio-decode")]
pub(crate) fn shortest_codeword(
    decode: impl Fn(&mut BitReader<'_>) -> Option<u16>,
    symbol: u16,
) -> String {
    for width in 1..=20usize {
        for value in 0..1u32.checked_shl(width as u32).unwrap_or(0) {
            let candidate = format!("{value:0width$b}");
            let (data, _) = pack_bits(&candidate);
            let mut reader = BitReader::new(&data);
            if decode(&mut reader) == Some(symbol) && reader.bit_position() == width as u64 {
                return candidate;
            }
        }
    }
    panic!("码本里应存在符号 {symbol}");
}
#[cfg(feature = "audio-decode")]
pub(crate) fn shortest_aspx_codeword(table: &HuffmanTable) -> String {
    for width in 1..=20usize {
        for value in 0..1u32.checked_shl(width as u32).unwrap_or(0) {
            let candidate = format!("{value:0width$b}");
            let (data, _) = pack_bits(&candidate);
            let mut reader = BitReader::new(&data);
            if table.decode(&mut reader).is_ok() && reader.bit_position() == width as u64 {
                return candidate;
            }
        }
    }
    panic!("A-SPX 码本应至少有一条不超过 20 位的码字");
}
#[cfg(feature = "audio-decode")]
pub(crate) fn output_test_scene() -> MetadataElement {
    MetadataElement {
        element_id: MetadataElementId::new(1),
        substream_index: 2,
        object_index: 1,
        kind: MetadataElementKind::DynamicObject,
        common: None,
        common_conflict: false,
    }
}
#[cfg(feature = "audio-decode")]
pub(crate) fn output_test_event(sample_position: i64, ramp_samples: u32) -> MetadataEvent {
    let mut state = default_output_metadata_event().state;
    state.active = true;
    MetadataEvent {
        sample_position,
        element_id: MetadataElementId::new(1),
        stream_order: 0,
        ramp_samples,
        state,
        additional: AdditionalObjectMetadata::default(),
    }
}
#[cfg(feature = "audio-decode")]
pub(crate) fn implicit_timing(blocks: u8) -> OamdTimingData {
    let mut source = format!("0 {blocks:03b}");
    for _ in 0..blocks {
        source.push_str(" 000000 00");
    }
    let (data, _) = pack_bits(&source);
    OamdTimingData::parse(&mut BitReader::new(&data)).expect("测试 timing 应可解析")
}
#[cfg(feature = "audio-decode")]
pub(crate) fn explicit_timing(sample_offset: u8, blocks: u8) -> OamdTimingData {
    let mut source = format!("11 {sample_offset:05b} {blocks:03b}");
    for _ in 0..blocks {
        source.push_str(" 000000 00");
    }
    let (data, _) = pack_bits(&source);
    OamdTimingData::parse(&mut BitReader::new(&data)).expect("测试 timing 应可解析")
}
pub(crate) fn topology_with_two_oamd_substreams(payloads: [u8; 4]) -> (Vec<u8>, Ac4Topology) {
    topology_with_group0_oamd(payloads, "1 0 1 0 1 1 00 1 0 1 0 0000 1 0 0 1 01 0")
}
pub(crate) fn topology_with_group0_oamd(payloads: [u8; 4], group0: &str) -> (Vec<u8>, Ac4Topology) {
    let toc = "10 0000000000 0 1 1101 1 1 0 0";
    // presentation_config=0，引用 group 0 与 group 1。
    let presentation = "0 000 0 000 0 00 000 0 00 00 0 0 000 001 0 0 0 1 00";
    // 两个 A-JOC group 分别引用 OAMD 0/2 与 audio 1/3。
    let group1 = "1 0 1 0 1 1 10 1 0 1 0 0000 1 0 0 1 11 00 0 0";
    // 四个 substream，每个一字节。
    let table = concat!(
        "00 00 0 ",
        "0 0000000001 ",
        "0 0000000001 ",
        "0 0000000001 ",
        "0 0000000001"
    );
    let (data, count) = pack_bits(presentation);
    let mut reader = BitReader::new(&data);
    Ac4PresentationV1Info::parse(&mut reader, 2, 13).unwrap();
    assert_eq!(reader.bit_position(), count as u64);
    let (data, count) = pack_bits(group0);
    let mut reader = BitReader::new(&data);
    Ac4SubstreamGroupInfo::parse(&mut reader, 2, 1, 1).unwrap();
    assert_eq!(reader.bit_position(), count as u64);
    let (data, count) = pack_bits(group1);
    let mut reader = BitReader::new(&data);
    Ac4SubstreamGroupInfo::parse(&mut reader, 2, 1, 1).unwrap();
    assert_eq!(reader.bit_position(), count as u64);

    let joined = [toc, presentation, group0, group1, table].join(" ");
    let (mut frame, bit_count) = pack_bits(&joined);
    // 偶数下标是 OAMD，奇数下标是本阶段不解析的音频载荷。
    frame.extend_from_slice(&payloads);
    let topology = Ac4Topology::parse(&frame).unwrap();
    assert_eq!(
        topology.bits_consumed, bit_count as u64,
        "{:?}",
        topology.index_table
    );
    (frame, topology)
}
pub(crate) fn minimal_audio_payload() -> Vec<u8> {
    let (payload, bit_count) = pack_bits(
        "000000000000000 0 \
         0 \
         0 0 0 \
         0000001 0 \
         0 \
         0 \
         00",
    );
    assert_eq!(bit_count, 32);
    payload
}

#[cfg(feature = "audio-decode")]
const MINIMAL_FULL_AUDIO_PAYLOAD: [u8; 24] = [
    0x00, 0x28, 0x40, 0x85, 0x88, 0x40, 0x10, 0x00, 0x00, 0x0f, 0x80, 0x00, 0x00, 0x00, 0x00, 0x0e,
    0xfe, 0x44, 0x02, 0x00, 0xc3, 0x00, 0x00, 0x20,
];

#[cfg(feature = "audio-decode")]
fn minimal_full_audio_topology_with_payload(
    sequence_counter: u16,
    payload: &[u8],
) -> (Vec<u8>, Ac4Topology) {
    let toc = format!("10 {sequence_counter:010b} 0 1 0001 1 1 0 0");
    let presentation = "1 0 000 0 0 00 000 0 00 00 0 000 0 0 0 1 00";
    let group = "1 0 1 0 0 1 0 0 0000 1 0 0000 1 0 0 1 01 0";
    let size = format!("{size:010b}", size = payload.len());
    let table = ["10 0 0000000000 0 ", &size].concat();
    let (mut frame, _) = pack_bits(&[toc.as_str(), presentation, group, &table].join(" "));
    frame.extend_from_slice(payload);
    let topology = Ac4Topology::parse(&frame).expect("最小 Full A-JOC topology 应可解析");
    validate_group_references(&topology).expect("最小 Full A-JOC group 引用应闭合");
    validate_substream_references(&topology).expect("最小 Full A-JOC substream 引用应闭合");
    (frame, topology)
}

/// 单 presentation、单物理 A-JOC substream 的最小可完整重建帧。
///
/// payload 与 Scene Session 的事务夹具相同；这里同时返回 topology，供旧 census
/// 与统一 Full engine 的 observation 做逐字段等价比较。
#[cfg(feature = "audio-decode")]
pub(crate) fn minimal_full_audio_topology(sequence_counter: u16) -> (Vec<u8>, Ac4Topology) {
    minimal_full_audio_topology_with_payload(sequence_counter, &MINIMAL_FULL_AUDIO_PAYLOAD)
}

/// 与最小 Full 帧相同，但打开 `b_compand_avg`，构成合法且受 A-SPX 门禁拦截的帧。
#[cfg(feature = "audio-decode")]
pub(crate) fn minimal_full_audio_topology_with_active_companding(
    sequence_counter: u16,
) -> (Vec<u8>, Ac4Topology) {
    let mut payload = MINIMAL_FULL_AUDIO_PAYLOAD;
    // 15 位 audio_size + b_more_bits 后，audio_data 的 inactive/codec/config/
    // compand_on 共 18 位；下一位即 b_compand_avg。
    const COMPAND_AVG_BIT: usize = 34;
    payload[COMPAND_AVG_BIT / 8] |= 1 << (7 - COMPAND_AVG_BIT % 8);
    minimal_full_audio_topology_with_payload(sequence_counter, &payload)
}
#[cfg(feature = "audio-decode")]
pub(crate) fn position_block(block_index: u8, position: PositionUpdate) -> OamdMetadataBlock {
    OamdMetadataBlock {
        object_index: 0,
        block_index,
        info: ObjectInfoBlock {
            basic_info_status: InfoStatus::AllNew,
            render_info_status: InfoStatus::AllNew,
            diff_pos_coding: matches!(position, PositionUpdate::Differential(_)),
            position_present: true,
            basic_info: Some(ObjectBasicInfo {
                default_metadata: true,
                ..ObjectBasicInfo::default()
            }),
            render_info: Some(ObjectRenderInfo {
                position: Some(position),
                zone: Some(ZoneUpdate {
                    grouped_defaults: true,
                    ..ZoneUpdate::default()
                }),
                other_properties: Some(OtherPropertiesUpdate {
                    grouped_defaults: true,
                    ..OtherPropertiesUpdate::default()
                }),
            }),
            ..ObjectInfoBlock::default()
        },
    }
}
/// 两个声道编码 group 以 7.1 与 stereo 上下文引用同一条物理音频 substream。
/// 这是 DEE legacy IMS 的实际拓扑形状。
pub(crate) fn topology_with_shared_channel_audio_substream() -> (Vec<u8>, Ac4Topology) {
    let toc = "10 0000000000 0 1 1101 1 1 0 0";
    // presentation_config=0，引用 group 0 与 group 1；presentation substream=0。
    let presentation = "0 000 0 000 0 00 000 0 00 00 0 0 000 001 0 0 0 1 00";
    // 两个 channel info 都引用 audio substream 1。group 0 的 ch_mode=6
    // (7.1_3/4/0.1)，group 1 的 ch_mode=1 (stereo)。
    let surround = "1 0 1 1 1111001 0 0 1 01 0";
    let stereo = "1 0 1 1 10 0 0 1 01 0";
    let table = "10 0 0000000001 0 0000000100";
    let joined = [toc, presentation, surround, stereo, table].join(" ");
    let (mut frame, _) = pack_bits(&joined);
    frame.push(0); // presentation substream，本阶段不解析。
    frame.extend_from_slice(&minimal_audio_payload());
    let topology = Ac4Topology::parse(&frame).expect("共享声道载荷拓扑应能解析");
    validate_group_references(&topology).unwrap();
    validate_substream_references(&topology).unwrap();
    (frame, topology)
}
/// 标准 DEE IMS 形状：presentation v2 只声明 7.1 group，但物理 audio
/// substream 的 metadata 使用 stereo 分支。
pub(crate) fn topology_with_ims_v2_stereo_metadata() -> (Vec<u8>, Ac4Topology) {
    ims_stereo_metadata_topology(2)
}
/// 同一码流形状，只把 presentation 降到 v1。
///
/// 兼容候选的适用范围以 v2 为界，用它做反例才能分辨「按版本收窄」与「对任意
/// 版本都补候选」——两个 fixture 之间只差 `presentation_version()` 的一个比特。
pub(crate) fn topology_with_ims_v1_stereo_metadata() -> (Vec<u8>, Ac4Topology) {
    ims_stereo_metadata_topology(1)
}
fn ims_stereo_metadata_topology(version: u32) -> (Vec<u8>, Ac4Topology) {
    let toc = "10 0000000000 0 1 1101 1 1 0 0";
    // presentation_version() 是一元码：version 个 1 后跟一个 0。改动它只让
    // TOC 少一个比特，91→90，div_ceil(8) 同为 12 字节，后续偏移不变。
    let mut unary = "1".repeat(version as usize);
    unary.push('0');
    let presentation = format!("1 {unary} 000 0 00 000 0 00 00 0 000 0 0 0 0 00");
    let surround = "1 0 1 1 1111001 0 0 1 01 0";
    let table = "10 0 0000000001 0 0000001000";
    let joined = [toc, &presentation, surround, table].join(" ");
    let (mut frame, _) = pack_bits(&joined);
    assert_eq!(frame.len(), 12, "改版本号不得改变 TOC 的字节长度");
    frame.push(0); // presentation substream，本阶段不解析。

    // audio_size=0；basic metadata 使用 stereo previous-downmix 分支；内嵌
    // 一个零字节的 EMDF ID 18 envelope。7.1 metadata 分支会错把 EMDF ID
    // 读成 tools size 扩展并越界。
    let (payload, bit_count) = pack_bits(
        "000000000000000 0 \
         1 0 1 101 10 0 \
         0 0 0 \
         0000001 0 \
         0 \
         1 \
         10010 0 0 0 0 1 00000000 0 \
         00000 00",
    );
    assert_eq!(bit_count, 64);
    assert_eq!(payload.len(), 8);
    frame.extend_from_slice(&payload);

    let topology = Ac4Topology::parse(&frame).expect("IMS stereo metadata 拓扑应能解析");
    assert_eq!(topology.presentations()[0].presentation_version, version);
    validate_group_references(&topology).unwrap();
    validate_substream_references(&topology).unwrap();
    (frame, topology)
}
/// 两个 group 的 A-JOC 下标都超出 [`MAX_SUBSTREAMS`]。
///
/// `substream_index` 读作 2 比特、取值 3 时以 `variable_bits(2)` 扩展，可以
/// 任意大；CLI 对 `validate_substream_references` 只计数不阻断，因此超界的
/// 下标确实会走到音频巡检里。`11 001101010` 解为 `3 + 29 = 32`，
/// `11 001101100` 解为 `3 + 30 = 33`。
#[cfg(feature = "audio-decode")]
pub(crate) fn topology_with_out_of_range_ajoc_index() -> (Vec<u8>, Ac4Topology) {
    let toc = "10 0000000000 0 1 1101 1 1 0 0";
    let presentation = "0 000 0 000 0 00 000 0 00 00 0 0 000 001 0 0 0 1 00";
    let group0 = "1 0 1 0 1 1 00 1 0 1 0 0000 1 0 0 1 11 001101010 0";
    let group1 = "1 0 1 0 1 1 10 1 0 1 0 0000 1 0 0 1 11 001101100 0";
    let table = concat!(
        "00 00 0 ",
        "0 0000000001 ",
        "0 0000000100 ",
        "0 0000000001 ",
        "0 0000000100"
    );
    let joined = [toc, presentation, group0, group1, table].join(" ");
    let (mut frame, _) = pack_bits(&joined);
    let audio = minimal_audio_payload();
    frame.push(0);
    frame.extend_from_slice(&audio);
    frame.push(0);
    frame.extend_from_slice(&audio);
    let topology = Ac4Topology::parse(&frame).expect("拓扑本身仍应可解析");
    (frame, topology)
}
/// 两个 group 引用同一条物理 A-JOC 音频 substream。
#[cfg(feature = "audio-decode")]
pub(crate) fn topology_with_shared_audio_substream() -> (Vec<u8>, Ac4Topology) {
    let toc = "10 0000000000 0 1 1101 1 1 0 0";
    let presentation = "0 000 0 000 0 00 000 0 00 00 0 0 000 001 0 0 0 1 00";
    // group 0 引用 OAMD 0 / audio 1，group 1 引用 OAMD 2 / audio 1。
    let group0 = "1 0 1 0 1 1 00 1 0 1 0 0000 1 0 0 1 01 0";
    let group1 = "1 0 1 0 1 1 10 1 0 1 0 0000 1 0 0 1 01 0";
    let table = concat!("11 ", "0 0000000001 ", "0 0000000100 ", "0 0000000001");
    let joined = [toc, presentation, group0, group1, table].join(" ");
    let (mut frame, _) = pack_bits(&joined);
    frame.push(0);
    frame.extend_from_slice(&minimal_audio_payload());
    frame.push(0);
    let topology = Ac4Topology::parse(&frame).unwrap();
    validate_group_references(&topology).unwrap();
    validate_substream_references(&topology).unwrap();
    (frame, topology)
}
/// group 0 只属于普通 presentation，group 1 只属于 alternative presentation。
/// 前者使用 direct-object，后者使用 A-JOC，以便错误的全局 alternative 会让
/// group 0 误入尚未覆盖的 oamd_dyndata_single 分支。
pub(crate) fn topology_with_scoped_alternative() -> (Vec<u8>, Ac4Topology) {
    // 两个 presentation：variable_bits(2)=0，因此 n_presentations=2。
    let toc = "10 0000000000 0 1 1101 1 0 1 00 0 0 0";
    let normal = "1 0 000 0 00 000 0 00 00 0 000 0 0 0 0 00";
    let alternative = "1 0 000 0 00 000 0 00 00 0 001 0 0 1 0 01";
    // presentation substream 占 0/1，普通 direct-object 音频占 2。
    let direct = "1 0 1 0 0 0 010 1 0 0 0 0 10 0";
    // alternative presentation 的 A-JOC 音频占 3。
    let ajoc = "1 0 1 0 0 1 0 1 0 0000 1 0 0 1 11 00 0 0";
    let table = concat!(
        "00 00 0 ",
        "0 0000000001 ",
        "0 0000000001 ",
        "0 0000000100 ",
        "0 0000000100"
    );
    let joined = [toc, normal, alternative, direct, ajoc, table].join(" ");
    let (mut frame, _) = pack_bits(&joined);
    let audio = minimal_audio_payload();
    frame.extend_from_slice(&[0, 0]);
    frame.extend_from_slice(&audio);
    frame.extend_from_slice(&audio);
    let topology = Ac4Topology::parse(&frame).unwrap();
    validate_group_references(&topology).unwrap();
    validate_substream_references(&topology).unwrap();
    (frame, topology)
}
/// 构造双声道 A-SPX 元素；`right_uses_branches` 决定右路是否走 FIXVAR 与音调。
#[cfg(feature = "audio-decode")]
pub(crate) fn parse_two_channel_aspx(right_uses_branches: bool) -> AspxData {
    let config = AspxConfig {
        quant_mode_env: false,
        start_freq: 0,
        stop_freq: 0,
        master_freq_scale: true,
        interpolation: false,
        preflat: false,
        limiter: false,
        noise_sbg: 1,
        num_env_bits_fixfix: false,
        freq_res_mode: 3,
    };
    let bands = AspxBandTables::derive(
        config.master_freq_scale,
        config.start_freq,
        config.stop_freq,
        config.noise_sbg,
        0,
    )
    .expect("测试频带表应可推导");
    let highres = bands.num_sbg_sig_highres();
    let noise_sbg = bands.num_sbg_noise();

    let mut bits = String::from("000"); // I 帧 xover = 0
    bits.push_str("00"); // 左：FIXFIX、单包络
    bits.push('0'); // aspx_balance = 0，左右独立成帧
    if right_uses_branches {
        bits.push_str("10000000"); // 右：FIXVAR、零偏移、单包络、tsg_ptr=-1
    } else {
        bits.push_str("00"); // 右：FIXFIX、单包络
    }
    bits.push_str("0000"); // 两路各一份 signal/noise delta_dir
    for _ in 0..usize::from(noise_sbg).saturating_mul(2) {
        bits.push_str("00"); // 左右 tna_mode 全为 0
    }
    bits.push('0'); // 左路不传 add_harmonic
    bits.push(if right_uses_branches { '1' } else { '0' });
    if right_uses_branches {
        for group in 0..highres {
            bits.push(if group == 0 { '1' } else { '0' }); // 仅右路第一组置位
        }
    }
    bits.push('0'); // fic 不存在
    bits.push('0'); // tic 不存在

    for (kind, groups) in [
        (EnvelopeKind::Signal, highres),
        (EnvelopeKind::Noise, noise_sbg),
    ] {
        let first = shortest_aspx_codeword(table_for(get_aspx_hcb(
            kind,
            StereoMode::Level,
            false,
            HcbType::F0,
        )));
        let rest = shortest_aspx_codeword(table_for(get_aspx_hcb(
            kind,
            StereoMode::Level,
            false,
            HcbType::Df,
        )));
        for _ in 0..2 {
            bits.push_str(&first);
            for _ in 1..groups {
                bits.push_str(&rest);
            }
        }
    }

    let (data, bit_len) = pack_bits(&bits);
    let mut reader = BitReader::new(&data);
    let mut state = AspxState::new();
    state.reset_stop_pos(16);
    let parsed = AspxData::parse_2ch(&mut reader, &config, &mut state, 2048, true)
        .expect("双声道 fixture 应完整解析");
    assert_eq!(reader.bit_position(), u64::try_from(bit_len).unwrap_or(0));
    parsed
}
