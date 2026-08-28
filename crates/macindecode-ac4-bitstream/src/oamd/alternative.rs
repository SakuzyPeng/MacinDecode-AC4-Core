//! `oamd_dyndata_single()` 与 alternative object-property datasets。

use super::*;
use crate::reader::{BitReader, ReadError};

/// 一个已完整验证的 `oamd_dyndata_single()`。
///
/// 逐对象 block 与 alternative dataset 保持在原 payload 中；本结构只保存对象布局、
/// 数量和精确 bit range。这样普通音频 substream 不会为规范最坏情况下的全部更新固定
/// 携带大数组，调用方仍可通过迭代器读取每一个原始候选。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OamdDyndataSingle {
    objects: ObjectDescriptors,
    num_obj_info_blocks: u8,
    b_iframe: bool,
    blocks_bit_offset: u64,
    blocks_bit_len: u64,
    alternative: Option<OamdAlternativeData>,
    end_bit_offset: u64,
}

impl OamdDyndataSingle {
    /// 解析 `oamd_dyndata_single()`，见 P2 `6.2.8.3`。
    ///
    /// `objects` 必须按该物理 audio substream 中 object essence 的顺序排列。
    /// 解析会完整验证所有 block、dataset 与 additional-data envelope，但不选择或应用
    /// 任一 alternative dataset。
    ///
    /// # Errors
    ///
    /// 读取越界、对象/块容量超限、I-frame 块数为零或 additional data 不自洽时返回
    /// [`OamdError`]。
    pub fn parse(
        reader: &mut BitReader<'_>,
        objects: &[ObjectDescriptor],
        num_obj_info_blocks: u8,
        b_iframe: bool,
        b_alternative: bool,
    ) -> Result<Self, OamdError> {
        Self::parse_with_block_observer(
            reader,
            objects,
            num_obj_info_blocks,
            b_iframe,
            b_alternative,
            |_| {},
        )
    }

    /// 与 [`Self::parse`] 相同，但把已验证的逐对象 block 同步交给 crate 内调用方。
    ///
    /// `audio_data_ajoc()` 需要在一次解析中既保留 alternative dataset 边界，又填充
    /// 既有 OAMD 状态工作区；observer 避免为此重复读取完整元素。
    pub(crate) fn parse_with_block_observer(
        reader: &mut BitReader<'_>,
        objects: &[ObjectDescriptor],
        num_obj_info_blocks: u8,
        b_iframe: bool,
        b_alternative: bool,
        mut observe_block: impl FnMut(OamdMetadataBlock),
    ) -> Result<Self, OamdError> {
        if objects.len() > MAX_OAMD_OBJECTS {
            return Err(OamdError::TooManyObjects {
                limit: MAX_OAMD_OBJECTS,
            });
        }
        if usize::from(num_obj_info_blocks) > MAX_OBJ_INFO_BLOCKS {
            return Err(OamdError::TooManyBlocks {
                declared: u32::from(num_obj_info_blocks),
            });
        }
        if b_iframe && num_obj_info_blocks == 0 {
            return Err(OamdError::ZeroBlocksInIframe);
        }

        let objects = ObjectDescriptors::try_from_slice(objects)?;
        let blocks_bit_offset = reader.bit_position();
        for (object_index, object) in objects.as_slice().iter().enumerate() {
            for block in 0..num_obj_info_blocks {
                let b_no_delta = b_iframe && block == 0;
                let info = ObjectInfoBlock::parse(reader, b_no_delta, object.is_dynamic_object())?;
                observe_block(OamdMetadataBlock {
                    object_index: u8::try_from(object_index).unwrap_or(u8::MAX),
                    block_index: block,
                    info,
                });
            }
        }
        let blocks_bit_len = reader.bit_position().saturating_sub(blocks_bit_offset);

        let alternative = if b_alternative {
            let b_ducking_disabled = reader.read_flag()?;
            let category_base = u32::try_from(reader.read_bits(2)?).unwrap_or(u32::MAX);
            let object_sound_category = if category_base == 3 {
                reader.variable_bits_scaled_u32(2, category_base, 0)?
            } else {
                category_base
            };
            let count_base = u32::try_from(reader.read_bits(2)?).unwrap_or(u32::MAX);
            let n_data_sets = if count_base == 3 {
                reader.variable_bits_scaled_u32(2, count_base, 0)?
            } else {
                count_base
            };
            let data_sets_bit_offset = reader.bit_position();
            for index in 0..n_data_sets {
                parse_alternative_data_set(reader, &objects, index)?;
            }
            Some(OamdAlternativeData {
                b_ducking_disabled,
                object_sound_category,
                n_data_sets,
                data_sets_bit_offset,
                data_sets_bit_len: reader.bit_position().saturating_sub(data_sets_bit_offset),
            })
        } else {
            None
        };

        Ok(Self {
            objects,
            num_obj_info_blocks,
            b_iframe,
            blocks_bit_offset,
            blocks_bit_len,
            alternative,
            end_bit_offset: reader.bit_position(),
        })
    }

    /// 本元素描述的对象；顺序与物理 substream 中的 object essence 一致。
    #[must_use]
    pub fn objects(&self) -> &[ObjectDescriptor] {
        self.objects.as_slice()
    }

    /// 每个对象在本帧携带的 `object_info_block()` 数量。
    #[must_use]
    pub const fn num_obj_info_blocks(self) -> u8 {
        self.num_obj_info_blocks
    }

    /// 传入本元素的 `b_iframe`。
    #[must_use]
    pub const fn b_iframe(self) -> bool {
        self.b_iframe
    }

    /// 全部对象合计的 metadata block 数。
    #[must_use]
    pub fn metadata_block_count(&self) -> usize {
        self.objects
            .as_slice()
            .len()
            .saturating_mul(usize::from(self.num_obj_info_blocks))
    }

    /// `oamd_dyndata_single()` 结束位置相对原 payload 的 bit offset。
    #[must_use]
    pub const fn end_bit_offset(self) -> u64 {
        self.end_bit_offset
    }

    /// `b_alternative` 为真时的 dataset header；普通路径为 `None`。
    #[must_use]
    pub const fn alternative(self) -> Option<OamdAlternativeData> {
        self.alternative
    }

    /// 按对象在外、块在内的码流顺序遍历原始 metadata block。
    ///
    /// `payload` 必须是解析本结构时使用的同一切片。
    pub fn metadata_blocks<'a>(
        self,
        payload: &'a [u8],
    ) -> Result<OamdDyndataSingleBlockIter<'a>, OamdError> {
        Ok(OamdDyndataSingleBlockIter {
            reader: BitReader::new_bounded(payload, self.blocks_bit_offset, self.blocks_bit_len)?,
            objects: self.objects,
            num_obj_info_blocks: self.num_obj_info_blocks,
            b_iframe: self.b_iframe,
            next_block: 0,
            remaining: self.metadata_block_count(),
            failed: false,
        })
    }

    /// 按传输顺序遍历全部 alternative datasets。
    ///
    /// 返回 `None` 只表示解析时 `b_alternative == 0`；dataset 数为零时返回一个空迭代器。
    /// `payload` 必须是解析本结构时使用的同一切片。
    pub fn alternative_data_sets<'a>(
        self,
        payload: &'a [u8],
    ) -> Result<Option<OamdAlternativeDataSetIter<'a>>, OamdError> {
        let Some(alternative) = self.alternative else {
            return Ok(None);
        };
        Ok(Some(OamdAlternativeDataSetIter {
            reader: BitReader::new_bounded(
                payload,
                alternative.data_sets_bit_offset,
                alternative.data_sets_bit_len,
            )?,
            source: payload,
            objects: self.objects,
            b_iframe: self.b_iframe,
            next_index: 0,
            remaining: alternative.n_data_sets,
            failed: false,
        }))
    }
}

