//! `ac4_presentation_substream()` 的 alternative 选择前缀。
//!
//! 对应 `TS103190-2:v1.3.1:6.2.2.3` 中 `b_alternative` 控制的第一段，
//! 语义见 `6.3.3.1.1` 至 `6.3.3.1.15`。
//!
//! 本模块只解析 presentation 名称分片、播放目标和逐音频 substream 的
//! activation/dataset map。紧随其后的 `b_additional_data`、响度、DRC、group gain、
//! associated audio、custom downmix 与 loudness correction 属于公共 metadata 后缀，
//! 由 [`Ac4PresentationSubstreamSelection::common_metadata_bit_offset`] 明确定界，但本阶段
//! 不解释，也不执行任何处理。

use crate::presentation::MAX_GROUPS_PER_PRESENTATION;
use crate::reader::{BitReader, ReadError};
use crate::substream::MAX_LF_SUBSTREAMS;
use core::fmt;

/// alternative presentation 可保存的 target 数上限。
///
/// 规范用 `variable_bits(2)` 扩展该计数而未给出上限。本 crate 不分配内存，并把
/// 单 presentation 的 target 数限制为与 substream index table 相同的 32。
pub const MAX_ALTERNATIVE_PRESENTATION_TARGETS: usize = 32;

/// 当前拓扑容量下，一个 presentation 按语法顺序可包含的音频 substream 数上限。
///
/// `n_substreams_in_presentation` 按 `ac4_sgi_specifier()` 的外层顺序和每个 group 的
/// `ac4_substream_group_info()` 内层顺序计数，包含 dialogue-enhancement substream；
/// 它不是去重后的物理 substream 数。
pub const MAX_AUDIO_SUBSTREAMS_PER_PRESENTATION: usize =
    MAX_GROUPS_PER_PRESENTATION * MAX_LF_SUBSTREAMS;

/// presentation selection 前缀中超出固定容量的结构。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresentationSubstreamCapacity {
    /// alternative target 数。
    Targets,
    /// 一个 presentation 中按语法顺序出现的音频 substream 数。
    AudioSubstreams,
}

impl fmt::Display for PresentationSubstreamCapacity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match *self {
            PresentationSubstreamCapacity::Targets => "alternative presentation targets",
            PresentationSubstreamCapacity::AudioSubstreams => "audio substreams in a presentation",
        })
    }
}

/// alternative presentation selection 前缀解析失败的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresentationSubstreamError {
    /// 底层读取失败或变长字段溢出。
    Read(ReadError),
    /// alternative presentation 没有可映射的音频 substream。
    ///
    /// 这通常表示调用方没有取得完整的 SGI/group 拓扑，不能把未知计数当成零继续解析。
    MissingAudioSubstreams,
    /// 结构规模超出固定容量。
    CapacityExceeded {
        /// 超限的结构种类。
        what: PresentationSubstreamCapacity,
        /// 码流或上下文声明的数量。
        declared: u32,
        /// 实现上限。
        limit: usize,
    },
}

impl fmt::Display for PresentationSubstreamError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            PresentationSubstreamError::Read(error) => {
                write!(
                    formatter,
                    "Failed to read presentation selection metadata: {error}"
                )
            }
            PresentationSubstreamError::MissingAudioSubstreams => formatter.write_str(
                "Alternative presentation selection requires at least one mapped audio substream",
            ),
            PresentationSubstreamError::CapacityExceeded {
                what,
                declared,
                limit,
            } => write!(
                formatter,
                "{what} count {declared} exceeds implementation limit {limit}"
            ),
        }
    }
}

impl core::error::Error for PresentationSubstreamError {}

impl From<ReadError> for PresentationSubstreamError {
    fn from(error: ReadError) -> Self {
        Self::Read(error)
    }
}

/// 解析 alternative selection 前缀所需的 TOC 上下文。
///
/// 应优先由
/// [`crate::topology::Ac4Topology::presentation_substream_selection_context`] 取得；构造
/// 测试也可直接使用 [`Self::new`]。substream 数按规范的 outer-loop/inner-loop 顺序
/// 计数，不按物理 index 去重。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PresentationSubstreamSelectionContext {
    alternative: bool,
    n_audio_substreams: u32,
}

impl PresentationSubstreamSelectionContext {
    /// 构造 selection 前缀上下文。
    #[must_use]
    pub const fn new(alternative: bool, n_audio_substreams: u32) -> Self {
        Self {
            alternative,
            n_audio_substreams,
        }
    }

    /// TOC 中的 `b_alternative`。
    #[must_use]
    pub const fn alternative(self) -> bool {
        self.alternative
    }

