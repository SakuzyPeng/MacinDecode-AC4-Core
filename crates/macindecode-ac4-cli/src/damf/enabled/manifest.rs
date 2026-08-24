//! DAMF manifest 生成。

use super::*;

pub(super) fn build_manifest(
    stem: &str,
    fps: &str,
    selected: &[SelectedObject],
    presentation_type: DamfPresentationType,
    essence: DamfEssence,
    warnings: &mut WarningSet,
) -> String {
    let common = selected.first().and_then(|item| item.scene.common);
    if selected.iter().any(|item| item.scene.common != common) {
        warnings.push(
            "presentation",
            None,
            "common",
            "所选 A-JOC 子流的 presentation 级 OAMD common 不一致，manifest 使用首个值",
        );
    }
    if selected.iter().any(|item| item.scene.common_conflict) {
        warnings.push(
            "presentation",
            None,
            "common",
            "输入时间线中的 OAMD common 发生变化，manifest 使用首个稳定值",
        );
    }
    let mut screen_ratio = 1.0;
    let mut bed_distribution = false;
    let mut warp_mode = "LoRo";
    if let Some(common) = common {
        screen_ratio = common
            .master_screen_size_ratio_code
            .map_or(1.0, |code| f64::from(code.saturating_add(1)) / 33.0);
        bed_distribution = common.bed_object_chan_distribute;
        if common.trim.present {
            warp_mode = match common.trim.warp_mode {
                0 => "None",
                1 => "LoRo",
                _ => "LoRo",
            };
        }
        if common.trim.present && (common.trim.warp_mode > 1 || common.trim.global_trim_mode != 0) {
            warnings.push(
                "presentation",
                None,
                "trim",
                "AC-4 的九配置 trim 与 DAMF 四配置 trimMode 不存在无损的一一对应，保留 DAMF 默认值",
            );
        }
        if common.bed_render_info.present {
            warnings.push(
                "presentation",
                None,
                "bed_render_info",
                "AC-4 bed render 工具没有等价的 DAMF manifest 表达，保留兼容默认值",
            );
        }
    }

    let creation_tool = match essence {
        DamfEssence::Probe => "MacinDecode-AC4-Core synthetic OAMD probe",
        DamfEssence::Full { .. } => "MacinDecode-AC4-Core full A-JOC",
    };
    let bed_description = match essence {
        DamfEssence::Probe => "Synthetic silent 7.1.2 bed".to_owned(),
        DamfEssence::Full { has_lfe: true } => {
            "AC-4 full 7.1.2 compatibility bed (LFE essence; other channels silent)".to_owned()
        }
        DamfEssence::Full { has_lfe: false } => {
            "AC-4 full 7.1.2 compatibility bed (silent)".to_owned()
        }
    };
    let mut lines = vec![
        format!("version: {}", presentation_type.version()),
        "presentations:".to_owned(),
        format!("  - type: {}", presentation_type.as_str()),
        "    simplified: false".to_owned(),
        format!(
            "    metadata: {}",
            yaml_quote(&format!("{stem}.atmos.metadata"))
        ),
        format!("    audio: {}", yaml_quote(&format!("{stem}.atmos.audio"))),
        "    offset: 0".to_owned(),
        format!("    fps: {fps}"),
        "    scBedConfiguration: [3]".to_owned(),
        format!("    creationTool: {creation_tool}"),
        format!("    creationToolVersion: {}", env!("CARGO_PKG_VERSION")),
        "    downmixType_5to2: LoRo_Stereo".to_owned(),
        "    51-to-20_LsRs90degPhaseShift: false".to_owned(),
        format!("    warpMode: {warp_mode}"),
        format!("    screenSizeRatio: {}", number(screen_ratio)),
        format!("    bedDistribution: {bed_distribution}"),
        "    trimMode:".to_owned(),
        "      SomeSurroundsNoHeights:".to_owned(),
        "        {}".to_owned(),
        "      SomeSurroundsSomeHeights:".to_owned(),
        "        {}".to_owned(),
        "      SomeSurroundsManyHeights:".to_owned(),
        "        {}".to_owned(),
        "      ManySurroundsNoHeights:".to_owned(),
        "        {}".to_owned(),
        "    bedInstances:".to_owned(),
        match essence {
            DamfEssence::Probe => format!("      - description: {bed_description}"),
            DamfEssence::Full { .. } => {
                format!("      - description: {}", yaml_quote(&bed_description))
            }
        },
        "        channels:".to_owned(),
    ];
    for (index, channel) in BED_CHANNELS.iter().enumerate() {
        lines.push(format!("          - channel: {channel}"));
        lines.push(format!("            ID: {index}"));
    }
    lines.push("    objects:".to_owned());
    for object in selected {
        let description = match essence {
            DamfEssence::Probe => format!("AC-4 {} pink probe", scene_selector(&object.scene)),
            DamfEssence::Full { .. } => {
                format!("AC-4 full object {}", scene_selector(&object.scene))
            }
        };
        lines.push(format!("      - description: {}", yaml_quote(&description)));
        lines.push(format!("        ID: {}", object.damf_id));
    }
    finish_yaml_lines(lines)
}
