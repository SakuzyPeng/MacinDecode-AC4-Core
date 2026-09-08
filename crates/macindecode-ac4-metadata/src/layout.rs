//! 已验证 Core 网格模板。识别只判断 metadata 几何，不声明 PCM 可直接导出。
use macindecode_ac4_bitstream::oamd::{
    AdditionalObjectMetadata, BedRenderInfo, OamdCommonData, ObjectMetadataState, Trim,
    TrimConfigMode, WidthUpdate, ZoneUpdate,
};

pub const GRID_5: [(u8, u8, i8); 5] = [(0, 0, 0), (62, 0, 0), (31, 0, 0), (0, 62, 0), (62, 62, 0)];
pub const GRID_7: [(u8, u8, i8); 7] = [
    (0, 0, 0),
    (62, 0, 0),
    (31, 0, 0),
    (0, 62, 0),
    (62, 62, 0),
    (0, 31, 15),
    (62, 31, 15),
];
pub const GRID_9: [(u8, u8, i8); 9] = [
    (0, 0, 0),
    (62, 0, 0),
    (31, 0, 0),
    (0, 62, 0),
    (62, 62, 0),
    (0, 0, 15),
    (62, 0, 15),
    (0, 62, 15),
    (62, 62, 15),
];
pub const GRID_11: [(u8, u8, i8); 11] = [
    (0, 0, 0),
    (62, 0, 0),
    (31, 0, 0),
    (0, 31, 0),
    (62, 31, 0),
    (0, 0, 15),
    (62, 0, 15),
    (0, 62, 15),
    (62, 62, 15),
    (0, 62, 0),
    (62, 62, 0),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoreSpeakerLayout {
    pub name: &'static str,
    pub positions: &'static [(u8, u8, i8)],
    /// 按 Core 全频带 q 序号排列；LFE 身份单独验证。
    pub speakers: &'static [&'static str],
}
pub const fn layout_for_count(count: usize) -> Option<CoreSpeakerLayout> {
    match count {
        5 => Some(CoreSpeakerLayout {
            name: "5.1",
            positions: &GRID_5,
            speakers: &["L", "R", "C", "Ls", "Rs"],
        }),
        7 => Some(CoreSpeakerLayout {
            name: "5.1.2",
            positions: &GRID_7,
            speakers: &["L", "R", "C", "Ls", "Rs", "Ltm", "Rtm"],
        }),
        9 => Some(CoreSpeakerLayout {
            name: "5.1.4",
            positions: &GRID_9,
            speakers: &["L", "R", "C", "Ls", "Rs", "Vhl", "Vhr", "Ltr", "Rtr"],
        }),
        11 => Some(CoreSpeakerLayout {
            name: "7.1.4",
            positions: &GRID_11,
            speakers: &[
                "L", "R", "C", "Ls", "Rs", "Vhl", "Vhr", "Ltr", "Rtr", "Rls", "Rrs",
            ],
        }),
        _ => None,
    }
}
pub const fn neutral_width(width: Option<WidthUpdate>) -> bool {
    matches!(
        width,
        None | Some(WidthUpdate::Uniform(0)) | Some(WidthUpdate::Cartesian { x: 0, y: 0, z: 0 })
    )
}
pub fn validate_grid_object(
    state: ObjectMetadataState,
    additional: AdditionalObjectMetadata,
    expected: (u8, u8, i8),
) -> Result<(), &'static str> {
    let render = state.render.ok_or("object render state incomplete")?;
    if (render.position.x, render.position.y, render.position.z) != expected
        || additional.extended_position.is_some()
    {
        return Err("object position does not match the fixed grid");
    }
    if !neutral_width(render.other_properties.width) {
        return Err("object width is nonzero");
    }
    let ZoneUpdate {
        grouped_defaults,
        group_zone_flag,
        zone_mask,
    } = render.zone;
    let flag = group_zone_flag.unwrap_or(0);
    let snap = !grouped_defaults && flag & 1 != 0;
    let elevation = grouped_defaults || flag & 2 == 0;
    if snap || !elevation || zone_mask.unwrap_or(0) != 0 {
        return Err("object zone/channel lock is not neutral");
    }
    let p = render.other_properties;
    if p.screen_factor_code.is_some()
        || p.depth_factor.is_some()
        || p.object_at_infinity.is_some()
        || p.distance_factor_code.is_some()
        || p.divergence_mode.is_some()
        || p.divergence_table.is_some()
        || p.divergence_code.is_some()
    {
        return Err("object uses spatial modifiers");
    }
    Ok(())
}

