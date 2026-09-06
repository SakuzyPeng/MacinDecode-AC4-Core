//! 流式检视与完整字节入口共享 Aggregator，只改变 packet 的取得方式。
use super::*;
use macindecode_ac4_mp4::reader::{
    MAX_METADATA_BYTES, Mp4MetadataBytes, read_access_unit, read_mp4_metadata, read_sync_frame,
};
use std::io::{Read, Seek, SeekFrom};

const MAX_STREAM_ISSUES: usize = 1024;

fn failure(input: &str, format: InspectSourceKind, cause: impl fmt::Display) -> InspectError {
    InspectError::Parse {
        input: input.to_owned(),
        format,
        cause: cause.to_string(),
    }
}

fn trim_issues(aggregate: &mut Aggregator, omitted: &mut u64) {
    let excess = aggregate.issues.len().saturating_sub(MAX_STREAM_ISSUES);
    *omitted = omitted.saturating_add(u64::try_from(excess).unwrap_or(u64::MAX));
    aggregate.issues.truncate(MAX_STREAM_ISSUES);
}

fn finish_issues(aggregate: &mut Aggregator, omitted: u64) {
    if omitted != 0 {
        aggregate.issues.push(InspectIssue::warning(
            "issues_truncated",
            format!("{omitted} additional issues omitted from the streaming report"),
            None,
        ));
    }
}

/// 从可定位输入流检视完整媒体，MP4 只保留 `moov`，音频逐帧读取。
///
/// 每份流式报告最多保留 1024 条逐帧问题，并用额外一条提示说明被省略数量。
///
/// # Errors
/// 返回空输入、I/O、资源上限、容器或 AC-4 语法错误。
pub fn inspect_reader<R: Read + Seek>(
    reader: &mut R,
    source: InspectSourceHint<'_>,
) -> Result<InspectReport, InspectError> {
    let input = source.name.unwrap_or("<reader>");
    let fail = |error: io::Error| failure(input, InspectSourceKind::Mp4, error);
    reader.seek(SeekFrom::Start(0)).map_err(fail)?;
    let mut prefix = [0u8; 2];
    let mut count = 0usize;
    while count < prefix.len() {
        let output = prefix
            .get_mut(count..)
            .ok_or_else(|| failure(input, InspectSourceKind::Mp4, "Invalid input prefix length"))?;
        match reader.read(output) {
            Ok(0) => break,
            Ok(read) => count = count.saturating_add(read),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(fail(error)),
        }
    }
    if count == 0 {
        return Err(InspectError::EmptyInput {
            input: input.to_owned(),
        });
    }
    reader.seek(SeekFrom::Start(0)).map_err(fail)?;
    let raw = match source.format {
        InspectInputFormat::Auto => count == 2 && matches!(prefix, [0xac, 0x40 | 0x41]),
        InspectInputFormat::AnnexG => true,
        InspectInputFormat::Mp4 => false,
    };
    if raw {
        inspect_raw_reader(reader, input)
    } else {
        let metadata = read_mp4_metadata(reader, MAX_METADATA_BYTES)
            .map_err(|error| failure(input, InspectSourceKind::Mp4, error))?;
        inspect_mp4_reader(reader, &metadata, input)
    }
}