    /// presentation 内按规范顺序出现的音频 substream 数。
    #[must_use]
    pub const fn n_audio_substreams(self) -> u32 {
        self.n_audio_substreams
    }
}

/// presentation name 在当前帧中的分片形态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresentationNameChunkKind {
    /// 长度为零；码流没有提供可判读的终止或分片标记。
    Empty,
    /// 最后一个字节为零，当前分片就是完整名称。
    Complete,
    /// 名称仍会在后续 codec frame 中继续。
    Intermediate,
    /// 序列化名称的最后一块；末字节给出总分片数。
    Final {
        /// 完整名称包含的分片总数。
        total_chunks: u8,
    },
}

/// 可能不在字节边界上的 presentation name 原始 8 比特元素视图。
///
/// 名称可能跨 codec frame 分片，单个分片也可能切开 UTF-8 code point，因此本层不做
/// 有损 UTF-8 替换或跨帧拼接。
#[derive(Debug, Clone, Copy)]
pub struct PresentationNameBytes<'a> {
    source: &'a [u8],
    bit_offset: u64,
    len: u8,
}

impl<'a, 'b> PartialEq<PresentationNameBytes<'b>> for PresentationNameBytes<'a> {
    fn eq(&self, other: &PresentationNameBytes<'b>) -> bool {
        self.len == other.len && (*self).iter().eq((*other).iter())
    }
}

impl Eq for PresentationNameBytes<'_> {}

impl<'a> PresentationNameBytes<'a> {
    fn read(
        reader: &mut BitReader<'a>,
        source: &'a [u8],
        len: u8,
    ) -> Result<Self, PresentationSubstreamError> {
        let bit_offset = reader.bit_position();
        reader.skip_bits(u64::from(len).saturating_mul(8))?;
        Ok(Self {
            source,
            bit_offset,
            len,
        })
    }

    /// 原始 8 比特元素数量。
    #[must_use]
    pub const fn len(self) -> usize {
        self.len as usize
    }

    /// 是否为空分片。
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.len == 0
    }

    /// 读取一个原始 8 比特元素。
    #[must_use]
    pub fn get(self, index: usize) -> Option<u8> {
        if index >= self.len() {
            return None;
        }
        let relative = u64::try_from(index).ok()?.checked_mul(8)?;
        let mut reader = BitReader::new(self.source);
        reader
            .skip_bits(self.bit_offset.checked_add(relative)?)
            .ok()?;
        u8::try_from(reader.read_bits(8).ok()?).ok()
    }

    /// 若名称分片恰好位于字节边界，返回零拷贝切片。
    #[must_use]
    pub fn as_aligned_slice(self) -> Option<&'a [u8]> {
        if !self.bit_offset.is_multiple_of(8) {
            return None;
        }
        let start = usize::try_from(self.bit_offset / 8).ok()?;
        let end = start.checked_add(self.len())?;
        self.source.get(start..end)
    }

    /// 按码流顺序遍历原始 8 比特元素。
    #[must_use]
    pub fn iter(self) -> PresentationNameByteIter<'a> {
        PresentationNameByteIter::new(self)
    }

    /// 根据 `6.3.3.1.4` 判断当前名称分片的形态。
    #[must_use]
    pub fn chunk_kind(self) -> PresentationNameChunkKind {
        let Some(last_index) = self.len().checked_sub(1) else {
            return PresentationNameChunkKind::Empty;
        };
        let Some(last) = self.get(last_index) else {
            return PresentationNameChunkKind::Empty;
        };
        if last == 0 {
            return PresentationNameChunkKind::Complete;
        }
        if last_index > 0 && self.get(last_index.saturating_sub(1)) == Some(0) {
            return PresentationNameChunkKind::Final { total_chunks: last };
        }
        PresentationNameChunkKind::Intermediate
    }
}

impl<'a> IntoIterator for PresentationNameBytes<'a> {
    type Item = u8;
    type IntoIter = PresentationNameByteIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// [`PresentationNameBytes`] 的无分配迭代器。
#[derive(Debug, Clone)]
pub struct PresentationNameByteIter<'a> {
    reader: BitReader<'a>,
    remaining: usize,
}

impl<'a> PresentationNameByteIter<'a> {
    fn new(name: PresentationNameBytes<'a>) -> Self {
        let mut reader = BitReader::new(name.source);
        let remaining = if reader.skip_bits(name.bit_offset).is_ok() {
            name.len()
        } else {
            0
        };
        Self { reader, remaining }
    }
}