/// `oamd_dyndata_single()` 中 alternative dataset 列表的公共 header。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OamdAlternativeData {
    /// `b_ducking_disabled` 原值。
    pub b_ducking_disabled: bool,
    /// 含 `variable_bits(2)` 扩展后的 `object_sound_category` 原值。
    pub object_sound_category: u32,
    /// 含 `variable_bits(2)` 扩展后的 dataset 数量。
    pub n_data_sets: u32,
    data_sets_bit_offset: u64,
    data_sets_bit_len: u64,
}

/// `oamd_dyndata_single()` 的逐对象 block 迭代器。
#[derive(Debug, Clone)]
pub struct OamdDyndataSingleBlockIter<'a> {
    reader: BitReader<'a>,
    objects: ObjectDescriptors,
    num_obj_info_blocks: u8,
    b_iframe: bool,
    next_block: usize,
    remaining: usize,
    failed: bool,
}

impl Iterator for OamdDyndataSingleBlockIter<'_> {
    type Item = Result<OamdMetadataBlock, OamdError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed || self.remaining == 0 {
            return None;
        }
        let blocks = usize::from(self.num_obj_info_blocks);
        if blocks == 0 {
            self.remaining = 0;
            return None;
        }
        let object_index = self.next_block.checked_div(blocks).unwrap_or(usize::MAX);
        let block_index = self.next_block.checked_rem(blocks).unwrap_or(usize::MAX);
        let Some(object) = self.objects.as_slice().get(object_index).copied() else {
            self.failed = true;
            self.remaining = 0;
            return Some(Err(OamdError::AlternativeDataWithoutObjects {
                data_sets: 0,
                bit_position: self.reader.bit_position(),
            }));
        };
        let parsed = ObjectInfoBlock::parse(
            &mut self.reader,
            self.b_iframe && block_index == 0,
            object.is_dynamic_object(),
        );
        match parsed {
            Ok(info) => {
                self.next_block = self.next_block.saturating_add(1);
                self.remaining = self.remaining.saturating_sub(1);
                Some(Ok(OamdMetadataBlock {
                    object_index: u8::try_from(object_index).unwrap_or(u8::MAX),
                    block_index: u8::try_from(block_index).unwrap_or(u8::MAX),
                    info,
                }))
            }
            Err(error) => {
                self.failed = true;
                self.remaining = 0;
                Some(Err(error))
            }
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (0, Some(self.remaining))
    }
}

/// alternative dataset 中一个数据点的作用范围。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OamdAlternativeDataPointTarget {
    /// 该数据点适用于 dataset 中的全部对象。
    AllObjects,
    /// 该数据点只适用于给定对象下标。
    Object(u8),
}

/// alternative dataset 中一个 gain/position 数据点。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OamdAlternativeDataPoint {
    /// 数据点在本 dataset 内的顺序下标。
    pub data_point_index: u8,
    /// 公共数据点或逐对象数据点。
    pub target: OamdAlternativeDataPointTarget,
    /// 语法 gate 使用的对象描述；公共数据点取对象 0 的描述。
    pub descriptor: ObjectDescriptor,
    /// `b_alt_gain == 1` 时的 6 比特增益码；否则为 `None`。
    pub alternative_gain: Option<u8>,
    /// `b_alt_position == 1` 时的标准精度绝对位置；否则为 `None`。
    pub alternative_position: Option<AbsolutePosition>,
}

/// alternative dataset additional data 中一个动态对象的扩展精度位置更新。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OamdAlternativeExtendedPosition {
    /// 对象在当前 audio substream 中的下标。
    pub object_index: u8,
    /// `b_ext_prec_alt_pos == 1` 时的 `ext_prec_pos()`；否则为 `None`。
    pub position: Option<ExtendedPrecisionPosition>,
}

const EMPTY_ALTERNATIVE_DATA_POINT: OamdAlternativeDataPoint = OamdAlternativeDataPoint {
    data_point_index: 0,
    target: OamdAlternativeDataPointTarget::Object(0),
    descriptor: ObjectDescriptor {
        obj_type: ObjectType::Dynamic,
        b_lfe: false,
        b_ajoc_coded: false,
    },
    alternative_gain: None,
    alternative_position: None,
};

const EMPTY_ALTERNATIVE_EXTENDED_POSITION: OamdAlternativeExtendedPosition =
    OamdAlternativeExtendedPosition {
        object_index: 0,
        position: None,
    };

/// 一次非 keep alternative dataset 中当前规范已定义的 object-property 原值。
///
/// 本快照拥有 gain、标准精度位置与扩展精度位置，可跨 payload 生命周期保留并由
/// [`OamdAlternativeDataSetState`] 在 `b_keep` 帧复用。additional-data 中规范未定义的
/// opaque `skip_data` 仍只由 [`OamdAlternativeDataSet::opaque_additional_data`] 原样暴露；
/// 本类型不猜测未来扩展是否属于 `b_keep` 的 object-property update。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OamdAlternativePropertySnapshot {
    /// dataset 在 `n_alt_data_sets` 循环内的零基下标。
    pub data_set_index: u32,
    /// 非 ISF 隐式公共分支传输的 `b_common_data`。
    pub common_data: Option<bool>,
    data_points: [OamdAlternativeDataPoint; MAX_OAMD_OBJECTS],
    data_points_len: usize,
    extended_positions: [OamdAlternativeExtendedPosition; MAX_OAMD_OBJECTS],
    extended_positions_len: usize,
}

impl OamdAlternativePropertySnapshot {
    /// 当前有效的 gain/标准精度位置数据点，保持码流顺序与作用范围。
    #[must_use]
    pub fn data_points(&self) -> &[OamdAlternativeDataPoint] {
        self.data_points.get(..self.data_points_len).unwrap_or(&[])
    }

    /// 当前有效的扩展精度位置原值，保持对象顺序。
    #[must_use]
    pub fn extended_positions(&self) -> &[OamdAlternativeExtendedPosition] {
        self.extended_positions
            .get(..self.extended_positions_len)
            .unwrap_or(&[])
    }
}

/// 延续一个 alternative dataset 的已定义 object-property update 时的状态错误。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OamdAlternativeDataSetStateError {
    /// 回取已验证 bit range 时发生底层 OAMD 错误。
    Oamd(OamdError),
    /// 同一状态实例收到了另一个 dataset loop index。
    DataSetIndexChanged {
        /// 状态已经绑定的零基下标。
        previous: u32,
        /// 当前更新的零基下标。
        current: u32,
    },
    /// dependent frame 未经 reset 改变了本物理 substream 的对象布局。
    ObjectLayoutChanged {
        /// 发生布局变化的 dataset 零基下标。
        data_set_index: u32,
    },
    /// `b_keep == 1`，但当前连续解码区间没有上一份 object-property update。
    HistoryUnavailable {
        /// 缺少历史的 dataset 零基下标。
        data_set_index: u32,
    },
}

