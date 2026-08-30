//! MP4 呈现时间线到整批 PCM 制品的投影。
//!
//! AC-4 轨、bounded access unit、sample table 与 edit list 已由
//! `macindecode-ac4-mp4::Ac4Mp4` 收口；本层只把连续解码 PCM 按公共入口给出的
//! priming 与 media span 投影到制品时间线，不碰码流语义。

#[cfg(feature = "audio-decode")]
use crate::metadata_batch::MediaSpan;
#[cfg(feature = "audio-decode")]
use crate::pcm_batch::PcmBatch;

/// 把连续的 codec/media 时间线 PCM 投影到 MP4 edit 声明的呈现时间线。
///
/// 当前容器导出只接受至多一个非 empty edit，因此投影由三段组成：前导 empty
/// edit 的静音、被选中的连续媒体区间、尾随 empty edit 的静音。`source_start`
/// 是首个媒体 edit 在解码 PCM 中的起点；`media_span` 则是该区间在输出中的位置。
#[cfg(feature = "audio-decode")]
pub(crate) fn project_pcm_batch_to_presentation(
    pcm: Option<PcmBatch>,
    source_start: u64,
    output_frames: u64,
    media_span: Option<MediaSpan>,
) -> Result<Option<PcmBatch>, String> {
    let Some(mut pcm) = pcm else {
        return Ok(None);
    };
    let output_frames =
        usize::try_from(output_frames).map_err(|_| "PCM presentation duration exceeds usize")?;
    let source_start = usize::try_from(source_start).map_err(|_| "PCM edit start exceeds usize")?;
    let (visible_start, visible_end) = match media_span {
        Some(span) => (
            usize::try_from(span.start_sample)
                .map_err(|_| "PCM visible-range start exceeds usize")?,
            usize::try_from(span.end_sample).map_err(|_| "PCM visible-range end exceeds usize")?,
        ),
        None => (0, 0),
    };
    if visible_start > visible_end || visible_end > output_frames {
        return Err("PCM edit visible range exceeds the presentation timeline".to_owned());
    }
    let visible_frames = visible_end
        .checked_sub(visible_start)
        .ok_or("PCM edit visible range is negative")?;
    let source_end = source_start
        .checked_add(visible_frames)
        .ok_or("PCM edit source-range overflow")?;

    // 不设「投影是恒等就跳过」的快路径：实测八条向量的 `media_time` 恒为 2 048
    // （一帧 priming），`source_start == 0` 这一支从不成立，其中的长度判断更是
    // 双重不可达。恒等时照走一遍只多一次 `output_frames` 的拷贝，而少了一段没有
    // 判据能覆盖的分支——注入把长度判断删掉，单元测试与基线都不失败。
    for track in &mut pcm.tracks {
        let visible = track.samples.get(source_start..source_end).ok_or_else(|| {
            format!(
                "PCM edit source range {source_start}..{source_end} exceeds the {} samples in substream {} output {}",
                track.samples.len(),
                track.substream_index,
                track.output_index
            )
        })?;
        let mut projected = Vec::new();
        projected
            .try_reserve_exact(output_frames)
            .map_err(|error| {
                format!("Failed to allocate {output_frames} presentation PCM samples: {error}")
            })?;
        projected.resize(visible_start, 0.0);
        projected.extend_from_slice(visible);
        projected.resize(output_frames, 0.0);
        track.samples = projected;
    }
    Ok(Some(pcm))
}

#[cfg(test)]
#[cfg_attr(
    feature = "audio-decode",
    expect(
        clippy::indexing_slicing,
        reason = "测试按固定语法打包极小 TOC，下标越界即是该用例要报告的失败"
    )
)]
mod tests {
    #[cfg(feature = "audio-decode")]
    use super::*;
    #[cfg(feature = "audio-decode")]
    use crate::pcm_batch::{PcmBatch, PcmTrack, PcmTrackSource};

    #[cfg(feature = "audio-decode")]
    fn transport_track(samples: Vec<f32>) -> PcmTrack {
        PcmTrack {
            substream_index: 2,
            output_index: 0,
            scene_element_id: None,
            source: PcmTrackSource::TransportChannel {
                element_index: 0,
                channel_index: 0,
            },
            samples,
        }
    }
    #[cfg(feature = "audio-decode")]
    #[test]
    fn mp4_edit_projects_pcm_batch_to_the_presented_timeline() {
        let pcm = PcmBatch {
            sample_rate: 48_000,
            tracks: vec![transport_track(vec![0.0, 1.0, 2.0, 3.0, 4.0, 5.0])],
        };
        let projected = project_pcm_batch_to_presentation(
            Some(pcm),
            2,
            5,
            Some(MediaSpan {
                start_sample: 1,
                end_sample: 4,
            }),
        )
        .expect("edit 区间应可投影")
        .expect("已打开 PCM 留存");
        assert_eq!(projected.tracks[0].samples, [0.0, 2.0, 3.0, 4.0, 0.0]);

        let silent = project_pcm_batch_to_presentation(Some(projected), 0, 3, None)
            .expect("全 empty edit 应输出静音")
            .expect("声道描述仍需保留");
        assert_eq!(silent.tracks[0].samples, [0.0; 3]);
    }
    /// 无 priming、无裁剪时投影必须是恒等，且解码输出比呈现时长多出来的尾部
    /// 会被裁掉——先前的快路径正是在这两种情形上不可达，删掉后由本用例覆盖。
    #[cfg(feature = "audio-decode")]
    #[test]
    fn mp4_edit_projects_an_identity_span_without_copying_extra_samples() {
        let make = |samples: Vec<f32>| PcmBatch {
            sample_rate: 48_000,
            tracks: vec![transport_track(samples)],
        };
        let span = Some(MediaSpan {
            start_sample: 0,
            end_sample: 4,
        });

        let identity =
            project_pcm_batch_to_presentation(Some(make(vec![1.0, 2.0, 3.0, 4.0])), 0, 4, span)
                .expect("恒等投影应成功")
                .expect("已打开 PCM 留存");
        assert_eq!(identity.tracks[0].samples, [1.0, 2.0, 3.0, 4.0]);

        // 解码输出比呈现时长长：多出来的尾部必须裁掉，而不是原样留下。
        let trimmed = project_pcm_batch_to_presentation(
            Some(make(vec![1.0, 2.0, 3.0, 4.0, 5.0])),
            0,
            4,
            span,
        )
        .expect("裁尾应成功")
        .expect("已打开 PCM 留存");
        assert_eq!(trimmed.tracks[0].samples, [1.0, 2.0, 3.0, 4.0]);
    }
    #[cfg(feature = "audio-decode")]
    #[test]
    fn mp4_edit_rejects_a_pcm_source_range_past_the_decoder_output() {
        let pcm = PcmBatch {
            sample_rate: 48_000,
            tracks: vec![transport_track(vec![0.0; 4])],
        };
        let error = project_pcm_batch_to_presentation(
            Some(pcm),
            3,
            3,
            Some(MediaSpan {
                start_sample: 0,
                end_sample: 3,
            }),
        )
        .expect_err("源区间越界必须失败");
        assert!(error.contains("source range 3..6"), "{error}");
    }
}
