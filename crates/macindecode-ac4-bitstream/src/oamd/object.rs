//! OAMD 原始逐对象更新语法。

use super::*;

/// 对象类型，由 `bed_dyn_obj_assignment()` 给出，见 `6.2.1.10`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectType {
    /// 床对象。
    Bed,
    /// 动态对象。
    Dynamic,
    /// ISF 对象。
    Isf,
}

/// 单个对象在 OAMD 中的分类，决定它是否携带动态数据。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObjectDescriptor {
    /// 对象类型。
    pub obj_type: ObjectType,
    /// 该对象是否为 LFE。
    pub b_lfe: bool,
    /// 该对象是否由 A-JOC 编码。
    ///
    /// 为真时它的动态数据不在 `oamd_substream`，而在 `audio_data_ajoc`（表 7）。
    pub b_ajoc_coded: bool,
}

impl ObjectDescriptor {
    /// 该对象是否按动态对象处理。
    ///
    /// 按 `6.2.8.3`/`6.2.8.4`：只有类型为 `DYN` 且非 LFE 的对象才携带
    /// `object_render_info`。
    #[must_use]
    pub const fn is_dynamic_object(&self) -> bool {
        matches!(self.obj_type, ObjectType::Dynamic) && !self.b_lfe
    }
}

/// `object_basic_info` 与 `object_render_info` 的更新状态，见 `6.2.8.5`。
///
/// 这是 OAMD 跨帧状态延续的核心：`REUSE` 与 `PART_REUSE` 表示本帧未传输
/// 完整字段，必须沿用前序状态。在随机访问点之后遇到它们即说明状态不完整。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InfoStatus {
    /// 对象不活动，使用规范默认值，不依赖前序帧。
    #[default]
    Default,
    /// 本帧传输了全部字段。
    AllNew,
    /// 完全沿用前序帧。
    Reuse,
    /// 部分沿用前序帧，仅 `object_render_info` 可能出现。
    PartReuse,
}

impl InfoStatus {
    /// 该状态是否依赖前序帧的解码结果。
    #[must_use]
    pub const fn depends_on_history(&self) -> bool {
        matches!(self, InfoStatus::Reuse | InfoStatus::PartReuse)
    }
}

/// `object_basic_info()` 的原始量化码值，见 `6.2.8.6`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ObjectBasicInfo {
    /// 是否使用规范默认的 0 dB 增益与优先级 1。
    pub default_metadata: bool,
    /// `basic_info_md` 前缀码；默认元数据时不传输。
    pub basic_info_md: Option<u8>,
    /// `object_gain_code` 前缀码；`0b11` 表示沿用前一更新。
    pub object_gain_code: Option<u8>,
    /// 显式的 6 比特增益码值。
    pub object_gain_value: Option<u8>,
    /// 显式的 5 比特优先级码值。
    pub object_priority_code: Option<u8>,
}

impl ObjectBasicInfo {
    /// 基本信息内部是否引用前一更新的增益。
    #[must_use]
    pub const fn depends_on_history(&self) -> bool {
        matches!(self.object_gain_code, Some(0b11))
    }

    pub(super) fn parse(reader: &mut BitReader<'_>) -> Result<Self, OamdError> {
        let default_metadata = reader.read_flag()?;
        if default_metadata {
            return Ok(Self {
                default_metadata,
                ..Self::default()
            });
        }

        let basic_info_md = read_prefix_code(reader)?;
        let mut out = Self {
            default_metadata,
            basic_info_md: Some(basic_info_md),
            ..Self::default()
        };
        if matches!(basic_info_md, 0b0 | 0b10) {
            let gain_code = read_prefix_code(reader)?;
            out.object_gain_code = Some(gain_code);
            if gain_code == 0b0 {
                out.object_gain_value = Some(u8::try_from(reader.read_bits(6)?).unwrap_or(u8::MAX));
            }
        }
        if matches!(basic_info_md, 0b10 | 0b11) {
            out.object_priority_code = Some(u8::try_from(reader.read_bits(5)?).unwrap_or(u8::MAX));
        }
        Ok(out)
    }
}

pub(super) fn read_prefix_code(reader: &mut BitReader<'_>) -> Result<u8, OamdError> {
    if !reader.read_flag()? {
        return Ok(0b0);
    }
    Ok(if reader.read_flag()? { 0b11 } else { 0b10 })
}

/// 绝对位置的原始量化码值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AbsolutePosition {
    /// X 轴 6 比特码值。
    pub x: u8,
    /// Y 轴 6 比特码值。
    pub y: u8,
    /// Z 轴符号码值。
    pub z_sign: bool,
    /// Z 轴 4 比特幅值。
    pub z: u8,
}

