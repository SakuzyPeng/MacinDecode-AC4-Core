//! 容器时间线：movie header、media header 与 edit list。
//!
//! 依据 ISO/IEC 14496-12。
//!
//! 容器时间、编解码器时间与呈现时间必须分层表示：编码器为满足帧长会在
//! 首尾补入采样，容器则通过 edit list 声明其中哪一段对外可见。把 `mdhd`
//! 的时长直接当作节目时长会把这段补偿计入，因此二者在此分别保留。

use core::fmt;

/// 时间线解析失败的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimelineError {
    /// box 负载不足以容纳声明的字段。
    Truncated {
        /// 出问题的 box 类型。
        box_type: [u8; 4],
    },
    /// 版本字段取值超出已定义范围。
    UnsupportedVersion {
        /// 出问题的 box 类型。
        box_type: [u8; 4],
        /// 读到的版本。
        version: u8,
    },
    /// edit list 条目数超过调用方提供的固定容量。
    TooManyEditEntries {
        /// 文件声明的条目数。
        declared: u32,
        /// 调用方可保存的条目数。
        capacity: usize,
    },
    /// 当前只支持规范常用的 1.0 播放速率。
    UnsupportedMediaRate {
        /// 播放速率整数部分。
        integer: i16,
        /// 播放速率小数部分。
        fraction: i16,
    },
    /// movie 或 media 时间刻度为 0，无法进行换算。
    ZeroTimescale,
    /// `media_time` 为规范未定义的负值；仅 `-1` 表示空编辑。
    InvalidMediaTime {
        /// 文件中的原始值。
        value: i64,
    },
    /// 时间换算结果超出公开整数类型的范围。
    TimeOverflow,
}

impl fmt::Display for TimelineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = |bytes: &[u8; 4]| -> [char; 4] {
            let mut out = ['.'; 4];
            for (slot, &byte) in out.iter_mut().zip(bytes.iter()) {
                if byte.is_ascii_graphic() {
                    *slot = byte as char;
                }
            }
            out
        };
        match *self {
            TimelineError::Truncated { box_type } => {
                let chars = name(&box_type);
                write!(
                    f,
                    "{}{}{}{} 负载不完整",
                    chars[0], chars[1], chars[2], chars[3]
                )
            }
            TimelineError::UnsupportedVersion { box_type, version } => {
                let chars = name(&box_type);
                write!(
                    f,
                    "{}{}{}{} 版本 {version} 未定义",
                    chars[0], chars[1], chars[2], chars[3]
                )
            }
            TimelineError::TooManyEditEntries { declared, capacity } => {
                write!(f, "elst 声明 {declared} 条编辑，超过容量 {capacity}")
            }
            TimelineError::UnsupportedMediaRate { integer, fraction } => {
                write!(f, "暂不支持 edit list 播放速率 {integer}.{fraction}")
            }
            TimelineError::ZeroTimescale => write!(f, "movie/media 时间刻度不得为 0"),
            TimelineError::InvalidMediaTime { value } => {
                write!(f, "edit list media_time {value} 未定义")
            }
            TimelineError::TimeOverflow => write!(f, "容器时间换算溢出"),
        }
    }
}

impl core::error::Error for TimelineError {}

fn read_u32(data: &[u8], at: usize) -> Option<u32> {
    let bytes = data.get(at..at.checked_add(4)?)?;
    Some(u32::from_be_bytes([
        *bytes.first()?,
        *bytes.get(1)?,
        *bytes.get(2)?,
        *bytes.get(3)?,
    ]))
}

fn read_u64(data: &[u8], at: usize) -> Option<u64> {
    let high = u64::from(read_u32(data, at)?);
    let low = u64::from(read_u32(data, at.checked_add(4)?)?);
    Some((high << 32) | low)
}

fn read_i16(data: &[u8], at: usize) -> Option<i16> {
    let bytes = data.get(at..at.checked_add(2)?)?;
    Some(i16::from_be_bytes([*bytes.first()?, *bytes.get(1)?]))
}

/// `mvhd` 或 `mdhd` 中的时间刻度与时长。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeaderTiming {
    /// 每秒的时间单位数。
    pub timescale: u32,
    /// 以该时间刻度表示的时长。
    pub duration: u64,
}

