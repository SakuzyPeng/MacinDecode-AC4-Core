//! Sample table：把 `stbl` 下的各表折算为每个 sample 的字节范围与时间。
//!
//! 依据 ISO/IEC 14496-12。
//!
//! 各表分别描述时长、大小、chunk 归属与 chunk 偏移，必须联合遍历才能定位
//! 单个 sample。此处以游标推进的方式产出，不做任何分配，也不预先展开成
//! 数组：sample 数量由文件声明，展开会让损坏输入直接决定内存用量。

use core::fmt;

/// sample table 解析失败的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleTableError {
    /// 缺少必需的子 box。
    MissingBox {
        /// 缺失的 box 类型。
        box_type: [u8; 4],
    },
    /// 子 box 负载不足以容纳其声明的条目。
    Truncated {
        /// 出问题的 box 类型。
        box_type: [u8; 4],
    },
    /// FullBox 版本超出当前规范定义。
    UnsupportedVersion {
        /// 出问题的 box 类型。
        box_type: [u8; 4],
        /// 实际版本号。
        version: u8,
    },
    /// `stsc` 缺少有效映射、首项不从 chunk 1 开始、条目非递增，或 chunk 样本数为 0。
    ChunkMappingInvalid {
        /// 出问题的条目序号。
        entry: u32,
    },
    /// `stsc` 的 sample-description 下标为规范禁止的 0。
    SampleDescriptionInvalid {
        /// 出问题的 `stsc` 条目序号。
        entry: u32,
        /// 实际的 sample-description 下标。
        index: u32,
    },
}

impl fmt::Display for SampleTableError {
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
            SampleTableError::MissingBox { box_type } => {
                let c = name(&box_type);
                write!(f, "Missing {}{}{}{}", c[0], c[1], c[2], c[3])
            }
            SampleTableError::Truncated { box_type } => {
                let c = name(&box_type);
                write!(f, "Truncated {}{}{}{} entry", c[0], c[1], c[2], c[3])
            }
            SampleTableError::UnsupportedVersion { box_type, version } => {
                let c = name(&box_type);
                write!(
                    f,
                    "Undefined {}{}{}{} version {version}",
                    c[0], c[1], c[2], c[3]
                )
            }
            SampleTableError::ChunkMappingInvalid { entry } => {
                write!(f, "Invalid stsc chunk mapping at entry {entry}")
            }
            SampleTableError::SampleDescriptionInvalid { entry, index } => {
                write!(
                    f,
                    "Invalid sample_description_index {index} in stsc entry {entry}"
                )
            }
        }
    }
}

impl core::error::Error for SampleTableError {}

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

fn read_i32(data: &[u8], at: usize) -> Option<i32> {
    let bytes = data.get(at..at.checked_add(4)?)?;
    Some(i32::from_be_bytes([
        *bytes.first()?,
        *bytes.get(1)?,
        *bytes.get(2)?,
        *bytes.get(3)?,
    ]))
}

/// 一个 sample 的位置与时间。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SampleInfo {
    /// 从 0 开始的序号。
    pub index: u32,
    /// 在文件中的字节偏移。
    pub offset: u64,
    /// 字节数。
    pub size: u32,
    /// `stsc` 为该 sample 选择的、从 1 开始的 `stsd` entry 下标。
    pub sample_description_index: u32,
    /// 解码时间，以 media 时间刻度表示。
    pub decode_time: u64,
    /// composition time，即应用 `ctts` 后、应用 edit list 前的 media PTS。
    ///
    /// 版本 1 的 `ctts` 允许负偏移，因此该值必须为有符号整数。
    pub composition_time: i64,
    /// 本 sample 的时长。
    pub duration: u32,
    /// 是否为同步样本。
    ///
    /// 没有 `stss` 时全部 sample 均为同步样本，见 ISO/IEC 14496-12。
    pub is_sync: bool,
}

/// `stbl` 中与定位相关的各表。
#[derive(Debug, Clone)]
pub struct SampleTable<'a> {
    stts: &'a [u8],
    stsc: &'a [u8],
    stsz: &'a [u8],
    ctts: Option<&'a [u8]>,
    chunk_offsets: &'a [u8],
    chunk_offsets_are_64bit: bool,
    stss: Option<&'a [u8]>,
    sample_count: u32,
}

