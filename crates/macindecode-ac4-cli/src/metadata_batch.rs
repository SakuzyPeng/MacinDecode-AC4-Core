//! 公共 Scene 借用元数据到 artifact writer 所需 owned 时间线的批量适配。
//!
//! Session 已经解析唯一 presentation 与解码模式；本层只为跨 AU 的容器 edit、
//! ADM/DAMF/CoreCAF 写出拥有元素描述与 raw OAMD 状态，不重新实现 presentation
//! 选择。事件通过配置代次内稳定的 `SceneElementId` 关联元素，码流下标只作为
//! artifact 标签保留。

use crate::container::{scale_i64_round, scale_u64_round};
use macindecode_ac4_bitstream::oamd::{
    AdditionalObjectMetadata, OamdCommonData, ObjectBasicState, ObjectGainState,
    ObjectMetadataState, ObjectPriorityState, ObjectRenderState, OtherPropertiesUpdate,
    PositionCoding, QuantizedPosition, ZoneUpdate,
};
use macindecode_ac4_scene::{DecodeMode, SceneElementId};

/// 写出批次中保留的 Scene 元素标识。
///
/// 数值直接来自公共 `SceneElementId`；独立包装避免 artifact 测试伪造公共 Session
/// 才能分配的标识，同时仍让元素与事件按配置代次内的真实身份关联。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct MetadataElementId(u64);

impl MetadataElementId {
    #[cfg(test)]
    pub(crate) const fn new(value: u64) -> Self {
        Self(value)
    }
}

impl From<SceneElementId> for MetadataElementId {
    fn from(value: SceneElementId) -> Self {
        Self(value.get())
    }
}

/// artifact 元数据元素的受支持种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MetadataElementKind {
    DynamicObject,
    LfeBed,
}

/// 可选择的物理 A-JOC 对象。
#[cfg(feature = "audio-decode")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MetadataElement {
    pub element_id: MetadataElementId,
    pub substream_index: u32,
    pub object_index: u8,
    pub kind: MetadataElementKind,
    pub common: Option<OamdCommonData>,
    pub common_conflict: bool,
}

/// 已合并复用和差分后的完整对象事件。
#[cfg(feature = "audio-decode")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MetadataEvent {
    /// 应用容器 edit 后的输入采样位置；preroll 为负数。
    pub sample_position: i64,
    pub element_id: MetadataElementId,
    /// 同一绝对 offset 的 Scene 更新按此码流到达顺序稳定排序。
    pub stream_order: u64,
    pub ramp_samples: u32,
    pub state: ObjectMetadataState,
    pub additional: AdditionalObjectMetadata,
}

/// 呈现时间轴中真正引用媒体的连续区间。
#[cfg(feature = "audio-decode")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MediaSpan {
    pub start_sample: u64,
    pub end_sample: u64,
}

/// 一份输入中可导出的完整场景时间线。
#[cfg(feature = "audio-decode")]
#[derive(Debug)]
pub(crate) struct MetadataBatch {
    pub sample_rate: u32,
    pub duration_samples: u64,
    pub media_span: Option<MediaSpan>,
    pub decode_mode: DecodeMode,
    pub elements: Vec<MetadataElement>,
    pub events: Vec<MetadataEvent>,
}

/// 已换算到导出采样率的完整对象事件。
#[cfg(feature = "audio-decode")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OutputMetadataEvent {
    pub sample: u64,
    pub ramp: u64,
    pub state: ObjectMetadataState,
    pub additional: AdditionalObjectMetadata,
}

