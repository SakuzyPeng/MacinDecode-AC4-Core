//! OAMD 跨帧状态与事务提交。

use super::*;

/// 已解释但仍保持整数码值的对象增益状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectGainState {
    /// `object_basic_info` 的 0 dB 默认值。
    Default,
    /// 静音，即负无穷 dB。
    NegativeInfinity,
    /// 显式的六比特 `object_gain_value`。
    Quantized(u8),
}

/// 已解释但仍保持整数码值的对象优先级状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectPriorityState {
    /// `object_basic_info` 的默认优先级 1。
    Default,
    /// 不活动对象的优先级 0。
    Minimum,
    /// 显式的五比特优先级码值。
    Quantized(u8),
}

/// 合并复用后的完整基本信息状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObjectBasicState {
    /// 当前增益状态。
    pub gain: ObjectGainState,
    /// 当前优先级状态。
    pub priority: ObjectPriorityState,
}

/// 标准精度的完整位置码值。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PositionCoding {
    /// 最近一次显式位置使用绝对编码；保留 Z 符号以正确解释扩展精度增量。
    #[default]
    AbsolutePositive,
    /// 最近一次绝对位置使用负 Z 符号。
    AbsoluteNegative,
    /// 最近一次显式位置使用差分编码，扩展精度 Z 是有符号加数。
    Differential,
}

/// 标准精度的完整位置码值及其最近编码方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuantizedPosition {
    /// 已裁剪到 `[0, 62]` 的 X 轴码值。
    pub x: u8,
    /// 已裁剪到 `[0, 62]` 的 Y 轴码值。
    pub y: u8,
    /// 已裁剪到 `[-15, 15]` 的 Z 轴有符号码值。
    pub z: i8,
    /// 最近一次显式位置的编码方式。
    pub coding: PositionCoding,
}

/// 合并部分复用后的完整渲染状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObjectRenderState {
    /// 当前标准精度位置。
    pub position: QuantizedPosition,
    /// 最近一次完整的区域属性组。
    pub zone: ZoneUpdate,
    /// 最近一次完整的其他属性组。
    ///
    /// `object_div_mode = 0b01` 的字段内复用已经解析为前一有效量化值；当前块的
    /// 原始编码仍保留在 [`OamdMetadataBlock`] 中。
    pub other_properties: OtherPropertiesUpdate,
}

impl ObjectRenderState {
    const fn defaults() -> Self {
        Self {
            position: QuantizedPosition {
                x: 31,
                y: 31,
                z: 0,
                coding: PositionCoding::AbsolutePositive,
            },
            zone: ZoneUpdate {
                grouped_defaults: true,
                group_zone_flag: None,
                zone_mask: None,
            },
            other_properties: OtherPropertiesUpdate {
                grouped_defaults: true,
                group_other_mask: None,
                width: None,
                screen_factor_code: None,
                depth_factor: None,
                object_at_infinity: None,
                distance_factor_code: None,
                divergence_mode: None,
                divergence_table: None,
                divergence_code: None,
            },
        }
    }
}

/// 一个对象在应用当前更新后的有效状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObjectMetadataState {
    /// 对象当前是否活动。
    pub active: bool,
    /// 完整基本信息；尚未收到自足更新时为 `None`。
    pub basic: Option<ObjectBasicState>,
    /// 完整渲染信息；尚未收到自足更新时为 `None`。
    pub render: Option<ObjectRenderState>,
}

const EMPTY_ADDITIONAL_OBJECT_METADATA: AdditionalObjectMetadata = AdditionalObjectMetadata {
    trim_disabled: false,
    extended_position: None,
    headphone: None,
};

impl ObjectMetadataState {
    const EMPTY: Self = Self {
        active: false,
        basic: None,
        render: None,
    };
}

/// 状态延续缺失或无效的字段类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OamdStateField {
    /// 完整基本信息。
    Basic,
    /// 基本信息中的前一增益。
    Gain,
    /// 完整渲染信息。
    Render,
    /// 差分位置的前一位置。
    Position,
    /// `object_div_mode = 0b01` 所需的前一 divergence。
    Divergence,
    /// 解析结果缺少状态所需的显式字段。
    ParsedUpdate,
}

impl fmt::Display for OamdStateField {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Basic => "basic information",
            Self::Gain => "object gain",
            Self::Render => "rendering information",
            Self::Position => "object position",
            Self::Divergence => "object divergence",
            Self::ParsedUpdate => "explicit update fields",
        })
    }
}

