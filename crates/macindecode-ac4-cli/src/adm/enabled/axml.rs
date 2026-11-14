//! OAMD 到通用母版值的映射；XML 与 CHNA 序列化由 macin-atmos 负责。
use super::*;
use macin_atmos::{Extent, Gain, MasterSpec, ObjectSpec, ObjectState, TrackId};

pub(super) fn build_axml(
    name: &str,
    metadata: &MetadataBatch,
    selected: &[SelectedObject],
    duration: u64,
    compatibility: AdmCompatibility,
    warnings: &mut WarningSet,
) -> Result<String, String> {
    build(
        name,
        metadata,
        selected,
        duration,
        OUTPUT_SAMPLE_RATE,
        compatibility,
        false,
        warnings,
    )
}
#[allow(clippy::too_many_arguments)]
pub(super) fn build_full_axml(
    name: &str,
    metadata: &MetadataBatch,
    selected: &[SelectedObject],
    duration: u64,
    sample_rate: u32,
    compatibility: AdmCompatibility,
    warnings: &mut WarningSet,
) -> Result<String, String> {
    build(
        name,
        metadata,
        selected,
        duration,
        sample_rate,
        compatibility,
        true,
        warnings,
    )
}
fn master_spec(
    name: &str,
    selected: &[SelectedObject],
    rate: u32,
    compatibility: AdmCompatibility,
    full: bool,
) -> MasterSpec {
    MasterSpec {
        name: name.to_owned(),
        sample_rate: rate,
        profile: compatibility.into(),
        bed_name: if full {
            "AC-4 full 7.1.2 bed (LFE essence; other channels silent)"
        } else {
            "Synthetic silent 7.1.2 bed"
        }
        .into(),
        bed_pack_name: if full {
            "AC-4 full 7.1.2 compatibility bed"
        } else {
            "Synthetic 7.1.2 bed"
        }
        .into(),
        objects: selected
            .iter()
            .map(|object| {
                ObjectSpec::new(
                    TrackId(u64::from(object.ordinal)),
                    if full {
                        format!("AC-4 full object {}", scene_selector(&object.scene))
                    } else {
                        format!("AC-4 {} pink probe", scene_selector(&object.scene))
                    },
                )
            })
            .collect(),
        ..MasterSpec::default()
    }
}
#[allow(clippy::too_many_arguments)]
fn build(
    name: &str,
    metadata: &MetadataBatch,
    selected: &[SelectedObject],
    duration: u64,
    rate: u32,
    compatibility: AdmCompatibility,
    full: bool,
    warnings: &mut WarningSet,
) -> Result<String, String> {
    let spec = master_spec(name, selected, rate, compatibility, full);
    let mut blocks = Vec::new();
    for object in selected {
        let events = project_metadata_events(metadata, &object.scene, rate, duration)?;
        let mut timeline = Vec::new();
        for (i, event) in events.iter().copied().enumerate() {
            let end = events
                .get(i.saturating_add(1))
                .map_or(duration, |e| e.sample);
            timeline.push(map_block(
                event,
                end.saturating_sub(event.sample),
                &scene_selector(&object.scene),
                warnings,
            )?);
        }
        blocks.push(timeline);
    }
    macin_atmos::metadata::build_axml(&spec, duration, &blocks).map_err(|e| e.to_string())
}
fn map_block(
    event: OutputEvent,
    duration: u64,
    selector: &str,
    warnings: &mut WarningSet,
) -> Result<macin_atmos::metadata::AdmBlock, String> {
    let render = event.state.render.ok_or_else(|| {
        format!(
            "Object {selector} lacks a complete render state at sample {}",
            event.sample
        )
    })?;
    let basic = event.state.basic.ok_or_else(|| {
        format!(
            "Object {selector} lacks a complete basic state at sample {}",
            event.sample
        )
    })?;
    let (x, y, z) = position(render.position, event.additional);
    let other = render.other_properties;
    let extent = match other.width {
        Some(WidthUpdate::Uniform(code)) if code <= 31 => {
            Some(Extent::Uniform(f64::from(code) / 31.))
        }
        Some(WidthUpdate::Cartesian { x, y, z }) if x <= 31 && y <= 31 && z <= 31 => {
            Some(Extent::Cartesian([
                f64::from(x) / 31.,
                f64::from(y) / 31.,
                f64::from(z) / 31.,
            ]))
        }
        Some(_) => {
            return Err(format!(
                "Object {selector} uses a width outside the five-bit range at sample {}",
                event.sample
            ));
        }
        None => None,
    };
    let (snap, elevation, zone) = zone_fields(render.zone);
    if !elevation {
        warnings.push(
            selector,
            i64::try_from(event.sample).ok(),
            "elevation",
            "The OAMD elevation constraint has no general ADM Objects equivalent",
        );
    }
    if zone != 0 {
        if zone > 6 {
            return Err(format!("Object {selector} uses reserved zone_mask {zone}"));
        }
        warnings.push(
            selector,
            i64::try_from(event.sample).ok(),
            "zones",
            "OAMD discrete rendering zones have no verified ADM zoneExclusion geometry equivalent",
        );
    }
    let mut ramp = event.ramp;
    if ramp > duration {
        warnings.push(selector,i64::try_from(event.sample).ok(),"ramp","OAMD ramp crosses the next event; ADM interpolationLength is truncated to the current block duration");
        ramp = duration;
    }
    if other.screen_factor_code.is_some() {
        warnings.push(
            selector,
            i64::try_from(event.sample).ok(),
            "screen_factor",
            "Continuous OAMD screen factor can only be reduced to the ADM screenRef Boolean",
        );
    }
    let priority = importance(basic.priority);
    if let ObjectPriorityState::Quantized(code) = basic.priority
        && u16::from(code).saturating_mul(10) != u16::from(priority).saturating_mul(31)
    {
        warnings.push(
            selector,
            i64::try_from(event.sample).ok(),
            "importance",
            "Five-bit OAMD priority was rounded to ADM importance 0..10",
        );
    }
    append_unmapped_event_warnings(selector, event, other, warnings)?;
    Ok(macin_atmos::metadata::AdmBlock {
        sample: event.sample,
        duration,
        ramp_samples: ramp,
        state: ObjectState {
            active: event.state.active,
            position: [x, y, z],
            gain: Gain::Linear(effective_gain(event.state.active, basic.gain)),
            extent,
            importance: f64::from(priority) / 10.,
            snap,
            screen_ref: other.screen_factor_code.is_some(),
            ..ObjectState::default()
        },
    })
}
pub(super) fn build_chna(selected: &[SelectedObject]) -> Result<Vec<u8>, String> {
    macin_atmos::metadata::chna(&master_spec(
        "Atmos_Master",
        selected,
        48000,
        AdmCompatibility::Standard,
        false,
    ))
    .and_then(|c| c.encode().map_err(Into::into))
    .map_err(|e| e.to_string())
}
pub(super) fn append_unmapped_event_warnings(
    selector: &str,
    event: OutputEvent,
    other: OtherPropertiesUpdate,
    warnings: &mut WarningSet,
) -> Result<(), String> {
    let sample = i64::try_from(event.sample).ok();
    if other.depth_factor.is_some() {
        warnings.push(
            selector,
            sample,
            "depth_factor",
            "OAMD depth factor is not an ADM Objects source depth extent and was not mapped",
        );
    }
    if other.object_at_infinity == Some(true) || other.distance_factor_code.is_some() {
        warnings.push(
            selector,
            sample,
            "distance",
            "Cartesian ADM position has no lossless equivalent for OAMD distance/infinity",
        );
    }
    if other.divergence_mode.is_some()
        || other.divergence_table.is_some()
        || other.divergence_code.is_some()
    {
        if other.divergence_mode == Some(3) {
            return Err(format!(
                "Object {selector} uses reserved object_div_mode 3 at sample {}",
                event.sample
            ));
        }
        if other.divergence_code == Some(0) {
            return Err(format!(
                "Object {selector} uses reserved object_div_code 0 at sample {}",
                event.sample
            ));
        }
        warnings.push(
            selector,
            sample,
            "divergence",
            "The OAMD divergence quantization table is not verified and was not mapped to ADM objectDivergence",
        );
    }
    if event.additional.trim_disabled {
        warnings.push(
            selector,
            sample,
            "trim",
            "Per-object trim bypass has no general ADM equivalent",
        );
    }
    if event.additional.headphone.is_some() {
        warnings.push(
            selector,
            sample,
            "headphone",
            "OAMD near/far and head-tracking modes cannot be converted losslessly to ADM headphoneVirtualise",
        );
    }
    Ok(())
}

pub(super) fn zone_fields(zone: ZoneUpdate) -> (bool, bool, u8) {
    zone_components(zone)
}

pub(super) fn effective_gain(active: bool, gain: ObjectGainState) -> f64 {
    if !active {
        return 0.0;
    }
    let db = match gain {
        ObjectGainState::Default => return 1.0,
        ObjectGainState::NegativeInfinity => return 0.0,
        ObjectGainState::Quantized(code) if code <= 14 => f64::from(15u8.saturating_sub(code)),
        ObjectGainState::Quantized(code) => 14.0 - f64::from(code),
    };
    10.0f64.powf(db / 20.0)
}

pub(super) fn importance(priority: ObjectPriorityState) -> u8 {
    match priority {
        ObjectPriorityState::Default => 10,
        ObjectPriorityState::Minimum => 0,
        ObjectPriorityState::Quantized(code) => {
            let scaled = u16::from(code).saturating_mul(10).saturating_add(15) / 31;
            u8::try_from(scaled).unwrap_or(10).min(10)
        }
    }
}
