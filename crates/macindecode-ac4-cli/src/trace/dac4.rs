//! `dac4` 选择信令 trace，以及它与首帧 TOC 的独立交叉核对。
//!
//! DSI 只用于检视与能力选择，不能配置解码器。这里保留两边各自的值，再按
//! effective presentation ID 关联；数组顺序不是身份。双方各自只有一个无 ID
//! presentation 时才允许唯一回退，多个无 ID presentation 一律保持未匹配。

use macindecode_ac4_bitstream::{
    presentation::presentation_config_label, substream::SubstreamInfo, topology::Ac4Topology,
};
use macindecode_ac4_mp4::{
    Ac4BitrateDsi, Ac4BitrateMode, Ac4Dsi, Ac4DsiBytes, Ac4DsiChannelGroups, Ac4DsiEmdfInfo,
    Ac4DsiPresentationV1, Ac4DsiSubstream, Ac4DsiSubstreamGroup, Ac4DsiV1,
};
use serde_json::{Value, json};

#[derive(Debug, Clone, PartialEq, Eq)]
enum SubstreamSignature {
    Channel,
    Ajoc {
        static_downmix: bool,
        downmix_objects: u32,
        upmix_objects: u32,
    },
    DirectObject,
}

impl SubstreamSignature {
    const fn path(&self) -> &'static str {
        match self {
            Self::Channel => "channel",
            Self::Ajoc { .. } => "ajoc",
            Self::DirectObject => "direct_object",
        }
    }

    fn to_json(&self) -> Value {
        match *self {
            Self::Channel => json!({"path": self.path()}),
            Self::Ajoc {
                static_downmix,
                downmix_objects,
                upmix_objects,
            } => json!({
                "path": self.path(),
                "static_downmix": static_downmix,
                "downmix_objects": downmix_objects,
                "upmix_objects": upmix_objects,
            }),
            Self::DirectObject => json!({"path": self.path()}),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GroupSignature {
    channel_coded: bool,
    substreams: Vec<SubstreamSignature>,
}

impl GroupSignature {
    fn path(&self) -> &'static str {
        if self.channel_coded {
            return "channel";
        }
        let has_ajoc = self
            .substreams
            .iter()
            .any(|item| matches!(item, SubstreamSignature::Ajoc { .. }));
        let has_direct = self
            .substreams
            .iter()
            .any(|item| matches!(item, SubstreamSignature::DirectObject));
        match (has_ajoc, has_direct) {
            (true, true) => "mixed_object",
            (true, false) => "ajoc",
            (false, true) => "direct_object",
            (false, false) => "object_empty",
        }
    }

    fn to_json(&self) -> Value {
        json!({
            "path": self.path(),
            "substreams": self
                .substreams
                .iter()
                .map(SubstreamSignature::to_json)
                .collect::<Vec<_>>(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PresentationSummary {
    index: usize,
    presentation_id: Option<u32>,
    version: u32,
    details_available: bool,
    md_compat: Option<u8>,
    alternative: Option<bool>,
    groups: Option<Vec<GroupSignature>>,
}

impl PresentationSummary {
    fn reference_json(&self) -> Value {
        json!({
            "index": self.index,
            "presentation_id": self.presentation_id,
            "version": self.version,
        })
    }

    fn details_json(&self) -> Value {
        json!({
            "version": self.version,
            "md_compat": self.md_compat,
            "alternative": self.alternative,
            "groups": self.groups.as_ref().map(|groups| {
                groups.iter().map(GroupSignature::to_json).collect::<Vec<_>>()
            }),
        })
    }
}

/// 展开 `dac4`，并在有首帧完整 TOC 时生成 presentation 级一致性报告。
pub(super) fn describe(
    dsi: &Ac4Dsi<'_>,
    payload_bytes: usize,
    first_topology: Option<&Ac4Topology>,
    first_topology_error: Option<&str>,
) -> Result<Value, String> {
    let Some(v1) = dsi.v1().map_err(|error| error.to_string())? else {
        return Ok(json!({
            "payload_bytes": payload_bytes,
            "ac4_dsi_version": dsi.dsi_version,
            "bitstream_version": dsi.bitstream_version,
            "fs_index": dsi.fs_index,
            "base_sampling_frequency": dsi.base_sampling_frequency.hz(),
            "frame_rate_index": dsi.frame_rate_index,
            "n_presentations": dsi.n_presentations,
            "presentation_bytes": dsi.presentation_bytes.len(),
            "program_id": null,
            "stream_bitrate": null,
            "presentations": null,
            "first_toc_comparison": unavailable_comparison(
                "dac4 DSI version 0 has no presentation v1 view",
            ),
        }));
    };

    let (presentations, dsi_summaries) = collect_presentations(v1)?;
    let comparison = match first_topology {
        Some(topology) => compare_summaries(&dsi_summaries, &toc_summaries(topology)),
        None => unavailable_comparison(
            first_topology_error.unwrap_or("MP4 sample table contains no first frame"),
        ),
    };

    Ok(json!({
        "payload_bytes": payload_bytes,
        "ac4_dsi_version": dsi.dsi_version,
        "bitstream_version": dsi.bitstream_version,
        "fs_index": dsi.fs_index,
        "base_sampling_frequency": dsi.base_sampling_frequency.hz(),
        "frame_rate_index": dsi.frame_rate_index,
        "n_presentations": dsi.n_presentations,
        "presentation_bytes": dsi.presentation_bytes.len(),
        "presentation_array_bytes": v1.presentation_bytes().len(),
        "program_id": v1.program_id.map(program_id_json),
        "stream_bitrate": bitrate_json(v1.bitrate),
        "presentations": presentations,
        "first_toc_comparison": comparison,
    }))
}

fn unavailable_comparison(reason: &str) -> Value {
    json!({
        "available": false,
        "reason": reason,
        "consistent": null,
    })
}

fn program_id_json(program: macindecode_ac4_mp4::Ac4ProgramId) -> Value {
    json!({
        "short_id": program.short_id,
        "uuid": program.uuid,
    })
}

fn bitrate_mode_label(mode: Ac4BitrateMode) -> &'static str {
    match mode {
        Ac4BitrateMode::Unspecified => "unspecified",
        Ac4BitrateMode::Constant => "constant",
        Ac4BitrateMode::Average => "average",
        Ac4BitrateMode::Variable => "variable",
    }
}

fn bitrate_json(bitrate: Ac4BitrateDsi) -> Value {
    let precision = if bitrate.precision_unknown() {
        Value::Null
    } else {
        json!(bitrate.precision)
    };
    json!({
        "mode": {
            "code": bitrate.mode.code(),
            "name": bitrate_mode_label(bitrate.mode),
        },
        "bit_rate": bitrate.bit_rate,
        "precision": precision,
    })
}

fn emdf_json(info: Ac4DsiEmdfInfo) -> Value {
    json!({"version": info.version, "key_id": info.key_id})
}

fn bytes_json(bytes: Ac4DsiBytes<'_>) -> Value {
    json!({
        "length": bytes.len(),
        "values": bytes.iter().collect::<Vec<_>>(),
    })
}

fn channel_groups_json(groups: Ac4DsiChannelGroups) -> Value {
    let indices = (0u8..18)
        .filter(|&index| groups.contains(index))
        .collect::<Vec<_>>();
    json!({"raw": groups.raw(), "indices": indices})
}

fn collect_presentations(
    v1: Ac4DsiV1<'_>,
) -> Result<(Vec<Value>, Vec<PresentationSummary>), String> {
    let mut values = Vec::with_capacity(usize::from(v1.n_presentations()));
    let mut summaries = Vec::with_capacity(usize::from(v1.n_presentations()));
    for item in v1.presentations() {
        let envelope = item.map_err(|error| error.to_string())?;
        let Some(presentation) = envelope.v1().map_err(|error| error.to_string())? else {
            values.push(json!({
                "index": envelope.index,
                "version": envelope.version,
                "declared_bytes": envelope.declared_bytes,
                "details": null,
            }));
            summaries.push(PresentationSummary {
                index: usize::from(envelope.index),
                presentation_id: None,
                version: u32::from(envelope.version),
                details_available: false,
                md_compat: None,
                alternative: None,
                groups: None,
            });
            continue;
        };
        let (details, summary) = presentation_json(presentation, envelope.version)?;
        values.push(json!({
            "index": envelope.index,
            "version": envelope.version,
            "declared_bytes": envelope.declared_bytes,
            "details": details,
        }));
        summaries.push(summary);
    }
    Ok((values, summaries))
}

fn presentation_json(
    presentation: Ac4DsiPresentationV1<'_>,
    version: u8,
) -> Result<(Value, PresentationSummary), String> {
    let mut group_values = Vec::with_capacity(usize::from(presentation.n_substream_groups));
    let mut groups = Vec::with_capacity(usize::from(presentation.n_substream_groups));
    for item in presentation.substream_groups() {
        let group = item.map_err(|error| error.to_string())?;
        let (value, signature) = group_json(group)?;
        group_values.push(value);
        groups.push(signature);
    }

    let additional_emdf = presentation
        .additional_emdf()
        .map(emdf_json)
        .collect::<Vec<_>>();
    let alternative = presentation.alternative.map(|info| {
        json!({
            "presentation_name": {
                "utf8": info.presentation_name_utf8().ok(),
                "bytes": info.presentation_name(),
            },
            "targets": info.targets().map(|target| json!({
                "md_compat": target.md_compat,
                "device_category": target.device_category,
            })).collect::<Vec<_>>(),
        })
    });
    let indicators = presentation.indicators.map(|value| {
        json!({
            "dialogue_enhancement": value.dialogue_enhancement,
            "immersive_audio": value.immersive_audio,
            "reserved": value.reserved,
            "extended_presentation_id": value.extended_presentation_id,
            "reserved_id_bit": value.reserved_id_bit,
        })
    });
    let channel_layout = presentation.channel_layout.map(|value| {
        json!({
            "channel_mode": value.channel_mode,
            "four_back_channels_present": value.four_back_channels_present,
            "top_channel_pairs": value.top_channel_pairs,
            "channel_groups": channel_groups_json(value.channel_groups),
        })
    });
    let core_layout = presentation.core_layout.map(|value| {
        json!({
            "channel_coded": value.channel_coded,
            "channel_mode": value.channel_mode,
        })
    });
    let filter = presentation.filter.map(|value| {
        json!({
            "enabled": value.enabled,
            "data": bytes_json(value.data),
        })
    });
    let presentation_emdf = presentation.presentation_emdf.map(emdf_json);
    let config_extension = presentation.config_extension.map(bytes_json);
    let presentation_bitrate = presentation.bitrate.map(bitrate_json);

    let details = json!({
        "presentation_config": {
            "value": presentation.presentation_config,
            "role": if presentation.presentation_config == 31 {
                "single_group"
            } else {
                presentation_config_label(u32::from(presentation.presentation_config))
            },
        },
        "md_compat": presentation.md_compat,
        "presentation_id": presentation.presentation_id,
        "effective_presentation_id": presentation.effective_presentation_id(),
        "frame_rate_multiply_info": presentation.frame_rate_multiply_info,
        "frame_rate_fraction_info": presentation.frame_rate_fraction_info,
        "presentation_emdf": presentation_emdf,
        "channel_layout": channel_layout,
        "core_layout": core_layout,
        "filter": filter,
        "multi_pid": presentation.multi_pid,
        "config_extension": config_extension,
        "substream_groups": group_values,
        "pre_virtualized": presentation.pre_virtualized,
        "additional_emdf": additional_emdf,
        "bitrate": presentation_bitrate,
        "alternative": alternative,
        "alignment": {
            "before_alternative_bits": presentation.alternative_align_bits,
            "end_bits": presentation.end_align_bits,
        },
        "indicators": indicators,
        "skip_area": bytes_json(presentation.skip_area),
    });
    let summary = PresentationSummary {
        index: usize::from(presentation.index),
        presentation_id: presentation.effective_presentation_id().map(u32::from),
        version: u32::from(version),
        details_available: true,
        md_compat: presentation.md_compat,
        alternative: Some(presentation.alternative.is_some()),
        groups: Some(groups),
    };
    Ok((details, summary))
}

fn group_json(group: Ac4DsiSubstreamGroup<'_>) -> Result<(Value, GroupSignature), String> {
    let mut values = Vec::with_capacity(usize::from(group.n_substreams));
    let mut substreams = Vec::with_capacity(usize::from(group.n_substreams));
    for item in group.substreams() {
        let substream = item.map_err(|error| error.to_string())?;
        let (value, signature) = match substream {
            Ac4DsiSubstream::Channel(info) => (
                json!({
                    "path": "channel",
                    "sampling_frequency_multiplier": info.sampling_frequency_multiplier,
                    "bitrate_indicator": info.bitrate_indicator,
                    "channel_groups": channel_groups_json(info.channel_groups),
                }),
                SubstreamSignature::Channel,
            ),
            Ac4DsiSubstream::Object(info) => match info.ajoc {
                Some(ajoc) => {
                    let downmix_objects = ajoc.downmix_objects.map_or(5, u32::from);
                    (
                        json!({
                            "path": "ajoc",
                            "sampling_frequency_multiplier": info.sampling_frequency_multiplier,
                            "bitrate_indicator": info.bitrate_indicator,
                            "static_downmix": ajoc.static_downmix,
                            "downmix_objects": downmix_objects,
                            "upmix_objects": ajoc.upmix_objects,
                            "object_kinds": object_kinds_json(info.object_kinds),
                        }),
                        SubstreamSignature::Ajoc {
                            static_downmix: ajoc.static_downmix,
                            downmix_objects,
                            upmix_objects: u32::from(ajoc.upmix_objects),
                        },
                    )
                }
                None => (
                    json!({
                        "path": "direct_object",
                        "sampling_frequency_multiplier": info.sampling_frequency_multiplier,
                        "bitrate_indicator": info.bitrate_indicator,
                        "object_kinds": object_kinds_json(info.object_kinds),
                    }),
                    SubstreamSignature::DirectObject,
                ),
            },
        };
        values.push(value);
        substreams.push(signature);
    }

    let signature = GroupSignature {
        channel_coded: group.channel_coded,
        substreams,
    };
    let content_type = group.content_type.map(|value| {
        json!({
            "classifier": value.classifier,
            "language_tag": value.language_tag.map(bytes_json),
        })
    });
    let value = json!({
        "index": group.index,
        "path": signature.path(),
        "substreams_present": group.substreams_present,
        "hsf_ext": group.hsf_ext,
        "channel_coded": group.channel_coded,
        "n_substreams": group.n_substreams,
        "content_type": content_type,
        "substreams": values,
    });
    Ok((value, signature))
}

fn object_kinds_json(kinds: macindecode_ac4_mp4::Ac4DsiObjectKinds) -> Value {
    json!({
        "bed": kinds.bed,
        "dynamic": kinds.dynamic,
        "isf": kinds.isf,
        "reserved": kinds.reserved,
    })
}

fn toc_summaries(topology: &Ac4Topology) -> Vec<PresentationSummary> {
    topology
        .presentations()
        .iter()
        .enumerate()
        .map(|(index, presentation)| {
            let groups = presentation
                .group_indices()
                .iter()
                .map(|&group_index| {
                    usize::try_from(group_index)
                        .ok()
                        .and_then(|position| topology.groups().get(position))
                        .map(toc_group_signature)
                })
                .collect::<Option<Vec<_>>>();
            PresentationSummary {
                index,
                presentation_id: presentation.presentation_id,
                version: presentation.presentation_version,
                details_available: true,
                md_compat: presentation.md_compat,
                alternative: presentation
                    .substream
                    .map(|substream| substream.alternative),
                groups,
            }
        })
        .collect()
}

fn toc_group_signature(
    group: &macindecode_ac4_bitstream::substream::Ac4SubstreamGroupInfo,
) -> GroupSignature {
    let substreams = group
        .substreams()
        .iter()
        .map(|substream| match *substream {
            SubstreamInfo::Chan(_) => SubstreamSignature::Channel,
            SubstreamInfo::Ajoc(ref info) => SubstreamSignature::Ajoc {
                static_downmix: info.static_dmx,
                downmix_objects: info.n_dmx_signals,
                upmix_objects: info.n_upmix_signals,
            },
            SubstreamInfo::Obj(_) => SubstreamSignature::DirectObject,
        })
        .collect();
    GroupSignature {
        channel_coded: group.channel_coded,
        substreams,
    }
}

fn compare_summaries(dsi: &[PresentationSummary], toc: &[PresentationSummary]) -> Value {
    let pairs = matched_pairs(dsi, toc);
    let mut field_mismatches = 0usize;
    let presentations = pairs
        .iter()
        .map(|(dsi_item, toc_item, basis)| {
            let mismatches = compare_presentation(dsi_item, toc_item);
            field_mismatches = field_mismatches.saturating_add(mismatches.len());
            json!({
                "presentation_id": dsi_item.presentation_id,
                "match_basis": basis,
                "dsi_index": dsi_item.index,
                "toc_index": toc_item.index,
                "consistent": mismatches.is_empty(),
                "mismatches": mismatches,
                "dsi": dsi_item.details_json(),
                "toc": toc_item.details_json(),
            })
        })
        .collect::<Vec<_>>();
    let unmatched_dsi = dsi
        .iter()
        .filter(|item| {
            !pairs
                .iter()
                .any(|(matched, _, _)| matched.index == item.index)
        })
        .map(PresentationSummary::reference_json)
        .collect::<Vec<_>>();
    let unmatched_toc = toc
        .iter()
        .filter(|item| {
            !pairs
                .iter()
                .any(|(_, matched, _)| matched.index == item.index)
        })
        .map(PresentationSummary::reference_json)
        .collect::<Vec<_>>();
    let ambiguous_ids = ambiguous_ids(dsi, toc);
    let consistent = field_mismatches == 0
        && unmatched_dsi.is_empty()
        && unmatched_toc.is_empty()
        && ambiguous_ids.is_empty();

    json!({
        "available": true,
        "matching_key": "effective_presentation_id",
        "consistent": consistent,
        "matched_presentations": pairs.len(),
        "field_mismatches": field_mismatches,
        "ambiguous_presentation_ids": ambiguous_ids,
        "unmatched_dsi": unmatched_dsi,
        "unmatched_toc": unmatched_toc,
        "presentations": presentations,
    })
}

fn matched_pairs<'a>(
    dsi: &'a [PresentationSummary],
    toc: &'a [PresentationSummary],
) -> Vec<(
    &'a PresentationSummary,
    &'a PresentationSummary,
    &'static str,
)> {
    let mut out = Vec::new();
    for dsi_item in dsi {
        let own_count = dsi
            .iter()
            .filter(|candidate| candidate.presentation_id == dsi_item.presentation_id)
            .count();
        let mut candidates = toc
            .iter()
            .filter(|candidate| candidate.presentation_id == dsi_item.presentation_id);
        let first = candidates.next();
        let second = candidates.next();
        if own_count != 1 || second.is_some() {
            continue;
        }
        let Some(toc_item) = first else {
            continue;
        };
        let basis = if dsi_item.presentation_id.is_some() {
            "presentation_id"
        } else {
            "single_without_id"
        };
        out.push((dsi_item, toc_item, basis));
    }
    out
}

fn ambiguous_ids(dsi: &[PresentationSummary], toc: &[PresentationSummary]) -> Vec<u32> {
    let mut out = Vec::new();
    for item in dsi.iter().chain(toc) {
        let Some(id) = item.presentation_id else {
            continue;
        };
        let dsi_count = dsi
            .iter()
            .filter(|candidate| candidate.presentation_id == Some(id))
            .count();
        let toc_count = toc
            .iter()
            .filter(|candidate| candidate.presentation_id == Some(id))
            .count();
        if (dsi_count > 1 || toc_count > 1) && !out.contains(&id) {
            out.push(id);
        }
    }
    out.sort_unstable();
    out
}

fn compare_presentation(dsi: &PresentationSummary, toc: &PresentationSummary) -> Vec<String> {
    let mut mismatches = Vec::new();
    if dsi.version != toc.version {
        mismatches.push("version".to_owned());
    }
    if !dsi.details_available {
        mismatches.push("dsi_details_unavailable".to_owned());
        return mismatches;
    }
    if dsi.md_compat != toc.md_compat {
        mismatches.push("md_compat".to_owned());
    }
    if matches!((dsi.alternative, toc.alternative), (Some(left), Some(right)) if left != right) {
        mismatches.push("alternative".to_owned());
    }
    match (&dsi.groups, &toc.groups) {
        (Some(dsi_groups), Some(toc_groups)) => {
            compare_groups(dsi_groups, toc_groups, &mut mismatches);
        }
        _ => mismatches.push("groups_unavailable".to_owned()),
    }
    mismatches
}

fn compare_groups(dsi: &[GroupSignature], toc: &[GroupSignature], mismatches: &mut Vec<String>) {
    if dsi.len() != toc.len() {
        mismatches.push("group_count".to_owned());
    }
    for (group_index, (dsi_group, toc_group)) in dsi.iter().zip(toc).enumerate() {
        if dsi_group.channel_coded != toc_group.channel_coded {
            mismatches.push(format!("groups[{group_index}].path"));
        }
        if dsi_group.substreams.len() != toc_group.substreams.len() {
            mismatches.push(format!("groups[{group_index}].substream_count"));
        }
        for (substream_index, (dsi_substream, toc_substream)) in dsi_group
            .substreams
            .iter()
            .zip(&toc_group.substreams)
            .enumerate()
        {
            let prefix = format!("groups[{group_index}].substreams[{substream_index}]");
            match (dsi_substream, toc_substream) {
                (
                    SubstreamSignature::Ajoc {
                        static_downmix: dsi_static,
                        downmix_objects: dsi_downmix,
                        upmix_objects: dsi_upmix,
                    },
                    SubstreamSignature::Ajoc {
                        static_downmix: toc_static,
                        downmix_objects: toc_downmix,
                        upmix_objects: toc_upmix,
                    },
                ) => {
                    if dsi_static != toc_static {
                        mismatches.push(format!("{prefix}.static_downmix"));
                    }
                    if dsi_downmix != toc_downmix {
                        mismatches.push(format!("{prefix}.downmix_objects"));
                    }
                    if dsi_upmix != toc_upmix {
                        mismatches.push(format!("{prefix}.upmix_objects"));
                    }
                }
                (left, right) if left.path() != right.path() => {
                    mismatches.push(format!("{prefix}.path"));
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
#[expect(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    reason = "测试按固定语法打包极小 DSI/TOC，位宽和下标均受构造约束"
)]
mod tests {
    use super::*;

    #[derive(Debug, Default)]
    struct BitBuf {
        bytes: Vec<u8>,
        bits: usize,
    }

    impl BitBuf {
        fn push_bits(&mut self, value: u64, width: usize) {
            for shift in (0..width).rev() {
                if self.bits.is_multiple_of(8) {
                    self.bytes.push(0);
                }
                if value >> shift & 1 != 0 {
                    self.bytes[self.bits / 8] |= 1 << (7 - self.bits % 8);
                }
                self.bits += 1;
            }
        }

        fn push_bytes(&mut self, bytes: &[u8]) {
            for &byte in bytes {
                self.push_bits(u64::from(byte), 8);
            }
        }

        fn byte_align(&mut self) {
            while !self.bits.is_multiple_of(8) {
                self.push_bits(0, 1);
            }
        }
    }

    fn pack_bits(source: &str) -> Vec<u8> {
        let mut out = BitBuf::default();
        for bit in source.bytes().filter(|bit| matches!(*bit, b'0' | b'1')) {
            out.push_bits(u64::from(bit == b'1'), 1);
        }
        out.bytes
    }

    fn one_ajoc_dsi() -> Vec<u8> {
        let mut body = BitBuf::default();
        body.push_bits(31, 5); // single group
        body.push_bits(4, 3); // md_compat
        body.push_bits(1, 1); // presentation ID present
        body.push_bits(1, 5); // presentation ID
        body.push_bits(0, 2); // frame-rate multiplier
        body.push_bits(0, 2); // frame-rate fraction
        body.push_bits(0, 5); // presentation EMDF version
        body.push_bits(0, 10); // presentation EMDF key
        body.push_bits(0, 1); // presentation channel layout absent
        body.push_bits(0, 1); // core layout does not differ
        body.push_bits(0, 1); // filter absent
        body.push_bits(1, 1); // substreams present
        body.push_bits(0, 1); // HSF absent
        body.push_bits(0, 1); // object coded
        body.push_bits(1, 8); // one substream
        body.push_bits(0, 2); // sampling-frequency multiplier
        body.push_bits(0, 1); // bitrate indicator absent
        body.push_bits(1, 1); // A-JOC
        body.push_bits(0, 1); // dynamic downmix count
        body.push_bits(0, 4); // one downmix object
        body.push_bits(0, 6); // one upmix object
        body.push_bits(0b0100, 4); // dynamic objects only
        body.push_bits(0, 1); // content type absent
        body.push_bits(0, 1); // not pre-virtualized
        body.push_bits(0, 1); // no additional EMDF
        body.push_bits(0, 1); // presentation bitrate absent
        body.push_bits(0, 1); // not alternative
        body.byte_align();

        let mut dsi = BitBuf::default();
        dsi.push_bits(1, 3); // DSI v1
        dsi.push_bits(2, 7); // bitstream v2
        dsi.push_bits(1, 1); // 48 kHz
        dsi.push_bits(13, 4); // frame-rate index
        dsi.push_bits(1, 9); // one presentation
        dsi.push_bits(0, 1); // program ID absent
        dsi.push_bits(0, 2); // unspecified bitrate mode
        dsi.push_bits(0, 32);
        dsi.push_bits(u64::from(u32::MAX), 32);
        dsi.byte_align();
        dsi.push_bits(1, 8); // presentation v1
        dsi.push_bits(u64::try_from(body.bytes.len()).unwrap(), 8);
        dsi.push_bytes(&body.bytes);
        dsi.bytes
    }

    fn one_ajoc_topology() -> Ac4Topology {
        let toc = "10 0000000000 0 1 1101 1 1 0 0";
        let presentation = "1 10 100 1 01 0 00 000 0 00 00 0 000 0 0 0 1 00";
        let group = "1 0 1 0 0 1 0 0 0000 1 0 0000 1 0 0 1 01 0";
        let table = "10 0 0000000001 0 0000000001";
        let mut frame = pack_bits(&[toc, presentation, group, table].join(" "));
        frame.extend_from_slice(&[0, 0]);
        Ac4Topology::parse(&frame).expect("构造的 A-JOC TOC 应可解析")
    }

    fn presentation(
        index: usize,
        presentation_id: Option<u32>,
        version: u32,
        md_compat: u8,
        substream: SubstreamSignature,
    ) -> PresentationSummary {
        PresentationSummary {
            index,
            presentation_id,
            version,
            details_available: true,
            md_compat: Some(md_compat),
            alternative: Some(false),
            groups: Some(vec![GroupSignature {
                channel_coded: matches!(substream, SubstreamSignature::Channel),
                substreams: vec![substream],
            }]),
        }
    }

    fn ajoc(upmix_objects: u32) -> SubstreamSignature {
        SubstreamSignature::Ajoc {
            static_downmix: true,
            downmix_objects: 5,
            upmix_objects,
        }
    }

    #[test]
    fn matches_reordered_presentations_by_id() {
        let dsi = [
            presentation(0, Some(7), 1, 4, ajoc(20)),
            presentation(1, Some(3), 1, 0, SubstreamSignature::Channel),
        ];
        let toc = [
            presentation(0, Some(3), 1, 0, SubstreamSignature::Channel),
            presentation(1, Some(7), 1, 4, ajoc(20)),
        ];

        let comparison = compare_summaries(&dsi, &toc);
        assert_eq!(comparison["consistent"], true);
        assert_eq!(comparison["presentations"][0]["dsi_index"], 0);
        assert_eq!(comparison["presentations"][0]["toc_index"], 1);
        assert_eq!(comparison["presentations"][1]["dsi_index"], 1);
        assert_eq!(comparison["presentations"][1]["toc_index"], 0);
    }

    #[test]
    fn reports_ajoc_object_count_mismatch() {
        let dsi = [presentation(0, Some(7), 1, 4, ajoc(20))];
        let toc = [presentation(0, Some(7), 1, 4, ajoc(19))];

        let comparison = compare_summaries(&dsi, &toc);
        assert_eq!(comparison["consistent"], false);
        assert_eq!(comparison["field_mismatches"], 1);
        assert_eq!(
            comparison["presentations"][0]["mismatches"],
            json!(["groups[0].substreams[0].upmix_objects"])
        );
    }

    #[test]
    fn reports_selection_and_direct_object_path_mismatches() {
        let dsi = [presentation(
            0,
            Some(7),
            1,
            4,
            SubstreamSignature::DirectObject,
        )];
        let mut toc = [presentation(0, Some(7), 2, 3, ajoc(20))];
        toc[0].alternative = Some(true);

        let comparison = compare_summaries(&dsi, &toc);
        assert_eq!(comparison["consistent"], false);
        assert_eq!(
            comparison["presentations"][0]["mismatches"],
            json!([
                "version",
                "md_compat",
                "alternative",
                "groups[0].substreams[0].path"
            ])
        );
    }

    #[test]
    fn does_not_index_match_multiple_presentations_without_ids() {
        let dsi = [
            presentation(0, None, 1, 4, ajoc(20)),
            presentation(1, None, 1, 0, SubstreamSignature::Channel),
        ];
        let toc = [
            presentation(0, None, 1, 4, ajoc(20)),
            presentation(1, None, 1, 0, SubstreamSignature::Channel),
        ];

        let comparison = compare_summaries(&dsi, &toc);
        assert_eq!(comparison["consistent"], false);
        assert_eq!(comparison["matched_presentations"], 0);
        assert_eq!(
            comparison["unmatched_dsi"].as_array().map(Vec::len),
            Some(2)
        );
        assert_eq!(
            comparison["unmatched_toc"].as_array().map(Vec::len),
            Some(2)
        );
    }

    #[test]
    fn uniquely_matches_one_presentation_without_an_id() {
        let dsi = [presentation(0, None, 1, 4, ajoc(20))];
        let toc = [presentation(0, None, 1, 4, ajoc(20))];

        let comparison = compare_summaries(&dsi, &toc);
        assert_eq!(comparison["consistent"], true);
        assert_eq!(
            comparison["presentations"][0]["match_basis"],
            "single_without_id"
        );
    }

    #[test]
    fn duplicate_presentation_ids_remain_ambiguous() {
        let dsi = [
            presentation(0, Some(7), 1, 4, ajoc(20)),
            presentation(1, Some(7), 1, 4, ajoc(20)),
        ];
        let toc = [presentation(0, Some(7), 1, 4, ajoc(20))];

        let comparison = compare_summaries(&dsi, &toc);
        assert_eq!(comparison["matched_presentations"], 0);
        assert_eq!(comparison["ambiguous_presentation_ids"], json!([7]));
    }

    #[test]
    fn describe_exposes_v1_metadata_and_cross_checks_toc() {
        let bytes = one_ajoc_dsi();
        let dsi = Ac4Dsi::parse(&bytes).expect("构造的 DSI 应可解析");
        let topology = one_ajoc_topology();

        let trace = describe(&dsi, bytes.len(), Some(&topology), None).unwrap();
        let details = &trace["presentations"][0]["details"];
        assert_eq!(details["effective_presentation_id"], 1);
        assert_eq!(details["md_compat"], 4);
        assert_eq!(details["substream_groups"][0]["path"], "ajoc");
        assert_eq!(
            details["substream_groups"][0]["substreams"][0]["upmix_objects"],
            1
        );
        assert_eq!(trace["first_toc_comparison"]["consistent"], true);
        assert_eq!(
            trace["first_toc_comparison"]["presentations"][0]["match_basis"],
            "presentation_id"
        );
    }
}