/// 解析 `mvhd` 或 `mdhd` 的负载。
///
/// 两个 box 的版本与字段布局一致：版本 0 用 32 位时间字段，版本 1 用 64 位。
///
/// # Errors
///
/// 负载不足或版本未定义时返回错误。
pub fn parse_header_timing(
    box_type: [u8; 4],
    payload: &[u8],
) -> Result<HeaderTiming, TimelineError> {
    let version = *payload
        .first()
        .ok_or(TimelineError::Truncated { box_type })?;
    // 版本(1) + flags(3) + creation(4|8) + modification(4|8)
    let (timescale_at, duration_at) = match version {
        0 => (12usize, 16usize),
        1 => (20usize, 24usize),
        other => {
            return Err(TimelineError::UnsupportedVersion {
                box_type,
                version: other,
            });
        }
    };
    let timescale = read_u32(payload, timescale_at).ok_or(TimelineError::Truncated { box_type })?;
    let duration = if version == 1 {
        read_u64(payload, duration_at).ok_or(TimelineError::Truncated { box_type })?
    } else {
        u64::from(read_u32(payload, duration_at).ok_or(TimelineError::Truncated { box_type })?)
    };
    Ok(HeaderTiming {
        timescale,
        duration,
    })
}

/// `elst` 中的一条编辑。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EditListEntry {
    /// 该段在 movie 时间刻度下的时长。
    pub segment_duration: u64,
    /// 该段起点在 media 时间刻度下的位置；`-1` 表示空编辑。
    ///
    /// 正值即为需要跳过的前导采样数，也就是编码器 priming 的容器侧声明。
    pub media_time: i64,
    /// 播放速率的整数与小数部分。
    pub media_rate: (i16, i16),
}

impl EditListEntry {
    /// 是否为空编辑，即在媒体开始前插入的一段静默。
    #[must_use]
    pub const fn is_empty_edit(&self) -> bool {
        self.media_time == -1
    }
}

/// 解析 `elst` 负载，最多返回 `OUT` 条。
///
/// 返回实际条目数；条目数超过固定容量时显式报错，不允许静默丢弃编辑。
///
/// # Errors
///
/// 负载不足或版本未定义时返回错误。
pub fn parse_edit_list<const OUT: usize>(
    payload: &[u8],
    entries: &mut [EditListEntry; OUT],
) -> Result<usize, TimelineError> {
    const BOX_TYPE: [u8; 4] = *b"elst";
    let version = *payload
        .first()
        .ok_or(TimelineError::Truncated { box_type: BOX_TYPE })?;
    if version > 1 {
        return Err(TimelineError::UnsupportedVersion {
            box_type: BOX_TYPE,
            version,
        });
    }
    let count = read_u32(payload, 4).ok_or(TimelineError::Truncated { box_type: BOX_TYPE })?;
    if usize::try_from(count).unwrap_or(usize::MAX) > OUT {
        return Err(TimelineError::TooManyEditEntries {
            declared: count,
            capacity: OUT,
        });
    }
    let entry_len: usize = if version == 1 { 20 } else { 12 };

    let mut written = 0usize;
    for index in 0..count {
        let base = 8usize
            .checked_add(
                usize::try_from(index)
                    .ok()
                    .and_then(|i| i.checked_mul(entry_len))
                    .ok_or(TimelineError::Truncated { box_type: BOX_TYPE })?,
            )
            .ok_or(TimelineError::Truncated { box_type: BOX_TYPE })?;

        let (segment_duration, media_time, rate_at) = if version == 1 {
            let duration =
                read_u64(payload, base).ok_or(TimelineError::Truncated { box_type: BOX_TYPE })?;
            let media = read_u64(payload, base.saturating_add(8))
                .ok_or(TimelineError::Truncated { box_type: BOX_TYPE })?
                as i64;
            (duration, media, base.saturating_add(16))
        } else {
            let duration = u64::from(
                read_u32(payload, base).ok_or(TimelineError::Truncated { box_type: BOX_TYPE })?,
            );
            let media = i64::from(
                read_u32(payload, base.saturating_add(4))
                    .ok_or(TimelineError::Truncated { box_type: BOX_TYPE })? as i32,
            );
            (duration, media, base.saturating_add(8))
        };

        let integer =
            read_i16(payload, rate_at).ok_or(TimelineError::Truncated { box_type: BOX_TYPE })?;
        let fraction = read_i16(payload, rate_at.saturating_add(2))
            .ok_or(TimelineError::Truncated { box_type: BOX_TYPE })?;

        let slot = entries
            .get_mut(written)
            .ok_or(TimelineError::TooManyEditEntries {
                declared: count,
                capacity: OUT,
            })?;
        *slot = EditListEntry {
            segment_duration,
            media_time,
            media_rate: (integer, fraction),
        };
        written = written.saturating_add(1);
    }
    Ok(written)
}