impl<'a> SampleTable<'a> {
    /// 从 `stbl` 负载收集各子表。
    ///
    /// # Errors
    ///
    /// 缺少 `stts`、`stsc`、`stsz` 或 chunk 偏移表时返回
    /// [`SampleTableError::MissingBox`]。
    pub fn parse(stbl_payload: &'a [u8]) -> Result<Self, SampleTableError> {
        use crate::boxes::find_box;
        let need = |name: &[u8; 4]| SampleTableError::MissingBox { box_type: *name };

        let stts = find_box(stbl_payload, b"stts").ok_or_else(|| need(b"stts"))?;
        let stsc = find_box(stbl_payload, b"stsc").ok_or_else(|| need(b"stsc"))?;
        let stsz = find_box(stbl_payload, b"stsz").ok_or_else(|| need(b"stsz"))?;
        let stss = find_box(stbl_payload, b"stss").map(|item| item.payload);
        let ctts = find_box(stbl_payload, b"ctts").map(|item| item.payload);

        if let Some(payload) = ctts {
            let version = *payload
                .first()
                .ok_or(SampleTableError::Truncated { box_type: *b"ctts" })?;
            if version > 1 {
                return Err(SampleTableError::UnsupportedVersion {
                    box_type: *b"ctts",
                    version,
                });
            }
            read_u32(payload, 4).ok_or(SampleTableError::Truncated { box_type: *b"ctts" })?;
        }

        let (chunk_offsets, chunk_offsets_are_64bit) = match find_box(stbl_payload, b"stco") {
            Some(item) => (item.payload, false),
            None => (
                find_box(stbl_payload, b"co64")
                    .ok_or_else(|| need(b"stco"))?
                    .payload,
                true,
            ),
        };

        let sample_count =
            read_u32(stsz.payload, 8).ok_or(SampleTableError::Truncated { box_type: *b"stsz" })?;

        Ok(Self {
            stts: stts.payload,
            stsc: stsc.payload,
            stsz: stsz.payload,
            ctts,
            chunk_offsets,
            chunk_offsets_are_64bit,
            stss,
            sample_count,
        })
    }

    /// sample 总数，取自 `stsz`。
    #[must_use]
    pub const fn sample_count(&self) -> u32 {
        self.sample_count
    }

    fn sample_size(&self, index: u32) -> Option<u32> {
        let uniform = read_u32(self.stsz, 4)?;
        if uniform != 0 {
            return Some(uniform);
        }
        let at = 12usize.checked_add(usize::try_from(index).ok()?.checked_mul(4)?)?;
        read_u32(self.stsz, at)
    }

    fn chunk_offset(&self, chunk_index: u32) -> Option<u64> {
        let index = usize::try_from(chunk_index).ok()?;
        if self.chunk_offsets_are_64bit {
            read_u64(
                self.chunk_offsets,
                8usize.checked_add(index.checked_mul(8)?)?,
            )
        } else {
            read_u32(
                self.chunk_offsets,
                8usize.checked_add(index.checked_mul(4)?)?,
            )
            .map(u64::from)
        }
    }

    /// 该 sample 是否在 `stss` 中列出。
    ///
    /// `stss` 缺失表示全部 sample 都是同步样本。
    fn is_sync_sample(&self, index: u32) -> bool {
        let Some(stss) = self.stss else {
            return true;
        };
        let Some(count) = read_u32(stss, 4) else {
            return false;
        };
        // stss 中的序号从 1 开始且递增，可二分查找
        let target = index.saturating_add(1);
        let (mut low, mut high) = (0u32, count);
        while low < high {
            let mid = low.saturating_add(high.saturating_sub(low) / 2);
            let at = match usize::try_from(mid)
                .ok()
                .and_then(|m| m.checked_mul(4))
                .and_then(|m| 8usize.checked_add(m))
            {
                Some(at) => at,
                None => return false,
            };
            match read_u32(stss, at) {
                Some(value) if value == target => return true,
                Some(value) if value < target => low = mid.saturating_add(1),
                Some(_) => high = mid,
                None => return false,
            }
        }
        false
    }

