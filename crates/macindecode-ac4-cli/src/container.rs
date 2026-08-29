//! MP4 容器定位与呈现时间线投影。
//!
//! 找到 `ac-4` 轨、读出 sample table 与 edit list，并把核心带 PCM 从媒体
//! 时间线投影到呈现时间线。这一层只认容器结构，不碰码流语义。

#[cfg(feature = "audio-decode")]
use crate::metadata_batch::MediaSpan;
#[cfg(feature = "audio-decode")]
use crate::pcm_batch::PcmBatch;
pub(crate) use macindecode_ac4_mp4::find_ac4_track;
#[cfg(feature = "audio-decode")]
use macindecode_ac4_mp4::{EditListEntry, media_time_to_presentation};

/// 把 edit list 中唯一的媒体区段投影到输出采样时间轴。
///
/// `presentation_timing` 会对每个 edit 的 movie-timescale 时长分别向下换算；
/// 这里使用同一口径累加，再对绝对起止点换算一次，避免边界重复舍入。
#[cfg(feature = "audio-decode")]
pub(crate) fn presentation_media_span(
    sample_rate: u32,
    media_timescale: u32,
    movie_timescale: u32,
    edits: &[EditListEntry],
) -> Result<Option<MediaSpan>, String> {
    let mut cursor = 0u64;
    let mut media_span = None;
    for entry in edits {
        let duration = scale_u64_floor(
            entry.segment_duration,
            u64::from(media_timescale),
            u64::from(movie_timescale),
        )?;
        let end = cursor
            .checked_add(duration)
            .ok_or("MP4 edit presentation range overflow")?;
        if !entry.is_empty_edit() {
            media_span = Some(MediaSpan {
                start_sample: scale_u64_round(
                    cursor,
                    u64::from(sample_rate),
                    u64::from(media_timescale),
                )?,
                end_sample: scale_u64_round(
                    end,
                    u64::from(sample_rate),
                    u64::from(media_timescale),
                )?,
            });
        }
        cursor = end;
    }
    Ok(media_span)
}

/// 首版导出只接受至多一个非 empty 媒体 edit，因此整个媒体时间轴只发生一个
/// 仿射平移。把该平移量只换算一次可保留帧内不足一个 media tick 的事件偏移；
/// 逐事件先降到 media timescale 再升回采样率会产生不可逆的双重舍入。
#[cfg(feature = "audio-decode")]
pub(crate) fn presentation_sample_shift(
    sample_rate: u32,
    media_timescale: u32,
    movie_timescale: u32,
    edits: &[EditListEntry],
) -> Result<Option<i64>, String> {
    let Some(presentation_zero) =
        media_time_to_presentation(0, media_timescale, movie_timescale, edits)
            .map_err(|error| error.to_string())?
    else {
        return Ok(None);
    };
    scale_i64_round(
        presentation_zero,
        i64::from(sample_rate),
        i64::from(media_timescale),
    )
    .map(Some)
}

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

#[cfg(feature = "audio-decode")]
#[expect(
    clippy::arithmetic_side_effects,
    reason = "uses i128 and checks overflow at every step"
)]
pub(crate) fn scale_i64_round(value: i64, numerator: i64, denominator: i64) -> Result<i64, String> {
    if numerator <= 0 || denominator <= 0 {
        return Err("Timeline ratio must be positive".to_owned());
    }
    let scaled = i128::from(value)
        .checked_mul(i128::from(numerator))
        .ok_or("Timeline multiplication overflow")?;
    let half = i128::from(denominator) / 2;
    let rounded = if scaled >= 0 {
        scaled
            .checked_add(half)
            .ok_or("Timeline rounding overflow")?
    } else {
        scaled
            .checked_sub(half)
            .ok_or("Timeline rounding overflow")?
    } / i128::from(denominator);
    i64::try_from(rounded).map_err(|_| "Timeline value exceeds i64".to_owned())
}

