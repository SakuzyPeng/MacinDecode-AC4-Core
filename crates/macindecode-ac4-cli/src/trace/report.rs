//! MP4 与 Annex G trace 报告渲染。

use super::{
    AUDIO_SAMPLE_ENTRY_LEN, Ac4Dsi, Ac4Toc, BoxIter, EditListEntry, SampleDelta, SampleTable,
    SequenceTransition, SyncFrameIter, TopologyTrace, find_ac4_track, find_box, find_path,
    media_time_to_presentation, parse_edit_list, parse_header_timing, parse_stsz,
    presentation_timing,
};

/// 顶层 box 列表，用于确认容器整体结构。
fn top_level_summary(data: &[u8]) -> String {
    let mut out = String::from("[");
    let mut first = true;
    for item in BoxIter::new(data).flatten() {
        if !first {
            out.push_str(", ");
        }
        first = false;
        let name: String = item.type_str().iter().collect();
        out.push_str(&format!(
            "{{\"type\": \"{name}\", \"offset\": {}, \"size\": {}}}",
            item.offset, item.total_len
        ));
    }
    out.push(']');
    out
}

/// 逐帧解析 TOC，并与 dac4 的声明交叉核对。
///
/// 规范要求 dac4 中的 bitstream_version、fs_index 与 frame_rate_index
/// 同 ac4_toc 一致（表 E.5），因此不一致即为容器与码流失配。
struct FrameTrace {
    frames: u32,
    presented_frames: u32,
    sync_frames: u32,
    parse_failures: u32,
    mismatches: u32,
    iframe_frames: u32,
    sync_flag_mismatches: u32,
    first_sequence: Option<u16>,
    last_sequence: Option<u16>,
    sequence_gaps: u32,
    first_frames: String,
    topology: TopologyTrace,
}

fn trace_frames(
    data: &[u8],
    stbl: &[u8],
    dsi: &Ac4Dsi<'_>,
    media_timescale: u32,
    movie_timescale: u32,
    edits: &[EditListEntry],
    presented_duration: u64,
) -> Result<FrameTrace, String> {
    let table = SampleTable::parse(stbl).map_err(|e| e.to_string())?;
    let presented_end = i64::try_from(presented_duration)
        .map_err(|_| "Presentation duration exceeds the signed timeline range")?;
    let mut trace = FrameTrace {
        frames: 0,
        presented_frames: 0,
        sync_frames: 0,
        parse_failures: 0,
        mismatches: 0,
        iframe_frames: 0,
        sync_flag_mismatches: 0,
        first_sequence: None,
        last_sequence: None,
        sequence_gaps: 0,
        first_frames: String::from("["),
        topology: trace_topology(),
    };
    let mut previous_sequence: Option<u16> = None;

    for item in table.iter() {
        let info = item.map_err(|e| e.to_string())?;
        trace.frames = trace.frames.saturating_add(1);
        let presentation_start = media_time_to_presentation(
            info.composition_time,
            media_timescale,
            movie_timescale,
            edits,
        )
        .map_err(|error| error.to_string())?;
        let media_end = info
            .composition_time
            .checked_add(i64::from(info.duration))
            .ok_or("Sample PTS end-position overflow")?;
        let presentation_end =
            media_time_to_presentation(media_end, media_timescale, movie_timescale, edits)
                .map_err(|error| error.to_string())?;
        if matches!(
            (presentation_start, presentation_end),
            (Some(start), Some(end))
                if start < presented_end && end > 0
        ) {
            trace.presented_frames = trace.presented_frames.saturating_add(1);
        }
        if info.is_sync {
            trace.sync_frames = trace.sync_frames.saturating_add(1);
        }

        let start = usize::try_from(info.offset).unwrap_or(usize::MAX);
        let end = start.saturating_add(usize::try_from(info.size).unwrap_or(0));
        let Some(frame) = data.get(start..end) else {
            trace.parse_failures = trace.parse_failures.saturating_add(1);
            trace
                .topology
                .record_parse_failure(info.index, "Sample range exceeds the file size");
            continue;
        };
        trace
            .topology
            .observe(frame, info.index, Some(info.is_sync));
        let Ok(toc) = Ac4Toc::parse(frame) else {
            trace.parse_failures = trace.parse_failures.saturating_add(1);
            continue;
        };

        // E.2：同步样本即 b_iframe_global 为真的样本，stss 应与之一致
        if toc.iframe_global {
            trace.iframe_frames = trace.iframe_frames.saturating_add(1);
        }
        if toc.iframe_global != info.is_sync {
            trace.sync_flag_mismatches = trace.sync_flag_mismatches.saturating_add(1);
        }

        if toc.bitstream_version != u32::from(dsi.bitstream_version)
            || toc.fs_index != dsi.fs_index
            || toc.frame_rate_index != dsi.frame_rate_index
        {
            trace.mismatches = trace.mismatches.saturating_add(1);
        }

        if toc.sequence_transition(previous_sequence) == SequenceTransition::SourceChange {
            trace.sequence_gaps = trace.sequence_gaps.saturating_add(1);
        }
        previous_sequence = Some(toc.sequence_counter);
        if trace.first_sequence.is_none() {
            trace.first_sequence = Some(toc.sequence_counter);
        }
        trace.last_sequence = Some(toc.sequence_counter);

        if info.index < 3 {
            let presentation_time =
                presentation_start.map_or_else(|| "null".to_owned(), |value| value.to_string());
            if info.index > 0 {
                trace.first_frames.push_str(", ");
            }
            trace.first_frames.push_str(&format!(
                concat!(
                    "{{\"index\": {}, \"offset\": {}, \"size\": {}, ",
                    "\"decode_time\": {}, \"media_pts\": {}, ",
                    "\"presentation_time\": {}, \"duration\": {}, \"is_sync\": {}, ",
                    "\"sequence_counter\": {}, \"iframe_global\": {}, ",
                    "\"n_presentations\": {}, \"payload_base\": {}, ",
                    "\"toc_bits\": {}}}"
                ),
                info.index,
                info.offset,
                info.size,
                info.decode_time,
                info.composition_time,
                presentation_time,
                info.duration,
                info.is_sync,
                toc.sequence_counter,
                toc.iframe_global,
                toc.n_presentations,
                toc.payload_base,
                toc.bits_consumed
            ));
        }
    }
    trace.first_frames.push(']');
    Ok(trace)
}