impl Iterator for PresentationNameByteIter<'_> {
    type Item = u8;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        let value = self
            .reader
            .read_bits(8)
            .ok()
            .and_then(|value| u8::try_from(value).ok());
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

impl ExactSizeIterator for PresentationNameByteIter<'_> {}

/// 表 67 的四个 target device category 位。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AlternativeTargetDeviceCategories(u8);

impl AlternativeTargetDeviceCategories {
    fn parse(reader: &mut BitReader<'_>) -> Result<Self, PresentationSubstreamError> {
        Ok(Self(u8::try_from(reader.read_bits(4)?).unwrap_or(0)))
    }

    /// 取得规范数组下标 `0..=3` 对应的类别位。
    #[must_use]
    pub const fn contains(self, index: u8) -> bool {
        if index >= 4 {
            return false;
        }
        let shift = 3u32.saturating_sub(index as u32);
        self.0 & (1u8 << shift) != 0
    }

    /// 四个传输位的原值；最高位对应规范数组下标 0。
    #[must_use]
    pub const fn raw(self) -> u8 {
        self.0
    }
}

/// 一个 target 对某个音频 substream 的 activation/dataset 选择。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AlternativeSubstreamActivation {
    /// 在 `n_substreams_in_presentation` 规范顺序中的零基下标。
    pub substream_index: u32,
    /// `b_active`。
    pub active: bool,
    /// active 时传输的 `alt_data_set_index`；零表示不使用 alternative dataset。
    pub alternative_data_set_index: Option<u32>,
}

/// alternative presentation 的一个播放 target。
#[derive(Debug, Clone, Copy)]
pub struct AlternativePresentationTarget<'a> {
    /// 3 比特 decoder compatibility target level。
    pub target_level: u8,
    /// 表 67 的四个 target device category 位。
    pub device_categories: AlternativeTargetDeviceCategories,
    /// `b_tdc_extension` 为真时的 4 比特保留扩展；否则为 `None`。
    pub device_category_extension: Option<u8>,
    /// 最大 ducking depth 的 6 比特码值。
    pub max_ducking_depth: Option<u8>,
    /// target-specific loudness correction 的 5 比特码值。
    pub loudness_correction_target: Option<u8>,
    source: &'a [u8],
    activations_bit_offset: u64,
    n_audio_substreams: u32,
}

impl<'a, 'b> PartialEq<AlternativePresentationTarget<'b>> for AlternativePresentationTarget<'a> {
    fn eq(&self, other: &AlternativePresentationTarget<'b>) -> bool {
        self.target_level == other.target_level
            && self.device_categories == other.device_categories
            && self.device_category_extension == other.device_category_extension
            && self.max_ducking_depth == other.max_ducking_depth
            && self.loudness_correction_target == other.loudness_correction_target
            && (*self)
                .substream_activations()
                .eq((*other).substream_activations())
    }
}

impl Eq for AlternativePresentationTarget<'_> {}

impl<'a> AlternativePresentationTarget<'a> {
    /// 按规范的 presentation substream 顺序遍历 activation/dataset map。
    #[must_use]
    pub fn substream_activations(self) -> AlternativeSubstreamActivationIter<'a> {
        AlternativeSubstreamActivationIter::new(
            self.source,
            self.activations_bit_offset,
            self.n_audio_substreams,
        )
    }
}

/// 单个 target 的 activation/dataset map 迭代器。
#[derive(Debug, Clone)]
pub struct AlternativeSubstreamActivationIter<'a> {
    reader: BitReader<'a>,
    remaining: u32,
    index: u32,
    failed: bool,
}

impl<'a> AlternativeSubstreamActivationIter<'a> {
    fn new(source: &'a [u8], bit_offset: u64, count: u32) -> Self {
        let mut reader = BitReader::new(source);
        let failed = reader.skip_bits(bit_offset).is_err();
        Self {
            reader,
            remaining: if failed { 0 } else { count },
            index: 0,
            failed,
        }
    }
}

impl Iterator for AlternativeSubstreamActivationIter<'_> {
    type Item = Result<AlternativeSubstreamActivation, PresentationSubstreamError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed || self.remaining == 0 {
            return None;
        }
        let parsed = parse_activation(&mut self.reader, self.index);
        match parsed {
            Ok(activation) => {
                self.remaining = self.remaining.saturating_sub(1);
                self.index = self.index.saturating_add(1);
                Some(Ok(activation))
            }
            Err(error) => {
                self.failed = true;
                self.remaining = 0;
                Some(Err(error))
            }
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = usize::try_from(self.remaining).unwrap_or(usize::MAX);
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for AlternativeSubstreamActivationIter<'_> {}

/// `b_alternative` 为真时携带的名称与 target 列表。
#[derive(Debug, Clone, Copy)]
pub struct AlternativePresentationSelection<'a> {
    /// 可选 presentation name 分片。
    pub name: Option<PresentationNameBytes<'a>>,
    /// 播放 target 数量。
    pub n_targets: u32,
    /// 每个 target 的 activation map 长度。
    pub n_audio_substreams: u32,
    source: &'a [u8],
    targets_bit_offset: u64,
}