    /// 按序遍历全部 sample。
    #[must_use]
    pub fn iter(&'a self) -> SampleIter<'a> {
        SampleIter {
            table: self,
            index: 0,
            decode_time: 0,
            stts_entry: 0,
            stts_left_in_entry: 0,
            stts_current_delta: 0,
            ctts_entry: 0,
            ctts_left_in_entry: 0,
            ctts_current_offset: 0,
            stsc_entry: 0,
            chunk_index: 0,
            sample_in_chunk: 0,
            samples_per_chunk: 0,
            sample_description_index: 0,
            last_first_chunk: 0,
            offset_in_chunk: 0,
            failed: false,
        }
    }
}

/// 逐个产出 sample 位置与时间。
#[derive(Debug, Clone)]
pub struct SampleIter<'a> {
    table: &'a SampleTable<'a>,
    index: u32,
    decode_time: u64,
    stts_entry: u32,
    stts_left_in_entry: u32,
    stts_current_delta: u32,
    ctts_entry: u32,
    ctts_left_in_entry: u32,
    ctts_current_offset: i64,
    stsc_entry: u32,
    chunk_index: u32,
    sample_in_chunk: u32,
    samples_per_chunk: u32,
    sample_description_index: u32,
    last_first_chunk: u32,
    offset_in_chunk: u64,
    failed: bool,
}

impl SampleIter<'_> {
    /// 推进 `stts` 游标，取得当前 sample 的时长。
    fn next_duration(&mut self) -> Option<u32> {
        while self.stts_left_in_entry == 0 {
            let entry_count = read_u32(self.table.stts, 4)?;
            if self.stts_entry >= entry_count {
                return None;
            }
            let base =
                8usize.checked_add(usize::try_from(self.stts_entry).ok()?.checked_mul(8)?)?;
            self.stts_left_in_entry = read_u32(self.table.stts, base)?;
            self.stts_current_delta = read_u32(self.table.stts, base.checked_add(4)?)?;
            self.stts_entry = self.stts_entry.checked_add(1)?;
            // 条目声明 0 个 sample 时继续取下一条，避免死循环
        }
        self.stts_left_in_entry = self.stts_left_in_entry.saturating_sub(1);
        Some(self.stts_current_delta)
    }

    /// 推进 `ctts` 游标，取得当前 sample 相对 DTS 的 composition offset。
    fn next_composition_offset(&mut self) -> Option<i64> {
        let Some(ctts) = self.table.ctts else {
            return Some(0);
        };
        while self.ctts_left_in_entry == 0 {
            let entry_count = read_u32(ctts, 4)?;
            if self.ctts_entry >= entry_count {
                return None;
            }
            let base =
                8usize.checked_add(usize::try_from(self.ctts_entry).ok()?.checked_mul(8)?)?;
            self.ctts_left_in_entry = read_u32(ctts, base)?;
            self.ctts_current_offset = match ctts.first().copied()? {
                0 => i64::from(read_u32(ctts, base.checked_add(4)?)?),
                1 => i64::from(read_i32(ctts, base.checked_add(4)?)?),
                _ => return None,
            };
            self.ctts_entry = self.ctts_entry.checked_add(1)?;
            // 0 个 sample 的条目不推进时间，继续读取下一条。
        }
        self.ctts_left_in_entry = self.ctts_left_in_entry.saturating_sub(1);
        Some(self.ctts_current_offset)
    }

    /// 推进 `stsc` 游标，必要时切换到下一个 chunk。
    fn ensure_chunk(&mut self) -> Result<(), SampleTableError> {
        if self.sample_in_chunk < self.samples_per_chunk {
            return Ok(());
        }
        let truncated = || SampleTableError::Truncated { box_type: *b"stbl" };

        // 进入新 chunk：重置 chunk 内偏移
        if self.samples_per_chunk != 0 {
            self.chunk_index = self.chunk_index.checked_add(1).ok_or_else(truncated)?;
        }
        self.sample_in_chunk = 0;
        self.offset_in_chunk = 0;

        let entry_count = read_u32(self.table.stsc, 4).ok_or_else(truncated)?;
        // stsc 条目按 first_chunk 分段；当前 chunk 落在最后一个 first_chunk
        // 不大于它的条目所描述的区间内
        while self.stsc_entry < entry_count {
            let entry = self.stsc_entry;
            let base = usize::try_from(entry)
                .ok()
                .and_then(|entry| entry.checked_mul(12))
                .and_then(|entry| 8usize.checked_add(entry))
                .ok_or_else(truncated)?;
            let first_chunk = read_u32(self.table.stsc, base).ok_or_else(truncated)?;
            if first_chunk == 0
                || (entry == 0 && first_chunk != 1)
                || (entry != 0 && first_chunk <= self.last_first_chunk)
            {
                return Err(SampleTableError::ChunkMappingInvalid { entry });
            }
            // first_chunk 从 1 开始计数
            if first_chunk.saturating_sub(1) > self.chunk_index {
                break;
            }
            self.samples_per_chunk =
                read_u32(self.table.stsc, base.checked_add(4).ok_or_else(truncated)?)
                    .ok_or_else(truncated)?;
            self.sample_description_index =
                read_u32(self.table.stsc, base.checked_add(8).ok_or_else(truncated)?)
                    .ok_or_else(truncated)?;
            if self.samples_per_chunk == 0 {
                return Err(SampleTableError::ChunkMappingInvalid { entry });
            }
            if self.sample_description_index == 0 {
                return Err(SampleTableError::SampleDescriptionInvalid {
                    entry,
                    index: self.sample_description_index,
                });
            }
            self.stsc_entry = self.stsc_entry.checked_add(1).ok_or_else(truncated)?;
            self.last_first_chunk = first_chunk;
        }

        if self.samples_per_chunk == 0 {
            return Err(SampleTableError::ChunkMappingInvalid {
                entry: self.stsc_entry,
            });
        }
        Ok(())
    }
}