/// 差分位置的三个 3 比特码值，均以二补码解释。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DifferentialPosition {
    /// X 轴差分码值。
    pub x: u8,
    /// Y 轴差分码值。
    pub y: u8,
    /// Z 轴差分码值。
    pub z: u8,
}

/// 本更新携带的位置编码方式与原始码值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PositionUpdate {
    /// 完整位置。
    Absolute(AbsolutePosition),
    /// 相对前一更新的位置差分。
    Differential(DifferentialPosition),
}

/// 区域属性更新的原始码值。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ZoneUpdate {
    /// 是否使用 grouped defaults。
    pub grouped_defaults: bool,
    /// `group_zone_flag[]` 三比特码值。
    pub group_zone_flag: Option<u8>,
    /// 可选的三比特 `zone_mask`。
    pub zone_mask: Option<u8>,
}

/// 对象宽度更新的原始码值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WidthUpdate {
    /// 单一五比特宽度码。
    Uniform(u8),
    /// 分轴五比特宽度码。
    Cartesian { x: u8, y: u8, z: u8 },
}

/// 位置以外的渲染属性更新。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OtherPropertiesUpdate {
    /// 是否使用 grouped defaults。
    pub grouped_defaults: bool,
    /// `group_other_mask` 四比特码值。
    pub group_other_mask: Option<u8>,
    /// 可选宽度更新。
    pub width: Option<WidthUpdate>,
    /// 屏幕因子三比特码值。
    pub screen_factor_code: Option<u8>,
    /// 深度因子两比特码值。
    pub depth_factor: Option<u8>,
    /// 是否位于无限远。
    pub object_at_infinity: Option<bool>,
    /// 距离因子四比特码值。
    pub distance_factor_code: Option<u8>,
    /// divergence mode 两比特码值。
    pub divergence_mode: Option<u8>,
    /// divergence table 两比特码值。
    pub divergence_table: Option<u8>,
    /// divergence 六比特码值。
    pub divergence_code: Option<u8>,
}

/// `object_render_info()` 的逐组更新与原始量化码值。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ObjectRenderInfo {
    /// 位置更新；部分复用且未更新位置时为 `None`。
    pub position: Option<PositionUpdate>,
    /// 区域属性更新。
    pub zone: Option<ZoneUpdate>,
    /// 其他渲染属性更新。
    pub other_properties: Option<OtherPropertiesUpdate>,
}

impl ObjectRenderInfo {
    /// 位置是否采用差分编码。
    #[must_use]
    pub const fn diff_pos_coding(&self) -> bool {
        matches!(self.position, Some(PositionUpdate::Differential(_)))
    }
}

/// 扩展精度位置的原始码值。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ExtendedPrecisionPosition {
    /// `ext_prec_pos_presence[]` 三比特码值。
    pub presence: u8,
    /// X 轴两比特码值。
    pub x: Option<u8>,
    /// Y 轴两比特码值。
    pub y: Option<u8>,
    /// Z 轴两比特码值。
    pub z: Option<u8>,
}

/// 单对象耳机渲染信息。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObjectHeadphone {
    /// 两比特渲染模式。
    pub render_mode: u8,
    /// 是否禁用头部跟踪。
    pub head_tracking_disabled: bool,
}

/// `add_per_object_md()` 中的已定义字段。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AdditionalObjectMetadata {
    /// 是否禁用对象 trim。
    pub trim_disabled: bool,
    /// 可选扩展精度位置。
    pub extended_position: Option<ExtendedPrecisionPosition>,
    /// 可选单对象耳机渲染信息。
    pub headphone: Option<ObjectHeadphone>,
}

/// `object_info_block()`，见 `6.2.8.5`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ObjectInfoBlock {
    /// 对象在本块是否不活动。
    pub object_not_active: bool,
    /// `object_basic_info` 的更新状态。
    pub basic_info_status: InfoStatus,
    /// `object_render_info` 的更新状态。
    pub render_info_status: InfoStatus,
    /// 位置是否以差分方式编码；差分即依赖前一块的位置。
    pub diff_pos_coding: bool,
    /// 本块是否携带位置字段。
    pub position_present: bool,
    /// 本块显式携带的基本信息码值。
    pub basic_info: Option<ObjectBasicInfo>,
    /// 本块显式携带的渲染信息码值。
    pub render_info: Option<ObjectRenderInfo>,
    /// 附加表内已定义的单对象字段。
    pub additional_metadata: Option<AdditionalObjectMetadata>,
    /// 附加表声明的字节数。
    pub additional_data_bytes: Option<u8>,
}