impl<'a, 'b> PartialEq<AlternativePresentationSelection<'b>>
    for AlternativePresentationSelection<'a>
{
    fn eq(&self, other: &AlternativePresentationSelection<'b>) -> bool {
        let names_equal = match (self.name, other.name) {
            (None, None) => true,
            (Some(left), Some(right)) => left == right,
            _ => false,
        };
        if !names_equal
            || self.n_targets != other.n_targets
            || self.n_audio_substreams != other.n_audio_substreams
        {
            return false;
        }

        let mut left = (*self).targets();
        let mut right = (*other).targets();
        loop {
            match (left.next(), right.next()) {
                (None, None) => return true,
                (Some(Ok(left)), Some(Ok(right))) if left == right => {}
                (Some(Err(left)), Some(Err(right))) if left == right => {}
                _ => return false,
            }
        }
    }
}

impl Eq for AlternativePresentationSelection<'_> {}

impl<'a> AlternativePresentationSelection<'a> {
    /// 按码流顺序遍历播放 target。
    #[must_use]
    pub fn targets(self) -> AlternativePresentationTargetIter<'a> {
        AlternativePresentationTargetIter::new(
            self.source,
            self.targets_bit_offset,
            self.n_targets,
            self.n_audio_substreams,
        )
    }
}

/// alternative presentation target 迭代器。
#[derive(Debug, Clone)]
pub struct AlternativePresentationTargetIter<'a> {
    reader: BitReader<'a>,
    source: &'a [u8],
    remaining: u32,
    n_audio_substreams: u32,
    failed: bool,
}

impl<'a> AlternativePresentationTargetIter<'a> {
    fn new(source: &'a [u8], bit_offset: u64, count: u32, n_audio_substreams: u32) -> Self {
        let mut reader = BitReader::new(source);
        let failed = reader.skip_bits(bit_offset).is_err();
        Self {
            reader,
            source,
            remaining: if failed { 0 } else { count },
            n_audio_substreams,
            failed,
        }
    }
}

impl<'a> Iterator for AlternativePresentationTargetIter<'a> {
    type Item = Result<AlternativePresentationTarget<'a>, PresentationSubstreamError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed || self.remaining == 0 {
            return None;
        }
        let parsed = parse_target(&mut self.reader, self.source, self.n_audio_substreams);
        match parsed {
            Ok(target) => {
                self.remaining = self.remaining.saturating_sub(1);
                Some(Ok(target))
            }
            Err(error) => {
                self.failed = true;
                self.remaining = 0;
                Some(Err(error))
            }
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = usize::try_from(self.remaining).unwrap_or(usize::MAX);
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for AlternativePresentationTargetIter<'_> {}

/// `ac4_presentation_substream()` 的 selection 前缀。
///
/// 普通 presentation 没有该前缀，`alternative` 为 `None` 且公共 metadata 从 bit 0
/// 开始。alternative presentation 完整验证名称、target 与 activation/dataset map 后，
/// `common_metadata_bit_offset` 指向紧随其后的 `b_additional_data`。
#[derive(Debug, Clone, Copy)]
pub struct Ac4PresentationSubstreamSelection<'a> {
    /// alternative presentation 的选择信令；普通 presentation 为 `None`。
    pub alternative: Option<AlternativePresentationSelection<'a>>,
    /// 公共 presentation metadata 后缀在该 substream payload 内的比特偏移。
    pub common_metadata_bit_offset: u64,
}

impl<'a, 'b> PartialEq<Ac4PresentationSubstreamSelection<'b>>
    for Ac4PresentationSubstreamSelection<'a>
{
    fn eq(&self, other: &Ac4PresentationSubstreamSelection<'b>) -> bool {
        if self.common_metadata_bit_offset != other.common_metadata_bit_offset {
            return false;
        }
        match (self.alternative, other.alternative) {
            (None, None) => true,
            (Some(left), Some(right)) => left == right,
            _ => false,
        }
    }
}

impl Eq for Ac4PresentationSubstreamSelection<'_> {}