/// 检视 raw AC-4 裸流，即 Annex G 定义的 sync frame 序列。
pub(super) fn trace_raw(data: &[u8]) -> Result<String, String> {
    let mut frames = 0u32;
    let mut iframes = 0u32;
    let mut escaped_sizes = 0u32;
    let mut crc_frames = 0u32;
    let mut crc_failures = 0u32;
    let mut toc_failures = 0u32;
    let mut payload_bytes = 0u64;
    let mut first = String::from("[");
    let mut first_sequence: Option<u16> = None;
    let mut last_sequence: Option<u16> = None;
    let mut sequence_gaps = 0u32;
    let mut previous_sequence: Option<u16> = None;
    // 裸流没有 sample table，拓扑与 OAMD 巡检因此不做 stss 比对，其余检查
    // 与容器路径完全相同。
    let mut topology = trace_topology();

    for item in SyncFrameIter::new(data) {
        let frame = item.map_err(|error| error.to_string())?;
        let index = frames;
        frames = frames.saturating_add(1);
        payload_bytes = payload_bytes.saturating_add(u64::from(frame.frame_size));

        // 头部长度可反推 frame_size 是否走了 0xFFFF 转义
        let header = frame
            .total_size
            .saturating_sub(frame.frame_size as usize)
            .saturating_sub(if frame.crc_word.is_some() { 2 } else { 0 });
        if header > 4 {
            escaped_sizes = escaped_sizes.saturating_add(1);
        }

        if frame.crc_word.is_some() {
            crc_frames = crc_frames.saturating_add(1);
            if frame.verify_crc(data) != Some(true) {
                crc_failures = crc_failures.saturating_add(1);
            }
        }

        let Ok(toc) = Ac4Toc::parse(frame.raw_frame) else {
            toc_failures = toc_failures.saturating_add(1);
            continue;
        };
        if toc.iframe_global {
            iframes = iframes.saturating_add(1);
        }
        if toc.sequence_transition(previous_sequence) == SequenceTransition::SourceChange {
            sequence_gaps = sequence_gaps.saturating_add(1);
        }
        topology.observe(frame.raw_frame, index, None);

        previous_sequence = Some(toc.sequence_counter);
        if first_sequence.is_none() {
            first_sequence = Some(toc.sequence_counter);
        }
        last_sequence = Some(toc.sequence_counter);

        if index < 3 {
            if index > 0 {
                first.push_str(", ");
            }
            first.push_str(&format!(
                concat!(
                    "{{\"index\": {}, \"offset\": {}, \"sync_word\": \"0x{:04X}\", ",
                    "\"frame_size\": {}, \"header_bytes\": {}, \"sequence_counter\": {}, ",
                    "\"iframe_global\": {}, \"fs_index\": {}, \"frame_rate_index\": {}}}"
                ),
                index,
                frame.offset,
                frame.sync_word.as_u16(),
                frame.frame_size,
                header,
                toc.sequence_counter,
                toc.iframe_global,
                toc.fs_index,
                toc.frame_rate_index
            ));
        }
    }
    first.push(']');

    Ok(format!(
        concat!(
            "{{\n",
            "  \"format\": \"raw_ac4_syncframe\",\n",
            "  \"frames\": {{\n",
            "    \"count\": {},\n",
            "    \"payload_bytes\": {},\n",
            "    \"escaped_frame_sizes\": {},\n",
            "    \"crc_protected\": {},\n",
            "    \"crc_failures\": {},\n",
            "    \"toc_parse_failures\": {},\n",
            "    \"iframe_global_frames\": {},\n",
            "    \"sequence_first\": {},\n",
            "    \"sequence_last\": {},\n",
            "    \"sequence_discontinuities\": {},\n",
            "    \"first\": {}\n",
            "  }},\n",
            "  \"topology\": {}\n",
            "}}"
        ),
        frames,
        payload_bytes,
        escaped_sizes,
        crc_frames,
        crc_failures,
        toc_failures,
        iframes,
        first_sequence.map_or_else(|| "null".to_owned(), |v| v.to_string()),
        last_sequence.map_or_else(|| "null".to_owned(), |v| v.to_string()),
        sequence_gaps,
        first,
        topology.to_json(),
    ))
}

