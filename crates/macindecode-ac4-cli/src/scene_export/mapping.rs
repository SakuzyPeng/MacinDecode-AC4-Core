//! 格式间映射告警与坐标/zone 换算。

use macindecode_ac4_bitstream::oamd::{
    AdditionalObjectMetadata, PositionCoding, QuantizedPosition, ZoneUpdate,
};

#[derive(Debug, Clone)]
pub(crate) struct MappingWarning {
    pub(crate) selector: String,
    pub(crate) sample: Option<i64>,
    pub(crate) field: &'static str,
    pub(crate) detail: String,
}

#[derive(Debug, Default)]
pub(crate) struct WarningSet {
    pub(crate) items: Vec<MappingWarning>,
}

impl WarningSet {
    pub(crate) fn push(
        &mut self,
        selector: &str,
        sample: Option<i64>,
        field: &'static str,
        detail: impl Into<String>,
    ) {
        let detail = detail.into();
        if self
            .items
            .iter()
            .any(|item| item.selector == selector && item.field == field && item.detail == detail)
        {
            return;
        }
        self.items.push(MappingWarning {
            selector: selector.to_owned(),
            sample,
            field,
            detail,
        });
    }
}

pub(crate) fn position(
    base: QuantizedPosition,
    additional: AdditionalObjectMetadata,
) -> (f64, f64, f64) {
    let semantic = |code: Option<u8>| match code {
        Some(0) => 1.0,
        Some(1) => 2.0,
        Some(2) => -1.0,
        Some(3) => -2.0,
        _ => 0.0,
    };
    let ext = additional.extended_position.unwrap_or_default();
    let x = f64::from(base.x) / 31.0 - 1.0 + semantic(ext.x) / 155.0;
    let y = 1.0 - f64::from(base.y) / 31.0 - semantic(ext.y) / 155.0;
    let z_extension = match base.coding {
        PositionCoding::AbsoluteNegative => -semantic(ext.z),
        PositionCoding::AbsolutePositive | PositionCoding::Differential => semantic(ext.z),
    };
    let z = f64::from(base.z) / 15.0 + z_extension / 75.0;
    (x.clamp(-1.0, 1.0), y.clamp(-1.0, 1.0), z.clamp(-1.0, 1.0))
}

pub(crate) fn zone_components(zone: ZoneUpdate) -> (bool, bool, u8) {
    let flag = zone.group_zone_flag.unwrap_or(0);
    let snap = !zone.grouped_defaults && flag & 0b001 != 0;
    let elevation = zone.grouped_defaults || flag & 0b010 == 0;
    (snap, elevation, zone.zone_mask.unwrap_or(0))
}