impl<'a> Ac4PresentationSubstreamSelection<'a> {
    /// 解析 `b_alternative` 控制的 selection 前缀。
    ///
    /// `payload` 必须恰好是 TOC 中 presentation substream index 对应的有界 payload。
    /// 本函数不解析或验证 `common_metadata_bit_offset` 之后的公共 metadata 后缀。
    ///
    /// # Errors
    ///
    /// 名称、target 或 activation map 截断，变长字段溢出，或计数超过固定容量时
    /// 返回错误。
    pub fn parse(
        payload: &'a [u8],
        context: PresentationSubstreamSelectionContext,
    ) -> Result<Self, PresentationSubstreamError> {
        if context.alternative {
            if context.n_audio_substreams == 0 {
                return Err(PresentationSubstreamError::MissingAudioSubstreams);
            }
            let limit = u32::try_from(MAX_AUDIO_SUBSTREAMS_PER_PRESENTATION).unwrap_or(u32::MAX);
            if context.n_audio_substreams > limit {
                return Err(PresentationSubstreamError::CapacityExceeded {
                    what: PresentationSubstreamCapacity::AudioSubstreams,
                    declared: context.n_audio_substreams,
                    limit: MAX_AUDIO_SUBSTREAMS_PER_PRESENTATION,
                });
            }
        }

        let mut reader = BitReader::new(payload);
        let alternative = if context.alternative {
            Some(parse_alternative(
                &mut reader,
                payload,
                context.n_audio_substreams,
            )?)
        } else {
            None
        };

        Ok(Self {
            alternative,
            common_metadata_bit_offset: reader.bit_position(),
        })
    }
}

fn parse_alternative<'a>(
    reader: &mut BitReader<'a>,
    source: &'a [u8],
    n_audio_substreams: u32,
) -> Result<AlternativePresentationSelection<'a>, PresentationSubstreamError> {
    let name = if reader.read_flag()? {
        let len = if reader.read_flag()? {
            u8::try_from(reader.read_bits(5)?).unwrap_or(0)
        } else {
            32
        };
        Some(PresentationNameBytes::read(reader, source, len)?)
    } else {
        None
    };

    let mut n_targets = u32::try_from(reader.read_bits(2)?)
        .unwrap_or(0)
        .saturating_add(1);
    if n_targets == 4 {
        n_targets = reader.variable_bits_scaled_u32(2, n_targets, 0)?;
    }
    if n_targets > u32::try_from(MAX_ALTERNATIVE_PRESENTATION_TARGETS).unwrap_or(u32::MAX) {
        return Err(PresentationSubstreamError::CapacityExceeded {
            what: PresentationSubstreamCapacity::Targets,
            declared: n_targets,
            limit: MAX_ALTERNATIVE_PRESENTATION_TARGETS,
        });
    }

    let targets_bit_offset = reader.bit_position();
    for _ in 0..n_targets {
        parse_target(reader, source, n_audio_substreams)?;
    }

    Ok(AlternativePresentationSelection {
        name,
        n_targets,
        n_audio_substreams,
        source,
        targets_bit_offset,
    })
}

fn parse_target<'a>(
    reader: &mut BitReader<'a>,
    source: &'a [u8],
    n_audio_substreams: u32,
) -> Result<AlternativePresentationTarget<'a>, PresentationSubstreamError> {
    let target_level = u8::try_from(reader.read_bits(3)?).unwrap_or(0);
    let device_categories = AlternativeTargetDeviceCategories::parse(reader)?;
    let device_category_extension = if reader.read_flag()? {
        Some(u8::try_from(reader.read_bits(4)?).unwrap_or(0))
    } else {
        None
    };
    let max_ducking_depth = if reader.read_flag()? {
        Some(u8::try_from(reader.read_bits(6)?).unwrap_or(0))
    } else {
        None
    };
    let loudness_correction_target = if reader.read_flag()? {
        Some(u8::try_from(reader.read_bits(5)?).unwrap_or(0))
    } else {
        None
    };

    let activations_bit_offset = reader.bit_position();
    for index in 0..n_audio_substreams {
        parse_activation(reader, index)?;
    }

    Ok(AlternativePresentationTarget {
        target_level,
        device_categories,
        device_category_extension,
        max_ducking_depth,
        loudness_correction_target,
        source,
        activations_bit_offset,
        n_audio_substreams,
    })
}