/// feature 构建的 trace 同样驱动 QMF/full 数值链，但不留存 PCM。
///
/// 这样 `audio_check.sh` 能以真实帧计数证明对象矩阵和 wet 分支确实执行；默认
/// 构建仍保持纯结构巡检，不分配规范表驱动的工作区。
fn trace_topology() -> TopologyTrace {
    #[cfg(feature = "audio-decode")]
    {
        TopologyTrace::new_tracing()
    }
    #[cfg(not(feature = "audio-decode"))]
    {
        TopologyTrace::new()
    }
}

pub(super) fn trace(data: &[u8]) -> Result<String, String> {
    let moov = find_box(data, b"moov").ok_or("moov box not found")?;
    let mvhd = find_box(moov.payload, b"mvhd").ok_or("mvhd box not found")?;
    let movie = parse_header_timing(*b"mvhd", mvhd.payload).map_err(|e| e.to_string())?;

    let track =
        find_ac4_track(moov.payload).ok_or("No track with an ac-4 sample entry was found")?;
    let track_index = track.index;
    let trak = track.trak;
    let mdia = track.mdia;
    let stbl = track.stbl;
    let entry = track.sample_entry;
    let mdhd = find_box(mdia.payload, b"mdhd").ok_or("mdhd box not found")?;
    let media = parse_header_timing(*b"mdhd", mdhd.payload).map_err(|e| e.to_string())?;
    let timescale = media.timescale;
    let duration = media.duration;

    // edit list 声明容器侧可见的区段；缺失时呈现时长等于媒体时长
    let mut edit_storage = [EditListEntry {
        segment_duration: 0,
        media_time: 0,
        media_rate: (0, 0),
    }; 8];
    let edit_count = find_path(trak.payload, &[*b"edts", *b"elst"])
        .map(|elst| parse_edit_list(elst.payload, &mut edit_storage))
        .transpose()
        .map_err(|e: macindecode_ac4_mp4::TimelineError| e.to_string())?
        .unwrap_or(0);
    let edits = edit_storage.get(..edit_count).unwrap_or(&[]);
    let presentation =
        presentation_timing(media, movie.timescale, edits).map_err(|error| error.to_string())?;

    let specific = entry
        .payload
        .get(AUDIO_SAMPLE_ENTRY_LEN..)
        .and_then(|tail| find_box(tail, b"dac4"))
        .ok_or("ac-4 sample entry has no dac4 box")?;

    let dsi = Ac4Dsi::parse(specific.payload).map_err(|error| error.to_string())?;

    let trace = trace_frames(
        data,
        stbl.payload,
        &dsi,
        media.timescale,
        movie.timescale,
        edits,
        presentation.presented_duration,
    )?;

    let (sample_count, sample_bytes) = find_box(stbl.payload, b"stsz")
        .and_then(|stsz| parse_stsz(stsz.payload))
        .unwrap_or((0, 0));

    let frame_rate = dsi.frame_rate();
    let timeline = dsi.media_timeline();

    let frame_rate_json = frame_rate.map_or_else(
        || "null".to_owned(),
        |rate| {
            format!(
                "{{\"numerator\": {}, \"denominator\": {}, \"frame_length_base\": {}}}",
                rate.numerator, rate.denominator, rate.frame_length_base
            )
        },
    );
    let timeline_json = timeline.map_or_else(
        || "null".to_owned(),
        |line| {
            let delta = match line.sample_delta {
                SampleDelta::Constant(value) => format!("{{\"constant\": {value}}}"),
                SampleDelta::Alternating(first, second) => {
                    format!("{{\"alternating\": [{first}, {second}]}}")
                }
            };
            format!(
                "{{\"timescale\": {}, \"sample_delta\": {delta}}}",
                line.timescale
            )
        },
    );

    let seconds = |value: u64| -> f64 {
        if timescale == 0 {
            0.0
        } else {
            value as f64 / f64::from(timescale)
        }
    };
    let duration_seconds = seconds(duration);
    let presented_seconds = seconds(presentation.presented_duration);

    let mut edits_json = String::from("[");
    for (index, entry) in edits.iter().enumerate() {
        if index > 0 {
            edits_json.push_str(", ");
        }
        edits_json.push_str(&format!(
            "{{\"segment_duration\": {}, \"media_time\": {}, \"empty_edit\": {}}}",
            entry.segment_duration,
            entry.media_time,
            entry.is_empty_edit()
        ));
    }
    edits_json.push(']');

    Ok(format!(
        concat!(
            "{{\n",
            "  \"container\": {{\n",
            "    \"top_level_boxes\": {},\n",
            "    \"track_index\": {},\n",
            "    \"media_timescale\": {},\n",
            "    \"media_duration\": {},\n",
            "    \"duration_seconds\": {:.6},\n",
            "    \"sample_count\": {},\n",
            "    \"sample_bytes\": {}\n",
            "  }},\n",
            "  \"presentation\": {{\n",
            "    \"movie_timescale\": {},\n",
            "    \"edit_list\": {},\n",
            "    \"priming_samples\": {},\n",
            "    \"presented_duration\": {},\n",
            "    \"presented_seconds\": {:.6}\n",
            "  }},\n",
            "  \"frames\": {{\n",
            "    \"count\": {},\n",
            "    \"presented_count\": {},\n",
            "    \"sync_frames\": {},\n",
            "    \"toc_parse_failures\": {},\n",
            "    \"dac4_toc_mismatches\": {},\n",
            "    \"iframe_global_frames\": {},\n",
            "    \"stss_iframe_mismatches\": {},\n",
            "    \"sequence_first\": {},\n",
            "    \"sequence_last\": {},\n",
            "    \"sequence_discontinuities\": {},\n",
            "    \"first\": {}\n",
            "  }},\n",
            "  \"topology\": {},\n",
            "  \"dac4\": {{\n",
            "    \"payload_bytes\": {},\n",
            "    \"ac4_dsi_version\": {},\n",
            "    \"bitstream_version\": {},\n",
            "    \"fs_index\": {},\n",
            "    \"base_sampling_frequency\": {},\n",
            "    \"frame_rate_index\": {},\n",
            "    \"n_presentations\": {},\n",
            "    \"presentation_bytes\": {}\n",
            "  }},\n",
            "  \"derived\": {{\n",
            "    \"frame_rate\": {},\n",
            "    \"media_timeline\": {}\n",
            "  }}\n",
            "}}"
        ),
        top_level_summary(data),
        track_index,
        timescale,
        duration,
        duration_seconds,
        sample_count,
        sample_bytes,
        movie.timescale,
        edits_json,
        presentation.priming,
        presentation.presented_duration,
        presented_seconds,
        trace.frames,
        trace.presented_frames,
        trace.sync_frames,
        trace.parse_failures,
        trace.mismatches,
        trace.iframe_frames,
        trace.sync_flag_mismatches,
        trace
            .first_sequence
            .map_or_else(|| "null".to_owned(), |v| v.to_string()),
        trace
            .last_sequence
            .map_or_else(|| "null".to_owned(), |v| v.to_string()),
        trace.sequence_gaps,
        trace.first_frames,
        trace.topology.to_json(),
        specific.payload.len(),
        dsi.dsi_version,
        dsi.bitstream_version,
        dsi.fs_index,
        dsi.base_sampling_frequency.hz(),
        dsi.frame_rate_index,
        dsi.n_presentations,
        dsi.presentation_bytes.len(),
        frame_rate_json,
        timeline_json,
    ))
}
