//! DAMF manifest 生成。

use super::*;

pub(super) fn build_manifest(
    stem: &str,
    fps: &str,
    selected: &[SelectedObject],
    presentation_type: DamfPresentationType,
    essence: DamfEssence,
    warnings: &mut WarningSet,
) -> Result<String, String> {
    let common = selected.first().and_then(|item| item.scene.common);
    if selected.iter().any(|item| item.scene.common != common) {
        warnings.push(
            "presentation",
            None,
            "common",
            "Selected A-JOC substreams have inconsistent presentation-level OAMD common metadata; the manifest uses the first value",
        );
    }
    if selected.iter().any(|item| item.scene.common_conflict) {
        warnings.push(
            "presentation",
            None,
            "common",
            "OAMD common changes over the input timeline; the manifest uses the first stable value",
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
                "AC-4 nine-configuration trim has no lossless one-to-one mapping to DAMF four-configuration trimMode; keeping the DAMF default",
            );
        }
        if common.bed_render_info.present {
            warnings.push(
                "presentation",
                None,
                "bed_render_info",
                "The AC-4 bed-render tool has no equivalent DAMF manifest representation; keeping the compatible default",
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
    let objects = selected
        .iter()
        .map(|object| {
            macin_atmos::ObjectSpec::new(
                macin_atmos::TrackId(u64::from(object.damf_id)),
                match essence {
                    DamfEssence::Probe => {
                        format!("AC-4 {} pink probe", scene_selector(&object.scene))
                    }
                    DamfEssence::Full { .. } => {
                        format!("AC-4 full object {}", scene_selector(&object.scene))
                    }
                },
            )
        })
        .collect();
    let frame_rate = match fps {
        "23.976" => macin_atmos::FrameRate::Fps23976,
        "24" => macin_atmos::FrameRate::Fps24,
        "25" => macin_atmos::FrameRate::Fps25,
        "29.97" => macin_atmos::FrameRate::Fps2997,
        "29.97df" => macin_atmos::FrameRate::Fps2997Drop,
        "30" => macin_atmos::FrameRate::Fps30,
        _ => return Err(format!("Unsupported master frame rate {fps}")),
    };
    let spec = macin_atmos::MasterSpec {
        stem: stem.to_owned(),
        frame_rate,
        profile: presentation_type.into(),
        objects,
        bed_name: bed_description,
        creation_tool: creation_tool.to_owned(),
        creation_tool_version: env!("CARGO_PKG_VERSION").to_owned(),
        screen_size_ratio: screen_ratio,
        bed_distribution,
        warp_mode: if warp_mode == "None" {
            macin_atmos::WarpMode::None
        } else {
            macin_atmos::WarpMode::LoRo
        },
        ..Default::default()
    };
    macin_atmos::metadata::damf_manifest(&spec).map_err(|e| e.to_string())
}