impl ObjectInfoBlock {
    /// 本块是否依赖前序状态。
    #[must_use]
    pub const fn depends_on_history(&self) -> bool {
        self.basic_info_status.depends_on_history()
            || self.render_info_status.depends_on_history()
            || self.diff_pos_coding
            || match self.basic_info {
                Some(info) => info.depends_on_history(),
                None => false,
            }
    }

    /// 解析 `object_info_block(b_no_delta, b_dynamic_object)`。
    ///
    /// `b_no_delta` 为真表示本块不得引用前序状态，对应 I-frame 的第一个块。
    ///
    /// # Errors
    ///
    /// 读取越界时返回 [`OamdError::Read`]。
    pub fn parse(
        reader: &mut BitReader<'_>,
        b_no_delta: bool,
        b_dynamic_object: bool,
    ) -> Result<Self, OamdError> {
        let object_not_active = reader.read_flag()?;

        let basic_info_status = if object_not_active {
            InfoStatus::Default
        } else if b_no_delta || !reader.read_flag()? {
            InfoStatus::AllNew
        } else {
            InfoStatus::Reuse
        };
        let basic_info = if basic_info_status == InfoStatus::AllNew {
            Some(ObjectBasicInfo::parse(reader)?)
        } else {
            None
        };

        let render_info_status = if object_not_active || !b_dynamic_object {
            InfoStatus::Default
        } else if b_no_delta {
            InfoStatus::AllNew
        } else if reader.read_flag()? {
            InfoStatus::Reuse
        } else if reader.read_flag()? {
            InfoStatus::PartReuse
        } else {
            InfoStatus::AllNew
        };

        let render_info = if matches!(
            render_info_status,
            InfoStatus::AllNew | InfoStatus::PartReuse
        ) {
            Some(ObjectRenderInfo::parse(
                reader,
                render_info_status,
                b_no_delta,
            )?)
        } else {
            None
        };
        let diff_pos_coding = render_info.is_some_and(|info| info.diff_pos_coding());
        let position_present = render_info.is_some_and(|info| info.position.is_some());

        let (additional_metadata, additional_data_bytes) = if reader.read_flag()? {
            let atd_size = reader.read_bits(4)?.saturating_add(1);
            let start = reader.bit_position();
            let metadata =
                AdditionalObjectMetadata::parse(reader, object_not_active, b_dynamic_object)?;
            let used = reader.bit_position().saturating_sub(start);
            let total = atd_size.saturating_mul(8);
            let remain = total
                .checked_sub(used)
                .ok_or(OamdError::AdditionalDataUnderflow {
                    declared_bytes: atd_size,
                    used_bits: used,
                })?;
            reader.skip_bits(remain)?;
            (
                Some(metadata),
                Some(u8::try_from(atd_size).unwrap_or(u8::MAX)),
            )
        } else {
            (None, None)
        };

        Ok(Self {
            object_not_active,
            basic_info_status,
            render_info_status,
            diff_pos_coding,
            position_present,
            basic_info,
            render_info,
            additional_metadata,
            additional_data_bytes,
        })
    }
}

