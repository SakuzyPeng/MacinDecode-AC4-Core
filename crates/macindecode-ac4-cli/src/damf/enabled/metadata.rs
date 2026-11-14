//! DAMF 对象 metadata 生成。

use super::*;
use crate::metadata_batch::{project_damf_object_events, project_headphone_events};
use macindecode_ac4_scene::{HeadTrackingPolicy, HeadphonePolicyState, HeadphoneRenderMode};

pub(super) fn build_metadata(
    metadata: &MetadataBatch,
    selected: &[SelectedObject],
    duration: u64,
    warnings: &mut WarningSet,
) -> Result<String, String> {
    let mut lines = vec!["sampleRate: 48000".to_owned(), "events:".to_owned()];
    lines.extend(
        macin_atmos::metadata::damf_bed_events()
            .lines()
            .map(str::to_owned),
    );

    for object in selected {
        let selector = scene_selector(&object.scene);
        let mut events = output_events(metadata, object, duration)?
            .into_iter()
            .peekable();
        let mut policies =
            project_headphone_events(metadata, &object.scene, OUTPUT_SAMPLE_RATE, duration)?
                .into_iter()
                .peekable();
        let mut policy = HeadphonePolicyState::Unspecified;
        let mut previous_fields = None;
        loop {
            let sample = match (events.peek(), policies.peek()) {
                (Some(event), Some(headphone)) => event.sample.min(headphone.sample),
                (Some(event), None) => event.sample,
                (None, Some(headphone)) => headphone.sample,
                (None, None) => break,
            };
            while policies.peek().is_some_and(|event| event.sample == sample) {
                policy = policies.next().ok_or("Missing headphone event")?.policy;
            }
            let fields = headphone_fields(policy)
                .map_err(|error| format!("Object {selector} at sample {sample}: {error}"))?;
            if events.peek().is_some_and(|event| event.sample == sample) {
                let event = events.next().ok_or("Missing object event")?;
                append_object_event(&mut lines, object, event, policy, &selector, warnings)?;
            } else {
                // 有效策略即使映射为相同字段，也可能有损；警告不能随字段一起去重。
                headphone_warning(policy, &selector, sample, warnings);
                if previous_fields != Some(fields) {
                    let mut patch = macin_atmos::ObjectPatch::default();
                    if previous_fields.is_none_or(|previous: (&str, &str)| previous.0 != fields.0) {
                        patch.head_tracking = Some(tracking_value(fields.0));
                    }
                    if previous_fields.is_none_or(|previous| previous.1 != fields.1) {
                        patch.binaural_mode = Some(binaural_value(fields.1));
                    }
                    lines.extend(
                        macin_atmos::metadata::damf_event(object.damf_id, sample, patch, None)
                            .map_err(|e| e.to_string())?
                            .lines()
                            .map(str::to_owned),
                    );
                    // 局部事件不写 rampLength、位置或增益；未改变字段继续原来的 ramp。
                }
            }
            previous_fields = Some(fields);
        }
    }
    Ok(finish_yaml_lines(lines))
}

pub(super) fn output_events(
    metadata: &MetadataBatch,
    selected: &SelectedObject,
    duration: u64,
) -> Result<Vec<OutputEvent>, String> {
    project_damf_object_events(metadata, &selected.scene, OUTPUT_SAMPLE_RATE, duration)
}

#[cfg(test)]
pub(super) fn default_inactive_event() -> OutputEvent {
    default_output_metadata_event()
}