/// 应用 OAMD 更新时的状态错误。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OamdStateError {
    /// 更新引用了 reset 前或尚未建立的状态。
    HistoryUnavailable {
        /// 对象下标。
        object_index: u8,
        /// 缺失的状态字段。
        field: OamdStateField,
    },
    /// 解析结果中的对象下标超出固定容量。
    ObjectIndexOutOfRange {
        /// 对象下标。
        object_index: u8,
        /// 本实现的对象上限。
        limit: usize,
    },
}

impl fmt::Display for OamdStateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::HistoryUnavailable {
                object_index,
                field,
            } => write!(
                f,
                "{field} for object {object_index} depends on unavailable prior state"
            ),
            Self::ObjectIndexOutOfRange {
                object_index,
                limit,
            } => write!(f, "OAMD object index {object_index} exceeds limit {limit}"),
        }
    }
}

impl core::error::Error for OamdStateError {}

/// 单个 OAMD substream 的跨帧状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OamdState {
    previous_num_obj_info_blocks: Option<u8>,
    effective_common: Option<OamdCommonData>,
    effective_timing: Option<OamdTimingData>,
    objects: [ObjectMetadataState; MAX_OAMD_OBJECTS],
    additional: [AdditionalObjectMetadata; MAX_OAMD_OBJECTS],
}

impl Default for OamdState {
    fn default() -> Self {
        Self::new()
    }
}

impl OamdState {
    /// 创建不含任何跨帧历史的状态。
    #[must_use]
    pub const fn new() -> Self {
        Self {
            previous_num_obj_info_blocks: None,
            effective_common: None,
            effective_timing: None,
            objects: [ObjectMetadataState::EMPTY; MAX_OAMD_OBJECTS],
            additional: [EMPTY_ADDITIONAL_OBJECT_METADATA; MAX_OAMD_OBJECTS],
        }
    }

    /// 前一帧有效的 `num_obj_info_blocks`。
    #[must_use]
    pub const fn previous_num_obj_info_blocks(&self) -> Option<u8> {
        self.previous_num_obj_info_blocks
    }

    /// 返回一个对象的当前有效状态。
    #[must_use]
    pub fn object(&self, index: usize) -> Option<&ObjectMetadataState> {
        self.objects.get(index)
    }

    /// 最近一次生效的公共 OAMD 数据。
    #[must_use]
    pub const fn effective_common(&self) -> Option<OamdCommonData> {
        self.effective_common
    }

    /// 最近一次生效的完整时间数据。
    #[must_use]
    pub const fn effective_timing(&self) -> Option<OamdTimingData> {
        self.effective_timing
    }

    /// 返回对象当前生效的附加元数据。
    #[must_use]
    pub fn object_additional(&self, index: usize) -> Option<&AdditionalObjectMetadata> {
        self.additional.get(index)
    }

    /// 在 seek、配置变化或不连续处清除全部历史。
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// 按码流顺序应用一个已解析载荷。
    ///
    /// 更新是事务性的：任何复用或差分缺少前序状态时，原状态保持不变。
    pub fn apply(&mut self, payload: &OamdSubstreamPayload) -> Result<(), OamdStateError> {
        self.apply_frame(payload.metadata_blocks(), payload.common, payload.timing)
    }

    /// 应用一帧的公共数据、时间数据与逐对象更新。
    ///
    /// `common`/`timing` 为 `None` 时沿用前一帧；整帧事务性提交。
    pub fn apply_frame(
        &mut self,
        blocks: &[OamdMetadataBlock],
        common: Option<OamdCommonData>,
        timing: Option<OamdTimingData>,
    ) -> Result<(), OamdStateError> {
        let mut next = *self;
        for update in blocks {
            next.apply_block(update)?;
        }
        if let Some(common) = common {
            next.effective_common = Some(common);
        }
        if let Some(timing) = timing {
            next.previous_num_obj_info_blocks = Some(timing.num_obj_info_blocks);
            next.effective_timing = Some(timing);
        }
        *self = next;
        Ok(())
    }