impl Iterator for SampleIter<'_> {
    type Item = Result<SampleInfo, SampleTableError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed || self.index >= self.table.sample_count {
            return None;
        }

        let mut step = || -> Result<SampleInfo, SampleTableError> {
            let truncated = || SampleTableError::Truncated { box_type: *b"stbl" };
            self.ensure_chunk()?;
            let size = self.table.sample_size(self.index).ok_or_else(truncated)?;
            let chunk_start = self
                .table
                .chunk_offset(self.chunk_index)
                .ok_or_else(truncated)?;
            let offset = chunk_start
                .checked_add(self.offset_in_chunk)
                .ok_or_else(truncated)?;
            let duration = self.next_duration().ok_or_else(truncated)?;
            let composition_offset = self.next_composition_offset().ok_or_else(truncated)?;
            let composition_time = i64::try_from(self.decode_time)
                .map_err(|_| truncated())?
                .checked_add(composition_offset)
                .ok_or_else(truncated)?;
            let info = SampleInfo {
                index: self.index,
                offset,
                size,
                sample_description_index: self.sample_description_index,
                decode_time: self.decode_time,
                composition_time,
                duration,
                is_sync: self.table.is_sync_sample(self.index),
            };

            self.offset_in_chunk = self
                .offset_in_chunk
                .checked_add(u64::from(size))
                .ok_or_else(truncated)?;
            self.sample_in_chunk = self.sample_in_chunk.checked_add(1).ok_or_else(truncated)?;
            self.decode_time = self
                .decode_time
                .checked_add(u64::from(duration))
                .ok_or_else(truncated)?;
            self.index = self.index.checked_add(1).ok_or_else(truncated)?;
            Ok(info)
        };

        match step() {
            Ok(info) => Some(Ok(info)),
            Err(error) => {
                self.failed = true;
                Some(Err(error))
            }
        }
    }
}

#[cfg(test)]
#[expect(clippy::indexing_slicing, reason = "测试内按固定布局构造并检视 stbl")]
mod tests {
    extern crate std;

    use super::*;
    use std::vec::Vec;