pub(super) fn append_object_event(
    lines: &mut Vec<String>,
    object: &SelectedObject,
    event: OutputEvent,
    policy: HeadphonePolicyState,
    selector: &str,
    warnings: &mut WarningSet,
) -> Result<(), String> {
    let state = event.state;
    let render = state.render.ok_or_else(|| {
        format!(
            "Object {selector} lacks a complete render state at sample {}",
            event.sample
        )
    })?;
    let basic = state.basic.ok_or_else(|| {
        format!(
            "Object {selector} lacks a complete basic state at sample {}",
            event.sample
        )
    })?;
    let (x, y, z) = position(render.position, event.additional);
    let (snap, elevation, zones) = zone_fields(
        render.zone,
        selector,
        i64::try_from(event.sample).ok(),
        warnings,
    )?;
    let other = render.other_properties;
    if other.object_at_infinity == Some(true) || other.distance_factor_code.is_some() {
        warnings.push(
            selector,
            i64::try_from(event.sample).ok(),
            "distance",
            "DAMF object events have no equivalent distance/infinity field",
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
            i64::try_from(event.sample).ok(),
            "divergence",
            "DAMF decorr and AC-4 divergence have different semantics; decorr remains 0",
        );
    }
    let gain = match basic.gain {
        ObjectGainState::Default => macin_atmos::Gain::Decibels(0.),
        ObjectGainState::NegativeInfinity => macin_atmos::Gain::Silence,
        ObjectGainState::Quantized(code) if code <= 14 => {
            macin_atmos::Gain::Decibels(f64::from(15u8.saturating_sub(code)))
        }
        ObjectGainState::Quantized(code) => macin_atmos::Gain::Decibels(14. - f64::from(code)),
    };
    let importance = match basic.priority {
        ObjectPriorityState::Default => 1.0,
        ObjectPriorityState::Minimum => 0.0,
        ObjectPriorityState::Quantized(code) => f64::from(code) / 31.0,
    };
    headphone_warning(policy, selector, event.sample, warnings);
    let (head_track, binaural) = headphone_fields(policy)
        .map_err(|error| format!("Object {selector} at sample {}: {error}", event.sample))?;

    let extent = match other.width {
        Some(WidthUpdate::Uniform(code)) => {
            Some(macin_atmos::Extent::Uniform(f64::from(code) / 31.))
        }
        Some(WidthUpdate::Cartesian { x, y, z }) => Some(macin_atmos::Extent::Cartesian([
            f64::from(x) / 31.,
            f64::from(y) / 31.,
            f64::from(z) / 31.,
        ])),
        None => None,
    };
    let screen = other
        .screen_factor_code
        .map_or(0.0, |code| f64::from(code.saturating_add(1)) / 8.0);
    let depth = other.depth_factor.map_or(0.0, |code| {
        [0.25, 0.5, 1.0, 2.0]
            .get(usize::from(code))
            .copied()
            .unwrap_or(0.0)
    });
    let mapped = macin_atmos::ObjectState {
        active: state.active,
        position: [x, y, z],
        gain,
        extent,
        snap,
        elevation,
        zone: match zones {
            "no back" => macin_atmos::Zone::NoBack,
            "no sides" => macin_atmos::Zone::NoSides,
            "center back" => macin_atmos::Zone::CenterBack,
            "screen only" => macin_atmos::Zone::ScreenOnly,
            "surround only" => macin_atmos::Zone::SurroundOnly,
            _ => macin_atmos::Zone::All,
        },
        importance,
        trim_bypass: event.additional.trim_disabled,
        screen_factor: screen,
        depth_factor: depth,
        head_tracking: tracking_value(head_track),
        binaural_mode: binaural_value(binaural),
        ..Default::default()
    };
    lines.extend(
        macin_atmos::metadata::damf_event(
            object.damf_id,
            event.sample,
            macin_atmos::ObjectPatch::full(mapped),
            Some(event.ramp),
        )
        .map_err(|e| e.to_string())?
        .lines()
        .map(str::to_owned),
    );
    Ok(())
}

pub(super) fn zone_fields(
    zone: ZoneUpdate,
    selector: &str,
    sample: Option<i64>,
    warnings: &mut WarningSet,
) -> Result<(bool, bool, &'static str), String> {
    let (snap, elevation, mask) = zone_components(zone);
    let zones = match mask {
        0 => "all",
        1 => "no back",
        2 => "no sides",
        3 => "center back",
        4 => "screen only",
        5 => "surround only",
        6 => {
            warnings.push(
                selector,
                sample,
                "zones",
                "AC-4 only-proscenium has no DAMF zones equivalent; falling back to all",
            );
            "all"
        }
        value => return Err(format!("Object {selector} uses reserved zone_mask {value}")),
    };
    Ok((snap, elevation, zones))
}

pub(super) fn headphone_fields(
    policy: HeadphonePolicyState,
) -> Result<(&'static str, &'static str), String> {
    let policy = match policy {
        HeadphonePolicyState::Resolved(policy) => policy,
        HeadphonePolicyState::Unspecified => return Ok(("scene relative", "undefined")),
        HeadphonePolicyState::Unsupported(issue) => return Err(issue.to_string()),
        _ => return Err("Unsupported headphone policy state".to_owned()),
    };
    let tracking = match policy.head_tracking() {
        HeadTrackingPolicy::SceneRelative => "scene relative",
        HeadTrackingPolicy::HeadRelative => "head relative",
        _ => return Err("Unsupported head-tracking policy".to_owned()),
    };
    let render = match policy.render_mode() {
        HeadphoneRenderMode::Bypass => "off",
        HeadphoneRenderMode::Near => "near",
        HeadphoneRenderMode::Far => "far",
        HeadphoneRenderMode::Mid => "undefined",
        _ => return Err("Unsupported headphone render mode".to_owned()),
    };
    Ok((tracking, render))
}

fn headphone_warning(
    policy: HeadphonePolicyState,
    selector: &str,
    sample: u64,
    warnings: &mut WarningSet,
) {
    if matches!(policy, HeadphonePolicyState::Resolved(policy) if policy.render_mode() == HeadphoneRenderMode::Mid)
    {
        warnings.push(
            selector,
            i64::try_from(sample).ok(),
            "binaural_render_mode",
            "DAMF does not accept AC-4 Mid headphone mode; falling back to undefined",
        );
    }
}

fn tracking_value(value: &str) -> macin_atmos::HeadTracking {
    if value == "head relative" {
        macin_atmos::HeadTracking::HeadRelative
    } else {
        macin_atmos::HeadTracking::SceneRelative
    }
}
fn binaural_value(value: &str) -> macin_atmos::BinauralMode {
    match value {
        "off" => macin_atmos::BinauralMode::Off,
        "near" => macin_atmos::BinauralMode::Near,
        "far" => macin_atmos::BinauralMode::Far,
        _ => macin_atmos::BinauralMode::Undefined,
    }
}
