//! Core 声明、源 AU 网格证据与保守的扬声器布局派生。
use super::*;
use macindecode_ac4_metadata::{MetadataObjectState, MetadataStatus, ObjectMetadataDomain, layout};

#[derive(Debug, Clone, Default, Serialize)]
pub struct InspectCoreLayouts {
    pub declarations: Vec<InspectCoreDeclaration>,
    pub observations: Vec<InspectCoreObservation>,
}
impl InspectCoreLayouts {
    pub(super) fn render_text(&self, output: &mut String) {
        if self.declarations.is_empty() && self.observations.is_empty() {
            return;
        }
        let _ = writeln!(output, "Core layouts:");
        for declaration in &self.declarations {
            let _ = writeln!(
                output,
                "  Declaration ({}, presentation {:?}, generation {:?}):",
                declaration.source,
                declaration.presentation_index,
                declaration.configuration_generation
            );
            text_field(
                output,
                2,
                "Declared core layout",
                &declaration.declared_core_layout,
            );
            if let Some(reason) = &declaration.association_reason {
                let _ = writeln!(output, "    Association: {reason}");
            }
        }
        for observation in &self.observations {
            let grid = &observation.observed_core_grid;
            let _ = writeln!(
                output,
                "  Substream {}, generation {}, epoch {}, source AU {}..{}:",
                observation.substream_index,
                observation.configuration_generation,
                observation.observation_epoch,
                observation.first_access_unit,
                observation.last_access_unit
            );
            let _ = writeln!(
                output,
                "    Observed grid: {:?}; stability {:?}; {} observed / {} missing frame(s); {} change(s)",
                grid.status,
                grid.stability,
                grid.observed_frames,
                grid.missing_frames,
                grid.change_count
            );
            if let Some(reason) = &grid.reason {
                let _ = writeln!(output, "    Evidence: {reason}");
            }
            for object in &grid.initial_grid {
                let _ = writeln!(
                    output,
                    "    Object {}: {}{}; position {:?}; extended {:?}",
                    object.object_index,
                    object.kind,
                    if object.lfe { " (LFE)" } else { "" },
                    object.position,
                    object.extended_position
                );
            }
            text_field(
                output,
                2,
                "Derived speaker layout",
                &observation.derived_speaker_layout,
            );
            for mapping in &observation.object_to_speaker {
                let _ = writeln!(
                    output,
                    "    Object {} -> {}",
                    mapping.object_index, mapping.speaker
                );
            }
        }
    }
}
#[derive(Debug, Clone, Serialize)]
pub struct InspectCoreDeclaration {
    pub source: &'static str,
    pub configuration_generation: Option<u32>,
    pub presentation_index: Option<u32>,
    pub presentation_id: Option<u32>,
    pub substream_index: Option<u32>,
    pub declared_core_layout: ReportedField,
    pub association_reason: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InspectCoreGridObject {
    pub object_index: u8,
    pub lfe: bool,
    pub kind: String,
    pub position: Option<(u8, u8, i8)>,
    pub extended_position: Option<InspectCoreExtendedPosition>,
    pub position_coding: Option<String>,
    #[serde(skip)]
    geometry: Option<(i16, i16, i16)>,
}
impl InspectCoreGridObject {
    fn same_geometry(&self, other: &Self) -> bool {
        self.object_index == other.object_index
            && self.lfe == other.lfe
            && self.kind == other.kind
            && self.geometry == other.geometry
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct InspectCoreExtendedPosition {
    pub presence: u8,
    pub x: Option<u8>,
    pub y: Option<u8>,
    pub z: Option<u8>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CoreGridStability {
    Unknown,
    Stable,
    Changed,
}
#[derive(Debug, Clone, Copy, Serialize)]
pub struct InspectCoreChange {
    pub access_unit_index: u64,
    pub offset_samples: Option<u32>,
}
#[derive(Debug, Clone, Serialize)]
pub struct InspectCoreGrid {
    pub status: FieldStatus,
    pub initial_grid: Vec<InspectCoreGridObject>,
    pub stability: CoreGridStability,
    pub observed_frames: u64,
    pub missing_frames: u64,
    pub change_count: u64,
    pub first_change: Option<InspectCoreChange>,
    pub reason: Option<String>,
}
#[derive(Debug, Clone, Serialize)]
pub struct InspectCoreSpeakerMapping {
    pub object_index: u8,
    pub speaker: &'static str,
}
#[derive(Debug, Clone, Serialize)]
pub struct InspectCoreObservation {
    pub configuration_generation: u32,
    pub observation_epoch: u64,
    pub substream_index: u32,
    pub first_access_unit: u64,
    pub last_access_unit: u64,
    pub observed_core_grid: InspectCoreGrid,
    pub derived_speaker_layout: ReportedField,
    pub object_to_speaker: Vec<InspectCoreSpeakerMapping>,
}

#[derive(Debug, Default)]
pub(super) struct CoreLayoutAccumulator {
    report: InspectCoreLayouts,
    entries: BTreeMap<(u64, u32, u32), usize>,
    latest: BTreeMap<(u64, u32, u32), Vec<InspectCoreGridObject>>,
    presentations: BTreeMap<(u32, u32), Option<u32>>,
    toc_seen: BTreeSet<(u32, u32, u32)>,
    active: Vec<usize>,
    incomplete_spatial: BTreeSet<(u64, u32, u32)>,
}
fn grid_object(object: &MetadataObjectState) -> InspectCoreGridObject {
    InspectCoreGridObject {
        position_coding: object
            .state
            .render
            .map(|r| format!("{:?}", r.position.coding)),
        geometry: if object.descriptor.b_lfe {
            None
        } else {
            layout::effective_position_key(object.state, object.additional)
        },
        object_index: object.object_index,
        lfe: object.descriptor.b_lfe,
        kind: format!("{:?}", object.descriptor.obj_type),
        position: object
            .state
            .render
            .map(|r| (r.position.x, r.position.y, r.position.z)),
        extended_position: object.additional.extended_position.map(|p| {
            InspectCoreExtendedPosition {
                presence: p.presence,
                x: p.x,
                y: p.y,
                z: p.z,
            }
        }),
    }
}
#[derive(Debug, Clone, Copy)]
struct CoreSourceFrame<'a> {
    topology: &'a Ac4Topology,
    access_unit_index: u64,
    generation: u32,
    epoch: u64,
    status: MetadataStatus,
    objects: &'a [MetadataObjectState],
    updates: &'a [macindecode_ac4_metadata::MetadataObjectUpdate],
    diagnostics: &'a [macindecode_ac4_metadata::MetadataDiagnostic],
    groups: &'a [macindecode_ac4_metadata::group_oamd::PreparedGroupOamdState],
}

impl CoreLayoutAccumulator {
    pub fn observe(&mut self, au: MetadataAccessUnit<'_>, detail: MetadataDetail) {
        let Some(topology) = au.topology() else {
            self.mark_topology_gap(au.context().index());
            return;
        };
        self.observe_source(
            CoreSourceFrame {
                topology,
                access_unit_index: au.context().index(),
                generation: au.configuration_generation(),
                epoch: au.observation_epoch(),
                status: au.status(),
                objects: au.objects(),
                updates: au.object_updates(),
                diagnostics: au.diagnostics(),
                groups: au.group_oamd(),
            },
            detail,
        );
    }
    fn observe_source(&mut self, source: CoreSourceFrame<'_>, detail: MetadataDetail) {
        self.active.clear();
        let topology = source.topology;
        let frame = source.access_unit_index;
        let generation = source.generation;
        let epoch = source.epoch;
        let mut substreams = BTreeMap::new();
        for (pi, presentation) in topology.presentations().iter().enumerate() {
            let pi = u32::try_from(pi).unwrap_or(u32::MAX);
            self.presentations
                .insert((generation, pi), presentation.presentation_id);
            for &group_index in presentation.group_indices() {
                let Some(group) = usize::try_from(group_index)
                    .ok()
                    .and_then(|i| topology.groups().get(i))
                else {
                    continue;
                };
                for info in group.substreams() {
                    let SubstreamInfo::Ajoc(info) = info else {
                        continue;
                    };
                    let Some(index) = info.substream_index() else {
                        continue;
                    };
                    substreams.insert(index, (info.n_dmx_signals, info.b_lfe));
                    if self.toc_seen.insert((generation, pi, index)) {
                        self.report.declarations.push(InspectCoreDeclaration{source:"toc",configuration_generation:Some(generation),presentation_index:Some(pi),presentation_id:presentation.presentation_id,substream_index:Some(index),declared_core_layout:ReportedField::present(json!({"coding":"ajoc","static_downmix":info.static_dmx,"fullband_signals":info.n_dmx_signals,"lfe":info.b_lfe})),association_reason:None});
                    }
                }
            }
        }
        for (index, (fullband, lfe)) in substreams {
            let key = (epoch, generation, index);
            let entry = *self.entries.entry(key).or_insert_with(|| {
                let entry = self.report.observations.len();
                self.report.observations.push(InspectCoreObservation {
                    configuration_generation: generation,
                    observation_epoch: epoch,
                    substream_index: index,
                    first_access_unit: frame,
                    last_access_unit: frame,
                    observed_core_grid: InspectCoreGrid {
                        status: FieldStatus::Unknown,
                        initial_grid: Vec::new(),
                        stability: CoreGridStability::Unknown,
                        observed_frames: 0,
                        missing_frames: 0,
                        change_count: 0,
                        first_change: None,
                        reason: Some("complete metadata scan was not requested".to_owned()),
                    },
                    derived_speaker_layout: ReportedField::unknown(
                        "Core grid has not been observed",
                    ),
                    object_to_speaker: Vec::new(),
                });
                entry
            });
            let Some(report) = self.report.observations.get_mut(entry) else {
                continue;
            };
            self.active.push(entry);
            report.last_access_unit = frame;
            if detail == MetadataDetail::Basic {
                continue;
            }
            let states: Vec<_> = source
                .objects
                .iter()
                .filter(|o| o.substream_index == index && o.domain == ObjectMetadataDomain::Core)
                .copied()
                .collect();
            let expected = usize::try_from(fullband)
                .unwrap_or(usize::MAX)
                .saturating_add(usize::from(lfe));
            let complete = states.len() == expected
                && states
                    .iter()
                    .filter(|o| !o.descriptor.b_lfe)
                    .all(|o| o.state.render.is_some());
            if !complete || source.status == MetadataStatus::WaitingForRandomAccess {
                let grid = &mut report.observed_core_grid;
                grid.missing_frames = grid.missing_frames.saturating_add(1);
                grid.status = FieldStatus::Unknown;
                grid.stability = CoreGridStability::Unknown;
                grid.reason = Some(
                    source
                        .diagnostics
                        .iter()
                        .find(|d| d.error.context().substream_index == Some(index))
                        .map_or_else(
                            || {
                                "Core OAMD state is incomplete or waiting for random access"
                                    .to_owned()
                            },
                            |d| d.error.to_string(),
                        ),
                );
                report.derived_speaker_layout =
                    ReportedField::unknown("Core grid observation has coverage gaps");
                report.object_to_speaker.clear();
                continue;
            }
            let mut first: Vec<_> = states.iter().map(grid_object).collect();
            // 第一份网格使用每个对象的首个源更新，覆盖首 AU 内随后再次移动的情况。
            if report.observed_core_grid.initial_grid.is_empty() {
                for object in &mut first {
                    if let Some(update) = source.updates.iter().find(|u| {
                        u.target.substream_index == index
                            && u.target.domain == ObjectMetadataDomain::Core
                            && u.target.object_index == object.object_index
                    }) {
                        *object = grid_object(&update.target);
                    }
                }
                report.observed_core_grid.initial_grid = first.clone();
            }
            let current = self.latest.entry(key).or_insert(first);
            for update in source.updates.iter().filter(|u| {
                u.target.substream_index == index && u.target.domain == ObjectMetadataDomain::Core
            }) {
                let next = grid_object(&update.target);
                if let Some(previous) = current
                    .iter_mut()
                    .find(|o| o.object_index == next.object_index)
                    && !previous.same_geometry(&next)
                {
                    *previous = next;
                    let grid = &mut report.observed_core_grid;
                    grid.change_count = grid.change_count.saturating_add(1);
                    grid.first_change.get_or_insert(InspectCoreChange {
                        access_unit_index: frame,
                        offset_samples: update.offset_samples,
                    });
                }
            }
            let grid = &mut report.observed_core_grid;
            grid.observed_frames = grid.observed_frames.saturating_add(1);
            grid.status = if grid.missing_frames == 0 {
                FieldStatus::Present
            } else {
                FieldStatus::Unknown
            };
            grid.stability = if grid.change_count > 0 {
                CoreGridStability::Changed
            } else if grid.missing_frames > 0 {
                CoreGridStability::Unknown
            } else {
                CoreGridStability::Stable
            };
            if grid.missing_frames == 0 {
                grid.reason = None;
            }
            let missing_common = topology.groups().iter().enumerate().any(|(gi, g)| {
                g.substreams()
                    .iter()
                    .any(|s| s.substream_index() == Some(index))
                    && source.diagnostics.iter().any(|d| {
                        d.partition == macindecode_ac4_metadata::MetadataPartition::GroupOamd
                            && d.error.context().group_index == u32::try_from(gi).ok()
                    })
            });
            if missing_common {
                self.incomplete_spatial.insert(key);
            }
            let common_unavailable = self.incomplete_spatial.contains(&key);
            let validation = (|| {
                if common_unavailable {
                    return Err("group OAMD metadata is unavailable in the observed interval");
                }
                if grid.missing_frames > 0 {
                    return Err("Core grid observation has coverage gaps");
                }
                if grid.stability != CoreGridStability::Stable {
                    return Err("Core geometry changes within the observed interval");
                }
                if !lfe || states.iter().filter(|o| o.descriptor.b_lfe).count() != 1 {
                    return Err("a unique LFE component was not observed");
                }
                let template =
                    layout::layout_for_count(usize::try_from(fullband).unwrap_or(usize::MAX))
                        .ok_or("Core object count has no verified speaker-grid template")?;
                for (object, &expected) in states
                    .iter()
                    .filter(|o| !o.descriptor.b_lfe)
                    .zip(template.positions)
                {
                    if object.descriptor.obj_type
                        != macindecode_ac4_bitstream::oamd::ObjectType::Dynamic
                    {
                        return Err("Core fullband assignment is not a dynamic object grid");
                    }
                    layout::validate_grid_object(object.state, object.additional, expected)?;
                }
                for update in source.updates.iter().filter(|u| {
                    u.target.substream_index == index
                        && u.target.domain == ObjectMetadataDomain::Core
                        && !u.target.descriptor.b_lfe
                }) {
                    let q = states
                        .iter()
                        .filter(|o| !o.descriptor.b_lfe)
                        .position(|o| o.object_index == update.target.object_index)
                        .ok_or("Core object identity is inconsistent")?;
                    let expected = *template
                        .positions
                        .get(q)
                        .ok_or("Core grid template is incomplete")?;
                    layout::validate_grid_object(
                        update.target.state,
                        update.target.additional,
                        expected,
                    )?;
                }
                for (gi, group) in topology.groups().iter().enumerate().filter(|(_, g)| {
                    g.substreams()
                        .iter()
                        .any(|s| s.substream_index() == Some(index))
                }) {
                    let _ = group;
                    if let Some(common) = source
                        .groups
                        .iter()
                        .find(|g| usize::try_from(g.group_index) == Ok(gi))
                        .and_then(|g| g.effective_common)
                    {
                        layout::validate_grid_common(common)?;
                    }
                }
                Ok(template)
            })();
            match validation {
                Ok(template) => {
                    // 先前存在不兼容空间属性时，后来的普通帧不能抹掉该证据。
                    if report.derived_speaker_layout.status != FieldStatus::Unsupported {
                        report.derived_speaker_layout = ReportedField::present(template.name);
                        report.object_to_speaker.clear();
                        for (object, &speaker) in states
                            .iter()
                            .filter(|o| !o.descriptor.b_lfe)
                            .zip(template.speakers)
                        {
                            report.object_to_speaker.push(InspectCoreSpeakerMapping {
                                object_index: object.object_index,
                                speaker,
                            });
                        }
                        if let Some(object) = states.iter().find(|o| o.descriptor.b_lfe) {
                            report.object_to_speaker.push(InspectCoreSpeakerMapping {
                                object_index: object.object_index,
                                speaker: "LFE",
                            });
                        }
                    }
                }
                Err(reason) => {
                    report.derived_speaker_layout = if common_unavailable
                        || grid.missing_frames > 0
                        || grid.stability != CoreGridStability::Stable
                    {
                        ReportedField::unknown(reason)
                    } else {
                        ReportedField::unsupported(reason)
                    };
                    report.object_to_speaker.clear();
                }
            }
        }
    }
    fn mark_topology_gap(&mut self, frame: u64) {
        for &index in &self.active {
            if let Some(report) = self.report.observations.get_mut(index) {
                report.last_access_unit = frame;
                let grid = &mut report.observed_core_grid;
                grid.missing_frames = grid.missing_frames.saturating_add(1);
                grid.status = FieldStatus::Unknown;
                grid.stability = CoreGridStability::Unknown;
                grid.reason = Some("topology is unavailable in the observed interval".to_owned());
                report.derived_speaker_layout =
                    ReportedField::unknown("Core grid observation has coverage gaps");
                report.object_to_speaker.clear();
            }
        }
    }
    pub fn apply_dsi(&mut self, dsi: &DsiSummary) -> Vec<InspectIssue> {
        let mut issues = Vec::new();
        for presentation in &dsi.presentations {
            let Some(layout) = presentation.core_layout else {
                continue;
            };
            let duplicate = dsi.unavailable_presentation_identities > 0
                || dsi
                    .presentations
                    .iter()
                    .filter(|p| p.effective_id == presentation.effective_id)
                    .count()
                    != 1;
            let generations: BTreeSet<_> = self.presentations.keys().map(|(g, _)| *g).collect();
            let mut matched = false;
            for generation in generations {
                let candidates: Vec<_> = self
                    .presentations
                    .iter()
                    .filter(|((g, _), id)| *g == generation && **id == presentation.effective_id)
                    .collect();
                if duplicate || candidates.len() != 1 {
                    continue;
                }
                let Some((&(_, index), _)) = candidates.first().copied() else {
                    continue;
                };
                matched = true;
                // TS103190-2:v1.3.1:E.10.2，表 E.14。声明只作用于 presentation。
                let label = if layout.channel_coded {
                    match layout.channel_mode {
                        Some(0) => Some("5.0"),
                        Some(1) => Some("5.1"),
                        Some(2) => Some("5.0.2"),
                        Some(3) => Some("5.1.2"),
                        _ => None,
                    }
                } else {
                    None
                };
                let sources: BTreeSet<_> = self
                    .report
                    .declarations
                    .iter()
                    .filter(|d| {
                        d.source == "toc"
                            && d.configuration_generation == Some(generation)
                            && d.presentation_index == Some(index)
                    })
                    .filter_map(|d| d.substream_index)
                    .collect();
                if sources.len() == 1
                    && let Some(label) = label
                {
                    for observed in self.report.observations.iter().filter(|o| {
                        o.configuration_generation == generation
                            && sources.contains(&o.substream_index)
                    }) {
                        if let Some(actual) = observed
                            .derived_speaker_layout
                            .value
                            .as_ref()
                            .and_then(Value::as_str)
                            && observed.derived_speaker_layout.status == FieldStatus::Present
                            && actual != label
                        {
                            issues.push(InspectIssue::warning("core_layout_conflict",format!("DSI declares {label}, but the observed Core grid derives {actual} in generation {generation}"),Some(observed.first_access_unit)).presentation(presentation.effective_id).substream(observed.substream_index));
                        }
                    }
                }
                self.report.declarations.push(InspectCoreDeclaration{source:"dsi",configuration_generation:Some(generation),presentation_index:Some(index),presentation_id:presentation.effective_id,substream_index:None,declared_core_layout:ReportedField::present(json!({"channel_coded":layout.channel_coded,"channel_mode":layout.channel_mode})),association_reason:None});
            }
            if !matched {
                self.report.declarations.push(InspectCoreDeclaration{source:"dsi",configuration_generation:None,presentation_index:None,presentation_id:presentation.effective_id,substream_index:None,declared_core_layout:ReportedField::present(json!({"channel_coded":layout.channel_coded,"channel_mode":layout.channel_mode})),association_reason:Some("DSI presentation identity could not be associated uniquely".to_owned())});
            }
        }
        issues
    }
    pub fn finish(self) -> InspectCoreLayouts {
        self.report
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use macindecode_ac4_bitstream::oamd::{
        AdditionalObjectMetadata, OamdMetadataBlock, ObjectBasicState, ObjectDescriptor,
        ObjectGainState, ObjectHeadphone, ObjectMetadataState, ObjectPriorityState,
        ObjectRenderState, ObjectType, OtherPropertiesUpdate, PositionCoding, QuantizedPosition,
        WidthUpdate, ZoneUpdate,
    };
    use macindecode_ac4_metadata::MetadataObjectUpdate;

    #[allow(
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        reason = "按有界测试位串构造 topology"
    )]
    fn topology(count: usize, lfe: bool) -> Ac4Topology {
        let bits = format!(
            "10 0000000000 0 1 1101 1 1 0 0 1 0 000 0 00 000 0 00 00 0 000 0 0 0 1 00 1 0 1 0 0 1 {} 0 {:04b} 1 0 {:04b} 1 0 0 1 01 0 10 0 0000000000 0 0000000000",
            u8::from(lfe),
            count - 1,
            count - 1
        );
        let bits: Vec<_> = bits.bytes().filter(|b| matches!(b, b'0' | b'1')).collect();
        let mut bytes = vec![0; bits.len().div_ceil(8)];
        for (i, b) in bits.into_iter().enumerate() {
            if b == b'1' {
                bytes[i / 8] |= 1 << (7 - i % 8);
            }
        }
        Ac4Topology::parse(&bytes).unwrap()
    }
    fn object(index: u8, position: (u8, u8, i8), lfe: bool) -> MetadataObjectState {
        MetadataObjectState {
            substream_index: 1,
            domain: ObjectMetadataDomain::Core,
            object_index: index,
            descriptor: ObjectDescriptor {
                obj_type: if lfe {
                    ObjectType::Bed
                } else {
                    ObjectType::Dynamic
                },
                b_lfe: lfe,
                b_ajoc_coded: true,
            },
            state: ObjectMetadataState {
                active: true,
                basic: Some(ObjectBasicState {
                    gain: ObjectGainState::Default,
                    priority: ObjectPriorityState::Default,
                }),
                render: Some(ObjectRenderState {
                    position: QuantizedPosition {
                        x: position.0,
                        y: position.1,
                        z: position.2,
                        coding: PositionCoding::AbsolutePositive,
                    },
                    zone: ZoneUpdate {
                        grouped_defaults: true,
                        ..Default::default()
                    },
                    other_properties: OtherPropertiesUpdate {
                        grouped_defaults: true,
                        ..Default::default()
                    },
                }),
            },
            additional: AdditionalObjectMetadata::default(),
        }
    }
    fn objects(count: usize, lfe: bool) -> Vec<MetadataObjectState> {
        let mut objects = Vec::new();
        if lfe {
            objects.push(object(0, (31, 31, 0), true));
        }
        for (i, &p) in layout::layout_for_count(count)
            .unwrap()
            .positions
            .iter()
            .enumerate()
        {
            objects.push(object(
                u8::try_from(i.checked_add(usize::from(lfe)).unwrap()).unwrap(),
                p,
                false,
            ));
        }
        objects
    }
    fn source<'a>(
        topology: &'a Ac4Topology,
        objects: &'a [MetadataObjectState],
        updates: &'a [MetadataObjectUpdate],
        frame: u64,
    ) -> CoreSourceFrame<'a> {
        CoreSourceFrame {
            topology,
            access_unit_index: frame,
            generation: 1,
            epoch: 1,
            status: MetadataStatus::Ready,
            objects,
            updates,
            diagnostics: &[],
            groups: &[],
        }
    }
    fn update(target: MetadataObjectState, offset: u32) -> MetadataObjectUpdate {
        MetadataObjectUpdate {
            raw: OamdMetadataBlock {
                object_index: target.object_index,
                ..Default::default()
            },
            target,
            timing: None,
            offset_samples: Some(offset),
            ramp_duration_samples: Some(64),
        }
    }
    #[test]
    fn derives_only_the_four_verified_grid_templates() {
        for (count, label) in [(5, "5.1"), (7, "5.1.2"), (9, "5.1.4"), (11, "7.1.4")] {
            let topology = topology(count, true);
            let objects = objects(count, true);
            let mut accumulator = CoreLayoutAccumulator::default();
            accumulator.observe_source(source(&topology, &objects, &[], 0), MetadataDetail::Full);
            let report = accumulator.finish();
            let observed = report.observations.first().unwrap();
            assert_eq!(observed.derived_speaker_layout.value, Some(json!(label)));
            assert_eq!(
                observed.observed_core_grid.stability,
                CoreGridStability::Stable
            );
            assert_eq!(observed.object_to_speaker.len(), count + 1);
        }
    }
    #[test]
    fn gain_and_headphone_changes_do_not_change_geometry() {
        let topology = topology(5, true);
        let mut objects = objects(5, true);
        let mut accumulator = CoreLayoutAccumulator::default();
        accumulator.observe_source(source(&topology, &objects, &[], 0), MetadataDetail::Full);
        let target = objects.get_mut(1).unwrap();
        target.state.basic.as_mut().unwrap().gain = ObjectGainState::Quantized(12);
        target.additional.headphone = Some(ObjectHeadphone {
            render_mode: 1,
            head_tracking_disabled: true,
        });
        let updates = [update(*target, 200)];
        accumulator.observe_source(
            source(&topology, &objects, &updates, 1),
            MetadataDetail::Full,
        );
        let report = accumulator.finish();
        let observed = report.observations.first().unwrap();
        assert_eq!(observed.observed_core_grid.change_count, 0);
        assert_eq!(observed.derived_speaker_layout.value, Some(json!("5.1")));
    }
    #[test]
    fn observes_move_and_return_inside_the_first_au() {
        let topology = topology(5, true);
        let objects = objects(5, true);
        let initial = *objects.get(1).unwrap();
        let mut moved = initial;
        moved.state.render.as_mut().unwrap().position.x = 1;
        let updates = [update(initial, 0), update(moved, 100), update(initial, 200)];
        let mut accumulator = CoreLayoutAccumulator::default();
        accumulator.observe_source(
            source(&topology, &objects, &updates, 0),
            MetadataDetail::Full,
        );
        let report = accumulator.finish();
        let observed = report.observations.first().unwrap();
        assert_eq!(observed.observed_core_grid.change_count, 2);
        assert_eq!(
            observed
                .observed_core_grid
                .first_change
                .unwrap()
                .offset_samples,
            Some(100)
        );
        assert_eq!(
            observed.observed_core_grid.stability,
            CoreGridStability::Changed
        );
        assert!(observed.derived_speaker_layout.value.is_none());
    }
    #[test]
    fn object_count_alone_and_spatial_modifiers_cannot_prove_a_layout() {
        let topology = topology(5, true);
        for spatial in [false, true] {
            let mut objects = objects(5, true);
            let render = objects.get_mut(1).unwrap().state.render.as_mut().unwrap();
            if spatial {
                render.other_properties.width = Some(WidthUpdate::Uniform(1));
            } else {
                render.position.x = 12;
            }
            let mut accumulator = CoreLayoutAccumulator::default();
            accumulator.observe_source(source(&topology, &objects, &[], 0), MetadataDetail::Full);
            let report = accumulator.finish();
            let observed = report.observations.first().unwrap();
            assert_eq!(
                observed.observed_core_grid.stability,
                CoreGridStability::Stable
            );
            assert_eq!(
                observed.derived_speaker_layout.status,
                FieldStatus::Unsupported
            );
        }
    }
    #[test]
    fn gaps_remain_unknown_after_later_success_and_epochs_are_separate() {
        let topology = topology(5, true);
        let objects = objects(5, true);
        let mut accumulator = CoreLayoutAccumulator::default();
        accumulator.observe_source(source(&topology, &objects, &[], 0), MetadataDetail::Full);
        accumulator.observe_source(source(&topology, &[], &[], 1), MetadataDetail::Full);
        accumulator.observe_source(source(&topology, &objects, &[], 2), MetadataDetail::Full);
        let mut next = source(&topology, &objects, &[], 3);
        next.epoch = 2;
        next.generation = 2;
        accumulator.observe_source(next, MetadataDetail::Full);
        let report = accumulator.finish();
        assert_eq!(report.observations.len(), 2);
        let first = report.observations.first().unwrap();
        assert_eq!(first.observed_core_grid.missing_frames, 1);
        assert_eq!(first.derived_speaker_layout.status, FieldStatus::Unknown);
        assert_eq!(
            report
                .observations
                .last()
                .unwrap()
                .derived_speaker_layout
                .value,
            Some(json!("5.1"))
        );
    }
    #[test]
    fn basic_and_missing_lfe_do_not_claim_a_verified_layout() {
        let topology = topology(5, false);
        let objects = objects(5, false);
        for detail in [MetadataDetail::Basic, MetadataDetail::Full] {
            let mut accumulator = CoreLayoutAccumulator::default();
            accumulator.observe_source(source(&topology, &objects, &[], 0), detail);
            let report = accumulator.finish();
            let observed = report.observations.first().unwrap();
            assert!(observed.derived_speaker_layout.value.is_none());
            if detail == MetadataDetail::Basic {
                assert!(observed.observed_core_grid.initial_grid.is_empty());
                assert!(
                    observed
                        .observed_core_grid
                        .reason
                        .as_ref()
                        .unwrap()
                        .contains("not requested")
                );
            }
        }
    }
    fn dsi(mode: u8) -> DsiSummary {
        DsiSummary {
            unavailable_presentation_identities: 0,
            bitstream_version: 2,
            sample_rate: 48000,
            frame_rate_numerator: 24000,
            frame_rate_denominator: 1001,
            bitrate: None,
            presentations: vec![DsiPresentationSummary {
                core_layout: Some(macindecode_ac4_mp4::dsi::Ac4DsiPresentationCoreLayout {
                    channel_coded: true,
                    channel_mode: Some(mode),
                }),
                index: 0,
                effective_id: None,
                presentation_config: 0,
                md_compat: None,
                multi_pid: None,
                channel_mode: None,
                bitrate: None,
                alternative: false,
                indicators: None,
                languages: vec![],
                group_classifiers: vec![],
            }],
        }
    }
    #[test]
    fn dsi_conflicts_are_reported_without_overwriting_the_observed_grid() {
        let topology = topology(5, true);
        let objects = objects(5, true);
        let mut accumulator = CoreLayoutAccumulator::default();
        accumulator.observe_source(source(&topology, &objects, &[], 0), MetadataDetail::Full);
        let issues = accumulator.apply_dsi(&dsi(3));
        assert_eq!(issues.first().unwrap().code, "core_layout_conflict");
        assert_eq!(
            accumulator
                .finish()
                .observations
                .first()
                .unwrap()
                .derived_speaker_layout
                .value,
            Some(json!("5.1"))
        );
    }
    #[test]
    fn unknown_or_ambiguous_dsi_identity_is_not_associated() {
        for unknown in [false, true] {
            let topology = topology(5, true);
            let objects = objects(5, true);
            let mut accumulator = CoreLayoutAccumulator::default();
            accumulator.observe_source(source(&topology, &objects, &[], 0), MetadataDetail::Full);
            let mut dsi = dsi(1);
            if unknown {
                dsi.unavailable_presentation_identities = 1;
            } else {
                dsi.presentations
                    .push(dsi.presentations.first().unwrap().clone());
            }
            assert!(accumulator.apply_dsi(&dsi).is_empty());
            let report = accumulator.finish();
            assert!(
                report
                    .declarations
                    .iter()
                    .filter(|d| d.source == "dsi")
                    .all(|d| d.presentation_index.is_none() && d.association_reason.is_some())
            );
        }
    }