    /// 按码流顺序应用一批逐对象更新。
    ///
    /// A-JOC 路径的动态数据不在 `oamd_substream()` 里，而在 `audio_data_ajoc()`
    /// 内的两处 `oamd_dyndata_single()`（P2 表 7），因此没有 [`OamdSubstreamPayload`]
    /// 可传；两条路径共用同一套状态延续规则，只是入口不同。
    ///
    /// `num_obj_info_blocks` 为 `None` 表示本帧未传输时间数据，沿用前值。
    ///
    /// # Errors
    ///
    /// 见 [`OamdStateError`]。整批更新是事务性的：任一块失败则状态不变。
    pub fn apply_blocks(
        &mut self,
        blocks: &[OamdMetadataBlock],
        num_obj_info_blocks: Option<u8>,
    ) -> Result<(), OamdStateError> {
        let mut next = *self;
        for update in blocks {
            next.apply_block(update)?;
        }
        if let Some(count) = num_obj_info_blocks {
            next.previous_num_obj_info_blocks = Some(count);
            if next
                .effective_timing
                .is_some_and(|timing| timing.num_obj_info_blocks != count)
            {
                next.effective_timing = None;
            }
        }
        *self = next;
        Ok(())
    }

    pub(super) fn apply_block(&mut self, update: &OamdMetadataBlock) -> Result<(), OamdStateError> {
        let index = usize::from(update.object_index);
        let previous =
            self.objects
                .get(index)
                .copied()
                .ok_or(OamdStateError::ObjectIndexOutOfRange {
                    object_index: update.object_index,
                    limit: MAX_OAMD_OBJECTS,
                })?;
        let mut resolved = previous;
        resolved.active = !update.info.object_not_active;

        resolved.basic = match update.info.basic_info_status {
            InfoStatus::Default => Some(ObjectBasicState {
                gain: ObjectGainState::NegativeInfinity,
                priority: ObjectPriorityState::Minimum,
            }),
            InfoStatus::Reuse => {
                Some(previous.basic.ok_or(OamdStateError::HistoryUnavailable {
                    object_index: update.object_index,
                    field: OamdStateField::Basic,
                })?)
            }
            InfoStatus::AllNew => Some(resolve_basic_info(
                update
                    .info
                    .basic_info
                    .ok_or(OamdStateError::HistoryUnavailable {
                        object_index: update.object_index,
                        field: OamdStateField::ParsedUpdate,
                    })?,
                previous.basic,
                update.object_index,
            )?),
            InfoStatus::PartReuse => {
                return Err(OamdStateError::HistoryUnavailable {
                    object_index: update.object_index,
                    field: OamdStateField::ParsedUpdate,
                });
            }
        };

        resolved.render = match update.info.render_info_status {
            InfoStatus::Default => Some(ObjectRenderState::defaults()),
            InfoStatus::Reuse => {
                Some(previous.render.ok_or(OamdStateError::HistoryUnavailable {
                    object_index: update.object_index,
                    field: OamdStateField::Render,
                })?)
            }
            InfoStatus::AllNew | InfoStatus::PartReuse => Some(resolve_render_info(
                update
                    .info
                    .render_info
                    .ok_or(OamdStateError::HistoryUnavailable {
                        object_index: update.object_index,
                        field: OamdStateField::ParsedUpdate,
                    })?,
                previous.render,
                update.info.render_info_status,
                update.object_index,
            )?),
        };

        let slot = self
            .objects
            .get_mut(index)
            .ok_or(OamdStateError::ObjectIndexOutOfRange {
                object_index: update.object_index,
                limit: MAX_OAMD_OBJECTS,
            })?;
        *slot = resolved;
        let additional =
            self.additional
                .get_mut(index)
                .ok_or(OamdStateError::ObjectIndexOutOfRange {
                    object_index: update.object_index,
                    limit: MAX_OAMD_OBJECTS,
                })?;
        // `add_per_object_md()` 属于当前 object_info_block，没有 REUSE 语义。
        // 本块未携带该表时，扩展位置与耳机字段都恢复为默认值。
        *additional = update.info.additional_metadata.unwrap_or_default();
        Ok(())
    }
}