impl fmt::Display for OamdAlternativeDataSetStateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Oamd(error) => write!(
                formatter,
                "Failed to recover an alternative OAMD property update: {error}"
            ),
            Self::DataSetIndexChanged { previous, current } => write!(
                formatter,
                "Alternative OAMD state is bound to dataset {previous}, but received dataset {current}; isolate state by physical substream and dataset index"
            ),
            Self::ObjectLayoutChanged { data_set_index } => write!(
                formatter,
                "Dependent alternative OAMD dataset {data_set_index} changed object layout; reset state at the topology boundary"
            ),
            Self::HistoryUnavailable { data_set_index } => write!(
                formatter,
                "Alternative OAMD dataset {data_set_index} uses b_keep without a previous object-property update"
            ),
        }
    }
}

impl core::error::Error for OamdAlternativeDataSetStateError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Oamd(error) => Some(error),
            _ => None,
        }
    }
}

impl From<OamdError> for OamdAlternativeDataSetStateError {
    fn from(error: OamdError) -> Self {
        Self::Oamd(error)
    }
}

/// 一个 alternative dataset loop index 的当前有效 object-property 状态。
///
/// 状态实例必须按 `(physical audio substream, data_set_index)` 隔离；它不读取 presentation
/// target，也不选择 dataset。一次成功的新更新会复制规范已定义的量化原值，后续 dependent
/// frame 的 `b_keep` 返回同一快照。规范没有为“无历史的 keep”定义默认 object properties，
/// 因而首次 keep、reset 后 keep 以及 I-frame keep 都失败关闭。
///
/// I-frame 的新更新不依赖旧对象布局；dependent frame 若改变布局则要求调用方先
/// [`reset`](Self::reset)。seek、换源、拓扑变化、不连续或丢帧后也应 reset。所有更新均为
/// 事务性，本类型不反量化、不选择 target，也不把属性应用到 OAMD 或 PCM。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OamdAlternativeDataSetState {
    data_set_index: Option<u32>,
    objects: Option<ObjectDescriptors>,
    effective: Option<OamdAlternativePropertySnapshot>,
}

impl OamdAlternativeDataSetState {
    /// 创建尚未绑定 dataset loop index 的空状态。
    #[must_use]
    pub const fn new() -> Self {
        Self {
            data_set_index: None,
            objects: None,
            effective: None,
        }
    }

    /// 当前状态绑定的 dataset 零基下标；首次成功更新前为 `None`。
    #[must_use]
    pub const fn data_set_index(self) -> Option<u32> {
        self.data_set_index
    }

    /// 最近一次成功提交后当前有效的已定义 object properties。
    #[must_use]
    pub const fn effective(self) -> Option<OamdAlternativePropertySnapshot> {
        self.effective
    }

    /// 清除 dataset 绑定、对象布局与有效更新。
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// 应用当前帧同一 dataset loop index 的原始更新。
    ///
    /// `data_set` 自带其 `oamd_dyndata_single()` 的精确 `b_iframe` 与对象布局。非 keep 更新复制
    /// 当前规范已定义的属性；dependent keep 沿用历史。调用方仍可从原始 `data_set` 取得当前帧
    /// 的 opaque additional-data view，本状态不解释或复制该保留区。
    ///
    /// # Errors
    ///
    /// 状态被误用于另一 dataset index、dependent frame 改变对象布局、keep 没有连续历史，或
    /// 回取已验证 bit range 失败时返回错误。任何失败都不会修改状态。
    pub fn apply(
        &mut self,
        data_set: OamdAlternativeDataSet<'_>,
    ) -> Result<OamdAlternativePropertySnapshot, OamdAlternativeDataSetStateError> {
        if let Some(previous) = self.data_set_index
            && previous != data_set.index
        {
            return Err(OamdAlternativeDataSetStateError::DataSetIndexChanged {
                previous,
                current: data_set.index,
            });
        }

        if !data_set.b_iframe
            && self
                .objects
                .is_some_and(|objects| objects != data_set.objects)
        {
            return Err(OamdAlternativeDataSetStateError::ObjectLayoutChanged {
                data_set_index: data_set.index,
            });
        }

        let effective = if data_set.keep {
            if data_set.b_iframe {
                None
            } else {
                self.effective
            }
            .ok_or(OamdAlternativeDataSetStateError::HistoryUnavailable {
                data_set_index: data_set.index,
            })?
        } else {
            snapshot_defined_properties(data_set)?
        };

        *self = Self {
            data_set_index: Some(data_set.index),
            objects: Some(data_set.objects),
            effective: Some(effective),
        };
        Ok(effective)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ParsedAlternativeAdditionalData {
    declared_bytes: u32,
    extended_positions_bit_offset: u64,
    extended_positions_bit_len: u64,
    opaque_bit_offset: u64,
    opaque_bit_len: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ParsedAlternativeDataSet {
    index: u32,
    keep: bool,
    common_data: Option<bool>,
    data_point_count: u8,
    data_points_bit_offset: u64,
    data_points_bit_len: u64,
    additional_data: Option<ParsedAlternativeAdditionalData>,
}

/// 一个 alternative object-property dataset。
///
/// 该值借用原 payload，仅保留当前 dataset 的边界与 header；数据点、扩展精度位置和
/// opaque additional data 分别通过只读方法取得。
#[derive(Debug, Clone, Copy)]
pub struct OamdAlternativeDataSet<'a> {
    /// dataset 在 `n_alt_data_sets` 循环内的零基下标。
    pub index: u32,
    /// `b_keep`；为真时本 dataset 沿用上一 object-property update。
    pub keep: bool,
    /// 非 keep、非 ISF 隐式公共分支时传输的 `b_common_data`；其他情况为 `None`。
    pub common_data: Option<bool>,
    /// 本 dataset 实际传输的 gain/position 数据点数。
    pub data_point_count: u8,
    /// `b_additional_data` 为真时声明的字节数。
    pub additional_data_bytes: Option<u32>,
    source: &'a [u8],
    objects: ObjectDescriptors,
    b_iframe: bool,
    parsed: ParsedAlternativeDataSet,
}

impl<'a> OamdAlternativeDataSet<'a> {
    /// 遍历本 dataset 的 gain/position 数据点。
    pub fn data_points(self) -> Result<OamdAlternativeDataPointIter<'a>, OamdError> {
        Ok(OamdAlternativeDataPointIter {
            reader: BitReader::new_bounded(
                self.source,
                self.parsed.data_points_bit_offset,
                self.parsed.data_points_bit_len,
            )?,
            objects: self.objects,
            common: self.common_data == Some(true)
                || self
                    .objects
                    .as_slice()
                    .first()
                    .is_some_and(|object| object.obj_type == ObjectType::Isf),
            next_index: 0,
            remaining: self.data_point_count,
            failed: false,
        })
    }

    /// 遍历 additional data 中每个非 LFE 动态对象的 `b_ext_prec_alt_pos` 更新。
    ///
    /// additional data 缺席时返回 `Ok(None)`；`b_keep == 1` 时返回的迭代器为空，
    /// 因为规范不传输扩展精度位置 gate。
    pub fn extended_positions(
        self,
    ) -> Result<Option<OamdAlternativeExtendedPositionIter<'a>>, OamdError> {
        let Some(additional) = self.parsed.additional_data else {
            return Ok(None);
        };
        let remaining = if self.keep {
            0
        } else {
            self.objects
                .as_slice()
                .iter()
                .filter(|object| object.is_dynamic_object())
                .count()
        };
        Ok(Some(OamdAlternativeExtendedPositionIter {
            reader: BitReader::new_bounded(
                self.source,
                additional.extended_positions_bit_offset,
                additional.extended_positions_bit_len,
            )?,
            objects: self.objects,
            next_object: 0,
            remaining,
            failed: false,
        }))
    }