/// 为 DAMF 与 ADM 导出统一应用 edit 可见区间、preroll 状态与边界静音。
#[cfg(feature = "audio-decode")]
pub(crate) fn project_metadata_events(
    batch: &MetadataBatch,
    selected: &MetadataElement,
    output_sample_rate: u32,
    duration: u64,
) -> Result<Vec<OutputMetadataEvent>, String> {
    let inactive = default_output_metadata_event();
    let Some(media_span) = batch.media_span else {
        return Ok(vec![inactive]);
    };
    if media_span.start_sample > media_span.end_sample
        || media_span.end_sample > batch.duration_samples
    {
        return Err("Media-edit visible range exceeds the presentation timeline".to_owned());
    }

    let visible_start = scale_u64_round(
        media_span.start_sample,
        u64::from(output_sample_rate),
        u64::from(batch.sample_rate),
    )?;
    let visible_end = scale_u64_round(
        media_span.end_sample,
        u64::from(output_sample_rate),
        u64::from(batch.sample_rate),
    )?;
    if visible_start > visible_end || visible_end > duration {
        return Err("Media-edit visible range exceeds the export duration".to_owned());
    }
    if visible_start == visible_end {
        return Ok(vec![inactive]);
    }
    let visible_start_i64 =
        i64::try_from(visible_start).map_err(|_| "Media-edit start exceeds i64")?;
    let visible_end_i64 = i64::try_from(visible_end).map_err(|_| "Media-edit end exceeds i64")?;

    let mut source = batch
        .events
        .iter()
        .copied()
        .filter(|event| event.element_id == selected.element_id)
        .collect::<Vec<_>>();
    source.sort_by_key(|event| (event.sample_position, event.stream_order));

    let mut before = None;
    let mut at_start = None;
    let mut inside: Vec<OutputMetadataEvent> = Vec::new();
    for event in source {
        let sample = scale_i64_round(
            event.sample_position,
            i64::from(output_sample_rate),
            i64::from(batch.sample_rate),
        )?;
        let source_end = event
            .sample_position
            .checked_add(i64::from(event.ramp_samples))
            .ok_or("Event-ramp end-position overflow")?;
        let end = scale_i64_round(
            source_end,
            i64::from(output_sample_rate),
            i64::from(batch.sample_rate),
        )?;
        let ramp = u64::try_from(end.saturating_sub(sample)).unwrap_or(0);
        if sample < visible_start_i64 {
            before = Some((event, end));
            continue;
        }
        if sample >= visible_end_i64 {
            continue;
        }
        let sample = u64::try_from(sample).map_err(|_| "Event sample position is negative")?;
        let mapped = OutputMetadataEvent {
            sample,
            ramp: ramp.min(duration.saturating_sub(sample)),
            state: event.state,
            additional: event.additional,
        };
        if sample == visible_start {
            at_start = Some(mapped);
        } else if inside
            .last()
            .is_some_and(|previous| previous.sample == sample)
        {
            if let Some(previous) = inside.last_mut() {
                *previous = mapped;
            }
        } else {
            inside.push(mapped);
        }
    }

    if let Some((_, end)) = before {
        if end > visible_start_i64 {
            return Err(format!(
                "Media-edit start falls inside the ramp for object {}:{}; lossless export is impossible",
                selected.substream_index, selected.object_index
            ));
        }
    }

    let mut mapped = Vec::new();
    if let Some(event) = at_start {
        mapped.push(event);
    } else if let Some((event, _)) = before {
        mapped.push(OutputMetadataEvent {
            sample: visible_start,
            ramp: 0,
            state: event.state,
            additional: event.additional,
        });
    }
    mapped.extend(inside);

    let mut output = vec![inactive];
    for event in mapped {
        if output
            .last()
            .is_some_and(|previous| previous.sample == event.sample)
        {
            if let Some(previous) = output.last_mut() {
                *previous = event;
            }
        } else {
            output.push(event);
        }
    }
    if visible_end < duration && output.last().is_some_and(|event| event.state.active) {
        let mut boundary = inactive;
        boundary.sample = visible_end;
        output.push(boundary);
    }
    Ok(output)
}

