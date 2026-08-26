//! `ac4_presentation_v1_dsi()` 与 `ac4_substream_group_dsi()`。
//!
//! 对应 `TS103190-2:v1.3.1:E.10`–`E.12`。这些字段只用于容器检视、
//! 能力选择与 TOC 交叉核对，不能替代逐 sample 的 TOC 配置。

use core::str::Utf8Error;

use macindecode_ac4_bitstream::{BitReader, ReadError};

use super::{Ac4BitrateDsi, Ac4DsiPresentation, DsiError};

/// 从可能不在字节边界上的位置读取的一段 8 比特元素。
///
/// `filter_data`、扩展配置和语言标签都可能相对 presentation body 非字节对齐，
/// 因而不能统一借用为普通 `&[u8]`。本视图保留精确边界且不分配内存。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ac4DsiBytes<'a> {
    source: &'a [u8],
    bit_offset: u64,
    len: usize,
}

impl<'a> Ac4DsiBytes<'a> {
    fn read(reader: &mut BitReader<'a>, source: &'a [u8], len: usize) -> Result<Self, DsiError> {
        let bit_offset = reader.bit_position();
        let bit_len = u64::try_from(len)
            .ok()
            .and_then(|value| value.checked_mul(8))
            .ok_or(DsiError::Truncated(ReadError::ValueOverflow {
                bit_position: bit_offset,
            }))?;
        reader.skip_bits(bit_len).map_err(DsiError::Truncated)?;
        Ok(Self {
            source,
            bit_offset,
            len,
        })
    }

    /// 元素数量；每个元素恰好 8 比特。
    #[must_use]
    pub const fn len(self) -> usize {
        self.len
    }

    /// 是否为空。
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.len == 0
    }

    /// 读取一个元素。
    #[must_use]
    pub fn get(self, index: usize) -> Option<u8> {
        if index >= self.len {
            return None;
        }
        let relative = u64::try_from(index).ok()?.checked_mul(8)?;
        let bit_offset = self.bit_offset.checked_add(relative)?;
        let mut reader = BitReader::new(self.source);
        reader.skip_bits(bit_offset).ok()?;
        u8::try_from(reader.read_bits(8).ok()?).ok()
    }

    /// 若视图恰好位于字节边界，返回零拷贝字节切片。
    #[must_use]
    pub fn as_aligned_slice(self) -> Option<&'a [u8]> {
        if !self.bit_offset.is_multiple_of(8) {
            return None;
        }
        let start = usize::try_from(self.bit_offset / 8).ok()?;
        let end = start.checked_add(self.len)?;
        self.source.get(start..end)
    }

    /// 按码流顺序遍历 8 比特元素。
    #[must_use]
    pub fn iter(self) -> Ac4DsiByteIter<'a> {
        Ac4DsiByteIter::new(self)
    }
}

impl<'a> IntoIterator for Ac4DsiBytes<'a> {
    type Item = u8;
    type IntoIter = Ac4DsiByteIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// [`Ac4DsiBytes`] 的无分配迭代器。
#[derive(Debug, Clone)]
pub struct Ac4DsiByteIter<'a> {
    reader: BitReader<'a>,
    remaining: usize,
}

impl<'a> Ac4DsiByteIter<'a> {
    fn new(bytes: Ac4DsiBytes<'a>) -> Self {
        let mut reader = BitReader::new(bytes.source);
        let remaining = if reader.skip_bits(bytes.bit_offset).is_ok() {
            bytes.len
        } else {
            0
        };
        Self { reader, remaining }
    }
}

impl Iterator for Ac4DsiByteIter<'_> {
    type Item = u8;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        let value = match self.reader.read_bits(8) {
            Ok(value) => u8::try_from(value).ok(),
            Err(_) => None,
        };
        if value.is_some() {
            self.remaining = self.remaining.saturating_sub(1);
        } else {
            self.remaining = 0;
        }
        value
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl ExactSizeIterator for Ac4DsiByteIter<'_> {}

/// DSI 中固定宽度的 EMDF 版本与认证 ID。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ac4DsiEmdfInfo {
    /// EMDF 语法版本。
    pub version: u8,
    /// 10 比特认证 ID。
    pub key_id: u16,
}

impl Ac4DsiEmdfInfo {
    fn parse(reader: &mut BitReader<'_>) -> Result<Self, DsiError> {
        Ok(Self {
            version: read_u8(reader, 5)?,
            key_id: read_u16(reader, 10)?,
        })
    }
}

/// presentation 末尾附加 EMDF 描述的迭代器。
#[derive(Debug, Clone)]
pub struct Ac4DsiEmdfIter<'a> {
    reader: BitReader<'a>,
    remaining: u8,
}

impl<'a> Ac4DsiEmdfIter<'a> {
    fn new(source: &'a [u8], bit_offset: u64, count: u8) -> Self {
        let mut reader = BitReader::new(source);
        let remaining = if reader.skip_bits(bit_offset).is_ok() {
            count
        } else {
            0
        };
        Self { reader, remaining }
    }
}

impl Iterator for Ac4DsiEmdfIter<'_> {
    type Item = Ac4DsiEmdfInfo;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        let value = Ac4DsiEmdfInfo::parse(&mut self.reader).ok();
        if value.is_some() {
            self.remaining = self.remaining.saturating_sub(1);
        } else {
            self.remaining = 0;
        }
        value
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = usize::from(self.remaining);
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for Ac4DsiEmdfIter<'_> {}

/// Annex A.27 的 18 个 audio channel group 位。
///
/// 位 `0…17` 与规范数组下标相同；首个传输位对应下标 17。
/// 下标 8 已弃用：presentation v1 要求该位为零，substream mask 中出现时则按
/// Annex A.27 NOTE 3 保留原值，由消费方将其解释为下标 7。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Ac4DsiChannelGroups(u32);

impl Ac4DsiChannelGroups {
    fn parse(reader: &mut BitReader<'_>) -> Result<Self, DsiError> {
        Ok(Self(read_u32(reader, 18)?))
    }

    fn parse_presentation(reader: &mut BitReader<'_>) -> Result<Self, DsiError> {
        let bit_position = reader.bit_position();
        let parsed = Self::parse(reader)?;
        if parsed.contains(8) {
            return Err(DsiError::ReservedValueNonZero {
                field: "presentation_v1_channel_groups[8]",
                // 数组按 17…0 传输，下标 8 是第十个比特。
                bit_position: bit_position.saturating_add(9),
                value: 1,
            });
        }
        Ok(parsed)
    }

    /// 按数组下标取得某个 channel group 是否存在。
    #[must_use]
    pub const fn contains(self, index: u8) -> bool {
        match 1u32.checked_shl(index as u32) {
            Some(mask) if index < 18 => self.0 & mask != 0,
            _ => false,
        }
    }

    /// 18 比特原值；bit `i` 对应规范数组下标 `i`。
    #[must_use]
    pub const fn raw(self) -> u32 {
        self.0
    }
}