/// 由容器时间与编辑信息导出的呈现时间线。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresentationTiming {
    /// `mdhd` 声明的媒体时长，含编码器补偿。
    pub media_duration: u64,
    /// 应用编辑后对外可见的时长，以 media 时间刻度表示。
    pub presented_duration: u64,
    /// 需要跳过的前导采样数，即容器侧声明的 priming。
    pub priming: u64,
}

/// 根据 movie 与 media 时间刻度，把编辑段折算为呈现时间线。
///
/// 无编辑时呈现时长等于媒体时长，priming 为 0：此时容器未声明任何补偿，
/// 但不代表编码器没有补入采样，只说明容器没有把它区分出来。
fn scaled_duration(
    segment_duration: u64,
    media_timescale: u32,
    movie_timescale: u32,
) -> Result<u64, TimelineError> {
    if media_timescale == 0 || movie_timescale == 0 {
        return Err(TimelineError::ZeroTimescale);
    }
    let scaled = u128::from(segment_duration)
        .checked_mul(u128::from(media_timescale))
        .ok_or(TimelineError::TimeOverflow)?
        .checked_div(u128::from(movie_timescale))
        .ok_or(TimelineError::ZeroTimescale)?;
    u64::try_from(scaled).map_err(|_| TimelineError::TimeOverflow)
}

fn validate_edit(entry: EditListEntry) -> Result<(), TimelineError> {
    if entry.media_rate != (1, 0) {
        return Err(TimelineError::UnsupportedMediaRate {
            integer: entry.media_rate.0,
            fraction: entry.media_rate.1,
        });
    }
    if entry.media_time < -1 {
        return Err(TimelineError::InvalidMediaTime {
            value: entry.media_time,
        });
    }
    Ok(())
}

pub fn presentation_timing(
    media: HeaderTiming,
    movie_timescale: u32,
    edits: &[EditListEntry],
) -> Result<PresentationTiming, TimelineError> {
    if media.timescale == 0 || movie_timescale == 0 {
        return Err(TimelineError::ZeroTimescale);
    }
    if edits.is_empty() {
        return Ok(PresentationTiming {
            media_duration: media.duration,
            presented_duration: media.duration,
            priming: 0,
        });
    }

    let mut presented = 0u64;
    let mut priming: Option<u64> = None;
    for &entry in edits {
        validate_edit(entry)?;
        let scaled = scaled_duration(entry.segment_duration, media.timescale, movie_timescale)?;
        presented = presented
            .checked_add(scaled)
            .ok_or(TimelineError::TimeOverflow)?;
        if !entry.is_empty_edit() && priming.is_none() {
            priming =
                Some(u64::try_from(entry.media_time).map_err(|_| TimelineError::TimeOverflow)?);
        }
    }

    Ok(PresentationTiming {
        media_duration: media.duration,
        presented_duration: presented,
        priming: priming.unwrap_or(0),
    })
}