pub(super) fn resolve_basic_info(
    info: ObjectBasicInfo,
    previous: Option<ObjectBasicState>,
    object_index: u8,
) -> Result<ObjectBasicState, OamdStateError> {
    if info.default_metadata {
        return Ok(ObjectBasicState {
            gain: ObjectGainState::Default,
            priority: ObjectPriorityState::Default,
        });
    }

    let gain = match info.basic_info_md {
        Some(0b11) => ObjectGainState::Default,
        Some(0b0 | 0b10) => match info.object_gain_code {
            Some(0b0) => ObjectGainState::Quantized(info.object_gain_value.ok_or(
                OamdStateError::HistoryUnavailable {
                    object_index,
                    field: OamdStateField::ParsedUpdate,
                },
            )?),
            Some(0b10) => ObjectGainState::NegativeInfinity,
            Some(0b11) => {
                previous
                    .map(|state| state.gain)
                    .ok_or(OamdStateError::HistoryUnavailable {
                        object_index,
                        field: OamdStateField::Gain,
                    })?
            }
            _ => {
                return Err(OamdStateError::HistoryUnavailable {
                    object_index,
                    field: OamdStateField::ParsedUpdate,
                });
            }
        },
        _ => {
            return Err(OamdStateError::HistoryUnavailable {
                object_index,
                field: OamdStateField::ParsedUpdate,
            });
        }
    };
    let priority = match info.basic_info_md {
        Some(0b0) => ObjectPriorityState::Default,
        Some(0b10 | 0b11) => ObjectPriorityState::Quantized(info.object_priority_code.ok_or(
            OamdStateError::HistoryUnavailable {
                object_index,
                field: OamdStateField::ParsedUpdate,
            },
        )?),
        _ => {
            return Err(OamdStateError::HistoryUnavailable {
                object_index,
                field: OamdStateField::ParsedUpdate,
            });
        }
    };
    Ok(ObjectBasicState { gain, priority })
}

pub(super) fn resolve_render_info(
    info: ObjectRenderInfo,
    previous: Option<ObjectRenderState>,
    status: InfoStatus,
    object_index: u8,
) -> Result<ObjectRenderState, OamdStateError> {
    let mut out = if status == InfoStatus::PartReuse {
        previous.ok_or(OamdStateError::HistoryUnavailable {
            object_index,
            field: OamdStateField::Render,
        })?
    } else {
        ObjectRenderState::defaults()
    };

    if let Some(position) = info.position {
        out.position = resolve_position(position, previous, object_index)?;
    } else if status == InfoStatus::AllNew {
        return Err(OamdStateError::HistoryUnavailable {
            object_index,
            field: OamdStateField::ParsedUpdate,
        });
    }
    if let Some(zone) = info.zone {
        out.zone = zone;
    } else if status == InfoStatus::AllNew {
        return Err(OamdStateError::HistoryUnavailable {
            object_index,
            field: OamdStateField::ParsedUpdate,
        });
    }
    if let Some(other) = info.other_properties {
        out.other_properties = resolve_other_properties(other, previous, object_index)?;
    } else if status == InfoStatus::AllNew {
        return Err(OamdStateError::HistoryUnavailable {
            object_index,
            field: OamdStateField::ParsedUpdate,
        });
    }
    Ok(out)
}

/// 解析 other-properties 组内部的 divergence 复用。
///
/// `TS103190-2:v1.3.1:6.3.9.8.21` 表 109 的 `object_div_mode = 0b01`
/// 沿用前一 `obj_info_block` 的 divergence，而不是把 mode 码本身当作新的有效值。
fn resolve_other_properties(
    mut current: OtherPropertiesUpdate,
    previous: Option<ObjectRenderState>,
    object_index: u8,
) -> Result<OtherPropertiesUpdate, OamdStateError> {
    if current.divergence_mode != Some(0b01) {
        return Ok(current);
    }

    let previous = previous
        .map(|state| state.other_properties)
        .filter(|state| {
            matches!(
                (
                    state.divergence_mode,
                    state.divergence_table,
                    state.divergence_code
                ),
                (Some(0b00), Some(_), None) | (Some(0b10), None, Some(_))
            )
        })
        .ok_or(OamdStateError::HistoryUnavailable {
            object_index,
            field: OamdStateField::Divergence,
        })?;
    current.divergence_mode = previous.divergence_mode;
    current.divergence_table = previous.divergence_table;
    current.divergence_code = previous.divergence_code;
    Ok(current)
}

pub(super) fn resolve_position(
    update: PositionUpdate,
    previous: Option<ObjectRenderState>,
    object_index: u8,
) -> Result<QuantizedPosition, OamdStateError> {
    match update {
        PositionUpdate::Absolute(position) => {
            let magnitude = i8::try_from(position.z.min(15)).unwrap_or(15);
            Ok(QuantizedPosition {
                x: position.x.min(62),
                y: position.y.min(62),
                z: if position.z_sign {
                    magnitude
                } else {
                    magnitude.saturating_neg()
                },
                coding: if position.z_sign {
                    PositionCoding::AbsolutePositive
                } else {
                    PositionCoding::AbsoluteNegative
                },
            })
        }
        PositionUpdate::Differential(position) => {
            let previous = previous.ok_or(OamdStateError::HistoryUnavailable {
                object_index,
                field: OamdStateField::Position,
            })?;
            let x = i16::from(previous.position.x)
                .saturating_add(decode_three_bit_delta(position.x))
                .clamp(0, 62);
            let y = i16::from(previous.position.y)
                .saturating_add(decode_three_bit_delta(position.y))
                .clamp(0, 62);
            let z = i16::from(previous.position.z)
                .saturating_add(decode_three_bit_delta(position.z))
                .clamp(-15, 15);
            Ok(QuantizedPosition {
                x: u8::try_from(x).unwrap_or(0),
                y: u8::try_from(y).unwrap_or(0),
                z: i8::try_from(z).unwrap_or(0),
                coding: PositionCoding::Differential,
            })
        }
    }
}