    /// additional data 中位于 `ext_prec_alt_pos()` 之后、规范未定义的 opaque bits。
    #[must_use]
    pub fn opaque_additional_data(self) -> Option<OamdAlternativeOpaqueBits<'a>> {
        let additional = self.parsed.additional_data?;
        OamdAlternativeOpaqueBits::new(
            self.source,
            additional.opaque_bit_offset,
            additional.opaque_bit_len,
        )
    }
}

/// alternative dataset 迭代器。
#[derive(Debug, Clone)]
pub struct OamdAlternativeDataSetIter<'a> {
    reader: BitReader<'a>,
    source: &'a [u8],
    objects: ObjectDescriptors,
    b_iframe: bool,
    next_index: u32,
    remaining: u32,
    failed: bool,
}

impl<'a> Iterator for OamdAlternativeDataSetIter<'a> {
    type Item = Result<OamdAlternativeDataSet<'a>, OamdError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed || self.remaining == 0 {
            return None;
        }
        match parse_alternative_data_set(&mut self.reader, &self.objects, self.next_index) {
            Ok(parsed) => {
                self.next_index = self.next_index.saturating_add(1);
                self.remaining = self.remaining.saturating_sub(1);
                Some(Ok(OamdAlternativeDataSet {
                    index: parsed.index,
                    keep: parsed.keep,
                    common_data: parsed.common_data,
                    data_point_count: parsed.data_point_count,
                    additional_data_bytes: parsed
                        .additional_data
                        .map(|additional| additional.declared_bytes),
                    source: self.source,
                    objects: self.objects,
                    b_iframe: self.b_iframe,
                    parsed,
                }))
            }
            Err(error) => {
                self.failed = true;
                self.remaining = 0;
                Some(Err(error))
            }
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        match usize::try_from(self.remaining) {
            Ok(remaining) => (0, Some(remaining)),
            Err(_) => (0, None),
        }
    }
}

/// alternative dataset 数据点迭代器。
#[derive(Debug, Clone)]
pub struct OamdAlternativeDataPointIter<'a> {
    reader: BitReader<'a>,
    objects: ObjectDescriptors,
    common: bool,
    next_index: u8,
    remaining: u8,
    failed: bool,
}

impl Iterator for OamdAlternativeDataPointIter<'_> {
    type Item = Result<OamdAlternativeDataPoint, OamdError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed || self.remaining == 0 {
            return None;
        }
        let index = usize::from(self.next_index);
        let Some(descriptor) = self.objects.as_slice().get(index).copied() else {
            self.failed = true;
            self.remaining = 0;
            return Some(Err(OamdError::AlternativeDataWithoutObjects {
                data_sets: 1,
                bit_position: self.reader.bit_position(),
            }));
        };
        match parse_alternative_data_point(&mut self.reader, descriptor) {
            Ok((alternative_gain, alternative_position)) => {
                let data_point_index = self.next_index;
                self.next_index = self.next_index.saturating_add(1);
                self.remaining = self.remaining.saturating_sub(1);
                Some(Ok(OamdAlternativeDataPoint {
                    data_point_index,
                    target: if self.common {
                        OamdAlternativeDataPointTarget::AllObjects
                    } else {
                        OamdAlternativeDataPointTarget::Object(data_point_index)
                    },
                    descriptor,
                    alternative_gain,
                    alternative_position,
                }))
            }
            Err(error) => {
                self.failed = true;
                self.remaining = 0;
                Some(Err(error))
            }
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (0, Some(usize::from(self.remaining)))
    }
}

/// alternative additional data 的扩展精度位置迭代器。
#[derive(Debug, Clone)]
pub struct OamdAlternativeExtendedPositionIter<'a> {
    reader: BitReader<'a>,
    objects: ObjectDescriptors,
    next_object: usize,
    remaining: usize,
    failed: bool,
}

impl Iterator for OamdAlternativeExtendedPositionIter<'_> {
    type Item = Result<OamdAlternativeExtendedPosition, OamdError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed || self.remaining == 0 {
            return None;
        }
        while let Some(object) = self.objects.as_slice().get(self.next_object).copied() {
            let object_index = self.next_object;
            self.next_object = self.next_object.saturating_add(1);
            if !object.is_dynamic_object() {
                continue;
            }
            let parsed = (|| {
                let present = self.reader.read_flag()?;
                let position = if present {
                    Some(ExtendedPrecisionPosition::parse(&mut self.reader)?)
                } else {
                    None
                };
                Ok(OamdAlternativeExtendedPosition {
                    object_index: u8::try_from(object_index).unwrap_or(u8::MAX),
                    position,
                })
            })();
            return match parsed {
                Ok(update) => {
                    self.remaining = self.remaining.saturating_sub(1);
                    Some(Ok(update))
                }
                Err(error) => {
                    self.failed = true;
                    self.remaining = 0;
                    Some(Err(error))
                }
            };
        }
        self.remaining = 0;
        None
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (0, Some(self.remaining))
    }
}

/// alternative additional data 中未定义尾部的零拷贝 bit view。
#[derive(Debug, Clone, Copy)]
pub struct OamdAlternativeOpaqueBits<'a> {
    source: &'a [u8],
    bit_offset: u64,
    bit_len: u64,
}

impl<'a, 'b> PartialEq<OamdAlternativeOpaqueBits<'b>> for OamdAlternativeOpaqueBits<'a> {
    fn eq(&self, other: &OamdAlternativeOpaqueBits<'b>) -> bool {
        self.bit_len == other.bit_len && (*self).iter().eq((*other).iter())
    }
}

impl Eq for OamdAlternativeOpaqueBits<'_> {}

impl<'a> OamdAlternativeOpaqueBits<'a> {
    fn new(source: &'a [u8], bit_offset: u64, bit_len: u64) -> Option<Self> {
        let end = bit_offset.checked_add(bit_len)?;
        if end > (source.len() as u64).saturating_mul(8) {
            return None;
        }
        Some(Self {
            source,
            bit_offset,
            bit_len,
        })
    }

    /// 视图起点相对原 payload 的 bit offset。
    #[must_use]
    pub const fn bit_offset(self) -> u64 {
        self.bit_offset
    }