/// presentation 的 channel-coded 布局摘要。
///
/// 本类型只保存 DSI 信令，不代表当前实现已接通 channel-based PCM 重建。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ac4DsiPresentationChannelLayout {
    /// 5 比特 presentation channel mode。
    pub channel_mode: u8,
    /// mode 11–14 时的四后方声道标志。
    pub four_back_channels_present: Option<bool>,
    /// mode 11–14 时的顶部声道对码值。
    pub top_channel_pairs: Option<u8>,
    /// 原始内容中的 channel groups。
    pub channel_groups: Ac4DsiChannelGroups,
}

/// core decode 与 full decode 不同情况下的 core 布局。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ac4DsiPresentationCoreLayout {
    /// `false` 时 core channel mode 不在 DSI 中给出。
    pub channel_coded: bool,
    /// 表 E.14 的 2 比特码值。
    pub channel_mode: Option<u8>,
}

/// presentation 过滤器信令。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ac4DsiPresentationFilter<'a> {
    /// presentation 是否允许播放。
    pub enabled: bool,
    /// 规范要求解析但忽略的过滤器数据。
    pub data: Ac4DsiBytes<'a>,
}

/// presentation 尾部的播放能力指示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ac4DsiPresentationIndicators {
    /// presentation 是否提供 dialogue enhancement。
    pub dialogue_enhancement: bool,
    /// presentation 是否含沉浸式音频；该位不影响解码。
    pub immersive_audio: bool,
    /// 4 比特保留字段，原样保留。
    pub reserved: u8,
    /// 若存在，覆盖 5 比特 `presentation_id`。
    pub extended_presentation_id: Option<u16>,
    /// 未携带扩展 ID 时的单个保留位。
    pub reserved_id_bit: Option<bool>,
}

/// 单个 channel-coded substream 的 DSI 摘要。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ac4DsiChannelSubstream {
    /// 表 E.5d 的采样率乘数码值。
    pub sampling_frequency_multiplier: u8,
    /// 表 90 的 `brate_ind`；没有码率指示时为 `None`。
    pub bitrate_indicator: Option<u8>,
    /// 原始内容中的 channel groups。
    pub channel_groups: Ac4DsiChannelGroups,
}

/// object/A-JOC substream 中的对象种类摘要。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ac4DsiObjectKinds {
    /// 是否含 bed objects。
    pub bed: bool,
    /// 是否含 dynamic objects。
    pub dynamic: bool,
    /// 是否含 ISF objects。
    pub isf: bool,
    /// 语法中的单个保留位，原样保留。
    pub reserved: bool,
}

/// A-JOC 专属的对象数量信令。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ac4DsiAjocInfo {
    /// 是否使用静态下混。
    pub static_downmix: bool,
    /// 动态下混时的对象数；静态下混时字段不传输。
    pub downmix_objects: Option<u8>,
    /// full/upmix 对象数。
    pub upmix_objects: u8,
}

/// object-coded substream；`ajoc == None` 即 direct-object。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ac4DsiObjectSubstream {
    /// 表 E.5d 的采样率乘数码值。
    pub sampling_frequency_multiplier: u8,
    /// 表 90 的 `brate_ind`；没有码率指示时为 `None`。
    pub bitrate_indicator: Option<u8>,
    /// A-JOC 参数；direct-object 时为 `None`。
    pub ajoc: Option<Ac4DsiAjocInfo>,
    /// substream 中携带的对象种类。
    pub object_kinds: Ac4DsiObjectKinds,
}

impl Ac4DsiObjectSubstream {
    /// 是否为 direct-object，而不是 A-JOC。
    #[must_use]
    pub const fn is_direct_object(self) -> bool {
        self.ajoc.is_none()
    }
}

/// `ac4_substream_group_dsi()` 中的一个 substream。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ac4DsiSubstream {
    /// channel-coded substream；仅解析信令，PCM 路径仍延后。
    Channel(Ac4DsiChannelSubstream),
    /// direct-object 或 A-JOC substream。
    Object(Ac4DsiObjectSubstream),
}

/// substream group 的内容分类与可选语言标签。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ac4DsiContentType<'a> {
    /// 3 比特内容分类码。
    pub classifier: u8,
    /// 最多 63 个 8 比特元素的语言标签。
    pub language_tag: Option<Ac4DsiBytes<'a>>,
}

/// `ac4_substream_group_dsi()` 的解析结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ac4DsiSubstreamGroup<'a> {
    /// 在当前 presentation 内的零基下标。
    pub index: u8,
    /// 引用的 substream 是否存储在本 track。
    pub substreams_present: bool,
    /// 是否有高采样率频谱扩展。
    pub hsf_ext: bool,
    /// group 是否为 channel-coded。
    pub channel_coded: bool,
    /// group 内的音频 substream 数。
    pub n_substreams: u8,
    /// group 级内容分类。
    pub content_type: Option<Ac4DsiContentType<'a>>,
    source: &'a [u8],
    substreams_bit_offset: u64,
}

impl<'a> Ac4DsiSubstreamGroup<'a> {
    /// 按 DSI 顺序遍历 group 中的 substream。
    #[must_use]
    pub fn substreams(self) -> Ac4DsiSubstreamIter<'a> {
        Ac4DsiSubstreamIter::new(
            self.source,
            self.substreams_bit_offset,
            self.n_substreams,
            self.channel_coded,
        )
    }
}

/// 一个 substream group 内的 substream 迭代器。
#[derive(Debug, Clone)]
pub struct Ac4DsiSubstreamIter<'a> {
    reader: BitReader<'a>,
    remaining: u8,
    channel_coded: bool,
    failed: bool,
}

impl<'a> Ac4DsiSubstreamIter<'a> {
    fn new(source: &'a [u8], bit_offset: u64, count: u8, channel_coded: bool) -> Self {
        let mut reader = BitReader::new(source);
        let failed = reader.skip_bits(bit_offset).is_err();
        Self {
            reader,
            remaining: if failed { 0 } else { count },
            channel_coded,
            failed,
        }
    }
}

impl Iterator for Ac4DsiSubstreamIter<'_> {
    type Item = Result<Ac4DsiSubstream, DsiError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed || self.remaining == 0 {
            return None;
        }
        let parsed = parse_substream(&mut self.reader, self.channel_coded);
        if parsed.is_ok() {
            self.remaining = self.remaining.saturating_sub(1);
        } else {
            self.failed = true;
            self.remaining = 0;
        }
        Some(parsed)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = if self.failed {
            0
        } else {
            usize::from(self.remaining)
        };
        (remaining, Some(remaining))
    }
}

/// presentation 内的 substream group 迭代器。
#[derive(Debug, Clone)]
pub struct Ac4DsiSubstreamGroupIter<'a> {
    reader: BitReader<'a>,
    source: &'a [u8],
    remaining: u8,
    index: u8,
    failed: bool,
}

impl<'a> Ac4DsiSubstreamGroupIter<'a> {
    fn new(source: &'a [u8], bit_offset: u64, count: u8) -> Self {
        let mut reader = BitReader::new(source);
        let failed = reader.skip_bits(bit_offset).is_err();
        Self {
            reader,
            source,
            remaining: if failed { 0 } else { count },
            index: 0,
            failed,
        }
    }
}