    /// 构造一个最小 `stbl`：单 chunk、等长 sample、恒定时长。
    fn build_stbl(sample_count: u32, size: u32, delta: u32, chunk_offset: u32) -> [u8; 132] {
        let mut out = [0u8; 132];
        let mut at = 0usize;
        let mut put = |bytes: &[u8], at: &mut usize| {
            for &byte in bytes {
                if let Some(slot) = out.get_mut(*at) {
                    *slot = byte;
                }
                *at = at.saturating_add(1);
            }
        };
        // stts：1 条目
        put(&32u32.to_be_bytes(), &mut at);
        put(b"stts", &mut at);
        put(&0u32.to_be_bytes(), &mut at);
        put(&1u32.to_be_bytes(), &mut at);
        put(&sample_count.to_be_bytes(), &mut at);
        put(&delta.to_be_bytes(), &mut at);
        put(&[0u8; 8], &mut at);
        // stsc：1 条目，全部 sample 在第 1 个 chunk
        put(&28u32.to_be_bytes(), &mut at);
        put(b"stsc", &mut at);
        put(&0u32.to_be_bytes(), &mut at);
        put(&1u32.to_be_bytes(), &mut at);
        put(&1u32.to_be_bytes(), &mut at);
        put(&sample_count.to_be_bytes(), &mut at);
        put(&1u32.to_be_bytes(), &mut at);
        // stsz：等长
        put(&20u32.to_be_bytes(), &mut at);
        put(b"stsz", &mut at);
        put(&0u32.to_be_bytes(), &mut at);
        put(&size.to_be_bytes(), &mut at);
        put(&sample_count.to_be_bytes(), &mut at);
        // stco：1 个 chunk
        put(&20u32.to_be_bytes(), &mut at);
        put(b"stco", &mut at);
        put(&0u32.to_be_bytes(), &mut at);
        put(&1u32.to_be_bytes(), &mut at);
        put(&chunk_offset.to_be_bytes(), &mut at);
        out
    }

    #[test]
    fn walks_uniform_table() {
        let data = build_stbl(4, 100, 2_048, 1_000);
        let table = SampleTable::parse(&data).unwrap();
        assert_eq!(table.sample_count(), 4);

        let mut seen = 0usize;
        for (expected, item) in table.iter().enumerate() {
            let info = item.unwrap();
            let index = expected as u32;
            assert_eq!(info.index, index);
            assert_eq!(info.size, 100);
            assert_eq!(info.sample_description_index, 1);
            assert_eq!(info.offset, 1_000 + u64::from(index) * 100);
            assert_eq!(info.decode_time, u64::from(index) * 2_048);
            assert_eq!(info.duration, 2_048);
            assert!(info.is_sync, "无 stss 时全部为同步样本");
            seen = seen.saturating_add(1);
        }
        assert_eq!(seen, 4);
    }

    #[test]
    fn preserves_and_validates_sample_description_index() {
        let mut data = build_stbl(1, 100, 2_048, 1_000);
        let sample_description_at = 32 + 24;
        data[sample_description_at..sample_description_at + 4].copy_from_slice(&7u32.to_be_bytes());
        let table = SampleTable::parse(&data).unwrap();
        assert_eq!(
            table
                .iter()
                .next()
                .expect("sample should exist")
                .unwrap()
                .sample_description_index,
            7
        );

        data[sample_description_at..sample_description_at + 4].copy_from_slice(&0u32.to_be_bytes());
        let table = SampleTable::parse(&data).unwrap();
        assert_eq!(
            table
                .iter()
                .next()
                .expect("sample should fail")
                .unwrap_err(),
            SampleTableError::SampleDescriptionInvalid { entry: 0, index: 0 }
        );
    }

    #[test]
    fn switches_sample_description_at_chunk_boundary() {
        let mut data = build_stbl(2, 100, 2_048, 1_000).to_vec();
        data[32..36].copy_from_slice(&40u32.to_be_bytes());
        data[44..48].copy_from_slice(&2u32.to_be_bytes());
        data[52..56].copy_from_slice(&1u32.to_be_bytes());
        let mut second_mapping = Vec::new();
        second_mapping.extend_from_slice(&2u32.to_be_bytes());
        second_mapping.extend_from_slice(&1u32.to_be_bytes());
        second_mapping.extend_from_slice(&2u32.to_be_bytes());
        data.splice(60..60, second_mapping);

        let stco_at = 92usize;
        data[stco_at..stco_at + 4].copy_from_slice(&24u32.to_be_bytes());
        data[stco_at + 12..stco_at + 16].copy_from_slice(&2u32.to_be_bytes());
        data.splice(stco_at + 20..stco_at + 20, 1_100u32.to_be_bytes());

        let table = SampleTable::parse(&data).unwrap();
        let samples = table
            .iter()
            .collect::<Result<Vec<_>, _>>()
            .expect("both chunk mappings should be valid");
        assert_eq!(samples.len(), 2);
        assert_eq!(samples[0].sample_description_index, 1);
        assert_eq!(samples[0].offset, 1_000);
        assert_eq!(samples[1].sample_description_index, 2);
        assert_eq!(samples[1].offset, 1_100);
    }