pub(super) fn decode_three_bit_delta(code: u8) -> i16 {
    let value = i16::from(code & 0b111);
    if value >= 4 {
        value.saturating_sub(8)
    } else {
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn complete_block(other_properties: OtherPropertiesUpdate) -> OamdMetadataBlock {
        OamdMetadataBlock {
            object_index: 0,
            block_index: 0,
            info: ObjectInfoBlock {
                basic_info_status: InfoStatus::AllNew,
                render_info_status: InfoStatus::AllNew,
                basic_info: Some(ObjectBasicInfo {
                    default_metadata: true,
                    ..ObjectBasicInfo::default()
                }),
                render_info: Some(ObjectRenderInfo {
                    position: Some(PositionUpdate::Absolute(AbsolutePosition {
                        x: 31,
                        y: 31,
                        z_sign: true,
                        z: 0,
                    })),
                    zone: Some(ZoneUpdate {
                        grouped_defaults: true,
                        ..ZoneUpdate::default()
                    }),
                    other_properties: Some(other_properties),
                }),
                ..ObjectInfoBlock::default()
            },
        }
    }

    fn reused_divergence_properties() -> OtherPropertiesUpdate {
        OtherPropertiesUpdate {
            grouped_defaults: false,
            group_other_mask: Some(0b1000),
            divergence_mode: Some(0b01),
            ..OtherPropertiesUpdate::default()
        }
    }

    fn reuse_divergence_block() -> OamdMetadataBlock {
        OamdMetadataBlock {
            object_index: 0,
            block_index: 0,
            info: ObjectInfoBlock {
                basic_info_status: InfoStatus::Reuse,
                render_info_status: InfoStatus::PartReuse,
                render_info: Some(ObjectRenderInfo {
                    other_properties: Some(reused_divergence_properties()),
                    ..ObjectRenderInfo::default()
                }),
                ..ObjectInfoBlock::default()
            },
        }
    }

    #[test]
    fn divergence_reuse_keeps_effective_value_and_raw_block() {
        let reuse = reuse_divergence_block();
        for previous in [
            OtherPropertiesUpdate {
                grouped_defaults: false,
                group_other_mask: Some(0b1000),
                divergence_mode: Some(0b00),
                divergence_table: Some(0b10),
                ..OtherPropertiesUpdate::default()
            },
            OtherPropertiesUpdate {
                grouped_defaults: false,
                group_other_mask: Some(0b1000),
                divergence_mode: Some(0b10),
                divergence_code: Some(33),
                ..OtherPropertiesUpdate::default()
            },
        ] {
            let mut state = OamdState::new();
            state
                .apply_blocks(&[complete_block(previous)], Some(1))
                .expect("显式 divergence 应建立状态");
            state
                .apply_blocks(&[reuse], None)
                .expect("有历史时 divergence REUSE 应成功");

            let effective = state
                .object(0)
                .and_then(|object| object.render)
                .map(|render| render.other_properties)
                .expect("应保留完整 render 状态");
            assert_eq!(effective.divergence_mode, previous.divergence_mode);
            assert_eq!(effective.divergence_table, previous.divergence_table);
            assert_eq!(effective.divergence_code, previous.divergence_code);
        }
        assert_eq!(
            reuse
                .info
                .render_info
                .and_then(|render| render.other_properties)
                .and_then(|other| other.divergence_mode),
            Some(0b01),
            "原始块仍须保留 REUSE 编码"
        );
    }

    #[test]
    fn divergence_reuse_without_an_effective_value_is_transactional() {
        let block = complete_block(reused_divergence_properties());
        let mut state = OamdState::new();

        assert_eq!(
            state.apply_blocks(&[block], Some(1)),
            Err(OamdStateError::HistoryUnavailable {
                object_index: 0,
                field: OamdStateField::Divergence,
            })
        );
        assert!(state.object(0).is_some_and(|object| object.basic.is_none()));
        assert_eq!(state.previous_num_obj_info_blocks(), None);
    }
}
