//! A-JOC core 场景导出的格式无关选择与 PCM 配对。
//!
//! ADM 与直接扬声器 CAF 都消费同一份第一 OAMD 和 `Qin_AJOC`。这里仅验证
//! 码流路径、对象图、q/LFE 顺序和呈现时长；具体格式是否允许某套位置/元数据，
//! 以及如何缩放样本，由各导出器决定。

use crate::metadata_batch::{MetadataBatch, MetadataElement, MetadataElementKind};
use crate::pcm_batch::{PcmBatch, PcmTrack, PcmTrackSource};
use macindecode_ac4_scene::DecodeMode;
use std::collections::BTreeSet;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CoreSceneErrorKind {
    SelectionInvalid,
    UnsupportedCodingPath,
    InternalInvariant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CoreSceneError {
    pub(crate) kind: CoreSceneErrorKind,
    pub(crate) message: String,
    pub(crate) context: Vec<(&'static str, String)>,
}

impl CoreSceneError {
    fn selection(message: impl Into<String>) -> Self {
        Self {
            kind: CoreSceneErrorKind::SelectionInvalid,
            message: message.into(),
            context: Vec::new(),
        }
    }

    fn unsupported(message: impl Into<String>) -> Self {
        Self {
            kind: CoreSceneErrorKind::UnsupportedCodingPath,
            message: message.into(),
            context: Vec::new(),
        }
    }

    fn invariant(message: impl Into<String>) -> Self {
        Self {
            kind: CoreSceneErrorKind::InternalInvariant,
            message: message.into(),
            context: Vec::new(),
        }
    }
}

impl fmt::Display for CoreSceneError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

#[derive(Debug)]
pub(crate) struct CoreSourceSelection {
    pub(crate) substream: u32,
    pub(crate) objects: Vec<MetadataElement>,
    pub(crate) lfe: Option<MetadataElement>,
}

#[derive(Debug)]
pub(crate) struct CoreMappedPcm<'a> {
    pub(crate) objects: Vec<&'a PcmTrack>,
    pub(crate) lfe: Option<&'a PcmTrack>,
    pub(crate) frames: usize,
}

pub(crate) fn select_core_sources(
    batch: &MetadataBatch,
) -> Result<CoreSourceSelection, CoreSceneError> {
    if batch.decode_mode != DecodeMode::Core {
        return Err(CoreSceneError::selection(
            "Core PCM mapping received a non-core scene batch",
        ));
    }
    let mut candidates = batch.elements.to_vec();
    candidates.sort_by_key(|item| (item.substream_index, item.object_index));
    if candidates.is_empty() {
        return Err(CoreSceneError::unsupported(
            "Selected presentation has no mappable A-JOC core objects",
        ));
    }

    let substreams = candidates
        .iter()
        .map(|item| item.substream_index)
        .collect::<BTreeSet<_>>();
    if substreams.len() != 1 {
        return Err(CoreSceneError::unsupported(format!(
            "Selected presentation references {} A-JOC core substreams; only one is currently supported",
            substreams.len()
        )));
    }
    let substream = substreams.first().copied().ok_or_else(|| {
        CoreSceneError::invariant("A-JOC core substream set is unexpectedly empty")
    })?;

    let mut lfe = None;
    let mut dynamic = Vec::new();
    for item in candidates {
        if item.kind == MetadataElementKind::LfeBed {
            if lfe.is_some() {
                return Err(CoreSceneError::unsupported(
                    "One A-JOC core substream declares multiple LFE objects",
                ));
            }
            if item.object_index != 0 {
                return Err(CoreSceneError::invariant(format!(
                    "LFE {}:{} must be a bed at object index 0",
                    item.substream_index, item.object_index
                )));
            }
            lfe = Some(item);
            continue;
        }
        dynamic.push(item);
    }
    if dynamic.is_empty() {
        return Err(CoreSceneError::unsupported(
            "Selected presentation has no dynamic full-range core objects",
        ));
    }
    dynamic.sort_by_key(|item| item.object_index);
    for (index, item) in dynamic.iter().enumerate() {
        let expected = u8::try_from(index.saturating_add(1))
            .map_err(|_| CoreSceneError::invariant("Core object count exceeds u8"))?;
        if item.object_index != expected {
            return Err(CoreSceneError::unsupported(format!(
                "Core full-range object indices are not contiguous from 1 through N: expected {expected}, got {}",
                item.object_index
            )));
        }
    }

    Ok(CoreSourceSelection {
        substream,
        objects: dynamic,
        lfe,
    })
}

pub(crate) fn map_core_pcm<'a>(
    batch: &MetadataBatch,
    pcm: &'a PcmBatch,
    selection: &CoreSourceSelection,
) -> Result<CoreMappedPcm<'a>, CoreSceneError> {
    if pcm.sample_rate != batch.sample_rate {
        return Err(CoreSceneError::invariant(format!(
            "PCM sample rate {} does not match scene sample rate {}",
            pcm.sample_rate, batch.sample_rate
        )));
    }
    let frames = usize::try_from(batch.duration_samples)
        .map_err(|_| CoreSceneError::invariant("Presentation duration exceeds usize"))?;
    let mut objects = Vec::new();
    let mut lfe = None;
    for channel in pcm
        .tracks
        .iter()
        .filter(|item| item.substream_index == selection.substream)
    {
        match channel.source {
            PcmTrackSource::AjocInput { .. } => objects.push(channel),
            PcmTrackSource::Lfe if lfe.is_none() => lfe = Some(channel),
            PcmTrackSource::Lfe => {
                return Err(CoreSceneError::invariant(
                    "One core substream decoded multiple LFE PCM tracks",
                ));
            }
            PcmTrackSource::TransportChannel { .. } => {
                return Err(CoreSceneError::invariant(
                    "Core export received transport-side element PCM that did not pass through A-SPX",
                ));
            }
            PcmTrackSource::AjocObject { .. } => {
                return Err(CoreSceneError::invariant(
                    "Core export received object PCM that already passed through full A-JOC upmix",
                ));
            }
        }
    }
    objects.sort_by_key(|item| match item.source {
        PcmTrackSource::AjocInput { input_index } => input_index,
        _ => usize::MAX,
    });
    if objects.len() != selection.objects.len() {
        return Err(CoreSceneError::invariant(format!(
            "Core OAMD has {} dynamic objects, but PCM has {} A-JOC inputs",
            selection.objects.len(),
            objects.len()
        )));
    }
    for (index, channel) in objects.iter().enumerate() {
        let PcmTrackSource::AjocInput { input_index } = channel.source else {
            return Err(CoreSceneError::invariant(
                "Core PCM object set contains a non-A-JOC-input source",
            ));
        };
        if input_index != index || channel.output_index != index {
            return Err(CoreSceneError::invariant(format!(
                "A-JOC input order is not contiguous: expected q{index} / output {index}, got q{input_index} / output {}",
                channel.output_index
            )));
        }
    }
    if selection.lfe.is_some() != lfe.is_some() {
        return Err(CoreSceneError::invariant(format!(
            "Core OAMD LFE={} does not match PCM LFE={}",
            selection.lfe.is_some(),
            lfe.is_some()
        )));
    }
    if let Some(channel) = lfe
        && channel.output_index != objects.len()
    {
        return Err(CoreSceneError::invariant(format!(
            "LFE must follow q0 through qN-1: expected output {}, got output {}",
            objects.len(),
            channel.output_index
        )));
    }
    for channel in objects.iter().copied().chain(lfe) {
        if channel.samples.len() != frames {
            return Err(CoreSceneError::invariant(format!(
                "Substream {} channel {} has {} samples; presentation duration is {frames}",
                channel.substream_index,
                channel.output_index,
                channel.samples.len()
            )));
        }
        if let Some(sample) = channel.samples.iter().find(|sample| !sample.is_finite()) {
            return Err(CoreSceneError::invariant(format!(
                "Core PCM contains a non-finite sample: {sample:?}"
            )));
        }
    }

    Ok(CoreMappedPcm {
        objects,
        lfe,
        frames,
    })
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::indexing_slicing,
        reason = "测试构造固定 q/LFE PCM 并定点注入错误"
    )]

    use super::*;
    use crate::metadata_batch::{MediaSpan, MetadataElementId};

    fn object(index: u8, lfe: bool) -> MetadataElement {
        MetadataElement {
            element_id: MetadataElementId::new(u64::from(index).saturating_add(u64::from(!lfe))),
            substream_index: 2,
            object_index: index,
            kind: if lfe {
                MetadataElementKind::LfeBed
            } else {
                MetadataElementKind::DynamicObject
            },
            common: None,
            common_conflict: false,
        }
    }

    fn batch(elements: Vec<MetadataElement>) -> MetadataBatch {
        MetadataBatch {
            sample_rate: 48_000,
            duration_samples: 2,
            media_span: Some(MediaSpan {
                start_sample: 0,
                end_sample: 2,
            }),
            decode_mode: DecodeMode::Core,
            elements,
            events: Vec::new(),
        }
    }

    fn pcm(include_lfe: bool) -> PcmBatch {
        let mut tracks = (0..5usize)
            .map(|q| PcmTrack {
                substream_index: 2,
                output_index: q,
                scene_element_id: None,
                source: PcmTrackSource::AjocInput { input_index: q },
                samples: vec![q as f32, q as f32 + 0.5],
            })
            .collect::<Vec<_>>();
        if include_lfe {
            tracks.push(PcmTrack {
                substream_index: 2,
                output_index: 5,
                scene_element_id: None,
                source: PcmTrackSource::Lfe,
                samples: vec![1.0, 2.0],
            });
        }
        PcmBatch {
            sample_rate: 48_000,
            tracks,
        }
    }

    #[test]
    fn selects_contiguous_dynamic_objects_and_retains_lfe_scene() {
        let mut objects = vec![object(0, true)];
        objects.extend((1..=5).map(|index| object(index, false)));
        let selection = select_core_sources(&batch(objects)).unwrap();
        assert_eq!(selection.substream, 2);
        assert_eq!(selection.objects.len(), 5);
        assert_eq!(selection.lfe.map(|item| item.object_index), Some(0));
    }

    #[test]
    fn rejects_duplicate_lfe_and_noncontiguous_q_indices() {
        let duplicate_lfe = batch(vec![object(0, true), object(0, true), object(1, false)]);
        assert_eq!(
            select_core_sources(&duplicate_lfe).unwrap_err().kind,
            CoreSceneErrorKind::UnsupportedCodingPath
        );
        let gap = batch(vec![object(0, true), object(1, false), object(3, false)]);
        assert_eq!(
            select_core_sources(&gap).unwrap_err().kind,
            CoreSceneErrorKind::UnsupportedCodingPath
        );
    }

    #[test]
    fn pcm_mapping_rejects_wrong_input_identity_missing_lfe_ragged_and_nonfinite_tracks() {
        let mut objects = vec![object(0, true)];
        objects.extend((1..=5).map(|index| object(index, false)));
        let scene = batch(objects);
        let selection = select_core_sources(&scene).unwrap();

        assert_eq!(
            map_core_pcm(&scene, &pcm(false), &selection)
                .unwrap_err()
                .kind,
            CoreSceneErrorKind::InternalInvariant
        );

        let mut duplicate_input = pcm(true);
        duplicate_input.tracks[1].source = PcmTrackSource::AjocInput { input_index: 0 };
        assert_eq!(
            map_core_pcm(&scene, &duplicate_input, &selection)
                .unwrap_err()
                .kind,
            CoreSceneErrorKind::InternalInvariant
        );

        let mut ragged = pcm(true);
        ragged.tracks[0].samples.pop();
        assert_eq!(
            map_core_pcm(&scene, &ragged, &selection).unwrap_err().kind,
            CoreSceneErrorKind::InternalInvariant
        );

        let mut nonfinite = pcm(true);
        nonfinite.tracks[1].samples[0] = f32::NAN;
        assert_eq!(
            map_core_pcm(&scene, &nonfinite, &selection)
                .unwrap_err()
                .kind,
            CoreSceneErrorKind::InternalInvariant
        );
    }
}