    #[test]
    fn extended_precision_uses_effective_coordinates_including_z_sign() {
        use macindecode_ac4_bitstream::oamd::ExtendedPrecisionPosition;
        let topology = topology(5, true);
        let mut objects = objects(5, true);
        let mut accumulator = CoreLayoutAccumulator::default();
        accumulator.observe_source(source(&topology, &objects, &[], 0), MetadataDetail::Full);
        let target = objects.get_mut(1).unwrap();
        target.additional.extended_position = Some(ExtendedPrecisionPosition {
            presence: 4,
            x: Some(3),
            y: None,
            z: None,
        });
        let clipped = update(*target, 0);
        target.additional.extended_position.as_mut().unwrap().z = Some(0);
        let positive = update(*target, 100);
        target.state.render.as_mut().unwrap().position.coding = PositionCoding::AbsoluteNegative;
        let negative = update(*target, 200);
        accumulator.observe_source(
            source(&topology, &objects, &[clipped, positive, negative], 1),
            MetadataDetail::Full,
        );
        let report = accumulator.finish();
        let grid = &report.observations.first().unwrap().observed_core_grid;
        assert_eq!(grid.change_count, 2);
        assert_eq!(grid.first_change.unwrap().offset_samples, Some(100));
    }

    #[test]
    fn unknown_topology_and_missing_common_leave_persistent_evidence_gaps() {
        let topology = topology(5, true);
        let objects = objects(5, true);
        let mut accumulator = CoreLayoutAccumulator::default();
        accumulator.observe_source(source(&topology, &objects, &[], 0), MetadataDetail::Full);
        accumulator.mark_topology_gap(1);
        accumulator.observe_source(source(&topology, &objects, &[], 2), MetadataDetail::Full);
        assert_eq!(
            accumulator
                .finish()
                .observations
                .first()
                .unwrap()
                .observed_core_grid
                .missing_frames,
            1
        );
        let mut accumulator = CoreLayoutAccumulator::default();
        let diagnostics = [macindecode_ac4_metadata::MetadataDiagnostic {
            partition: macindecode_ac4_metadata::MetadataPartition::GroupOamd,
            error: macindecode_ac4_metadata::MetadataError::new(
                MetadataErrorKind::HistoryUnavailable,
                macindecode_ac4_metadata::MetadataErrorContext::for_access_unit(0).with_group(0),
            ),
        }];
        let mut partial = source(&topology, &objects, &[], 0);
        partial.diagnostics = &diagnostics;
        partial.status = MetadataStatus::Partial;
        accumulator.observe_source(partial, MetadataDetail::Full);
        accumulator.observe_source(source(&topology, &objects, &[], 1), MetadataDetail::Full);
        let report = accumulator.finish();
        let observation = report.observations.first().unwrap();
        assert_eq!(
            observation.observed_core_grid.stability,
            CoreGridStability::Stable
        );
        assert_eq!(
            observation.derived_speaker_layout.status,
            FieldStatus::Unknown
        );
    }
}