impl<'a> Iterator for Ac4DsiSubstreamGroupIter<'a> {
    type Item = Result<Ac4DsiSubstreamGroup<'a>, DsiError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed || self.remaining == 0 {
            return None;
        }
        let parsed = parse_substream_group(&mut self.reader, self.source, self.index);
        if parsed.is_ok() {
            self.remaining = self.remaining.saturating_sub(1);
            self.index = self.index.saturating_add(1);
        } else {
            self.failed = true;
            self.remaining = 0;
        }
        Some(parsed)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = if self.failed {
            0
        } else {
            usize::from(self.remaining)
        };
        (remaining, Some(remaining))
    }
}

/// alternative presentation 的一个播放目标。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ac4DsiAlternativeTarget {
    /// 3 比特 decoder compatibility。
    pub md_compat: u8,
    /// 8 比特目标设备类别掩码。
    pub device_category: u8,
}

/// alternative presentation 播放目标迭代器。
#[derive(Debug, Clone)]
pub struct Ac4DsiAlternativeTargetIter<'a> {
    reader: BitReader<'a>,
    remaining: u8,
}

impl<'a> Ac4DsiAlternativeTargetIter<'a> {
    fn new(source: &'a [u8], bit_offset: u64, count: u8) -> Self {
        let mut reader = BitReader::new(source);
        let remaining = if reader.skip_bits(bit_offset).is_ok() {
            count
        } else {
            0
        };
        Self { reader, remaining }
    }
}

impl Iterator for Ac4DsiAlternativeTargetIter<'_> {
    type Item = Ac4DsiAlternativeTarget;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        let value = (|| {
            Some(Ac4DsiAlternativeTarget {
                md_compat: u8::try_from(self.reader.read_bits(3).ok()?).ok()?,
                device_category: u8::try_from(self.reader.read_bits(8).ok()?).ok()?,
            })
        })();
        if value.is_some() {
            self.remaining = self.remaining.saturating_sub(1);
        } else {
            self.remaining = 0;
        }
        value
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = usize::from(self.remaining);
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for Ac4DsiAlternativeTargetIter<'_> {}

/// `alternative_info()` 的名称与目标列表。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ac4DsiAlternativeInfo<'a> {
    presentation_name: &'a [u8],
    /// 播放目标数量。
    pub n_targets: u8,
    source: &'a [u8],
    targets_bit_offset: u64,
}

impl<'a> Ac4DsiAlternativeInfo<'a> {
    /// UTF-8 名称的原始字节。
    #[must_use]
    pub const fn presentation_name(self) -> &'a [u8] {
        self.presentation_name
    }

    /// 将名称解释为 UTF-8；无效编码不会在比特流解析阶段被替换。
    pub fn presentation_name_utf8(self) -> Result<&'a str, Utf8Error> {
        core::str::from_utf8(self.presentation_name)
    }

    /// 按码流顺序遍历播放目标。
    #[must_use]
    pub fn targets(self) -> Ac4DsiAlternativeTargetIter<'a> {
        Ac4DsiAlternativeTargetIter::new(self.source, self.targets_bit_offset, self.n_targets)
    }
}

/// 一个完整的 `ac4_presentation_v1_dsi()`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ac4DsiPresentationV1<'a> {
    /// 在 DSI presentation 数组中的下标。
    pub index: u16,
    /// 5 比特 presentation 配置；31 表示单 group。
    pub presentation_config: u8,
    /// decoder compatibility；配置 6 时不存在。
    pub md_compat: Option<u8>,
    /// 5 比特 presentation ID；尾部扩展 ID 可覆盖它。
    pub presentation_id: Option<u16>,
    /// 表 E.12 的 2 比特码值。
    pub frame_rate_multiply_info: Option<u8>,
    /// 表 E.13 的 2 比特码值。
    pub frame_rate_fraction_info: Option<u8>,
    /// presentation 自身的 EMDF 信息。
    pub presentation_emdf: Option<Ac4DsiEmdfInfo>,
    /// channel-coded presentation 布局；未声明时为 `None`。
    pub channel_layout: Option<Ac4DsiPresentationChannelLayout>,
    /// core/full 不同时的 core 布局。
    pub core_layout: Option<Ac4DsiPresentationCoreLayout>,
    /// 可选 presentation filter。
    pub filter: Option<Ac4DsiPresentationFilter<'a>>,
    /// 是否跨多个 PID；单 group 与配置 6 时不存在。
    pub multi_pid: Option<bool>,
    /// 扩展 presentation 配置中要求跳过的定界数据。
    pub config_extension: Option<Ac4DsiBytes<'a>>,
    /// presentation 内的 substream group 数。
    pub n_substream_groups: u8,
    /// 是否已针对耳机预虚拟化；配置 6 时不存在。
    pub pre_virtualized: Option<bool>,
    /// 附加 EMDF substream 数。
    pub n_additional_emdf_substreams: u8,
    /// presentation 级码率摘要。
    pub bitrate: Option<Ac4BitrateDsi>,
    /// alternative presentation 名称与目标。
    pub alternative: Option<Ac4DsiAlternativeInfo<'a>>,
    /// alternative 之前的对齐位数；非 alternative 时为 `None`。
    pub alternative_align_bits: Option<u8>,
    /// 尾部 indicator 之前的对齐位数。
    pub end_align_bits: u8,
    /// 兼容扩展 indicator；旧的短 DSI 中可以不存在。
    pub indicators: Option<Ac4DsiPresentationIndicators>,
    /// `pres_bytes` 内未被当前 presentation v1 语法解释的定界跳过区。
    pub skip_area: Ac4DsiBytes<'a>,
    source: &'a [u8],
    groups_bit_offset: u64,
    additional_emdf_bit_offset: u64,
}

impl<'a> Ac4DsiPresentationV1<'a> {
    /// 返回扩展 ID 覆盖后的 presentation ID。
    #[must_use]
    pub const fn effective_presentation_id(self) -> Option<u16> {
        match self.indicators {
            Some(indicators) => match indicators.extended_presentation_id {
                Some(value) => Some(value),
                None => self.presentation_id,
            },
            None => self.presentation_id,
        }
    }

