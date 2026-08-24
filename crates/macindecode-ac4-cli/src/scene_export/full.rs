//! A-JOC full 场景导出的格式无关选择与对象 PCM 配对。
//!
//! full OAMD 只在存在 LFE 时让它占据索引 `0`，此时动态全频对象为 `1..N`；
//! 无 LFE 时动态对象从 `0` 开始。重建 PCM 始终用 `AjocObject { object_index }` 表示
//! 矩阵输出，并按 `Pseudocode 15` 把可选 LFE 插入任意输出位置。本模块只按
//! 显式来源标签配对，不从交织位置反推对象身份。

use crate::metadata_batch::{MetadataBatch, MetadataElement, MetadataElementKind};
use crate::pcm_batch::{PcmBatch, PcmTrack, PcmTrackSource};
use macindecode_ac4_scene::DecodeMode;
use std::collections::BTreeSet;
use std::fmt;
use std::io::Write;

pub(crate) const FULL_BED_CHANNELS: usize = 10;
pub(crate) const FULL_LFE_BED_CHANNEL: usize = 3;
const PCM_S24_MAX: f64 = 8_388_607.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FullSceneErrorKind {
    SelectionInvalid,
    UnsupportedCodingPath,
    InternalInvariant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FullSceneError {
    pub(crate) kind: FullSceneErrorKind,
    pub(crate) message: String,
    pub(crate) context: Vec<(&'static str, String)>,
}

impl FullSceneError {
    fn selection(message: impl Into<String>) -> Self {
        Self {
            kind: FullSceneErrorKind::SelectionInvalid,
            message: message.into(),
            context: Vec::new(),
        }
    }

    fn unsupported(message: impl Into<String>) -> Self {
        Self {
            kind: FullSceneErrorKind::UnsupportedCodingPath,
            message: message.into(),
            context: Vec::new(),
        }
    }

    fn invariant(message: impl Into<String>) -> Self {
        Self {
            kind: FullSceneErrorKind::InternalInvariant,
            message: message.into(),
            context: Vec::new(),
        }
    }
}

impl fmt::Display for FullSceneError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

#[derive(Debug)]
pub(crate) struct FullSourceSelection {
    pub(crate) substream: u32,
    pub(crate) objects: Vec<MetadataElement>,
    pub(crate) lfe: Option<MetadataElement>,
}

#[derive(Debug)]
pub(crate) struct FullMappedPcm<'a> {
    /// 按零基 A-JOC 重建对象下标排列。
    pub(crate) objects: Vec<&'a PcmTrack>,
    pub(crate) lfe: Option<&'a PcmTrack>,
    pub(crate) frames: usize,
}

/// full ADM 与 DAMF 共用的节目级 PCM 视图。
///
/// 对象和 LFE 保持解码器内部 `±32 768` 量级；所有轨共享一个只衰减不放大的
/// 增益，避免矩阵叠加略微越界时逐轨削波或破坏相对电平。
#[derive(Debug)]
pub(crate) struct PreparedFullPcm<'a> {
    pub(crate) objects: Vec<&'a PcmTrack>,
    pub(crate) lfe: Option<&'a PcmTrack>,
    pub(crate) frames: usize,
    pub(crate) source_peak: f64,
    pub(crate) linear_gain: f64,
}