/// 有效 Cartesian 坐标的精确整数键：X/Y 分母 155，Z 分母 75。
/// `TS103190-2:v1.3.1:6.3.9`；与 Scene 的扩展精度及边界裁剪语义一致。
/// 编码方式不同、或扩展精度在边界被裁剪时，不应制造虚假的网格变化。
pub fn effective_position_key(
    state: ObjectMetadataState,
    additional: AdditionalObjectMetadata,
) -> Option<(i16, i16, i16)> {
    use macindecode_ac4_bitstream::oamd::PositionCoding;
    let base = state.render?.position;
    let extension = |code: Option<u8>| match code {
        Some(0) => 1i16,
        Some(1) => 2,
        Some(2) => -1,
        Some(3) => -2,
        _ => 0,
    };
    let extra = additional.extended_position.unwrap_or_default();
    let z_extra = if base.coding == PositionCoding::AbsoluteNegative {
        extension(extra.z).saturating_neg()
    } else {
        extension(extra.z)
    };
    Some((
        i16::from(base.x)
            .saturating_mul(5)
            .saturating_sub(155)
            .saturating_add(extension(extra.x))
            .clamp(-155, 155),
        155i16
            .saturating_sub(i16::from(base.y).saturating_mul(5))
            .saturating_sub(extension(extra.y))
            .clamp(-155, 155),
        i16::from(base.z)
            .saturating_mul(5)
            .saturating_add(z_extra)
            .clamp(-75, 75),
    ))
}
pub const OBSERVED_ENCODER_TRIM_MODES: [TrimConfigMode; 9] = [
    TrimConfigMode::Default,
    TrimConfigMode::Disabled,
    TrimConfigMode::Disabled,
    TrimConfigMode::Default,
    TrimConfigMode::Disabled,
    TrimConfigMode::Default,
    TrimConfigMode::Default,
    TrimConfigMode::Disabled,
    TrimConfigMode::Default,
];

pub fn direct_trim_is_supported(trim: Trim) -> bool {
    if !trim.present {
        return true;
    }
    if trim.configs.iter().any(|c| {
        c.centre.is_some()
            || c.surround.is_some()
            || c.height.is_some()
            || c.top_bottom_y.is_some()
            || c.listener_y.is_some()
    }) {
        return false;
    }
    let modes = trim.configs.map(|c| c.mode);
    let safe = match trim.global_trim_mode {
        0 | 1 => modes.iter().all(|m| *m == TrimConfigMode::Inherit),
        2 => modes
            .iter()
            .all(|m| matches!(m, TrimConfigMode::Default | TrimConfigMode::Disabled)),
        _ => false,
    };
    safe && (trim.warp_mode == 0
        || (trim.warp_mode == 3
            && trim.global_trim_mode == 2
            && modes == OBSERVED_ENCODER_TRIM_MODES))
}
pub fn validate_grid_common(common: OamdCommonData) -> Result<(), &'static str> {
    if !common.default_screen_size_ratio
        || common.master_screen_size_ratio_code.is_some()
        || common.bed_object_chan_distribute
        || common.bed_render_info != BedRenderInfo::default()
        || !direct_trim_is_supported(common.trim)
    {
        return Err("common uses unsupported screen, bed, warp or trim metadata");
    }
    Ok(())
}