    /// 按 DSI 顺序遍历 substream groups。
    #[must_use]
    pub fn substream_groups(self) -> Ac4DsiSubstreamGroupIter<'a> {
        Ac4DsiSubstreamGroupIter::new(self.source, self.groups_bit_offset, self.n_substream_groups)
    }

    /// 按 DSI 顺序遍历附加 EMDF 描述。
    #[must_use]
    pub fn additional_emdf(self) -> Ac4DsiEmdfIter<'a> {
        Ac4DsiEmdfIter::new(
            self.source,
            self.additional_emdf_bit_offset,
            self.n_additional_emdf_substreams,
        )
    }

    fn parse(presentation: Ac4DsiPresentation<'a>) -> Result<Self, DsiError> {
        let source = presentation.payload;
        let mut reader = BitReader::new(source);
        let presentation_config = read_u8(&mut reader, 5)?;

        let mut md_compat = None;
        let mut presentation_id = None;
        let mut frame_rate_multiply_info = None;
        let mut frame_rate_fraction_info = None;
        let mut presentation_emdf = None;
        let mut channel_layout = None;
        let mut core_layout = None;
        let mut filter = None;
        let mut multi_pid = None;
        let mut config_extension = None;
        let mut n_substream_groups = 0u8;
        let mut groups_bit_offset = reader.bit_position();
        let mut pre_virtualized = None;
        let mut add_emdf_substreams = presentation_config == 6;

        if presentation_config != 6 {
            md_compat = Some(read_u8(&mut reader, 3)?);
            if read_flag(&mut reader)? {
                presentation_id = Some(read_u16(&mut reader, 5)?);
            }
            frame_rate_multiply_info = Some(read_bounded_u8(
                &mut reader,
                2,
                2,
                "dsi_frame_rate_multiply_info",
            )?);
            frame_rate_fraction_info = Some(read_bounded_u8(
                &mut reader,
                2,
                2,
                "dsi_frame_rate_fraction_info",
            )?);
            presentation_emdf = Some(Ac4DsiEmdfInfo::parse(&mut reader)?);

            if read_flag(&mut reader)? {
                let channel_mode = read_bounded_u8(&mut reader, 5, 15, "dsi_presentation_ch_mode")?;
                let (four_back_channels_present, top_channel_pairs) =
                    if matches!(channel_mode, 11..=14) {
                        (
                            Some(read_flag(&mut reader)?),
                            Some(read_bounded_u8(
                                &mut reader,
                                2,
                                2,
                                "pres_top_channel_pairs",
                            )?),
                        )
                    } else {
                        (None, None)
                    };
                read_reserved_zero(&mut reader, 6, "presentation_reserved_zero")?;
                channel_layout = Some(Ac4DsiPresentationChannelLayout {
                    channel_mode,
                    four_back_channels_present,
                    top_channel_pairs,
                    channel_groups: Ac4DsiChannelGroups::parse_presentation(&mut reader)?,
                });
            }

            if read_flag(&mut reader)? {
                let channel_coded = read_flag(&mut reader)?;
                core_layout = Some(Ac4DsiPresentationCoreLayout {
                    channel_coded,
                    channel_mode: if channel_coded {
                        Some(read_u8(&mut reader, 2)?)
                    } else {
                        None
                    },
                });
            }

            if read_flag(&mut reader)? {
                let enabled = read_flag(&mut reader)?;
                let len = usize::from(read_u8(&mut reader, 8)?);
                filter = Some(Ac4DsiPresentationFilter {
                    enabled,
                    data: Ac4DsiBytes::read(&mut reader, source, len)?,
                });
            }

            groups_bit_offset = reader.bit_position();
            if presentation_config == 31 {
                n_substream_groups = 1;
                parse_substream_group(&mut reader, source, 0)?;
            } else {
                multi_pid = Some(read_flag(&mut reader)?);
                n_substream_groups = match presentation_config {
                    0..=2 => 2,
                    3 | 4 => 3,
                    5 => read_u8(&mut reader, 3)?.saturating_add(2),
                    _ => 0,
                };
                groups_bit_offset = reader.bit_position();
                for index in 0..n_substream_groups {
                    parse_substream_group(&mut reader, source, index)?;
                }
                if presentation_config > 5 {
                    let len = usize::from(read_u8(&mut reader, 7)?);
                    config_extension = Some(Ac4DsiBytes::read(&mut reader, source, len)?);
                }
            }

            pre_virtualized = Some(read_flag(&mut reader)?);
            add_emdf_substreams = read_flag(&mut reader)?;
        }

        let n_additional_emdf_substreams = if add_emdf_substreams {
            read_u8(&mut reader, 7)?
        } else {
            0
        };
        let additional_emdf_bit_offset = reader.bit_position();
        for _ in 0..n_additional_emdf_substreams {
            Ac4DsiEmdfInfo::parse(&mut reader)?;
        }

        let bitrate = if read_flag(&mut reader)? {
            Some(Ac4BitrateDsi::parse(&mut reader).map_err(DsiError::Truncated)?)
        } else {
            None
        };

        let (alternative, alternative_align_bits) = if read_flag(&mut reader)? {
            let align = align(&mut reader)?;
            (
                Some(parse_alternative_info(&mut reader, source)?),
                Some(align),
            )
        } else {
            (None, None)
        };

        let end_align_bits = align(&mut reader)?;
        let indicators = if reader.remaining_bits() >= 8 {
            let dialogue_enhancement = read_flag(&mut reader)?;
            let immersive_audio = read_flag(&mut reader)?;
            let reserved = read_u8(&mut reader, 4)?;
            let extended = read_flag(&mut reader)?;
            let (extended_presentation_id, reserved_id_bit) = if extended {
                (Some(read_u16(&mut reader, 9)?), None)
            } else {
                (None, Some(read_flag(&mut reader)?))
            };
            Some(Ac4DsiPresentationIndicators {
                dialogue_enhancement,
                immersive_audio,
                reserved,
                extended_presentation_id,
                reserved_id_bit,
            })
        } else {
            None
        };

        let skip_len = usize::try_from(reader.remaining_bits() / 8).unwrap_or(usize::MAX);
        let skip_area = Ac4DsiBytes::read(&mut reader, source, skip_len)?;

        Ok(Self {
            index: presentation.index,
            presentation_config,
            md_compat,
            presentation_id,
            frame_rate_multiply_info,
            frame_rate_fraction_info,
            presentation_emdf,
            channel_layout,
            core_layout,
            filter,
            multi_pid,
            config_extension,
            n_substream_groups,
            pre_virtualized,
            n_additional_emdf_substreams,
            bitrate,
            alternative,
            alternative_align_bits,
            end_align_bits,
            indicators,
            skip_area,
            source,
            groups_bit_offset,
            additional_emdf_bit_offset,
        })
    }
}

impl<'a> Ac4DsiPresentation<'a> {
    /// 解析 presentation version 1 的系统级信令。
    ///
    /// 其他 presentation 版本返回 `Ok(None)` 并保持 body 不透明。
    pub fn v1(self) -> Result<Option<Ac4DsiPresentationV1<'a>>, DsiError> {
        if self.version != 1 {
            return Ok(None);
        }
        Ac4DsiPresentationV1::parse(self).map(Some)
    }
}

fn parse_substream_group<'a>(
    reader: &mut BitReader<'a>,
    source: &'a [u8],
    index: u8,
) -> Result<Ac4DsiSubstreamGroup<'a>, DsiError> {
    let substreams_present = read_flag(reader)?;
    let hsf_ext = read_flag(reader)?;
    let channel_coded = read_flag(reader)?;
    let n_substreams = read_u8(reader, 8)?;
    let substreams_bit_offset = reader.bit_position();
    for _ in 0..n_substreams {
        parse_substream(reader, channel_coded)?;
    }
    let content_type = if read_flag(reader)? {
        let classifier = read_u8(reader, 3)?;
        let language_tag = if read_flag(reader)? {
            let len = usize::from(read_u8(reader, 6)?);
            Some(Ac4DsiBytes::read(reader, source, len)?)
        } else {
            None
        };
        Some(Ac4DsiContentType {
            classifier,
            language_tag,
        })
    } else {
        None
    };
    Ok(Ac4DsiSubstreamGroup {
        index,
        substreams_present,
        hsf_ext,
        channel_coded,
        n_substreams,
        content_type,
        source,
        substreams_bit_offset,
    })
}