/// 把 media PTS 映射到应用 edit list 后的呈现时间，结果使用 media 时间刻度。
///
/// 首个编辑之前的 priming sample 会得到负值；尾部 padding 会落在节目时长
/// 之后。多个不连续媒体编辑之间未被选中的时间返回 `None`。
///
/// # Errors
///
/// 时间刻度为 0、播放速率不是 1.0 或计算溢出时返回错误。
pub fn media_time_to_presentation(
    media_time: i64,
    media_timescale: u32,
    movie_timescale: u32,
    edits: &[EditListEntry],
) -> Result<Option<i64>, TimelineError> {
    if media_timescale == 0 || movie_timescale == 0 {
        return Err(TimelineError::ZeroTimescale);
    }
    if edits.is_empty() {
        return Ok(Some(media_time));
    }

    let target = i128::from(media_time);
    let mut presentation_cursor = 0i128;
    let mut first_media: Option<(i128, i128)> = None;
    let mut last_media: Option<(i128, i128, i128)> = None;

    for &entry in edits {
        validate_edit(entry)?;
        let span = i128::from(scaled_duration(
            entry.segment_duration,
            media_timescale,
            movie_timescale,
        )?);
        if entry.is_empty_edit() {
            presentation_cursor = presentation_cursor
                .checked_add(span)
                .ok_or(TimelineError::TimeOverflow)?;
            continue;
        }

        let media_start = i128::from(entry.media_time);
        let media_end = media_start
            .checked_add(span)
            .ok_or(TimelineError::TimeOverflow)?;
        first_media.get_or_insert((media_start, presentation_cursor));
        last_media = Some((media_start, media_end, presentation_cursor));
        if target >= media_start && target < media_end {
            let relative = target
                .checked_sub(media_start)
                .ok_or(TimelineError::TimeOverflow)?;
            let mapped = presentation_cursor
                .checked_add(relative)
                .ok_or(TimelineError::TimeOverflow)?;
            return i64::try_from(mapped)
                .map(Some)
                .map_err(|_| TimelineError::TimeOverflow);
        }
        presentation_cursor = presentation_cursor
            .checked_add(span)
            .ok_or(TimelineError::TimeOverflow)?;
    }

    let Some((first_start, first_cursor)) = first_media else {
        return Ok(None);
    };
    let (last_start, last_end, last_cursor) = last_media.ok_or(TimelineError::TimeOverflow)?;
    let mapped = if target < first_start {
        let relative = target
            .checked_sub(first_start)
            .ok_or(TimelineError::TimeOverflow)?;
        first_cursor
            .checked_add(relative)
            .ok_or(TimelineError::TimeOverflow)?
    } else if target >= last_end {
        let relative = target
            .checked_sub(last_start)
            .ok_or(TimelineError::TimeOverflow)?;
        last_cursor
            .checked_add(relative)
            .ok_or(TimelineError::TimeOverflow)?
    } else {
        return Ok(None);
    };
    i64::try_from(mapped)
        .map(Some)
        .map_err(|_| TimelineError::TimeOverflow)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mdhd_v0(timescale: u32, duration: u32) -> [u8; 24] {
        let mut out = [0u8; 24];
        out[12..16].copy_from_slice(&timescale.to_be_bytes());
        out[16..20].copy_from_slice(&duration.to_be_bytes());
        out
    }

    #[test]
    fn parses_version_0_header() {
        let payload = mdhd_v0(48_000, 292_864);
        let timing = parse_header_timing(*b"mdhd", &payload).unwrap();
        assert_eq!(timing.timescale, 48_000);
        assert_eq!(timing.duration, 292_864);
    }

    #[test]
    fn parses_version_1_header() {
        let mut payload = [0u8; 36];
        payload[0] = 1;
        payload[20..24].copy_from_slice(&48_000u32.to_be_bytes());
        payload[24..32].copy_from_slice(&1_000_000u64.to_be_bytes());
        let timing = parse_header_timing(*b"mdhd", &payload).unwrap();
        assert_eq!(timing.timescale, 48_000);
        assert_eq!(timing.duration, 1_000_000);
    }

    #[test]
    fn rejects_unknown_version() {
        let mut payload = [0u8; 24];
        payload[0] = 7;
        assert!(matches!(
            parse_header_timing(*b"mdhd", &payload).unwrap_err(),
            TimelineError::UnsupportedVersion { version: 7, .. }
        ));
    }

    #[test]
    fn rejects_truncated_header() {
        assert!(matches!(
            parse_header_timing(*b"mdhd", &[0u8; 8]).unwrap_err(),
            TimelineError::Truncated { .. }
        ));
    }

    #[test]
    fn parses_edit_list() {
        let mut payload = [0u8; 20];
        payload[4..8].copy_from_slice(&1u32.to_be_bytes()); // entry_count
        payload[8..12].copy_from_slice(&288_000u32.to_be_bytes()); // segment_duration
        payload[12..16].copy_from_slice(&4_864u32.to_be_bytes()); // media_time
        payload[16..18].copy_from_slice(&1i16.to_be_bytes());
        let mut entries = [EditListEntry {
            segment_duration: 0,
            media_time: 0,
            media_rate: (0, 0),
        }; 4];
        let count = parse_edit_list(&payload, &mut entries).unwrap();
        assert_eq!(count, 1);
        assert_eq!(entries[0].segment_duration, 288_000);
        assert_eq!(entries[0].media_time, 4_864);
        assert!(!entries[0].is_empty_edit());
    }

    /// media_time 为 -1 表示空编辑，不是负的 priming
    #[test]
    fn recognises_empty_edit() {
        let mut payload = [0u8; 20];
        payload[4..8].copy_from_slice(&1u32.to_be_bytes());
        payload[8..12].copy_from_slice(&1_000u32.to_be_bytes());
        payload[12..16].copy_from_slice(&(-1i32).to_be_bytes());
        let mut entries = [EditListEntry {
            segment_duration: 0,
            media_time: 0,
            media_rate: (0, 0),
        }; 2];
        parse_edit_list(&payload, &mut entries).unwrap();
        assert!(entries[0].is_empty_edit());
    }

    #[test]
    fn presentation_timing_applies_edit() {
        let media = HeaderTiming {
            timescale: 48_000,
            duration: 292_864,
        };
        let edits = [EditListEntry {
            segment_duration: 288_000,
            media_time: 4_864,
            media_rate: (1, 0),
        }];
        // movie 与 media 时间刻度相同的情形
        let timing = presentation_timing(media, 48_000, &edits).unwrap();
        assert_eq!(timing.media_duration, 292_864);
        assert_eq!(timing.presented_duration, 288_000);
        assert_eq!(timing.priming, 4_864);
    }

    #[test]
    fn presentation_timing_rescales_segment_duration() {
        let media = HeaderTiming {
            timescale: 48_000,
            duration: 292_864,
        };
        // movie 时间刻度 1000 下的 6 秒
        let edits = [EditListEntry {
            segment_duration: 6_000,
            media_time: 4_864,
            media_rate: (1, 0),
        }];
        let timing = presentation_timing(media, 1_000, &edits).unwrap();
        assert_eq!(timing.presented_duration, 288_000);
    }

    #[test]
    fn no_edit_list_means_full_media_duration() {
        let media = HeaderTiming {
            timescale: 48_000,
            duration: 292_864,
        };
        let timing = presentation_timing(media, 48_000, &[]).unwrap();
        assert_eq!(timing.presented_duration, 292_864);
        assert_eq!(timing.priming, 0);
    }

    #[test]
    fn rejects_zero_timescale_without_edit_list() {
        let media = HeaderTiming {
            timescale: 0,
            duration: 2_048,
        };
        assert_eq!(
            presentation_timing(media, 48_000, &[]).unwrap_err(),
            TimelineError::ZeroTimescale
        );
        assert_eq!(
            media_time_to_presentation(0, 48_000, 0, &[]).unwrap_err(),
            TimelineError::ZeroTimescale
        );
    }

    #[test]
    fn leading_empty_edit_counts_towards_presentation() {
        let media = HeaderTiming {
            timescale: 48_000,
            duration: 6_144,
        };
        let edits = [
            EditListEntry {
                segment_duration: 1_024,
                media_time: -1,
                media_rate: (1, 0),
            },
            EditListEntry {
                segment_duration: 2_048,
                media_time: 2_048,
                media_rate: (1, 0),
            },
        ];
        let timing = presentation_timing(media, 48_000, &edits).unwrap();
        assert_eq!(timing.presented_duration, 3_072);
        assert_eq!(timing.priming, 2_048);
        assert_eq!(
            media_time_to_presentation(2_048, 48_000, 48_000, &edits).unwrap(),
            Some(1_024)
        );
    }

    #[test]
    fn maps_priming_to_negative_presentation_time() {
        let edits = [EditListEntry {
            segment_duration: 2_048,
            media_time: 2_048,
            media_rate: (1, 0),
        }];
        assert_eq!(
            media_time_to_presentation(0, 48_000, 48_000, &edits).unwrap(),
            Some(-2_048)
        );
        assert_eq!(
            media_time_to_presentation(2_048, 48_000, 48_000, &edits).unwrap(),
            Some(0)
        );
        assert_eq!(
            media_time_to_presentation(4_096, 48_000, 48_000, &edits).unwrap(),
            Some(2_048)
        );
    }

    #[test]
    fn rejects_edit_list_larger_than_storage() {
        let mut payload = [0u8; 32];
        payload[4..8].copy_from_slice(&2u32.to_be_bytes());
        let mut entries = [EditListEntry {
            segment_duration: 0,
            media_time: 0,
            media_rate: (0, 0),
        }; 1];
        assert_eq!(
            parse_edit_list(&payload, &mut entries).unwrap_err(),
            TimelineError::TooManyEditEntries {
                declared: 2,
                capacity: 1
            }
        );
    }
}
