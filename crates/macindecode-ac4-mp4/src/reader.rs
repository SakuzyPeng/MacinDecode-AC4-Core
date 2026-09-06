//! 有界的可定位文件读取（`std` feature）。
//!
//! MP4 只载入 `moov`，跳过媒体与无关 box；sample 和 Annex G frame 使用调用方
//! 复用的缓冲。所有文件偏移均为 `u64`，不会按文件长度分配内存。

use core::fmt;
use std::io::{self, Read, Seek, SeekFrom};
use std::vec::Vec;

use crate::{Ac4Mp4Error, Ac4Mp4Metadata, SampleInfo};
use macindecode_ac4_bitstream::{SyncFrameError, SyncFrameIter};

/// 默认 `moov` 上限；畸形或异常大的元数据不会触发整文件分配。
pub const MAX_METADATA_BYTES: usize = 64 * 1024 * 1024;
/// Annex G 的 24-bit payload 上限，外加 sync header / CRC 的空间。
pub const MAX_ACCESS_UNIT_BYTES: usize = 16 * 1024 * 1024 + 9;

/// 文件读取、box 边界或资源预算错误。
#[derive(Debug)]
#[non_exhaustive]
pub enum MediaReadError {
    /// 底层 I/O 失败（包括截断）。
    Io(io::Error),
    /// 没有 `moov` box。
    MissingMoov,
    /// box 尺寸小于头部或超出文件。
    InvalidBox { offset: u64, size: u64 },
    /// 元数据超过调用方允许的上限。
    MetadataTooLarge { bytes: u64, limit: usize },
    /// 单个音频 packet 超过读取上限。
    AccessUnitTooLarge { bytes: u64, limit: usize },
    /// sample 字节范围溢出或超出源文件。
    InvalidSampleRange {
        offset: u64,
        size: u64,
        file_len: u64,
    },
    /// Annex G 头部或 packet 无效。
    SyncFrame(SyncFrameError),
}

impl fmt::Display for MediaReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => error.fmt(f),
            Self::MissingMoov => f.write_str("moov box not found"),
            Self::InvalidBox { offset, size } => {
                write!(f, "Invalid MP4 box at {offset}, size {size}")
            }
            Self::MetadataTooLarge { bytes, limit } => write!(
                f,
                "MP4 metadata requires {bytes} bytes, exceeding the {limit}-byte limit"
            ),
            Self::AccessUnitTooLarge { bytes, limit } => write!(
                f,
                "AC-4 packet requires {bytes} bytes, exceeding the {limit}-byte limit"
            ),
            Self::InvalidSampleRange {
                offset,
                size,
                file_len,
            } => write!(
                f,
                "MP4 sample at {offset} with size {size} exceeds the {file_len}-byte source"
            ),
            Self::SyncFrame(error) => error.fmt(f),
        }
    }
}

impl core::error::Error for MediaReadError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::SyncFrame(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for MediaReadError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

/// 一份独立的电影元数据；不含 `mdat`。
#[derive(Debug)]
pub struct Mp4MetadataBytes {
    /// 完整 `moov` box，包含头部。
    pub bytes: Vec<u8>,
    /// 原文件的字节长度，用于 sample 边界检查。
    pub file_len: u64,
}

impl Mp4MetadataBytes {
    /// 借用元数据，复用完整文件入口的轨道和时间线校验。
    ///
    /// # Errors
    /// 返回轨道、DSI、sample table 的解析错误。
    pub fn metadata(&self) -> Result<Ac4Mp4Metadata<'_>, Ac4Mp4Error> {
        Ac4Mp4Metadata::parse(&self.bytes)
    }
}