fn parse_activation(
    reader: &mut BitReader<'_>,
    substream_index: u32,
) -> Result<AlternativeSubstreamActivation, PresentationSubstreamError> {
    let active = reader.read_flag()?;
    let alternative_data_set_index = if active {
        let first = reader.read_flag()?;
        Some(if first {
            reader.variable_bits_scaled_u32(2, 1, 0)?
        } else {
            0
        })
    } else {
        None
    };
    Ok(AlternativeSubstreamActivation {
        substream_index,
        active,
        alternative_data_set_index,
    })
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;

    #[derive(Debug, Clone)]
    struct TestBits {
        bytes: [u8; 256],
        len: usize,
    }

    impl TestBits {
        const fn new() -> Self {
            Self {
                bytes: [0; 256],
                len: 0,
            }
        }

        #[expect(
            clippy::arithmetic_side_effects,
            clippy::indexing_slicing,
            reason = "测试 bit writer 的固定缓冲和宽度由用例控制"
        )]
        fn push(&mut self, value: u64, width: u32) {
            for shift in (0..width).rev() {
                if value & (1 << shift) != 0 {
                    self.bytes[self.len / 8] |= 1 << (7 - self.len % 8);
                }
                self.len += 1;
            }
        }

        fn push_byte(&mut self, value: u8) {
            self.push(u64::from(value), 8);
        }

        fn as_bytes(&self) -> &[u8] {
            self.bytes
                .get(..self.len.div_ceil(8))
                .unwrap_or(&self.bytes)
        }
    }

    fn push_minimal_target(bits: &mut TestBits, n_audio_substreams: u32) {
        bits.push(0, 3); // target_level
        bits.push(0, 4); // target_device_category[]
        bits.push(0, 1); // no extension
        bits.push(0, 1); // no ducking depth
        bits.push(0, 1); // no target loudness correction
        for _ in 0..n_audio_substreams {
            bits.push(0, 1); // inactive
        }
    }

    #[test]
    fn ordinary_presentation_has_no_selection_prefix() {
        let parsed = Ac4PresentationSubstreamSelection::parse(
            &[],
            PresentationSubstreamSelectionContext::new(false, 64),
        )
        .unwrap();

        assert_eq!(parsed.alternative, None);
        assert_eq!(parsed.common_metadata_bit_offset, 0);
    }

    #[test]
    fn equality_ignores_unparsed_common_metadata_suffix() {
        let mut prefix = TestBits::new();
        prefix.push(1, 1); // name present
        prefix.push(1, 1); // explicit length
        prefix.push(1, 5);
        prefix.push_byte(b'A');
        prefix.push(0, 2); // one target
        push_minimal_target(&mut prefix, 1);
        let expected_offset = prefix.len as u64;

        let mut left_bits = prefix.clone();
        left_bits.push(0, 4);
        let mut right_bits = prefix;
        right_bits.push(0b1111, 4);

        let context = PresentationSubstreamSelectionContext::new(true, 1);
        let left = Ac4PresentationSubstreamSelection::parse(left_bits.as_bytes(), context).unwrap();
        let right =
            Ac4PresentationSubstreamSelection::parse(right_bits.as_bytes(), context).unwrap();
        assert_eq!(left.common_metadata_bit_offset, expected_offset);
        assert_eq!(right.common_metadata_bit_offset, expected_offset);

        let left_alternative = left.alternative.unwrap();
        let right_alternative = right.alternative.unwrap();
        assert_eq!(left_alternative.name, right_alternative.name);
        assert_eq!(left_alternative, right_alternative);
        assert_eq!(
            left_alternative.targets().next().unwrap().unwrap(),
            right_alternative.targets().next().unwrap().unwrap()
        );
        assert_eq!(left, right);
    }

    #[test]
    fn parses_target_fields_and_dataset_map() {
        let mut bits = TestBits::new();
        bits.push(0, 1); // no name
        bits.push(0, 2); // one target
        bits.push(5, 3); // target_level
        bits.push(0b1010, 4); // device categories 0 and 2
        bits.push(1, 1); // extension present
        bits.push(0b1100, 4);
        bits.push(1, 1); // ducking present
        bits.push(33, 6);
        bits.push(1, 1); // loudness correction present
        bits.push(31, 5);
        bits.push(0, 1); // substream 0 inactive
        bits.push(1, 1); // substream 1 active
        bits.push(0, 1); // data set index 0
        bits.push(1, 1); // substream 2 active
        bits.push(1, 1); // extended data set index
        // variable_bits(2) = 6: (00, more), (10, stop); final index = 1 + 6 = 7
        bits.push(0, 2);
        bits.push(1, 1);
        bits.push(2, 2);
        bits.push(0, 1);
        let expected_offset = bits.len as u64;
        bits.push(0b10101, 5); // 尚未解析的公共 metadata 后缀

        let parsed = Ac4PresentationSubstreamSelection::parse(
            bits.as_bytes(),
            PresentationSubstreamSelectionContext::new(true, 3),
        )
        .unwrap();
        assert_eq!(parsed.common_metadata_bit_offset, expected_offset);

        let alternative = parsed.alternative.unwrap();
        assert_eq!(alternative.n_targets, 1);
        assert_eq!(alternative.n_audio_substreams, 3);
        assert_eq!(alternative.name, None);

        let target = alternative.targets().next().unwrap().unwrap();
        assert_eq!(target.target_level, 5);
        assert_eq!(target.device_categories.raw(), 0b1010);
        assert!(target.device_categories.contains(0));
        assert!(!target.device_categories.contains(1));
        assert!(target.device_categories.contains(2));
        assert!(!target.device_categories.contains(4));
        assert_eq!(target.device_category_extension, Some(0b1100));
        assert_eq!(target.max_ducking_depth, Some(33));
        assert_eq!(target.loudness_correction_target, Some(31));

        let mut activations = target.substream_activations();
        assert_eq!(
            activations.next().unwrap().unwrap(),
            AlternativeSubstreamActivation {
                substream_index: 0,
                active: false,
                alternative_data_set_index: None,
            }
        );
        assert_eq!(
            activations.next().unwrap().unwrap(),
            AlternativeSubstreamActivation {
                substream_index: 1,
                active: true,
                alternative_data_set_index: Some(0),
            }
        );
        assert_eq!(
            activations.next().unwrap().unwrap(),
            AlternativeSubstreamActivation {
                substream_index: 2,
                active: true,
                alternative_data_set_index: Some(7),
            }
        );
        assert!(activations.next().is_none());
    }

    #[test]
    fn preserves_unaligned_serialized_name_chunk() {
        let mut bits = TestBits::new();
        bits.push(1, 1); // name present
        bits.push(1, 1); // explicit length
        bits.push(3, 5);
        bits.push_byte(b'A');
        bits.push_byte(0);
        bits.push_byte(2); // final chunk, two chunks total
        bits.push(0, 2); // one target
        push_minimal_target(&mut bits, 1);

        let parsed = Ac4PresentationSubstreamSelection::parse(
            bits.as_bytes(),
            PresentationSubstreamSelectionContext::new(true, 1),
        )
        .unwrap();
        let name = parsed.alternative.unwrap().name.unwrap();

        assert_eq!(name.len(), 3);
        assert_eq!(name.iter().collect::<std::vec::Vec<_>>(), [b'A', 0, 2]);
        assert_eq!(name.as_aligned_slice(), None, "名称从 bit 7 开始");
        assert_eq!(
            name.chunk_kind(),
            PresentationNameChunkKind::Final { total_chunks: 2 }
        );
    }

    #[test]
    fn parses_extended_target_count() {
        let mut bits = TestBits::new();
        bits.push(0, 1); // no name
        bits.push(3, 2); // four targets before extension
        bits.push(2, 2); // variable_bits(2) = 2
        bits.push(0, 1);
        for _ in 0..6 {
            push_minimal_target(&mut bits, 1);
        }

        let parsed = Ac4PresentationSubstreamSelection::parse(
            bits.as_bytes(),
            PresentationSubstreamSelectionContext::new(true, 1),
        )
        .unwrap();
        let alternative = parsed.alternative.unwrap();
        assert_eq!(alternative.n_targets, 6);
        assert_eq!(alternative.targets().count(), 6);
    }

    #[test]
    fn accepts_fixed_name_and_target_capacity_boundaries() {
        let mut bits = TestBits::new();
        bits.push(1, 1); // name present
        bits.push(0, 1); // fixed 32-byte form
        for _ in 0..31 {
            bits.push_byte(b'N');
        }
        bits.push_byte(0); // complete name terminator
        bits.push(3, 2); // four targets before extension
        // variable_bits(2) = 28: 00/more, 10/more, 00/stop; total = 32
        bits.push(0, 2);
        bits.push(1, 1);
        bits.push(2, 2);
        bits.push(1, 1);
        bits.push(0, 2);
        bits.push(0, 1);
        for _ in 0..MAX_ALTERNATIVE_PRESENTATION_TARGETS {
            push_minimal_target(&mut bits, 1);
        }

        let parsed = Ac4PresentationSubstreamSelection::parse(
            bits.as_bytes(),
            PresentationSubstreamSelectionContext::new(true, 1),
        )
        .unwrap();
        let alternative = parsed.alternative.unwrap();
        let name = alternative.name.unwrap();
        assert_eq!(name.len(), 32);
        assert_eq!(name.get(0), Some(b'N'));
        assert_eq!(name.get(31), Some(0));
        assert_eq!(name.chunk_kind(), PresentationNameChunkKind::Complete);
        assert_eq!(alternative.n_targets, 32);
        assert_eq!(alternative.targets().count(), 32);
    }

    #[test]
    fn preserves_zero_length_name_chunk() {
        let mut bits = TestBits::new();
        bits.push(1, 1); // name present
        bits.push(1, 1); // explicit length
        bits.push(0, 5);
        bits.push(0, 2); // one target
        push_minimal_target(&mut bits, 1);

        let parsed = Ac4PresentationSubstreamSelection::parse(
            bits.as_bytes(),
            PresentationSubstreamSelectionContext::new(true, 1),
        )
        .unwrap();
        let name = parsed.alternative.unwrap().name.unwrap();
        assert!(name.is_empty());
        assert_eq!(name.chunk_kind(), PresentationNameChunkKind::Empty);
    }

    #[test]
    fn rejects_target_count_above_capacity_before_target_loop() {
        let mut bits = TestBits::new();
        bits.push(0, 1); // no name
        bits.push(3, 2); // base 4
        // variable_bits(2) = 29: 00/more, 10/more, 01/stop; total = 33
        bits.push(0, 2);
        bits.push(1, 1);
        bits.push(2, 2);
        bits.push(1, 1);
        bits.push(1, 2);
        bits.push(0, 1);

        assert_eq!(
            Ac4PresentationSubstreamSelection::parse(
                bits.as_bytes(),
                PresentationSubstreamSelectionContext::new(true, 1),
            )
            .unwrap_err(),
            PresentationSubstreamError::CapacityExceeded {
                what: PresentationSubstreamCapacity::Targets,
                declared: 33,
                limit: MAX_ALTERNATIVE_PRESENTATION_TARGETS,
            }
        );
    }

    #[test]
    fn rejects_target_count_extension_overflow() {
        let mut bits = TestBits::new();
        bits.push(0, 1); // no name
        bits.push(3, 2); // extended target count
        for _ in 0..40 {
            bits.push(3, 2);
            bits.push(1, 1);
        }

        assert!(matches!(
            Ac4PresentationSubstreamSelection::parse(
                bits.as_bytes(),
                PresentationSubstreamSelectionContext::new(true, 1),
            ),
            Err(PresentationSubstreamError::Read(
                ReadError::ValueOverflow { .. }
            ))
        ));
    }

    #[test]
    fn rejects_context_beyond_topology_capacity_without_reading_payload() {
        assert_eq!(
            Ac4PresentationSubstreamSelection::parse(
                &[],
                PresentationSubstreamSelectionContext::new(true, 65),
            )
            .unwrap_err(),
            PresentationSubstreamError::CapacityExceeded {
                what: PresentationSubstreamCapacity::AudioSubstreams,
                declared: 65,
                limit: MAX_AUDIO_SUBSTREAMS_PER_PRESENTATION,
            }
        );
    }

    #[test]
    fn refuses_to_treat_unknown_audio_substream_count_as_zero() {
        assert_eq!(
            Ac4PresentationSubstreamSelection::parse(
                &[],
                PresentationSubstreamSelectionContext::new(true, 0),
            )
            .unwrap_err(),
            PresentationSubstreamError::MissingAudioSubstreams
        );
    }

    #[test]
    fn rejects_truncated_name_and_activation_map() {
        let mut name = TestBits::new();
        name.push(1, 1);
        name.push(1, 1);
        name.push(2, 5); // declares two bytes
        name.push_byte(b'A'); // only one byte is present
        assert!(matches!(
            Ac4PresentationSubstreamSelection::parse(
                name.as_bytes(),
                PresentationSubstreamSelectionContext::new(true, 1),
            ),
            Err(PresentationSubstreamError::Read(
                ReadError::OutOfBounds { .. }
            ))
        ));

        let mut activation = TestBits::new();
        activation.push(0, 1); // no name
        activation.push(0, 2); // one target
        push_minimal_target(&mut activation, 0); // no activation bits available
        assert!(matches!(
            Ac4PresentationSubstreamSelection::parse(
                activation.as_bytes(),
                PresentationSubstreamSelectionContext::new(true, 7),
            ),
            Err(PresentationSubstreamError::Read(
                ReadError::OutOfBounds { .. }
            ))
        ));
    }
}