#[cfg(feature = "audio-decode")]
pub(crate) fn scale_u64_round(value: u64, numerator: u64, denominator: u64) -> Result<u64, String> {
    if numerator == 0 || denominator == 0 {
        return Err("Timeline ratio must be positive".to_owned());
    }
    let scaled = u128::from(value)
        .checked_mul(u128::from(numerator))
        .ok_or("Timeline multiplication overflow")?;
    let rounded = scaled
        .checked_add(u128::from(denominator / 2))
        .ok_or("Timeline rounding overflow")?
        .checked_div(u128::from(denominator))
        .ok_or("Timeline divisor is zero")?;
    u64::try_from(rounded).map_err(|_| "Timeline value exceeds u64".to_owned())
}

#[cfg(feature = "audio-decode")]
pub(crate) fn scale_u64_floor(value: u64, numerator: u64, denominator: u64) -> Result<u64, String> {
    if numerator == 0 || denominator == 0 {
        return Err("Timeline ratio must be positive".to_owned());
    }
    let scaled = u128::from(value)
        .checked_mul(u128::from(numerator))
        .ok_or("Timeline multiplication overflow")?
        .checked_div(u128::from(denominator))
        .ok_or("Timeline divisor is zero")?;
    u64::try_from(scaled).map_err(|_| "Timeline value exceeds u64".to_owned())
}

pub(crate) fn read_u32(data: &[u8], at: usize) -> Option<u32> {
    let bytes = data.get(at..at.checked_add(4)?)?;
    Some(u32::from_be_bytes([
        *bytes.first()?,
        *bytes.get(1)?,
        *bytes.get(2)?,
        *bytes.get(3)?,
    ]))
}

/// 从 `stsz` 读取 sample 数量与总字节数。
pub(crate) fn parse_stsz(payload: &[u8]) -> Option<(u32, u64)> {
    let uniform_size = read_u32(payload, 4)?;
    let count = read_u32(payload, 8)?;
    if uniform_size != 0 {
        let total = u64::from(uniform_size).checked_mul(u64::from(count))?;
        return Some((count, total));
    }
    let mut total = 0u64;
    for index in 0..count {
        let at = 12usize.checked_add(usize::try_from(index).ok()?.checked_mul(4)?)?;
        total = total.checked_add(u64::from(read_u32(payload, at)?))?;
    }
    Some((count, total))
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
    fn mp4_edit_shift_preserves_sub_tick_event_offsets() {
        let no_edit = presentation_sample_shift(48_000, 1_000, 1_000, &[]).unwrap();
        assert_eq!(no_edit, Some(0));
        assert_eq!(24i64.checked_add(no_edit.unwrap_or(0)), Some(24));

        // media_time=1 ms：整体左移 48 个采样；帧内 24-sample 偏移仍完整保留。
        let edit = [EditListEntry {
            segment_duration: 1_000,
            media_time: 1,
            media_rate: (1, 0),
        }];
        let shifted = presentation_sample_shift(48_000, 1_000, 1_000, &edit).unwrap();
        assert_eq!(shifted, Some(-48));
        assert_eq!(24i64.checked_add(shifted.unwrap_or(0)), Some(-24));

        let empty = [EditListEntry {
            segment_duration: 1_000,
            media_time: -1,
            media_rate: (1, 0),
        }];
        assert_eq!(
            presentation_sample_shift(48_000, 1_000, 1_000, &empty).unwrap(),
            None
        );
    }
    #[cfg(feature = "audio-decode")]
    #[test]
    fn mp4_edit_media_span_excludes_leading_and_trailing_empty_edits() {
        let edits = [
            EditListEntry {
                segment_duration: 1_000,
                media_time: -1,
                media_rate: (1, 0),
            },
            EditListEntry {
                segment_duration: 2_000,
                media_time: 2_048,
                media_rate: (1, 0),
            },
            EditListEntry {
                segment_duration: 500,
                media_time: -1,
                media_rate: (1, 0),
            },
        ];
        assert_eq!(
            presentation_media_span(48_000, 1_000, 1_000, &edits).unwrap(),
            Some(MediaSpan {
                start_sample: 48_000,
                end_sample: 144_000,
            })
        );
        assert_eq!(
            presentation_media_span(48_000, 1_000, 1_000, &edits[..1]).unwrap(),
            None
        );
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