/// 扫描顶层 box 并只读取 `moov`，支持 64-bit extended size 和末尾 `moov`。
///
/// # Errors
/// 截断、越界、缺失 `moov` 或元数据超过 `limit` 时返回 [`MediaReadError`]。
pub fn read_mp4_metadata<R: Read + Seek>(
    reader: &mut R,
    limit: usize,
) -> Result<Mp4MetadataBytes, MediaReadError> {
    let file_len = reader.seek(SeekFrom::End(0))?;
    let mut offset = 0u64;
    while offset < file_len {
        reader.seek(SeekFrom::Start(offset))?;
        let mut header = [0u8; 8];
        reader.read_exact(&mut header)?;
        let [a, b, c, d, t0, t1, t2, t3] = header;
        let size32 = u32::from_be_bytes([a, b, c, d]);
        let mut extended = [0u8; 8];
        let (size, mut header_len) = match size32 {
            0 => (file_len.saturating_sub(offset), 8usize),
            1 => {
                reader.read_exact(&mut extended)?;
                (u64::from_be_bytes(extended), 16usize)
            }
            size => (u64::from(size), 8usize),
        };
        if [t0, t1, t2, t3] == *b"uuid" {
            header_len = header_len.saturating_add(16);
        }
        let end = offset
            .checked_add(size)
            .filter(|&end| end <= file_len)
            .ok_or(MediaReadError::InvalidBox { offset, size })?;
        if size < header_len as u64 {
            return Err(MediaReadError::InvalidBox { offset, size });
        }
        if [t0, t1, t2, t3] == *b"moov" {
            let length = usize::try_from(size)
                .ok()
                .filter(|&n| n <= limit)
                .ok_or(MediaReadError::MetadataTooLarge { bytes: size, limit })?;
            let mut bytes = Vec::with_capacity(length);
            bytes.extend_from_slice(&header);
            if header_len == 16 {
                bytes.extend_from_slice(&extended);
            }
            bytes.resize(length, 0);
            let payload = bytes
                .get_mut(header_len..)
                .ok_or(MediaReadError::InvalidBox { offset, size })?;
            reader.read_exact(payload)?;
            return Ok(Mp4MetadataBytes { bytes, file_len });
        }
        offset = end;
    }
    Err(MediaReadError::MissingMoov)
}

/// 校验文件范围，不把 64-bit 文件长度缩窄为内存切片长度。
///
/// # Errors
/// sample 的 offset + size 溢出或超出 `file_len` 时返回边界错误。
pub fn validate_sample_range(info: SampleInfo, file_len: u64) -> Result<(), MediaReadError> {
    info.offset
        .checked_add(u64::from(info.size))
        .filter(|&end| end <= file_len)
        .map(|_| ())
        .ok_or(MediaReadError::InvalidSampleRange {
            offset: info.offset,
            size: u64::from(info.size),
            file_len,
        })
}

/// 定位一个 AU 并将其读入复用缓冲；不会读入整段 `mdat`。
///
/// `info` 应来自 [`Ac4Mp4Metadata::sample_infos`]，保证 sample description 已校验。
/// 对 `BufReader` 使用相对定位，允许相邻 AU 和缓冲内跳转复用已读取字节。
///
/// # Errors
/// 文件边界、packet 预算或底层 I/O 无效时返回错误。
pub fn read_access_unit<R: Read + Seek>(
    reader: &mut R,
    info: SampleInfo,
    file_len: u64,
    buffer: &mut Vec<u8>,
) -> Result<(), MediaReadError> {
    validate_sample_range(info, file_len)?;
    let length = usize::try_from(info.size)
        .ok()
        .filter(|&n| n <= MAX_ACCESS_UNIT_BYTES)
        .ok_or(MediaReadError::AccessUnitTooLarge {
            bytes: u64::from(info.size),
            limit: MAX_ACCESS_UNIT_BYTES,
        })?;
    let current = reader.stream_position()?;
    if let Ok(distance) = i64::try_from(current.abs_diff(info.offset)) {
        let distance = if info.offset >= current {
            distance
        } else {
            distance.saturating_neg()
        };
        if distance != 0 {
            reader.seek_relative(distance)?;
        }
    } else {
        reader.seek(SeekFrom::Start(info.offset))?;
    }
    buffer.resize(length, 0);
    reader.read_exact(buffer)?;
    Ok(())
}

