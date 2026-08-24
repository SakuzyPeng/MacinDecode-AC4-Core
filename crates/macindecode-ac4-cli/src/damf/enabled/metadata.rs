//! DAMF 对象 metadata 生成。

use super::*;

pub(super) fn build_metadata(
    metadata: &MetadataBatch,
    selected: &[SelectedObject],
    duration: u64,
    warnings: &mut WarningSet,
) -> Result<String, String> {
    let mut lines = vec!["sampleRate: 48000".to_owned(), "events:".to_owned()];
    for (index, channel) in BED_CHANNELS.iter().enumerate() {
        lines.extend([
            format!("  - ID: {index}"),
            "    samplePos: 0".to_owned(),
            "    active: true".to_owned(),
            "    importance: 1".to_owned(),
            "    gain: 0".to_owned(),
            "    rampLength: 0".to_owned(),
            "    trimBypass: false".to_owned(),
            "    headTrackMode: scene relative".to_owned(),
            if *channel == "LFE" {
                "    binauralRenderMode: off".to_owned()
            } else {
                "    binauralRenderMode: undefined".to_owned()
            },
        ]);
    }

    for object in selected {
        let selector = scene_selector(&object.scene);
        let events = output_events(metadata, object, duration)?;
        for event in events {
            append_object_event(&mut lines, object, event, &selector, warnings)?;
        }
    }
    Ok(finish_yaml_lines(lines))
}

pub(super) fn output_events(
    metadata: &MetadataBatch,
    selected: &SelectedObject,
    duration: u64,
) -> Result<Vec<OutputEvent>, String> {
    project_metadata_events(metadata, &selected.scene, OUTPUT_SAMPLE_RATE, duration)
}

#[cfg(test)]
pub(super) fn default_inactive_event() -> OutputEvent {
    default_output_metadata_event()
}

pub(super) fn append_object_event(
    lines: &mut Vec<String>,
    object: &SelectedObject,
    event: OutputEvent,
    selector: &str,
    warnings: &mut WarningSet,
) -> Result<(), String> {
    let state = event.state;
    let render = state.render.ok_or_else(|| {
        format!(
            "对象 {selector} 在 sample {} 缺少完整 render 状态",
            event.sample
        )
    })?;
    let basic = state.basic.ok_or_else(|| {
        format!(
            "对象 {selector} 在 sample {} 缺少完整 basic 状态",
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
            "DAMF 对象事件没有等价的 distance/infinity 字段",
        );
    }
    if other.divergence_mode.is_some()
        || other.divergence_table.is_some()
        || other.divergence_code.is_some()
    {
        if other.divergence_mode == Some(3) {
            return Err(format!(
                "对象 {selector} 在 sample {} 使用保留 object_div_mode 3",
                event.sample
            ));
        }
        if other.divergence_code == Some(0) {
            return Err(format!(
                "对象 {selector} 在 sample {} 使用保留 object_div_code 0",
                event.sample
            ));
        }
        warnings.push(
            selector,
            i64::try_from(event.sample).ok(),
            "divergence",
            "DAMF decorr 与 AC-4 divergence 语义不同，decorr 保持 0",
        );
    }
    let gain = match basic.gain {
        ObjectGainState::Default => "0".to_owned(),
        ObjectGainState::NegativeInfinity => "-.inf".to_owned(),
        ObjectGainState::Quantized(code) if code <= 14 => {
            i16::from(15u8.saturating_sub(code)).to_string()
        }
        ObjectGainState::Quantized(code) => 14i16.saturating_sub(i16::from(code)).to_string(),
    };
    let importance = match basic.priority {
        ObjectPriorityState::Default => 1.0,
        ObjectPriorityState::Minimum => 0.0,
        ObjectPriorityState::Quantized(code) => f64::from(code) / 31.0,
    };
    if event
        .additional
        .headphone
        .is_some_and(|headphone| headphone.render_mode == 3)
    {
        warnings.push(
            selector,
            i64::try_from(event.sample).ok(),
            "binaural_render_mode",
            "AC-4 Mid 耳机模式不被 DAMF 0.5.1 接受，回落到 undefined",
        );
    }
    let (head_track, binaural) = headphone_fields(event.additional, object.scene.common);

    lines.extend([
        format!("  - ID: {}", object.damf_id),
        format!("    samplePos: {}", event.sample),
        format!("    active: {}", state.active),
        format!("    pos: [{}, {}, {}]", number(x), number(y), number(z)),
        format!("    snap: {snap}"),
        format!("    elevation: {elevation}"),
        format!("    zones: {zones}"),
    ]);
    match other.width {
        Some(WidthUpdate::Uniform(code)) => {
            lines.push(format!("    size: {}", number(f64::from(code) / 31.0)));
        }
        Some(WidthUpdate::Cartesian { x, y, z }) => lines.push(format!(
            "    size3D: [{}, {}, {}]",
            number(f64::from(x) / 31.0),
            number(f64::from(y) / 31.0),
            number(f64::from(z) / 31.0)
        )),
        None => lines.push("    size: 0".to_owned()),
    }
    let screen = other
        .screen_factor_code
        .map_or(0.0, |code| f64::from(code.saturating_add(1)) / 8.0);
    let depth = other.depth_factor.map_or(0.0, |code| {
        [0.25, 0.5, 1.0, 2.0]
            .get(usize::from(code))
            .copied()
            .unwrap_or(0.0)
    });
    lines.extend([
        "    decorr: 0".to_owned(),
        format!("    importance: {}", number(importance)),
        format!("    gain: {gain}"),
        format!("    rampLength: {}", event.ramp),
        format!("    trimBypass: {}", event.additional.trim_disabled),
        "    dialog: -1".to_owned(),
        "    music: -1".to_owned(),
        format!("    screenFactor: {}", number(screen)),
        format!("    depthFactor: {}", number(depth)),
        format!("    headTrackMode: {head_track}"),
        format!("    binauralRenderMode: {binaural}"),
    ]);
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
                "AC-4 only-proscenium 没有 DAMF zones 等价值，回落到 all",
            );
            "all"
        }
        value => return Err(format!("对象 {selector} 使用保留 zone_mask {value}")),
    };
    Ok((snap, elevation, zones))
}

pub(super) fn headphone_fields(
    additional: AdditionalObjectMetadata,
    common: Option<macindecode_ac4_bitstream::oamd::OamdCommonData>,
) -> (&'static str, &'static str) {
    if let Some(headphone) = additional.headphone {
        return (
            if headphone.head_tracking_disabled {
                "head relative"
            } else {
                "scene relative"
            },
            match headphone.render_mode {
                0 => "off",
                1 => "near",
                2 => "far",
                3 => "undefined",
                _ => "undefined",
            },
        );
    }
    let Some(headphone) = common
        .map(|common| common.headphone)
        .filter(|item| item.present)
    else {
        return ("scene relative", "undefined");
    };
    (
        if headphone.head_track_disable_all == Some(true) {
            "head relative"
        } else {
            "scene relative"
        },
        match headphone.hp_operation_mode {
            0 => "off",
            1 => "near",
            2 => "far",
            _ => "undefined",
        },
    )
}