fn parse_substream(
    reader: &mut BitReader<'_>,
    channel_coded: bool,
) -> Result<Ac4DsiSubstream, DsiError> {
    let multiplier_position = reader.bit_position();
    let sampling_frequency_multiplier = read_u8(reader, 2)?;
    if sampling_frequency_multiplier > 2 {
        return Err(DsiError::InvalidFieldValue {
            field: "dsi_sf_multiplier",
            bit_position: multiplier_position,
            value: u64::from(sampling_frequency_multiplier),
        });
    }
    let bitrate_indicator = if read_flag(reader)? {
        let position = reader.bit_position();
        let value = read_u8(reader, 5)?;
        if value > 19 {
            return Err(DsiError::InvalidFieldValue {
                field: "substream_bitrate_indicator",
                bit_position: position,
                value: u64::from(value),
            });
        }
        Some(value)
    } else {
        None
    };

    if channel_coded {
        read_reserved_zero(reader, 6, "substream_reserved_zero")?;
        return Ok(Ac4DsiSubstream::Channel(Ac4DsiChannelSubstream {
            sampling_frequency_multiplier,
            bitrate_indicator,
            channel_groups: Ac4DsiChannelGroups::parse(reader)?,
        }));
    }

    let ajoc = if read_flag(reader)? {
        let static_downmix = read_flag(reader)?;
        let downmix_objects = if static_downmix {
            None
        } else {
            Some(read_u8(reader, 4)?.saturating_add(1))
        };
        Some(Ac4DsiAjocInfo {
            static_downmix,
            downmix_objects,
            upmix_objects: read_u8(reader, 6)?.saturating_add(1),
        })
    } else {
        None
    };
    Ok(Ac4DsiSubstream::Object(Ac4DsiObjectSubstream {
        sampling_frequency_multiplier,
        bitrate_indicator,
        ajoc,
        object_kinds: Ac4DsiObjectKinds {
            bed: read_flag(reader)?,
            dynamic: read_flag(reader)?,
            isf: read_flag(reader)?,
            reserved: read_flag(reader)?,
        },
    }))
}

fn parse_alternative_info<'a>(
    reader: &mut BitReader<'a>,
    source: &'a [u8],
) -> Result<Ac4DsiAlternativeInfo<'a>, DsiError> {
    let name_len = usize::from(read_u16(reader, 16)?);
    let name = Ac4DsiBytes::read(reader, source, name_len)?;
    let presentation_name = name.as_aligned_slice().ok_or(DsiError::InvalidFieldValue {
        field: "alternative_info_alignment",
        bit_position: name.bit_offset,
        value: name.bit_offset % 8,
    })?;
    let count_position = reader.bit_position();
    let n_targets = read_u8(reader, 5)?;
    if n_targets == 0 {
        return Err(DsiError::InvalidFieldValue {
            field: "n_targets",
            bit_position: count_position,
            value: 0,
        });
    }
    let targets_bit_offset = reader.bit_position();
    for _ in 0..n_targets {
        read_u8(reader, 3)?;
        read_u8(reader, 8)?;
    }
    Ok(Ac4DsiAlternativeInfo {
        presentation_name,
        n_targets,
        source,
        targets_bit_offset,
    })
}

fn read_reserved_zero(
    reader: &mut BitReader<'_>,
    width: u32,
    field: &'static str,
) -> Result<(), DsiError> {
    let bit_position = reader.bit_position();
    let value = reader.read_bits(width).map_err(DsiError::Truncated)?;
    if value != 0 {
        return Err(DsiError::ReservedValueNonZero {
            field,
            bit_position,
            value,
        });
    }
    Ok(())
}

fn read_flag(reader: &mut BitReader<'_>) -> Result<bool, DsiError> {
    reader.read_flag().map_err(DsiError::Truncated)
}

fn read_u8(reader: &mut BitReader<'_>, width: u32) -> Result<u8, DsiError> {
    u8::try_from(reader.read_bits(width).map_err(DsiError::Truncated)?).map_err(|_| {
        DsiError::Truncated(ReadError::ValueOverflow {
            bit_position: reader.bit_position().saturating_sub(u64::from(width)),
        })
    })
}

fn read_bounded_u8(
    reader: &mut BitReader<'_>,
    width: u32,
    maximum: u8,
    field: &'static str,
) -> Result<u8, DsiError> {
    let bit_position = reader.bit_position();
    let value = read_u8(reader, width)?;
    if value > maximum {
        return Err(DsiError::InvalidFieldValue {
            field,
            bit_position,
            value: u64::from(value),
        });
    }
    Ok(value)
}

fn read_u16(reader: &mut BitReader<'_>, width: u32) -> Result<u16, DsiError> {
    u16::try_from(reader.read_bits(width).map_err(DsiError::Truncated)?).map_err(|_| {
        DsiError::Truncated(ReadError::ValueOverflow {
            bit_position: reader.bit_position().saturating_sub(u64::from(width)),
        })
    })
}

fn read_u32(reader: &mut BitReader<'_>, width: u32) -> Result<u32, DsiError> {
    u32::try_from(reader.read_bits(width).map_err(DsiError::Truncated)?).map_err(|_| {
        DsiError::Truncated(ReadError::ValueOverflow {
            bit_position: reader.bit_position().saturating_sub(u64::from(width)),
        })
    })
}

fn align(reader: &mut BitReader<'_>) -> Result<u8, DsiError> {
    u8::try_from(reader.byte_align().map_err(DsiError::Truncated)?).map_err(|_| {
        DsiError::Truncated(ReadError::ValueOverflow {
            bit_position: reader.bit_position(),
        })
    })
}

#[cfg(test)]
#[expect(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    reason = "测试位打包器使用固定容量，所有构造都远小于容量"
)]
mod tests {
    extern crate std;

    use super::*;

    struct BitBuf {
        bytes: [u8; 1_024],
        bits: usize,
    }

    impl BitBuf {
        fn new() -> Self {
            Self {
                bytes: [0; 1_024],
                bits: 0,
            }
        }

        fn push_bits(&mut self, value: u64, width: usize) {
            for shift in (0..width).rev() {
                if value >> shift & 1 != 0 {
                    self.bytes[self.bits / 8] |= 1 << (7 - self.bits % 8);
                }
                self.bits += 1;
            }
        }

        fn push_bytes(&mut self, bytes: &[u8]) {
            for &byte in bytes {
                self.push_bits(u64::from(byte), 8);
            }
        }

        fn byte_align(&mut self) {
            while !self.bits.is_multiple_of(8) {
                self.push_bits(0, 1);
            }
        }

        fn as_slice(&self) -> &[u8] {
            &self.bytes[..self.bits.div_ceil(8)]
        }