/// 使用同一文件已经读取的 `moov` 进行流式检视，便于与解码器共享元数据。
///
/// # Errors
/// 文件边界、资源上限、I/O、DSI 或 AC-4 语法错误会终止检视。
pub fn inspect_mp4_reader<R: Read + Seek>(
    reader: &mut R,
    metadata: &Mp4MetadataBytes,
    input: &str,
) -> Result<InspectReport, InspectError> {
    let result = (|| -> Result<InspectReport, String> {
        let source = metadata.metadata().map_err(|error| error.to_string())?;
        let dsi_summary = collect_dsi_summary(source.dsi())?;
        let mut aggregate = Aggregator::default();
        let mut total_sample_bytes = 0u128;
        let mut duration_ticks = 0u128;
        let mut buffer = Vec::new();
        let mut omitted = 0;
        for item in source.sample_infos() {
            let info = item.map_err(|error| error.to_string())?;
            read_access_unit(reader, info, metadata.file_len, &mut buffer)
                .map_err(|error| error.to_string())?;
            total_sample_bytes = total_sample_bytes.saturating_add(u128::from(info.size));
            duration_ticks = duration_ticks.saturating_add(u128::from(info.duration));
            aggregate.observe(&buffer, u64::from(info.index))?;
            trim_issues(&mut aggregate, &mut omitted);
        }
        if aggregate.frame_count == 0 {
            return Err("AC-4 sample table contains no samples".to_owned());
        }
        finish_issues(&mut aggregate, omitted);
        let duration = DurationRatio {
            numerator: duration_ticks,
            denominator: u128::from(source.media_timing().timescale),
        };
        aggregate.finish(
            input,
            InspectSourceKind::Mp4,
            ReportedField::present(source.track().index),
            Some(duration),
            total_sample_bytes,
            Some(dsi_summary),
            ReportedField::not_applicable(),
            ReportedField::not_applicable(),
        )
    })();
    result.map_err(|cause| failure(input, InspectSourceKind::Mp4, cause))
}

/// 从当前位置逐帧检视 Annex G 输入，包含 CRC 检查。
///
/// # Errors
/// 空输入、截断、I/O 或 AC-4 语法错误会终止检视。
pub fn inspect_raw_reader<R: Read>(
    reader: &mut R,
    input: &str,
) -> Result<InspectReport, InspectError> {
    let result = (|| -> Result<InspectReport, String> {
        let mut aggregate = Aggregator::default();
        let mut total_transport_bytes = 0u128;
        let mut sync_words = BTreeSet::new();
        let mut crc_protected = 0u64;
        let mut crc_failures = 0u64;
        let mut omitted = 0;
        let mut buffer = Vec::new();
        while read_sync_frame(reader, &mut buffer).map_err(|error| error.to_string())? {
            let frame = SyncFrameIter::new(&buffer)
                .next()
                .ok_or("Missing sync frame")?
                .map_err(|error| error.to_string())?;
            let index = aggregate.frame_count;
            total_transport_bytes = total_transport_bytes.saturating_add(frame.total_size as u128);
            sync_words.insert(frame.sync_word.as_u16());
            if frame.crc_word.is_some() {
                crc_protected = crc_protected.saturating_add(1);
                if frame.verify_crc(&buffer) != Some(true) {
                    crc_failures = crc_failures.saturating_add(1);
                    aggregate.issues.push(InspectIssue::warning(
                        "crc_mismatch",
                        "Annex G CRC verification failed",
                        Some(index),
                    ));
                }
            }
            aggregate.observe(frame.raw_frame, index)?;
            trim_issues(&mut aggregate, &mut omitted);
        }
        if aggregate.frame_count == 0 {
            return Err("Input contains no AC-4 sync frames".to_owned());
        }
        finish_issues(&mut aggregate, omitted);
        let duration = aggregate.observed_duration;
        let sync_word = match sync_words.len() {
            0 => ReportedField::not_present(),
            1 => {
                let raw = sync_words.iter().next().copied().unwrap_or_default();
                ReportedField::present_raw(format!("0x{raw:04X}"), None, raw)
            }
            _ => {
                aggregate.issues.push(InspectIssue::warning(
                    "sync_word_changed",
                    "Annex G sync word changes between frames",
                    None,
                ));
                ReportedField::unknown("multiple sync words observed")
            }
        };
        let crc_errors = if crc_protected == 0 {
            ReportedField::not_present()
        } else {
            ReportedField::present_raw(
                crc_failures != 0,
                None,
                json!({"protected_frames": crc_protected, "failures": crc_failures}),
            )
        };
        aggregate.finish(
            input,
            InspectSourceKind::AnnexG,
            ReportedField::not_applicable(),
            duration,
            total_transport_bytes,
            None,
            sync_word,
            crc_errors,
        )
    })();
    result.map_err(|cause| failure(input, InspectSourceKind::AnnexG, cause))
}