/// 读取一整个 Annex G sync frame（含头部与 CRC），随后可用 `SyncFrameIter`
/// 在该有界缓冲中取得借用 view。干净 EOF 返回 `false`，截断返回错误。
///
/// # Errors
/// sync word、frame size、截断或底层 I/O 无效时返回错误。
pub fn read_sync_frame<R: Read>(
    reader: &mut R,
    buffer: &mut Vec<u8>,
) -> Result<bool, MediaReadError> {
    let mut first = [0u8; 1];
    loop {
        match reader.read(&mut first) {
            Ok(0) => {
                buffer.clear();
                return Ok(false);
            }
            Ok(_) => break,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error.into()),
        }
    }
    let mut rest = [0u8; 3];
    reader.read_exact(&mut rest)?;
    let [high] = first;
    let [low, size_high, size_low] = rest;
    let word = u16::from_be_bytes([high, low]);
    if word != 0xac40 && word != 0xac41 {
        return Err(MediaReadError::SyncFrame(SyncFrameError::InvalidSyncWord {
            offset: 0,
            value: word,
        }));
    }
    buffer.clear();
    buffer.extend_from_slice(&[high, low, size_high, size_low]);
    let size16 = u16::from_be_bytes([size_high, size_low]);
    let payload = if size16 == 0xffff {
        let mut extra = [0u8; 3];
        reader.read_exact(&mut extra)?;
        buffer.extend_from_slice(&extra);
        let [a, b, c] = extra;
        u32::from_be_bytes([0, a, b, c])
    } else {
        u32::from(size16)
    };
    let header_len = buffer.len();
    let length = usize::try_from(payload)
        .ok()
        .and_then(|n| n.checked_add(header_len))
        .and_then(|n| n.checked_add(if word == 0xac41 { 2 } else { 0 }))
        .filter(|&n| n <= MAX_ACCESS_UNIT_BYTES)
        .ok_or(MediaReadError::AccessUnitTooLarge {
            bytes: u64::from(payload),
            limit: MAX_ACCESS_UNIT_BYTES,
        })?;
    buffer.resize(length, 0);
    reader.read_exact(buffer.get_mut(header_len..).ok_or(
        MediaReadError::AccessUnitTooLarge {
            bytes: u64::from(payload),
            limit: MAX_ACCESS_UNIT_BYTES,
        },
    )?)?;
    SyncFrameIter::new(buffer)
        .next()
        .ok_or(MediaReadError::SyncFrame(SyncFrameError::EmptyFrame {
            offset: 0,
        }))?
        .map_err(MediaReadError::SyncFrame)?;
    Ok(true)
}

#[cfg(test)]
#[allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "fixtures use checked fixed layouts and small bounded buffers"
)]
mod tests {
    use super::*;
    use std::io::{BufReader, Cursor};
    use std::vec;