pub(crate) fn prepare_full_pcm(mapped: FullMappedPcm<'_>) -> PreparedFullPcm<'_> {
    let source_peak = mapped
        .objects
        .iter()
        .copied()
        .chain(mapped.lfe)
        .flat_map(|channel| channel.samples.iter().copied())
        .fold(0.0_f64, |peak, sample| peak.max(f64::from(sample).abs()));
    let linear_gain = if source_peak > 32_768.0 {
        32_768.0 / source_peak
    } else {
        1.0
    };
    PreparedFullPcm {
        objects: mapped.objects,
        lfe: mapped.lfe,
        frames: mapped.frames,
        source_peak,
        linear_gain,
    }
}

/// 写出十轨 compatibility bed（第 4 轨为可选 LFE）和随后的 full 对象。
///
/// 调用方只负责容器头；WAVE `data` 与 CAF audio payload 因而消费同一份逐帧字节流。
pub(crate) fn write_full_s24le<W: Write>(
    writer: &mut W,
    audio: &PreparedFullPcm<'_>,
) -> Result<(), String> {
    let channels = FULL_BED_CHANNELS.saturating_add(audio.objects.len());
    let mut chunk = Vec::with_capacity(channels.saturating_mul(3).saturating_mul(1024));
    for frame in 0..audio.frames {
        for bed in 0..FULL_BED_CHANNELS {
            let sample = if bed == FULL_LFE_BED_CHANNEL {
                audio
                    .lfe
                    .and_then(|channel| channel.samples.get(frame))
                    .copied()
                    .unwrap_or(0.0)
            } else {
                0.0
            };
            chunk.extend_from_slice(&full_s24_sample(sample, audio.linear_gain)?);
        }
        for channel in &audio.objects {
            let sample = channel
                .samples
                .get(frame)
                .copied()
                .ok_or("full 对象 PCM 短于声明时长")?;
            chunk.extend_from_slice(&full_s24_sample(sample, audio.linear_gain)?);
        }
        if chunk.len() >= channels.saturating_mul(3).saturating_mul(1024) {
            writer
                .write_all(&chunk)
                .map_err(|error| format!("写 full PCM 失败：{error}"))?;
            chunk.clear();
        }
    }
    if !chunk.is_empty() {
        writer
            .write_all(&chunk)
            .map_err(|error| format!("写 full PCM 失败：{error}"))?;
    }
    Ok(())
}

pub(crate) fn full_s24_sample(sample: f32, linear_gain: f64) -> Result<[u8; 3], String> {
    if !sample.is_finite() {
        return Err(format!("full PCM 样本 {sample:?} 不是有限值"));
    }
    if !linear_gain.is_finite() || linear_gain <= 0.0 || linear_gain > 1.0 {
        return Err(format!("full PCM 全局增益 {linear_gain:?} 无效"));
    }
    let normalized = f64::from(sample) * linear_gain;
    if normalized.abs() > 32_768.0 + f64::EPSILON {
        return Err(format!(
            "full PCM 样本 {sample:?} 经全局增益 {linear_gain:.12} 后仍超出 ±32768"
        ));
    }
    let value = (normalized / 32_768.0 * PCM_S24_MAX)
        .round()
        .clamp(-PCM_S24_MAX, PCM_S24_MAX) as i32;
    let bytes = value.to_le_bytes();
    Ok([bytes[0], bytes[1], bytes[2]])
}

pub(crate) fn select_full_sources(
    batch: &MetadataBatch,
) -> Result<FullSourceSelection, FullSceneError> {
    if batch.decode_mode != DecodeMode::Full {
        return Err(FullSceneError::selection(
            "full PCM 映射收到非 full Scene 批次",
        ));
    }
    let mut candidates = batch.elements.to_vec();
    candidates.sort_by_key(|item| (item.substream_index, item.object_index));
    if candidates.is_empty() {
        return Err(FullSceneError::unsupported(
            "所选 presentation 没有可映射的 A-JOC full 对象",
        ));
    }

    let substreams = candidates
        .iter()
        .map(|item| item.substream_index)
        .collect::<BTreeSet<_>>();
    if substreams.len() != 1 {
        return Err(FullSceneError::unsupported(format!(
            "所选 presentation 引用了 {} 条 A-JOC full substream；当前只支持一条",
            substreams.len()
        )));
    }
    let substream = substreams
        .first()
        .copied()
        .ok_or_else(|| FullSceneError::invariant("A-JOC full substream 集合意外为空"))?;

    let mut lfe = None;
    let mut dynamic = Vec::new();
    for item in candidates {
        if item.kind == MetadataElementKind::LfeBed {
            if lfe.is_some() {
                return Err(FullSceneError::unsupported(
                    "同一 A-JOC full substream 声明了多条 LFE",
                ));
            }
            if item.object_index != 0 {
                return Err(FullSceneError::invariant(format!(
                    "full LFE {}:{} 必须是对象索引 0 的 BED",
                    item.substream_index, item.object_index
                )));
            }
            lfe = Some(item);
            continue;
        }
        dynamic.push(item);
    }
    if dynamic.is_empty() {
        return Err(FullSceneError::unsupported(
            "所选 presentation 没有动态全频 full 对象",
        ));
    }
    dynamic.sort_by_key(|item| item.object_index);
    let first_dynamic = usize::from(lfe.is_some());
    for (index, item) in dynamic.iter().enumerate() {
        let expected = u8::try_from(index.saturating_add(first_dynamic))
            .map_err(|_| FullSceneError::invariant("full 对象数量超出 u8"))?;
        if item.object_index != expected {
            return Err(FullSceneError::unsupported(format!(
                "full 全频对象索引不是从 {first_dynamic} 开始连续：期待 {expected}，实际为 {}",
                item.object_index
            )));
        }
    }

    Ok(FullSourceSelection {
        substream,
        objects: dynamic,
        lfe,
    })
}

pub(crate) fn map_full_pcm<'a>(
    batch: &MetadataBatch,
    pcm: &'a PcmBatch,
    selection: &FullSourceSelection,
) -> Result<FullMappedPcm<'a>, FullSceneError> {
    if pcm.sample_rate != batch.sample_rate {
        return Err(FullSceneError::invariant(format!(
            "PCM 采样率 {} 与场景采样率 {} 不一致",
            pcm.sample_rate, batch.sample_rate
        )));
    }
    let frames = usize::try_from(batch.duration_samples)
        .map_err(|_| FullSceneError::invariant("呈现时长超出 usize"))?;
    let mut objects = vec![None; selection.objects.len()];
    let mut lfe = None;
    let mut output_channels = BTreeSet::new();

    for channel in pcm
        .tracks
        .iter()
        .filter(|item| item.substream_index == selection.substream)
    {
        if !output_channels.insert(channel.output_index) {
            return Err(FullSceneError::invariant(format!(
                "full 对象输出位置 {} 重复",
                channel.output_index
            )));
        }
        match channel.source {
            PcmTrackSource::AjocObject { object_index } => {
                let count = objects.len();
                let slot = objects.get_mut(object_index).ok_or_else(|| {
                    FullSceneError::invariant(format!(
                        "PCM 声明 A-JOC 对象 {object_index}，full OAMD 只有 {count} 个对象"
                    ))
                })?;
                if slot.replace(channel).is_some() {
                    return Err(FullSceneError::invariant(format!(
                        "A-JOC 对象 {object_index} 出现多路 PCM"
                    )));
                }
            }
            PcmTrackSource::Lfe if lfe.is_none() => lfe = Some(channel),
            PcmTrackSource::Lfe => {
                return Err(FullSceneError::invariant(
                    "同一 full substream 解出了多条 LFE PCM",
                ));
            }
            PcmTrackSource::TransportChannel { .. } => {
                return Err(FullSceneError::invariant(
                    "full 导出收到未经过 A-SPX 的传输侧元素 PCM",
                ));
            }
            PcmTrackSource::AjocInput { .. } => {
                return Err(FullSceneError::invariant(
                    "full 导出收到尚未经过 A-JOC 上混的输入 PCM",
                ));
            }
        }
    }

    if selection.lfe.is_some() != lfe.is_some() {
        return Err(FullSceneError::invariant(format!(
            "full OAMD 的 LFE={}，PCM 的 LFE={}，两者不一致",
            selection.lfe.is_some(),
            lfe.is_some()
        )));
    }
    let expected_channels = selection
        .objects
        .len()
        .saturating_add(usize::from(selection.lfe.is_some()));
    if output_channels.len() != expected_channels
        || output_channels.iter().copied().ne(0..expected_channels)
    {
        return Err(FullSceneError::invariant(format!(
            "Pseudocode 15 输出位置必须连续为 0..{expected_channels}，实际为 {output_channels:?}"
        )));
    }

    let objects = objects
        .into_iter()
        .enumerate()
        .map(|(object, channel)| {
            channel.ok_or_else(|| {
                FullSceneError::invariant(format!(
                    "full OAMD 对象 {} 缺少 PCM",
                    object.saturating_add(1)
                ))
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    for channel in objects.iter().copied().chain(lfe) {
        if channel.samples.len() != frames {
            return Err(FullSceneError::invariant(format!(
                "substream {} 输出 {} 有 {} 个样本，呈现时长为 {frames}",
                channel.substream_index,
                channel.output_index,
                channel.samples.len()
            )));
        }
        if let Some(sample) = channel.samples.iter().find(|sample| !sample.is_finite()) {
            return Err(FullSceneError::invariant(format!(
                "full 对象 PCM 含非有限样本 {sample:?}"
            )));
        }
    }

    Ok(FullMappedPcm {
        objects,
        lfe,
        frames,
    })
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::indexing_slicing,
        reason = "测试构造固定对象/LFE PCM 并定点注入错误"
    )]

    use super::*;
    use crate::metadata_batch::{MediaSpan, MetadataElementId};

    fn object(index: u8, lfe: bool) -> MetadataElement {
        MetadataElement {
            element_id: MetadataElementId::new(u64::from(index).saturating_add(if lfe {
                1
            } else {
                100
            })),
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
            decode_mode: DecodeMode::Full,
            elements,
            events: Vec::new(),
        }
    }

    fn pcm(lfe_position: Option<usize>) -> PcmBatch {
        let mut tracks = Vec::new();
        let mut output = 0usize;
        for object in 0..3usize {
            if lfe_position == Some(output) {
                tracks.push(PcmTrack {
                    substream_index: 2,
                    output_index: output,
                    scene_element_id: None,
                    source: PcmTrackSource::Lfe,
                    samples: vec![10.0, 11.0],
                });
                output = output.saturating_add(1);
            }
            tracks.push(PcmTrack {
                substream_index: 2,
                output_index: output,
                scene_element_id: None,
                source: PcmTrackSource::AjocObject {
                    object_index: object,
                },
                samples: vec![object as f32, object as f32 + 0.5],
            });
            output = output.saturating_add(1);
        }
        if lfe_position == Some(output) {
            tracks.push(PcmTrack {
                substream_index: 2,
                output_index: output,
                scene_element_id: None,
                source: PcmTrackSource::Lfe,
                samples: vec![10.0, 11.0],
            });
        }
        PcmBatch {
            sample_rate: 48_000,
            tracks,
        }
    }

    fn full_scene(include_lfe: bool) -> MetadataBatch {
        let mut elements = Vec::new();
        if include_lfe {
            elements.push(object(0, true));
        }
        let first_dynamic = u8::from(include_lfe);
        elements.extend(
            (first_dynamic..first_dynamic.saturating_add(3)).map(|index| object(index, false)),
        );
        batch(elements)
    }

    #[test]
    fn selects_only_full_objects_and_retains_lfe() {
        let scene = full_scene(true);
        let selected = select_full_sources(&scene).expect("full 对象应可选择");
        assert_eq!(selected.substream, 2);
        assert_eq!(
            selected
                .objects
                .iter()
                .map(|item| item.object_index)
                .collect::<Vec<_>>(),
            [1, 2, 3]
        );
        assert_eq!(selected.lfe.map(|item| item.object_index), Some(0));
    }

    #[test]
    fn selects_and_maps_zero_based_objects_without_lfe() {
        let scene = full_scene(false);
        let selected = select_full_sources(&scene).expect("无 LFE 的 full 对象应可选择");
        assert_eq!(
            selected
                .objects
                .iter()
                .map(|item| item.object_index)
                .collect::<Vec<_>>(),
            [0, 1, 2]
        );
        assert!(selected.lfe.is_none());

        let pcm = pcm(None);
        let mapped = map_full_pcm(&scene, &pcm, &selected).expect("零基 OAMD 应与对象 PCM 配对");
        assert_eq!(mapped.frames, 2);
        assert!(mapped.lfe.is_none());
        assert_eq!(
            mapped
                .objects
                .iter()
                .map(|item| item.source)
                .collect::<Vec<_>>(),
            [
                PcmTrackSource::AjocObject { object_index: 0 },
                PcmTrackSource::AjocObject { object_index: 1 },
                PcmTrackSource::AjocObject { object_index: 2 },
            ]
        );
    }

    #[test]
    fn full_selection_rejects_a_core_batch() {
        let mut scene = full_scene(true);
        scene.decode_mode = DecodeMode::Core;
        assert_eq!(
            select_full_sources(&scene).unwrap_err().kind,
            FullSceneErrorKind::SelectionInvalid
        );
    }

    #[test]
    fn maps_lfe_at_the_start_middle_or_end_without_changing_object_identity() {
        let scene = full_scene(true);
        let selected = select_full_sources(&scene).unwrap();
        for position in [0usize, 2, 3] {
            let pcm = pcm(Some(position));
            let mapped = map_full_pcm(&scene, &pcm, &selected).expect("显式来源应可配对");
            assert_eq!(mapped.lfe.map(|item| item.output_index), Some(position));
            assert_eq!(
                mapped
                    .objects
                    .iter()
                    .map(|item| item.source)
                    .collect::<Vec<_>>(),
                [
                    PcmTrackSource::AjocObject { object_index: 0 },
                    PcmTrackSource::AjocObject { object_index: 1 },
                    PcmTrackSource::AjocObject { object_index: 2 },
                ]
            );
        }
    }

    #[test]
    fn rejects_noncontiguous_oamd_and_missing_or_duplicate_pcm_objects() {
        let gap = batch(vec![object(0, true), object(1, false), object(3, false)]);
        assert_eq!(
            select_full_sources(&gap).unwrap_err().kind,
            FullSceneErrorKind::UnsupportedCodingPath
        );

        let mut second_substream = object(1, false);
        second_substream.substream_index = 3;
        let multiple = batch(vec![object(0, true), object(1, false), second_substream]);
        assert_eq!(
            select_full_sources(&multiple).unwrap_err().kind,
            FullSceneErrorKind::UnsupportedCodingPath
        );

        let malformed_lfe = batch(vec![object(1, true), object(1, false)]);
        assert_eq!(
            select_full_sources(&malformed_lfe).unwrap_err().kind,
            FullSceneErrorKind::InternalInvariant
        );

        let scene = full_scene(true);
        let selected = select_full_sources(&scene).unwrap();
        let mut missing = pcm(Some(0));
        missing
            .tracks
            .retain(|item| item.source != PcmTrackSource::AjocObject { object_index: 1 });
        assert_eq!(
            map_full_pcm(&scene, &missing, &selected).unwrap_err().kind,
            FullSceneErrorKind::InternalInvariant
        );

        let mut duplicate = pcm(Some(0));
        duplicate.tracks[2].source = PcmTrackSource::AjocObject { object_index: 0 };
        assert_eq!(
            map_full_pcm(&scene, &duplicate, &selected)
                .unwrap_err()
                .kind,
            FullSceneErrorKind::InternalInvariant
        );
    }

    #[test]
    fn rejects_wrong_source_shape_and_nonfinite_samples() {
        let scene = full_scene(true);
        let selected = select_full_sources(&scene).unwrap();

        let mut wrong_rate = pcm(Some(0));
        wrong_rate.sample_rate = 44_100;
        assert_eq!(
            map_full_pcm(&scene, &wrong_rate, &selected)
                .unwrap_err()
                .kind,
            FullSceneErrorKind::InternalInvariant
        );

        let mut wrong_source = pcm(Some(0));
        wrong_source.tracks[1].source = PcmTrackSource::AjocInput { input_index: 0 };
        assert_eq!(
            map_full_pcm(&scene, &wrong_source, &selected)
                .unwrap_err()
                .kind,
            FullSceneErrorKind::InternalInvariant
        );

        let mut wrong_topology = pcm(Some(0));
        wrong_topology.tracks[0].output_index = 9;
        assert_eq!(
            map_full_pcm(&scene, &wrong_topology, &selected)
                .unwrap_err()
                .kind,
            FullSceneErrorKind::InternalInvariant
        );

        let mut ragged = pcm(Some(2));
        ragged.tracks[0].samples.pop();
        assert_eq!(
            map_full_pcm(&scene, &ragged, &selected).unwrap_err().kind,
            FullSceneErrorKind::InternalInvariant
        );

        let mut nonfinite = pcm(Some(3));
        nonfinite.tracks[1].samples[0] = f32::NAN;
        assert_eq!(
            map_full_pcm(&scene, &nonfinite, &selected)
                .unwrap_err()
                .kind,
            FullSceneErrorKind::InternalInvariant
        );
    }
}