    #[test]
    fn decode_time_accumulates() {
        let data = build_stbl(3, 10, 1_920, 0);
        let table = SampleTable::parse(&data).unwrap();
        let times: [u64; 3] = {
            let mut out = [0u64; 3];
            for (slot, item) in out.iter_mut().zip(table.iter()) {
                *slot = item.unwrap().decode_time;
            }
            out
        };
        assert_eq!(times, [0, 1_920, 3_840]);
    }

    #[test]
    fn missing_box_is_reported() {
        // 只放一个 stts，缺 stsc/stsz/stco
        let mut data = [0u8; 16];
        data[..4].copy_from_slice(&16u32.to_be_bytes());
        data[4..8].copy_from_slice(b"stts");
        assert!(matches!(
            SampleTable::parse(&data).unwrap_err(),
            SampleTableError::MissingBox { .. }
        ));
    }

    #[test]
    fn zero_samples_yields_nothing() {
        let data = build_stbl(0, 100, 2_048, 0);
        let table = SampleTable::parse(&data).unwrap();
        assert_eq!(table.sample_count(), 0);
        assert_eq!(table.iter().count(), 0);
    }

    /// sample 数量超出 stsz 声明的条目时必须报错而非回绕
    #[test]
    fn truncated_size_table_reports_error() {
        let mut data = build_stbl(4, 0, 2_048, 0);
        // 把 stsz 改成变长模式但不提供条目
        let stsz_at = 32 + 28;
        data[stsz_at + 12..stsz_at + 16].copy_from_slice(&0u32.to_be_bytes());
        let table = SampleTable::parse(&data).unwrap();
        let last = table.iter().last().unwrap();
        assert!(last.is_err(), "缺少 sample 大小条目应报错");
    }

    #[test]
    fn applies_signed_composition_offsets() {
        let base = build_stbl(2, 10, 2_048, 0);
        let mut data = base[..100].to_vec();
        let mut ctts = [0u8; 32];
        ctts[..4].copy_from_slice(&32u32.to_be_bytes());
        ctts[4..8].copy_from_slice(b"ctts");
        ctts[8] = 1; // version 1 使用有符号 sample_offset
        ctts[12..16].copy_from_slice(&2u32.to_be_bytes());
        ctts[16..20].copy_from_slice(&1u32.to_be_bytes());
        ctts[20..24].copy_from_slice(&(-1_024i32).to_be_bytes());
        ctts[24..28].copy_from_slice(&1u32.to_be_bytes());
        ctts[28..32].copy_from_slice(&0i32.to_be_bytes());
        data.extend_from_slice(&ctts);

        let table = SampleTable::parse(&data).unwrap();
        let mut samples = table.iter();
        assert_eq!(samples.next().unwrap().unwrap().composition_time, -1_024);
        assert_eq!(samples.next().unwrap().unwrap().composition_time, 2_048);
        assert!(samples.next().is_none());
    }

    #[test]
    fn rejects_unknown_ctts_version() {
        let base = build_stbl(1, 10, 2_048, 0);
        let mut data = base[..100].to_vec();
        let mut ctts = [0u8; 16];
        ctts[..4].copy_from_slice(&16u32.to_be_bytes());
        ctts[4..8].copy_from_slice(b"ctts");
        ctts[8] = 2;
        data.extend_from_slice(&ctts);

        assert!(matches!(
            SampleTable::parse(&data).unwrap_err(),
            SampleTableError::UnsupportedVersion {
                box_type: [b'c', b't', b't', b's'],
                version: 2
            }
        ));
    }
}