    #[derive(Debug)]
    struct SparseReader {
        length: u64,
        position: u64,
        regions: Vec<(u64, Vec<u8>)>,
        bytes_read: usize,
    }
    impl Read for SparseReader {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            if out.is_empty() || self.position == self.length {
                return Ok(0);
            }
            let (start, data) = self
                .regions
                .iter()
                .find(|(start, data)| {
                    self.position >= *start && self.position < start + data.len() as u64
                })
                .ok_or_else(|| io::Error::other("reader touched the unallocated media payload"))?;
            let offset = usize::try_from(self.position - start).unwrap();
            let count = out.len().min(data.len() - offset);
            out[..count].copy_from_slice(&data[offset..offset + count]);
            self.position += count as u64;
            self.bytes_read += count;
            Ok(count)
        }
    }
    impl Seek for SparseReader {
        fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
            self.position = match position {
                SeekFrom::Start(n) => Some(n),
                SeekFrom::Current(n) => self.position.checked_add_signed(n),
                SeekFrom::End(n) => self.length.checked_add_signed(n),
            }
            .ok_or_else(|| io::Error::other("invalid seek"))?;
            Ok(self.position)
        }
    }

    fn sample(offset: u64, size: u32) -> SampleInfo {
        SampleInfo {
            index: 0,
            offset,
            size,
            sample_description_index: 1,
            decode_time: 0,
            composition_time: 0,
            duration: 2048,
            is_sync: true,
        }
    }

    #[test]
    fn finds_tail_metadata_after_an_eight_gib_mdat_without_reading_the_media() {
        let size = 8 * 1024 * 1024 * 1024u64;
        let mut header = vec![0, 0, 0, 1, b'm', b'd', b'a', b't'];
        header.extend_from_slice(&size.to_be_bytes());
        let moov = vec![0, 0, 0, 8, b'm', b'o', b'o', b'v'];
        let mut reader = SparseReader {
            length: size + 8,
            position: 0,
            regions: vec![(0, header), (size, moov.clone())],
            bytes_read: 0,
        };
        let metadata = read_mp4_metadata(&mut reader, MAX_METADATA_BYTES).unwrap();
        assert_eq!(metadata.bytes, moov);
        assert_eq!(metadata.file_len, size + 8);
        assert_eq!(reader.bytes_read, 24);
    }

    #[test]
    fn reads_samples_beyond_four_gib_using_only_a_packet_buffer() {
        let offset = 8 * 1024 * 1024 * 1024u64;
        let mut reader = SparseReader {
            length: offset + 3,
            position: 0,
            regions: vec![(offset, vec![11, 22, 33])],
            bytes_read: 0,
        };
        let mut buffer = Vec::new();
        read_access_unit(&mut reader, sample(offset, 3), offset + 3, &mut buffer).unwrap();
        assert_eq!(buffer, [11, 22, 33]);
        assert_eq!(reader.bytes_read, 3);
        assert!(buffer.capacity() < 1024);
    }

    #[test]
    fn budget_and_range_errors_precede_payload_allocation() {
        let size = 512 * 1024 * 1024u32;
        let mut header = size.to_be_bytes().to_vec();
        header.extend_from_slice(b"moov");
        let mut reader = SparseReader {
            length: u64::from(size),
            position: 0,
            regions: vec![(0, header)],
            bytes_read: 0,
        };
        assert!(matches!(
            read_mp4_metadata(&mut reader, MAX_METADATA_BYTES),
            Err(MediaReadError::MetadataTooLarge { .. })
        ));
        assert_eq!(reader.bytes_read, 8);
        let mut buffer = Vec::new();
        assert!(matches!(
            read_access_unit(&mut reader, sample(0, size), u64::from(size), &mut buffer),
            Err(MediaReadError::AccessUnitTooLarge { .. })
        ));
        assert_eq!(buffer.capacity(), 0);
        assert!(validate_sample_range(sample(u64::MAX - 1, 4), u64::MAX).is_err());
        assert!(validate_sample_range(sample(7, 4), 10).is_err());
    }

    #[test]
    fn validates_box_headers_including_uuid_and_zero_sized_boxes() {
        for data in [
            vec![0, 0, 0, 4, b'f', b'r', b'e', b'e'],
            vec![0, 0, 0, 8, b'u', b'u', b'i', b'd'],
            vec![0, 0, 0, 40, b'm', b'o', b'o', b'v'],
        ] {
            assert!(matches!(
                read_mp4_metadata(&mut Cursor::new(data), MAX_METADATA_BYTES),
                Err(MediaReadError::InvalidBox { .. })
            ));
        }
        assert!(matches!(
            read_mp4_metadata(&mut Cursor::new([0u8; 7]), MAX_METADATA_BYTES),
            Err(MediaReadError::Io(_))
        ));
        let data = vec![0, 0, 0, 0, b'm', b'o', b'o', b'v', 1, 2, 3];
        assert_eq!(
            read_mp4_metadata(&mut Cursor::new(&data), 100)
                .unwrap()
                .bytes,
            data
        );
    }

    #[test]
    fn packet_reads_preserve_buffered_forward_and_backward_seeks() {
        let data = (0u8..32).collect::<Vec<_>>();
        let mut reader = BufReader::with_capacity(8, Cursor::new(&data));
        let mut buffer = Vec::new();
        for (offset, size) in [(0, 3), (3, 2), (1, 4), (20, 7), (2, 2)] {
            read_access_unit(
                &mut reader,
                sample(offset, size),
                data.len() as u64,
                &mut buffer,
            )
            .unwrap();
            assert_eq!(
                buffer.as_slice(),
                &data[offset as usize..offset as usize + size as usize]
            );
        }
    }

    #[test]
    fn reads_plain_crc_and_extended_sync_frames_and_rejects_truncation() {
        let frames = [
            vec![0xac, 0x40, 0, 3, 1, 2, 3],
            vec![0xac, 0x41, 0, 2, 4, 5, 0, 0],
            vec![0xac, 0x40, 0xff, 0xff, 0, 0, 1, 6],
        ];
        let mut reader = Cursor::new(frames.concat());
        let mut buffer = Vec::new();
        for frame in frames {
            assert!(read_sync_frame(&mut reader, &mut buffer).unwrap());
            assert_eq!(buffer, frame);
        }
        assert!(!read_sync_frame(&mut reader, &mut buffer).unwrap());
        for data in [
            vec![0xac],
            vec![0xac, 0x40, 0, 2, 1],
            vec![0xac, 0x40, 0xff, 0xff, 0],
        ] {
            assert!(matches!(
                read_sync_frame(&mut Cursor::new(data), &mut buffer),
                Err(MediaReadError::Io(_))
            ));
        }
        assert!(matches!(
            read_sync_frame(&mut Cursor::new([1, 2, 0, 1, 0]), &mut buffer),
            Err(MediaReadError::SyncFrame(_))
        ));
        assert!(matches!(
            read_sync_frame(&mut Cursor::new([0xac, 0x40, 0, 0]), &mut buffer),
            Err(MediaReadError::SyncFrame(SyncFrameError::EmptyFrame { .. }))
        ));
    }
}