    /// opaque 尾部的 bit 数。
    #[must_use]
    pub const fn len_bits(self) -> u64 {
        self.bit_len
    }

    /// 视图是否为空。
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.bit_len == 0
    }

    /// 返回给定相对下标的 bit；越界返回 `None`。
    #[must_use]
    pub fn get(self, index: u64) -> Option<bool> {
        if index >= self.bit_len {
            return None;
        }
        let absolute = self.bit_offset.checked_add(index)?;
        let byte = self
            .source
            .get(usize::try_from(absolute / 8).ok()?)
            .copied()?;
        let shift = 7u32.saturating_sub(u32::try_from(absolute % 8).ok()?);
        Some(byte & (1u8 << shift) != 0)
    }

    /// 按传输顺序遍历 opaque bits。
    #[must_use]
    pub const fn iter(self) -> OamdAlternativeOpaqueBitIter<'a> {
        OamdAlternativeOpaqueBitIter {
            bits: self,
            next: 0,
        }
    }
}

/// [`OamdAlternativeOpaqueBits`] 的 bit 迭代器。
#[derive(Debug, Clone)]
pub struct OamdAlternativeOpaqueBitIter<'a> {
    bits: OamdAlternativeOpaqueBits<'a>,
    next: u64,
}

impl Iterator for OamdAlternativeOpaqueBitIter<'_> {
    type Item = bool;

    fn next(&mut self) -> Option<Self::Item> {
        let value = self.bits.get(self.next)?;
        self.next = self.next.saturating_add(1);
        Some(value)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.bits.bit_len.saturating_sub(self.next);
        match usize::try_from(remaining) {
            Ok(remaining) => (remaining, Some(remaining)),
            Err(_) => (usize::MAX, None),
        }
    }
}

fn snapshot_defined_properties(
    data_set: OamdAlternativeDataSet<'_>,
) -> Result<OamdAlternativePropertySnapshot, OamdError> {
    let mut snapshot = OamdAlternativePropertySnapshot {
        data_set_index: data_set.index,
        common_data: data_set.common_data,
        data_points: [EMPTY_ALTERNATIVE_DATA_POINT; MAX_OAMD_OBJECTS],
        data_points_len: 0,
        extended_positions: [EMPTY_ALTERNATIVE_EXTENDED_POSITION; MAX_OAMD_OBJECTS],
        extended_positions_len: 0,
    };

    for point in data_set.data_points()? {
        let point = point?;
        let Some(slot) = snapshot.data_points.get_mut(snapshot.data_points_len) else {
            return Err(OamdError::TooManyObjects {
                limit: MAX_OAMD_OBJECTS,
            });
        };
        *slot = point;
        snapshot.data_points_len = snapshot.data_points_len.saturating_add(1);
    }

    if let Some(positions) = data_set.extended_positions()? {
        for position in positions {
            let position = position?;
            let Some(slot) = snapshot
                .extended_positions
                .get_mut(snapshot.extended_positions_len)
            else {
                return Err(OamdError::TooManyObjects {
                    limit: MAX_OAMD_OBJECTS,
                });
            };
            *slot = position;
            snapshot.extended_positions_len = snapshot.extended_positions_len.saturating_add(1);
        }
    }

    Ok(snapshot)
}

fn parse_alternative_data_set(
    reader: &mut BitReader<'_>,
    objects: &ObjectDescriptors,
    index: u32,
) -> Result<ParsedAlternativeDataSet, OamdError> {
    let keep = reader.read_flag()?;
    let (common_data, data_point_count) = if keep {
        (None, 0)
    } else {
        let first = objects.as_slice().first().copied().ok_or(
            OamdError::AlternativeDataWithoutObjects {
                data_sets: 1,
                bit_position: reader.bit_position(),
            },
        )?;
        if first.obj_type == ObjectType::Isf {
            (None, 1)
        } else {
            let common = reader.read_flag()?;
            (
                Some(common),
                if common {
                    1
                } else {
                    u8::try_from(objects.as_slice().len()).unwrap_or(u8::MAX)
                },
            )
        }
    };

    let data_points_bit_offset = reader.bit_position();
    for data_point in 0..data_point_count {
        let descriptor = objects
            .as_slice()
            .get(usize::from(data_point))
            .copied()
            .ok_or(OamdError::AlternativeDataWithoutObjects {
                data_sets: 1,
                bit_position: reader.bit_position(),
            })?;
        parse_alternative_data_point(reader, descriptor)?;
    }
    let data_points_bit_len = reader.bit_position().saturating_sub(data_points_bit_offset);

    let additional_data = if reader.read_flag()? {
        let size_bit_position = reader.bit_position();
        let extension = reader.variable_bits_u32(2)?;
        let declared_bytes = extension.checked_add(1).ok_or(ReadError::ValueOverflow {
            bit_position: size_bit_position,
        })?;
        let declared_bits = u64::from(declared_bytes).saturating_mul(8);
        let mut bounded = reader.bounded_at_current(declared_bits)?;
        let extended_positions_bit_offset = bounded.bit_position();
        let parsed_extensions = parse_extended_positions(&mut bounded, objects, keep);
        if let Err(error) = parsed_extensions {
            return Err(match error {
                OamdError::Read(ReadError::OutOfBounds {
                    requested_bits,
                    bit_position,
                    ..
                }) => OamdError::AdditionalDataUnderflow {
                    declared_bytes: u64::from(declared_bytes),
                    used_bits: bit_position
                        .saturating_sub(extended_positions_bit_offset)
                        .saturating_add(u64::from(requested_bits)),
                },
                other => other,
            });
        }
        let extended_positions_bit_len = bounded
            .bit_position()
            .saturating_sub(extended_positions_bit_offset);
        let opaque_bit_offset = bounded.bit_position();
        let opaque_bit_len = bounded.remaining_bits();
        reader.skip_bits(declared_bits)?;
        Some(ParsedAlternativeAdditionalData {
            declared_bytes,
            extended_positions_bit_offset,
            extended_positions_bit_len,
            opaque_bit_offset,
            opaque_bit_len,
        })
    } else {
        None
    };

    Ok(ParsedAlternativeDataSet {
        index,
        keep,
        common_data,
        data_point_count,
        data_points_bit_offset,
        data_points_bit_len,
        additional_data,
    })
}

fn parse_alternative_data_point(
    reader: &mut BitReader<'_>,
    descriptor: ObjectDescriptor,
) -> Result<(Option<u8>, Option<AbsolutePosition>), OamdError> {
    let alternative_gain = if reader.read_flag()? {
        Some(u8::try_from(reader.read_bits(6)?).unwrap_or(u8::MAX))
    } else {
        None
    };
    let alternative_position = if descriptor.is_dynamic_object() && reader.read_flag()? {
        Some(AbsolutePosition {
            x: u8::try_from(reader.read_bits(6)?).unwrap_or(u8::MAX),
            y: u8::try_from(reader.read_bits(6)?).unwrap_or(u8::MAX),
            z_sign: reader.read_flag()?,
            z: u8::try_from(reader.read_bits(4)?).unwrap_or(u8::MAX),
        })
    } else {
        None
    };
    Ok((alternative_gain, alternative_position))
}