        fn presentation(&self) -> Ac4DsiPresentation<'_> {
            Ac4DsiPresentation {
                index: 7,
                version: 1,
                declared_bytes: u32::try_from(self.as_slice().len()).unwrap(),
                payload: self.as_slice(),
            }
        }
    }

    fn push_common(buf: &mut BitBuf, config: u8, md_compat: u8, presentation_id: Option<u8>) {
        buf.push_bits(u64::from(config), 5);
        buf.push_bits(u64::from(md_compat), 3);
        buf.push_bits(u64::from(presentation_id.is_some()), 1);
        if let Some(id) = presentation_id {
            buf.push_bits(u64::from(id), 5);
        }
        buf.push_bits(1, 2); // dsi_frame_rate_multiply_info
        buf.push_bits(2, 2); // dsi_frame_rate_fraction_info
        buf.push_bits(5, 5); // presentation_emdf_version
        buf.push_bits(0x155, 10); // presentation_key_id
    }

    fn push_object_group(buf: &mut BitBuf, ajoc: bool) {
        buf.push_bits(1, 1); // b_substreams_present
        buf.push_bits(0, 1); // b_hsf_ext
        buf.push_bits(0, 1); // b_channel_coded
        buf.push_bits(1, 8); // n_substreams
        buf.push_bits(1, 2); // dsi_sf_multiplier
        buf.push_bits(1, 1); // b_substream_bitrate_indicator
        buf.push_bits(10, 5);
        buf.push_bits(u64::from(ajoc), 1);
        if ajoc {
            buf.push_bits(0, 1); // b_static_dmx
            buf.push_bits(4, 4); // 5 downmix objects
            buf.push_bits(20, 6); // 21 upmix objects
        }
        buf.push_bits(0, 1); // bed
        buf.push_bits(1, 1); // dynamic
        buf.push_bits(0, 1); // ISF
        buf.push_bits(0, 1); // reserved
        buf.push_bits(0, 1); // b_content_type
    }

    #[test]
    fn parses_ajoc_groups_filter_emdf_and_indicators() {
        let mut body = BitBuf::new();
        push_common(&mut body, 31, 4, Some(3));
        body.push_bits(0, 1); // b_presentation_channel_coded
        body.push_bits(1, 1); // b_presentation_core_differs
        body.push_bits(1, 1); // b_presentation_core_channel_coded
        body.push_bits(2, 2); // core mode
        body.push_bits(1, 1); // b_presentation_filter
        body.push_bits(1, 1); // b_enable_presentation
        body.push_bits(2, 8);
        body.push_bytes(&[0xaa, 0x55]);

        body.push_bits(1, 1); // b_substreams_present
        body.push_bits(0, 1); // b_hsf_ext
        body.push_bits(0, 1); // b_channel_coded
        body.push_bits(2, 8); // n_substreams
        body.push_bits(1, 2); // substream 0 multiplier
        body.push_bits(1, 1);
        body.push_bits(10, 5);
        body.push_bits(1, 1); // b_ajoc
        body.push_bits(0, 1); // dynamic downmix
        body.push_bits(4, 4); // 5 downmix objects
        body.push_bits(20, 6); // 21 upmix objects
        body.push_bits(0, 1);
        body.push_bits(1, 1);
        body.push_bits(0, 1);
        body.push_bits(0, 1);
        body.push_bits(0, 2); // direct-object multiplier
        body.push_bits(0, 1); // no bitrate
        body.push_bits(0, 1); // not A-JOC
        body.push_bits(1, 1); // bed
        body.push_bits(0, 1); // dynamic
        body.push_bits(0, 1); // ISF
        body.push_bits(1, 1); // reserved is preserved
        body.push_bits(1, 1); // b_content_type
        body.push_bits(2, 3);
        body.push_bits(1, 1); // b_language_indicator
        body.push_bits(2, 6);
        body.push_bytes(b"en");

        body.push_bits(1, 1); // b_pre_virtualized
        body.push_bits(1, 1); // b_add_emdf_substreams
        body.push_bits(2, 7);
        body.push_bits(1, 5);
        body.push_bits(2, 10);
        body.push_bits(3, 5);
        body.push_bits(4, 10);
        body.push_bits(1, 1); // b_presentation_bitrate_info
        body.push_bits(2, 2);
        body.push_bits(768_000, 32);
        body.push_bits(1_000, 32);
        body.push_bits(0, 1); // b_alternative
        body.byte_align();
        body.push_bits(1, 1); // de_indicator
        body.push_bits(1, 1); // immersive_audio_indicator
        body.push_bits(0xa, 4);
        body.push_bits(1, 1); // b_extended_presentation_id
        body.push_bits(300, 9);

        let parsed = body.presentation().v1().unwrap().unwrap();
        assert_eq!(parsed.index, 7);
        assert_eq!(parsed.presentation_config, 31);
        assert_eq!(parsed.md_compat, Some(4));
        assert_eq!(parsed.presentation_id, Some(3));
        assert_eq!(parsed.frame_rate_multiply_info, Some(1));
        assert_eq!(parsed.frame_rate_fraction_info, Some(2));
        assert_eq!(parsed.presentation_emdf.unwrap().version, 5);
        assert_eq!(parsed.presentation_emdf.unwrap().key_id, 0x155);
        assert_eq!(parsed.core_layout.unwrap().channel_mode, Some(2));
        assert!(parsed.filter.unwrap().enabled);
        assert_eq!(parsed.filter.unwrap().data.get(1), Some(0x55));
        assert_eq!(parsed.filter.unwrap().data.get(2), None);
        assert_eq!(
            parsed.filter.unwrap().data.as_aligned_slice(),
            Some(&[0xaa, 0x55][..])
        );
        assert_eq!(
            parsed
                .filter
                .unwrap()
                .data
                .iter()
                .collect::<std::vec::Vec<_>>(),
            [0xaa, 0x55]
        );
        assert_eq!(parsed.n_substream_groups, 1);
        assert_eq!(parsed.pre_virtualized, Some(true));
        assert_eq!(parsed.n_additional_emdf_substreams, 2);
        assert_eq!(
            parsed.additional_emdf().collect::<std::vec::Vec<_>>(),
            [
                Ac4DsiEmdfInfo {
                    version: 1,
                    key_id: 2,
                },
                Ac4DsiEmdfInfo {
                    version: 3,
                    key_id: 4,
                },
            ]
        );
        assert_eq!(parsed.bitrate.unwrap().bit_rate, 768_000);
        assert_eq!(parsed.bitrate.unwrap().precision, 1_000);
        assert_eq!(parsed.effective_presentation_id(), Some(300));
        assert!(parsed.indicators.unwrap().dialogue_enhancement);
        assert!(parsed.indicators.unwrap().immersive_audio);
        assert_eq!(parsed.indicators.unwrap().reserved, 0xa);

        let group = parsed.substream_groups().next().unwrap().unwrap();
        assert_eq!(group.index, 0);
        assert_eq!(group.n_substreams, 2);
        assert_eq!(
            group
                .content_type
                .unwrap()
                .language_tag
                .unwrap()
                .iter()
                .collect::<std::vec::Vec<_>>(),
            b"en"
        );
        let substreams = group
            .substreams()
            .collect::<Result<std::vec::Vec<_>, _>>()
            .unwrap();
        let Ac4DsiSubstream::Object(ajoc) = substreams[0] else {
            panic!("expected A-JOC object substream");
        };
        assert_eq!(ajoc.ajoc.unwrap().downmix_objects, Some(5));
        assert_eq!(ajoc.ajoc.unwrap().upmix_objects, 21);
        let Ac4DsiSubstream::Object(direct) = substreams[1] else {
            panic!("expected direct-object substream");
        };
        assert!(direct.is_direct_object());
        assert!(direct.object_kinds.bed);
        assert!(direct.object_kinds.reserved);
    }

    #[test]
    fn parses_alternative_name_and_targets() {
        let mut body = BitBuf::new();
        push_common(&mut body, 31, 3, None);
        body.push_bits(0, 1); // presentation channel coded
        body.push_bits(0, 1); // core differs
        body.push_bits(0, 1); // filter
        push_object_group(&mut body, false);
        body.push_bits(0, 1); // pre-virtualized
        body.push_bits(0, 1); // additional EMDF
        body.push_bits(0, 1); // bitrate
        body.push_bits(1, 1); // alternative
        body.byte_align();
        body.push_bits(3, 16);
        body.push_bytes(b"alt");
        body.push_bits(2, 5);
        body.push_bits(2, 3);
        body.push_bits(0x81, 8);
        body.push_bits(4, 3);
        body.push_bits(0x24, 8);
        body.byte_align();

        let parsed = body.presentation().v1().unwrap().unwrap();
        let alternative = parsed.alternative.unwrap();
        assert_eq!(alternative.presentation_name(), b"alt");
        assert_eq!(alternative.presentation_name_utf8().unwrap(), "alt");
        assert_eq!(alternative.n_targets, 2);
        assert_eq!(
            alternative.targets().collect::<std::vec::Vec<_>>(),
            [
                Ac4DsiAlternativeTarget {
                    md_compat: 2,
                    device_category: 0x81,
                },
                Ac4DsiAlternativeTarget {
                    md_compat: 4,
                    device_category: 0x24,
                },
            ]
        );
        let group = parsed.substream_groups().next().unwrap().unwrap();
        let Ac4DsiSubstream::Object(direct) = group.substreams().next().unwrap().unwrap() else {
            panic!("expected direct-object substream");
        };
        assert!(direct.is_direct_object());
    }

    #[test]
    fn parses_config_five_group_count() {
        let mut body = BitBuf::new();
        push_common(&mut body, 5, 2, None);
        body.push_bits(0, 1); // presentation channel coded
        body.push_bits(0, 1); // core differs
        body.push_bits(0, 1); // filter
        body.push_bits(1, 1); // b_multi_pid
        body.push_bits(0, 3); // two groups
        push_object_group(&mut body, true);
        push_object_group(&mut body, false);
        body.push_bits(0, 1); // pre-virtualized
        body.push_bits(0, 1); // additional EMDF
        body.push_bits(0, 1); // bitrate
        body.push_bits(0, 1); // alternative
        body.byte_align();

        let parsed = body.presentation().v1().unwrap().unwrap();
        assert_eq!(parsed.multi_pid, Some(true));
        assert_eq!(parsed.n_substream_groups, 2);
        let groups = parsed
            .substream_groups()
            .collect::<Result<std::vec::Vec<_>, _>>()
            .unwrap();
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].index, 0);
        assert_eq!(groups[1].index, 1);
        let Ac4DsiSubstream::Object(first) = groups[0].substreams().next().unwrap().unwrap() else {
            panic!("expected object substream");
        };
        let Ac4DsiSubstream::Object(second) = groups[1].substreams().next().unwrap().unwrap()
        else {
            panic!("expected object substream");
        };
        assert!(first.ajoc.is_some());
        assert!(second.is_direct_object());
    }

    #[test]
    fn preserves_extended_config_bytes() {
        let mut body = BitBuf::new();
        push_common(&mut body, 7, 1, None);
        body.push_bits(0, 1); // presentation channel coded
        body.push_bits(0, 1); // core differs
        body.push_bits(0, 1); // filter
        body.push_bits(0, 1); // b_multi_pid
        body.push_bits(2, 7);
        body.push_bytes(&[0xde, 0xad]);
        body.push_bits(1, 1); // pre-virtualized
        body.push_bits(0, 1); // additional EMDF
        body.push_bits(0, 1); // bitrate
        body.push_bits(0, 1); // alternative
        body.byte_align();

        let parsed = body.presentation().v1().unwrap().unwrap();
        assert_eq!(parsed.n_substream_groups, 0);
        assert_eq!(parsed.multi_pid, Some(false));
        assert_eq!(parsed.pre_virtualized, Some(true));
        assert_eq!(parsed.config_extension.unwrap().as_aligned_slice(), None);
        assert_eq!(
            parsed
                .config_extension
                .unwrap()
                .iter()
                .collect::<std::vec::Vec<_>>(),
            [0xde, 0xad]
        );
    }

    #[test]
    fn preserves_channel_signalling_without_claiming_pcm_support() {
        let mut body = BitBuf::new();
        push_common(&mut body, 31, 0, None);
        body.push_bits(1, 1); // b_presentation_channel_coded
        body.push_bits(11, 5);
        body.push_bits(1, 1); // four back channels
        body.push_bits(2, 2); // top pairs
        body.push_bits(0, 6); // reserved_zero
        body.push_bits(0x2_0081, 18);
        body.push_bits(0, 1); // core differs
        body.push_bits(0, 1); // filter
        body.push_bits(1, 1); // substreams present
        body.push_bits(0, 1); // hsf
        body.push_bits(1, 1); // channel coded
        body.push_bits(1, 8);
        body.push_bits(2, 2); // sf multiplier
        body.push_bits(1, 1); // bitrate present
        body.push_bits(19, 5);
        body.push_bits(0, 6); // reserved_zero
        body.push_bits(0x1_0003, 18);
        body.push_bits(0, 1); // content type
        body.push_bits(0, 1); // pre-virtualized
        body.push_bits(0, 1); // additional EMDF
        body.push_bits(0, 1); // bitrate
        body.push_bits(0, 1); // alternative
        body.byte_align();

        let parsed = body.presentation().v1().unwrap().unwrap();
        let layout = parsed.channel_layout.unwrap();
        assert_eq!(layout.channel_mode, 11);
        assert_eq!(layout.four_back_channels_present, Some(true));
        assert_eq!(layout.top_channel_pairs, Some(2));
        assert!(layout.channel_groups.contains(17));
        assert!(layout.channel_groups.contains(7));
        assert!(layout.channel_groups.contains(0));
        assert!(!layout.channel_groups.contains(8));

        let group = parsed.substream_groups().next().unwrap().unwrap();
        assert!(group.channel_coded);
        let Ac4DsiSubstream::Channel(channel) = group.substreams().next().unwrap().unwrap() else {
            panic!("expected channel-coded substream");
        };
        assert_eq!(channel.sampling_frequency_multiplier, 2);
        assert_eq!(channel.bitrate_indicator, Some(19));
        assert!(channel.channel_groups.contains(16));
        assert!(channel.channel_groups.contains(1));
        assert!(channel.channel_groups.contains(0));
    }

    #[test]
    fn config_six_only_carries_additional_emdf() {
        let mut body = BitBuf::new();
        body.push_bits(6, 5);
        body.push_bits(2, 7);
        body.push_bits(7, 5);
        body.push_bits(11, 10);
        body.push_bits(8, 5);
        body.push_bits(12, 10);
        body.push_bits(0, 1); // bitrate
        body.push_bits(0, 1); // alternative
        body.byte_align();

        let parsed = body.presentation().v1().unwrap().unwrap();
        assert_eq!(parsed.presentation_config, 6);
        assert_eq!(parsed.md_compat, None);
        assert_eq!(parsed.pre_virtualized, None);
        assert_eq!(parsed.n_substream_groups, 0);
        assert_eq!(
            parsed.additional_emdf().collect::<std::vec::Vec<_>>(),
            [
                Ac4DsiEmdfInfo {
                    version: 7,
                    key_id: 11,
                },
                Ac4DsiEmdfInfo {
                    version: 8,
                    key_id: 12,
                },
            ]
        );
    }

    #[test]
    fn rejects_nonzero_reserved_zero() {
        let mut body = BitBuf::new();
        push_common(&mut body, 31, 0, None);
        body.push_bits(1, 1);
        body.push_bits(1, 5);
        body.push_bits(1, 6); // invalid reserved_zero

        assert!(matches!(
            body.presentation().v1(),
            Err(DsiError::ReservedValueNonZero {
                field: "presentation_reserved_zero",
                value: 1,
                ..
            })
        ));
    }

    #[test]
    fn rejects_reserved_frame_rate_code() {
        let mut body = BitBuf::new();
        body.push_bits(31, 5);
        body.push_bits(0, 3); // md_compat
        body.push_bits(0, 1); // presentation ID
        body.push_bits(3, 2); // reserved multiply code

        assert!(matches!(
            body.presentation().v1(),
            Err(DsiError::InvalidFieldValue {
                field: "dsi_frame_rate_multiply_info",
                value: 3,
                ..
            })
        ));
    }

    #[test]
    fn rejects_reserved_presentation_channel_group_bit() {
        let mut body = BitBuf::new();
        push_common(&mut body, 31, 0, None);
        body.push_bits(1, 1); // presentation channel coded
        body.push_bits(1, 5); // stereo
        body.push_bits(0, 6); // reserved_zero
        body.push_bits(1 << 8, 18); // channel group index 8 is reserved

        assert!(matches!(
            body.presentation().v1(),
            Err(DsiError::ReservedValueNonZero {
                field: "presentation_v1_channel_groups[8]",
                value: 1,
                ..
            })
        ));
    }

    #[test]
    fn preserves_deprecated_substream_channel_group_eight() {
        let mut body = BitBuf::new();
        push_common(&mut body, 31, 0, None);
        body.push_bits(0, 1); // presentation channel coded
        body.push_bits(0, 1); // core differs
        body.push_bits(0, 1); // filter
        body.push_bits(1, 1); // substreams present
        body.push_bits(0, 1); // hsf
        body.push_bits(1, 1); // channel coded
        body.push_bits(1, 8); // n_substreams
        body.push_bits(0, 2); // sf multiplier
        body.push_bits(0, 1); // bitrate absent
        body.push_bits(0, 6); // reserved_zero
        body.push_bits(1 << 8, 18); // deprecated group 8 is preserved
        body.push_bits(0, 1); // content type
        body.push_bits(0, 1); // pre-virtualized
        body.push_bits(0, 1); // additional EMDF
        body.push_bits(0, 1); // bitrate
        body.push_bits(0, 1); // alternative
        body.byte_align();

        let parsed = body.presentation().v1().unwrap().unwrap();
        let group = parsed.substream_groups().next().unwrap().unwrap();
        let Ac4DsiSubstream::Channel(channel) = group.substreams().next().unwrap().unwrap() else {
            panic!("expected channel-coded substream");
        };
        assert!(channel.channel_groups.contains(8));
        assert!(!channel.channel_groups.contains(7));
        assert_eq!(channel.channel_groups.raw(), 1 << 8);
    }

    #[test]
    fn rejects_reserved_sampling_frequency_multiplier() {
        let mut body = BitBuf::new();
        push_common(&mut body, 31, 0, None);
        body.push_bits(0, 1); // presentation channel coded
        body.push_bits(0, 1); // core differs
        body.push_bits(0, 1); // filter
        body.push_bits(1, 1); // substreams present
        body.push_bits(0, 1); // hsf
        body.push_bits(0, 1); // object coded
        body.push_bits(1, 8);
        body.push_bits(3, 2); // reserved dsi_sf_multiplier

        assert!(matches!(
            body.presentation().v1(),
            Err(DsiError::InvalidFieldValue {
                field: "dsi_sf_multiplier",
                value: 3,
                ..
            })
        ));
    }

    #[test]
    fn rejects_truncated_alternative_name() {
        let mut body = BitBuf::new();
        body.push_bits(6, 5);
        body.push_bits(0, 7);
        body.push_bits(0, 1); // bitrate
        body.push_bits(1, 1); // alternative
        body.byte_align();
        body.push_bits(2, 16);
        body.push_bytes(b"x");

        assert!(matches!(
            body.presentation().v1(),
            Err(DsiError::Truncated(ReadError::OutOfBounds { .. }))
        ));
    }

    #[test]
    fn rejects_zero_alternative_targets() {
        let mut body = BitBuf::new();
        body.push_bits(6, 5);
        body.push_bits(0, 7);
        body.push_bits(0, 1); // bitrate
        body.push_bits(1, 1); // alternative
        body.byte_align();
        body.push_bits(0, 16); // empty name
        body.push_bits(0, 5); // invalid target count

        assert!(matches!(
            body.presentation().v1(),
            Err(DsiError::InvalidFieldValue {
                field: "n_targets",
                value: 0,
                ..
            })
        ));
    }

    #[test]
    fn preserves_bounded_skip_area_after_known_indicator_extension() {
        let mut body = BitBuf::new();
        body.push_bits(6, 5);
        body.push_bits(0, 7);
        body.push_bits(0, 1); // bitrate
        body.push_bits(0, 1); // alternative
        body.byte_align();
        body.push_bits(0, 8); // indicators without extended ID
        body.push_bits(0xff, 8); // no known owner

        let parsed = body.presentation().v1().unwrap().unwrap();
        assert_eq!(parsed.skip_area.as_aligned_slice(), Some(&[0xff][..]));
    }

    #[test]
    fn real_ajoc_dsi_matches_toc_summary() {
        let body = [
            0xfc, 0x80, 0x00, 0x00, 0x08, 0x02, 0x28, 0x4d, 0x20, 0x00, 0xc0,
        ];
        let presentation = Ac4DsiPresentation {
            index: 0,
            version: 1,
            declared_bytes: 11,
            payload: &body,
        };
        let parsed = presentation.v1().unwrap().unwrap();
        assert_eq!(parsed.presentation_config, 31);
        assert_eq!(parsed.md_compat, Some(4));
        assert_eq!(parsed.presentation_id, Some(0));
        assert_eq!(parsed.n_substream_groups, 1);
        assert!(parsed.indicators.unwrap().dialogue_enhancement);
        assert!(parsed.indicators.unwrap().immersive_audio);
        assert!(parsed.skip_area.is_empty());

        let group = parsed.substream_groups().next().unwrap().unwrap();
        assert!(group.substreams_present);
        assert!(!group.channel_coded);
        assert_eq!(group.n_substreams, 1);
        assert_eq!(group.content_type.unwrap().classifier, 0);
        let Ac4DsiSubstream::Object(substream) = group.substreams().next().unwrap().unwrap() else {
            panic!("expected A-JOC object substream");
        };
        assert_eq!(substream.ajoc.unwrap().downmix_objects, Some(9));
        assert_eq!(substream.ajoc.unwrap().upmix_objects, 20);
        assert!(substream.object_kinds.dynamic);
    }
}