#[cfg(feature = "audio-decode")]
pub(crate) fn default_output_metadata_event() -> OutputMetadataEvent {
    OutputMetadataEvent {
        sample: 0,
        ramp: 0,
        state: ObjectMetadataState {
            active: false,
            basic: Some(ObjectBasicState {
                gain: ObjectGainState::NegativeInfinity,
                priority: ObjectPriorityState::Minimum,
            }),
            render: Some(ObjectRenderState {
                position: QuantizedPosition {
                    x: 31,
                    y: 31,
                    z: 0,
                    coding: PositionCoding::AbsolutePositive,
                },
                zone: ZoneUpdate {
                    grouped_defaults: true,
                    group_zone_flag: None,
                    zone_mask: None,
                },
                other_properties: OtherPropertiesUpdate {
                    grouped_defaults: true,
                    group_other_mask: None,
                    width: None,
                    screen_factor_code: None,
                    depth_factor: None,
                    object_at_infinity: None,
                    distance_factor_code: None,
                    divergence_mode: None,
                    divergence_table: None,
                    divergence_code: None,
                },
            }),
        },
        additional: AdditionalObjectMetadata::default(),
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "audio-decode")]
    use super::*;
    use crate::trace::testutil::*;
    #[cfg(feature = "audio-decode")]
    #[test]
    fn metadata_projection_keeps_empty_edits_inactive() {
        let scene = output_test_scene();
        let batch = MetadataBatch {
            sample_rate: 48_000,
            duration_samples: 4_000,
            media_span: Some(MediaSpan {
                start_sample: 1_000,
                end_sample: 3_000,
            }),
            decode_mode: DecodeMode::Full,
            elements: vec![scene],
            events: vec![output_test_event(500, 0)],
        };

        let output = project_metadata_events(&batch, &scene, 48_000, 4_000).unwrap();
        assert_eq!(
            output
                .iter()
                .map(|event| (event.sample, event.state.active))
                .collect::<Vec<_>>(),
            [(0, false), (1_000, true), (3_000, false)]
        );
    }
    #[cfg(feature = "audio-decode")]
    #[test]
    fn metadata_projection_rejects_edit_boundaries_inside_ramps() {
        let scene = output_test_scene();
        let batch = MetadataBatch {
            sample_rate: 48_000,
            duration_samples: 4_000,
            media_span: Some(MediaSpan {
                start_sample: 1_000,
                end_sample: 3_000,
            }),
            decode_mode: DecodeMode::Full,
            elements: vec![scene],
            events: vec![output_test_event(500, 600)],
        };
        assert!(
            project_metadata_events(&batch, &scene, 48_000, 4_000)
                .expect_err("edit 起点穿过 ramp 必须拒绝")
                .contains("ramp")
        );
    }
    #[cfg(feature = "audio-decode")]
    #[test]
    fn metadata_projection_preserves_a_ramp_cut_off_by_the_media_end() {
        let scene = output_test_scene();
        let batch = MetadataBatch {
            sample_rate: 48_000,
            duration_samples: 4_000,
            media_span: Some(MediaSpan {
                start_sample: 1_000,
                end_sample: 3_000,
            }),
            decode_mode: DecodeMode::Full,
            elements: vec![scene],
            events: vec![output_test_event(2_500, 600)],
        };

        let output = project_metadata_events(&batch, &scene, 48_000, 4_000).unwrap();
        assert_eq!(
            output
                .iter()
                .map(|event| (event.sample, event.ramp, event.state.active))
                .collect::<Vec<_>>(),
            [(0, 0, false), (2_500, 600, true), (3_000, 0, false)]
        );
    }

    #[cfg(feature = "audio-decode")]
    #[test]
    fn metadata_projection_uses_element_identity_and_stream_order() {
        let scene = output_test_scene();
        let mut later = output_test_event(0, 0);
        later.stream_order = 2;
        later.state.render.as_mut().unwrap().position.x = 62;
        let mut unrelated = output_test_event(0, 0);
        unrelated.element_id = MetadataElementId::new(999);
        unrelated.stream_order = 3;
        unrelated.state.render.as_mut().unwrap().position.x = 31;
        let mut earlier = output_test_event(0, 0);
        earlier.stream_order = 1;
        earlier.state.render.as_mut().unwrap().position.x = 0;
        let batch = MetadataBatch {
            sample_rate: 48_000,
            duration_samples: 1_000,
            media_span: Some(MediaSpan {
                start_sample: 0,
                end_sample: 1_000,
            }),
            decode_mode: DecodeMode::Full,
            elements: vec![scene],
            events: vec![later, unrelated, earlier],
        };

        let output = project_metadata_events(&batch, &scene, 48_000, 1_000).unwrap();
        assert_eq!(output.len(), 1);
        assert_eq!(
            output
                .first()
                .expect("应生成一份状态")
                .state
                .render
                .expect("应保留完整 render 状态")
                .position
                .x,
            62,
            "同 offset 必须按码流顺序取最后一份所选元素状态"
        );
    }
}