fn parse_extended_positions(
    reader: &mut BitReader<'_>,
    objects: &ObjectDescriptors,
    keep: bool,
) -> Result<(), OamdError> {
    if keep {
        return Ok(());
    }
    for object in objects.as_slice() {
        if object.is_dynamic_object() && reader.read_flag()? {
            ExtendedPrecisionPosition::parse(reader)?;
        }
    }
    Ok(())
}

#[cfg(test)]
#[expect(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    reason = "测试位串的索引和算术均由固定容量与显式 bit 长度约束"
)]
mod tests {
    extern crate std;

    use super::*;
    use std::vec::Vec;

    struct TestBits {
        bytes: [u8; 256],
        bit_len: usize,
    }

    impl TestBits {
        fn new() -> Self {
            Self {
                bytes: [0; 256],
                bit_len: 0,
            }
        }

        fn push(&mut self, value: bool) {
            if value {
                self.bytes[self.bit_len / 8] |= 1 << (7 - self.bit_len % 8);
            }
            self.bit_len += 1;
        }

        fn push_bits(&mut self, value: u64, width: u32) {
            for shift in (0..width).rev() {
                self.push(value & (1u64 << shift) != 0);
            }
        }

        fn as_slice(&self) -> &[u8] {
            &self.bytes[..self.bit_len.div_ceil(8)]
        }