impl ObjectRenderInfo {
    /// 解析 `object_render_info()`，见 `6.2.8.7`。
    pub(super) fn parse(
        reader: &mut BitReader<'_>,
        status: InfoStatus,
        b_no_delta: bool,
    ) -> Result<Self, OamdError> {
        let (other_present, zone_present, position_present) = if status == InfoStatus::AllNew {
            (true, true, true)
        } else {
            (
                reader.read_flag()?,
                reader.read_flag()?,
                reader.read_flag()?,
            )
        };

        let position = if position_present {
            let differential = if b_no_delta {
                false
            } else {
                reader.read_flag()?
            };
            if differential {
                Some(PositionUpdate::Differential(DifferentialPosition {
                    x: u8::try_from(reader.read_bits(3)?).unwrap_or(u8::MAX),
                    y: u8::try_from(reader.read_bits(3)?).unwrap_or(u8::MAX),
                    z: u8::try_from(reader.read_bits(3)?).unwrap_or(u8::MAX),
                }))
            } else {
                Some(PositionUpdate::Absolute(AbsolutePosition {
                    x: u8::try_from(reader.read_bits(6)?).unwrap_or(u8::MAX),
                    y: u8::try_from(reader.read_bits(6)?).unwrap_or(u8::MAX),
                    z_sign: reader.read_flag()?,
                    z: u8::try_from(reader.read_bits(4)?).unwrap_or(u8::MAX),
                }))
            }
        } else {
            None
        };

        let zone = if zone_present {
            let grouped_defaults = reader.read_flag()?;
            let (group_zone_flag, zone_mask) = if grouped_defaults {
                (None, None)
            } else {
                let flag = u8::try_from(reader.read_bits(3)?).unwrap_or(u8::MAX);
                let mask = if flag & 0b100 != 0 {
                    Some(u8::try_from(reader.read_bits(3)?).unwrap_or(u8::MAX))
                } else {
                    None
                };
                (Some(flag), mask)
            };
            Some(ZoneUpdate {
                grouped_defaults,
                group_zone_flag,
                zone_mask,
            })
        } else {
            None
        };

        let other_properties = if other_present {
            let grouped_defaults = reader.read_flag()?;
            let mut out = OtherPropertiesUpdate {
                grouped_defaults,
                ..OtherPropertiesUpdate::default()
            };
            if !grouped_defaults {
                let mask = u8::try_from(reader.read_bits(4)?).unwrap_or(u8::MAX);
                out.group_other_mask = Some(mask);
                if mask & 0b0001 != 0 {
                    out.width = Some(if reader.read_flag()? {
                        WidthUpdate::Cartesian {
                            x: u8::try_from(reader.read_bits(5)?).unwrap_or(u8::MAX),
                            y: u8::try_from(reader.read_bits(5)?).unwrap_or(u8::MAX),
                            z: u8::try_from(reader.read_bits(5)?).unwrap_or(u8::MAX),
                        }
                    } else {
                        WidthUpdate::Uniform(u8::try_from(reader.read_bits(5)?).unwrap_or(u8::MAX))
                    });
                }
                if mask & 0b0010 != 0 {
                    out.screen_factor_code =
                        Some(u8::try_from(reader.read_bits(3)?).unwrap_or(u8::MAX));
                    out.depth_factor = Some(u8::try_from(reader.read_bits(2)?).unwrap_or(u8::MAX));
                }
                if mask & 0b0100 != 0 {
                    let at_infinity = reader.read_flag()?;
                    out.object_at_infinity = Some(at_infinity);
                    if !at_infinity {
                        out.distance_factor_code =
                            Some(u8::try_from(reader.read_bits(4)?).unwrap_or(u8::MAX));
                    }
                }
                if mask & 0b1000 != 0 {
                    let mode = u8::try_from(reader.read_bits(2)?).unwrap_or(u8::MAX);
                    out.divergence_mode = Some(mode);
                    if mode == 0b00 {
                        out.divergence_table =
                            Some(u8::try_from(reader.read_bits(2)?).unwrap_or(u8::MAX));
                    } else if mode & 0b10 != 0 {
                        out.divergence_code =
                            Some(u8::try_from(reader.read_bits(6)?).unwrap_or(u8::MAX));
                    }
                }
            }
            Some(out)
        } else {
            None
        };

        Ok(Self {
            position,
            zone,
            other_properties,
        })
    }
}

impl AdditionalObjectMetadata {
    /// 解析 `add_per_object_md()`，见 `6.2.8.10`。
    pub(super) fn parse(
        reader: &mut BitReader<'_>,
        object_not_active: bool,
        b_dynamic_object: bool,
    ) -> Result<Self, OamdError> {
        let trim_disabled = reader.read_flag()?;
        let extended_position = if !object_not_active && b_dynamic_object && reader.read_flag()? {
            Some(ExtendedPrecisionPosition::parse(reader)?)
        } else {
            None
        };
        let headphone = if reader.read_flag()? {
            Some(ObjectHeadphone {
                render_mode: u8::try_from(reader.read_bits(2)?).unwrap_or(u8::MAX),
                head_tracking_disabled: reader.read_flag()?,
            })
        } else {
            None
        };
        Ok(Self {
            trim_disabled,
            extended_position,
            headphone,
        })
    }
}

impl ExtendedPrecisionPosition {
    /// 解析 `ext_prec_pos()`，见 `6.2.8.11`。
    pub(super) fn parse(reader: &mut BitReader<'_>) -> Result<Self, OamdError> {
        let presence = u8::try_from(reader.read_bits(3)?).unwrap_or(u8::MAX);
        let x = if presence & 0b100 != 0 {
            Some(u8::try_from(reader.read_bits(2)?).unwrap_or(u8::MAX))
        } else {
            None
        };
        let y = if presence & 0b010 != 0 {
            Some(u8::try_from(reader.read_bits(2)?).unwrap_or(u8::MAX))
        } else {
            None
        };
        let z = if presence & 0b001 != 0 {
            Some(u8::try_from(reader.read_bits(2)?).unwrap_or(u8::MAX))
        } else {
            None
        };
        Ok(Self { presence, x, y, z })
    }
}