        fn reader(&self) -> BitReader<'_> {
            BitReader::new_bounded(self.as_slice(), 0, self.bit_len as u64).unwrap()
        }
    }

    const BED: ObjectDescriptor = ObjectDescriptor {
        obj_type: ObjectType::Bed,
        b_lfe: false,
        b_ajoc_coded: false,
    };
    const DYNAMIC: ObjectDescriptor = ObjectDescriptor {
        obj_type: ObjectType::Dynamic,
        b_lfe: false,
        b_ajoc_coded: false,
    };
    const DYNAMIC_LFE: ObjectDescriptor = ObjectDescriptor {
        obj_type: ObjectType::Dynamic,
        b_lfe: true,
        b_ajoc_coded: false,
    };
    const ISF: ObjectDescriptor = ObjectDescriptor {
        obj_type: ObjectType::Isf,
        b_lfe: false,
        b_ajoc_coded: false,
    };

    fn data_set_at<'a>(
        bits: &'a TestBits,
        objects: &[ObjectDescriptor],
        num_obj_info_blocks: u8,
        b_iframe: bool,
        index: usize,
    ) -> OamdAlternativeDataSet<'a> {
        let mut reader = bits.reader();
        let parsed =
            OamdDyndataSingle::parse(&mut reader, objects, num_obj_info_blocks, b_iframe, true)
                .unwrap();
        assert_eq!(reader.bit_position(), bits.bit_len as u64);
        parsed
            .alternative_data_sets(bits.as_slice())
            .unwrap()
            .unwrap()
            .nth(index)
            .unwrap()
            .unwrap()
    }

    #[test]
    fn parses_and_preserves_every_alternative_dataset() {
        let objects = [BED, DYNAMIC, DYNAMIC_LFE];
        let mut bits = TestBits::new();

        // 三个 I-frame object_info_block：object_not_active=1, b_additional_data=0。
        for _ in &objects {
            bits.push(true);
            bits.push(false);
        }

        bits.push(true); // b_ducking_disabled
        bits.push_bits(3, 2); // category 扩展 gate
        bits.push_bits(1, 2); // variable_bits(2) = 1
        bits.push(false);
        bits.push_bits(2, 2); // 两个 datasets

        // dataset 0：逐对象数据。
        bits.push(false); // b_keep
        bits.push(false); // b_common_data
        bits.push(true); // BED b_alt_gain
        bits.push_bits(5, 6);
        bits.push(false); // DYN b_alt_gain
        bits.push(true); // DYN b_alt_position
        bits.push_bits(10, 6);
        bits.push_bits(20, 6);
        bits.push(true);
        bits.push_bits(7, 4);
        bits.push(true); // LFE b_alt_gain
        bits.push_bits(63, 6);
        bits.push(true); // b_additional_data
        bits.push_bits(1, 2); // (1 + 1) * 8 bits
        bits.push(false);
        bits.push(true); // dynamic object 1 的 b_ext_prec_alt_pos
        bits.push_bits(0b101, 3);
        bits.push_bits(2, 2);
        bits.push_bits(1, 2);
        bits.push_bits(0xa5, 8); // opaque tail

        // dataset 1：b_keep 不携带数据点或扩展位置，additional data 全部 opaque。
        bits.push(true);
        bits.push(true);
        bits.push_bits(0, 2); // 1 byte
        bits.push(false);
        bits.push_bits(0x3c, 8);

        let mut reader = bits.reader();
        let parsed = OamdDyndataSingle::parse(&mut reader, &objects, 1, true, true).unwrap();
        assert_eq!(reader.bit_position(), bits.bit_len as u64);
        assert_eq!(parsed.metadata_block_count(), 3);
        assert_eq!(parsed.objects(), objects);
        assert_eq!(parsed.num_obj_info_blocks(), 1);
        assert!(parsed.b_iframe());

        let blocks = parsed
            .metadata_blocks(bits.as_slice())
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[0].object_index, 0);
        assert_eq!(blocks[2].object_index, 2);
        assert!(blocks.iter().all(|block| block.info.object_not_active));

        let alternative = parsed.alternative().unwrap();
        assert!(alternative.b_ducking_disabled);
        assert_eq!(alternative.object_sound_category, 4);
        assert_eq!(alternative.n_data_sets, 2);

        let data_sets = parsed
            .alternative_data_sets(bits.as_slice())
            .unwrap()
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(data_sets.len(), 2, "解析层不得根据 target 自动选择 dataset");

        let first = data_sets[0];
        assert!(!first.keep);
        assert_eq!(first.common_data, Some(false));
        assert_eq!(first.data_point_count, 3);
        assert_eq!(first.additional_data_bytes, Some(2));
        let points = first
            .data_points()
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(points.len(), 3);
        assert_eq!(points[0].alternative_gain, Some(5));
        assert_eq!(points[0].alternative_position, None);
        assert_eq!(points[1].alternative_gain, None);
        assert_eq!(
            points[1].alternative_position,
            Some(AbsolutePosition {
                x: 10,
                y: 20,
                z_sign: true,
                z: 7,
            })
        );
        assert_eq!(points[2].alternative_gain, Some(63));
        assert_eq!(points[2].alternative_position, None, "LFE 不携带位置");
        assert!(points.iter().enumerate().all(|(index, point)| {
            point.target
                == OamdAlternativeDataPointTarget::Object(u8::try_from(index).unwrap_or(u8::MAX))
        }));

        let extended = first
            .extended_positions()
            .unwrap()
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            extended,
            [OamdAlternativeExtendedPosition {
                object_index: 1,
                position: Some(ExtendedPrecisionPosition {
                    presence: 0b101,
                    x: Some(2),
                    y: None,
                    z: Some(1),
                }),
            }]
        );
        let opaque = first.opaque_additional_data().unwrap();
        assert_eq!(opaque.len_bits(), 8);
        assert_eq!(
            opaque.iter().collect::<Vec<_>>(),
            [true, false, true, false, false, true, false, true]
        );

        let kept = data_sets[1];
        assert!(kept.keep);
        assert_eq!(kept.common_data, None);
        assert_eq!(kept.data_point_count, 0);
        assert!(kept.data_points().unwrap().next().is_none());
        assert!(kept.extended_positions().unwrap().unwrap().next().is_none());
        assert_eq!(
            kept.opaque_additional_data()
                .unwrap()
                .iter()
                .collect::<Vec<_>>(),
            [false, false, true, true, true, true, false, false]
        );
    }

    #[test]
    fn extends_counts_and_uses_the_isf_implicit_common_point() {
        let objects = [ISF, ISF];
        let mut bits = TestBits::new();
        bits.push(false); // ducking
        bits.push_bits(3, 2);
        bits.push_bits(2, 2); // category = 3 + 2 = 5
        bits.push(false);
        bits.push_bits(3, 2);
        bits.push_bits(1, 2); // datasets = 3 + 1 = 4
        bits.push(false);

        bits.push(false); // dataset 0: keep=0
        bits.push(false); // ISF gain absent；无 b_common_data
        bits.push(false); // additional absent
        for _ in 0..3 {
            bits.push(true); // keep
            bits.push(false); // additional absent
        }

        let mut reader = bits.reader();
        let parsed = OamdDyndataSingle::parse(&mut reader, &objects, 0, false, true).unwrap();
        let alternative = parsed.alternative().unwrap();
        assert_eq!(alternative.object_sound_category, 5);
        assert_eq!(alternative.n_data_sets, 4);
        let data_sets = parsed
            .alternative_data_sets(bits.as_slice())
            .unwrap()
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(data_sets.len(), 4);
        assert_eq!(data_sets[0].common_data, None, "ISF 分支不传该 flag");
        let points = data_sets[0]
            .data_points()
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(points.len(), 1);
        assert_eq!(points[0].target, OamdAlternativeDataPointTarget::AllObjects);
        assert!(data_sets.iter().skip(1).all(|data_set| data_set.keep));
    }

    #[test]
    fn zero_objects_allow_kept_datasets_but_reject_new_data() {
        let mut kept = TestBits::new();
        kept.push(false); // ducking
        kept.push_bits(0, 2); // category
        kept.push_bits(1, 2); // one dataset
        kept.push(true); // keep：不访问 obj_type[0]
        kept.push(false); // no additional data

        let mut reader = kept.reader();
        let parsed = OamdDyndataSingle::parse(&mut reader, &[], 0, false, true).unwrap();
        assert_eq!(reader.bit_position(), kept.bit_len as u64);
        let data_set = parsed
            .alternative_data_sets(kept.as_slice())
            .unwrap()
            .unwrap()
            .next()
            .unwrap()
            .unwrap();
        assert!(data_set.keep);
        assert_eq!(data_set.data_point_count, 0);

        let mut replaced = TestBits::new();
        replaced.push(false); // ducking
        replaced.push_bits(0, 2); // category
        replaced.push_bits(1, 2); // one dataset
        replaced.push(false); // keep=0 必须取得 obj_type[0]
        assert!(matches!(
            OamdDyndataSingle::parse(&mut replaced.reader(), &[], 0, false, true),
            Err(OamdError::AlternativeDataWithoutObjects { data_sets: 1, .. })
        ));
    }

    #[test]
    fn common_dynamic_data_keeps_one_gain_and_position_candidate() {
        let objects = [DYNAMIC, DYNAMIC];
        let mut bits = TestBits::new();
        bits.push(false); // ducking
        bits.push_bits(0, 2); // category
        bits.push_bits(1, 2); // one dataset
        bits.push(false); // keep
        bits.push(true); // common
        bits.push(true); // gain
        bits.push_bits(9, 6);
        bits.push(true); // position
        bits.push_bits(1, 6);
        bits.push_bits(2, 6);
        bits.push(false);
        bits.push_bits(3, 4);
        bits.push(false); // no additional data

        let parsed =
            OamdDyndataSingle::parse(&mut bits.reader(), &objects, 0, false, true).unwrap();
        let data_set = parsed
            .alternative_data_sets(bits.as_slice())
            .unwrap()
            .unwrap()
            .next()
            .unwrap()
            .unwrap();
        assert_eq!(data_set.common_data, Some(true));
        assert_eq!(data_set.data_point_count, 1);
        let point = data_set.data_points().unwrap().next().unwrap().unwrap();
        assert_eq!(point.target, OamdAlternativeDataPointTarget::AllObjects);
        assert_eq!(point.alternative_gain, Some(9));
        assert_eq!(
            point.alternative_position,
            Some(AbsolutePosition {
                x: 1,
                y: 2,
                z_sign: false,
                z: 3,
            })
        );
    }

    #[test]
    fn data_set_state_keeps_defined_properties_across_dependent_frames() {
        let objects = [DYNAMIC];
        let mut first = TestBits::new();
        first.push(true); // I-frame object_info_block: inactive
        first.push(false); // no object additional data
        first.push(false); // ducking
        first.push_bits(0, 2); // category
        first.push_bits(1, 2); // one dataset
        first.push(false); // new update
        first.push(false); // per-object data
        first.push(true); // gain present
        first.push_bits(7, 6);
        first.push(true); // position present
        first.push_bits(10, 6);
        first.push_bits(20, 6);
        first.push(true);
        first.push_bits(3, 4);
        first.push(true); // additional data present
        first.push_bits(0, 2); // one byte
        first.push(false);
        first.push(true); // extended position present
        first.push_bits(0b100, 3);
        first.push_bits(2, 2);
        first.push_bits(0b11, 2); // opaque tail

        let first_data_set = data_set_at(&first, &objects, 1, true, 0);
        assert_eq!(
            first_data_set
                .opaque_additional_data()
                .unwrap()
                .iter()
                .collect::<Vec<_>>(),
            [true, true]
        );

        let mut state = OamdAlternativeDataSetState::new();
        let effective = state.apply(first_data_set).unwrap();
        assert_eq!(state.data_set_index(), Some(0));
        assert_eq!(state.effective(), Some(effective));
        assert_eq!(effective.data_set_index, 0);
        assert_eq!(effective.common_data, Some(false));
        assert_eq!(
            effective.data_points(),
            &[OamdAlternativeDataPoint {
                data_point_index: 0,
                target: OamdAlternativeDataPointTarget::Object(0),
                descriptor: DYNAMIC,
                alternative_gain: Some(7),
                alternative_position: Some(AbsolutePosition {
                    x: 10,
                    y: 20,
                    z_sign: true,
                    z: 3,
                }),
            }]
        );
        assert_eq!(
            effective.extended_positions(),
            &[OamdAlternativeExtendedPosition {
                object_index: 0,
                position: Some(ExtendedPrecisionPosition {
                    presence: 0b100,
                    x: Some(2),
                    y: None,
                    z: None,
                }),
            }]
        );

        let mut kept = TestBits::new();
        kept.push(false); // ducking
        kept.push_bits(0, 2); // category
        kept.push_bits(1, 2); // one dataset
        kept.push(true); // keep previous object properties
        kept.push(true); // additional data present; keep makes all of it opaque
        kept.push_bits(0, 2); // one byte
        kept.push(false);
        kept.push_bits(0xa5, 8);
        let kept_data_set = data_set_at(&kept, &objects, 0, false, 0);
        assert_eq!(
            kept_data_set
                .opaque_additional_data()
                .unwrap()
                .iter()
                .collect::<Vec<_>>(),
            [true, false, true, false, false, true, false, true]
        );
        assert_eq!(state.apply(kept_data_set).unwrap(), effective);
        assert_eq!(state.effective(), Some(effective));
    }

    #[test]
    fn data_set_state_rejects_keep_without_continuous_history_transactionally() {
        let objects = [DYNAMIC];
        let mut kept = TestBits::new();
        kept.push(false); // ducking
        kept.push_bits(0, 2);
        kept.push_bits(1, 2);
        kept.push(true); // keep
        kept.push(false); // no additional data
        let dependent_keep = data_set_at(&kept, &objects, 0, false, 0);

        let mut state = OamdAlternativeDataSetState::new();
        assert_eq!(
            state.apply(dependent_keep),
            Err(OamdAlternativeDataSetStateError::HistoryUnavailable { data_set_index: 0 })
        );
        assert_eq!(state, OamdAlternativeDataSetState::new());

        let mut transmitted = TestBits::new();
        transmitted.push(false); // ducking
        transmitted.push_bits(0, 2);
        transmitted.push_bits(1, 2);
        transmitted.push(false); // new update
        transmitted.push(true); // common
        transmitted.push(true); // gain present
        transmitted.push_bits(12, 6);
        transmitted.push(false); // position absent
        transmitted.push(false); // additional absent
        let effective = state
            .apply(data_set_at(&transmitted, &objects, 0, false, 0))
            .unwrap();
        let previous = state;

        let mut independent_keep = TestBits::new();
        independent_keep.push(true); // I-frame object block: inactive
        independent_keep.push(false);
        independent_keep.push(false); // ducking
        independent_keep.push_bits(0, 2);
        independent_keep.push_bits(1, 2);
        independent_keep.push(true); // keep cannot use pre-I-frame history
        independent_keep.push(false); // additional absent
        assert_eq!(
            state.apply(data_set_at(&independent_keep, &objects, 1, true, 0)),
            Err(OamdAlternativeDataSetStateError::HistoryUnavailable { data_set_index: 0 })
        );
        assert_eq!(state, previous, "失败的独立帧不得半提交 reset");
        assert_eq!(state.effective(), Some(effective));

        state.reset();
        assert_eq!(state, OamdAlternativeDataSetState::new());
    }

    #[test]
    fn data_set_state_binds_index_and_rejects_dependent_layout_changes() {
        let mut dynamic = TestBits::new();
        dynamic.push(false); // ducking
        dynamic.push_bits(0, 2);
        dynamic.push_bits(1, 2);
        dynamic.push(false); // new
        dynamic.push(true); // common
        dynamic.push(false); // gain absent
        dynamic.push(false); // position absent
        dynamic.push(false); // additional absent
        let mut state = OamdAlternativeDataSetState::new();
        state
            .apply(data_set_at(&dynamic, &[DYNAMIC], 0, false, 0))
            .unwrap();
        let previous = state;

        let mut bed = TestBits::new();
        bed.push(false); // ducking
        bed.push_bits(0, 2);
        bed.push_bits(1, 2);
        bed.push(false); // new
        bed.push(true); // common
        bed.push(false); // gain absent; BED has no position flag
        bed.push(false); // additional absent
        assert_eq!(
            state.apply(data_set_at(&bed, &[BED], 0, false, 0)),
            Err(OamdAlternativeDataSetStateError::ObjectLayoutChanged { data_set_index: 0 })
        );
        assert_eq!(state, previous);

        let mut two_sets = TestBits::new();
        two_sets.push(false); // ducking
        two_sets.push_bits(0, 2);
        two_sets.push_bits(2, 2);
        for _ in 0..2 {
            two_sets.push(false); // new
            two_sets.push(true); // common
            two_sets.push(false); // gain absent
            two_sets.push(false); // position absent
            two_sets.push(false); // additional absent
        }
        assert_eq!(
            state.apply(data_set_at(&two_sets, &[DYNAMIC], 0, false, 1)),
            Err(OamdAlternativeDataSetStateError::DataSetIndexChanged {
                previous: 0,
                current: 1,
            })
        );
        assert_eq!(state, previous);

        let mut independent_bed = TestBits::new();
        independent_bed.push(true); // I-frame object block: inactive
        independent_bed.push(false);
        independent_bed.push(false); // ducking
        independent_bed.push_bits(0, 2);
        independent_bed.push_bits(1, 2);
        independent_bed.push(false); // new
        independent_bed.push(true); // common
        independent_bed.push(false); // gain absent
        independent_bed.push(false); // additional absent
        let rebased = state
            .apply(data_set_at(&independent_bed, &[BED], 1, true, 0))
            .unwrap();
        assert_eq!(rebased.data_points()[0].descriptor, BED);
    }

    #[test]
    fn additional_data_cannot_borrow_following_bits() {
        let objects = [DYNAMIC];
        let mut bits = TestBits::new();
        bits.push(false); // ducking
        bits.push_bits(0, 2); // category
        bits.push_bits(1, 2); // one dataset
        bits.push(false); // keep
        bits.push(true); // common
        bits.push(false); // gain absent
        bits.push(false); // position absent
        bits.push(true); // additional present
        bits.push_bits(0, 2); // variable_bits = 0 -> one byte
        bits.push(false);
        // 声明的 8 bits：presence flag + 111 + X + Y；Z 的两位故意放在 envelope 后。
        bits.push(true);
        bits.push_bits(0b111, 3);
        bits.push_bits(1, 2);
        bits.push_bits(2, 2);
        bits.push_bits(3, 2);
        bits.push_bits(0xff, 8); // 后续可读数据不得被借用

        let error =
            OamdDyndataSingle::parse(&mut bits.reader(), &objects, 0, false, true).unwrap_err();
        assert_eq!(
            error,
            OamdError::AdditionalDataUnderflow {
                declared_bytes: 1,
                used_bits: 10,
            }
        );
    }

    #[test]
    fn rejects_zero_iframe_blocks_and_dataset_count_overflow() {
        assert_eq!(
            OamdDyndataSingle::parse(&mut BitReader::new(&[]), &[BED], 0, true, true,),
            Err(OamdError::ZeroBlocksInIframe)
        );

        let mut bits = TestBits::new();
        bits.push(false); // ducking
        bits.push_bits(0, 2); // category
        bits.push_bits(3, 2); // dataset extension gate
        for _ in 0..15 {
            bits.push_bits(3, 2);
            bits.push(true);
        }
        bits.push_bits(0, 2);
        bits.push(false);
        assert!(matches!(
            OamdDyndataSingle::parse(&mut bits.reader(), &[BED], 0, false, true),
            Err(OamdError::Read(ReadError::ValueOverflow { .. }))
        ));
    }
}
