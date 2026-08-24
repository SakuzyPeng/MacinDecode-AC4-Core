//! ISO BMFF box 遍历。
//!
//! 依据 ISO/IEC 14496-12。此处只做定界与查找，不解释任何 AC-4 语义。

use core::fmt;

/// box 解析失败的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoxError {
    /// 剩余字节不足以容纳 box 头部。
    HeaderTruncated {
        /// box 起始偏移。
        offset: usize,
        /// 该偏移之后可用的字节数。
        available: usize,
    },
    /// 声明的尺寸小于头部长度，或超出所在容器。
    SizeInvalid {
        /// box 起始偏移。
        offset: usize,
        /// 声明的尺寸。
        declared: u64,
        /// 该偏移之后可用的字节数。
        available: usize,
    },
}

impl fmt::Display for BoxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            BoxError::HeaderTruncated { offset, available } => {
                write!(f, "偏移 {offset} 处 box 头部不完整，仅剩 {available} 字节")
            }
            BoxError::SizeInvalid {
                offset,
                declared,
                available,
            } => {
                write!(
                    f,
                    "偏移 {offset} 处 box 声明尺寸 {declared}，但仅有 {available} 字节可用"
                )
            }
        }
    }
}

impl core::error::Error for BoxError {}

/// 一个已定界的 box。
#[derive(Debug, Clone)]
pub struct Mp4Box<'a> {
    /// 四字符类型。
    pub box_type: [u8; 4],
    /// box 在输入切片中的起始偏移。
    pub offset: usize,
    /// 含头部在内的总字节数。
    pub total_len: usize,
    /// 头部字节数，随大尺寸与 uuid 扩展而变。
    pub header_len: usize,
    /// 头部之后的内容。
    pub payload: &'a [u8],
}

impl Mp4Box<'_> {
    /// 类型是否与给定四字符匹配。
    #[must_use]
    pub fn is(&self, name: &[u8; 4]) -> bool {
        self.box_type == *name
    }

    /// 类型的可打印形式；非 ASCII 字节以 `.` 代替。
    #[must_use]
    pub fn type_str(&self) -> [char; 4] {
        let mut out = ['.'; 4];
        for (slot, &byte) in out.iter_mut().zip(self.box_type.iter()) {
            if byte.is_ascii_graphic() {
                *slot = byte as char;
            }
        }
        out
    }
}

/// 在一段字节上顺序遍历同级 box。
#[derive(Debug, Clone)]
pub struct BoxIter<'a> {
    data: &'a [u8],
    position: usize,
    finished: bool,
}

impl<'a> BoxIter<'a> {
    /// 在给定切片上创建遍历器。
    #[must_use]
    pub const fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            position: 0,
            finished: false,
        }
    }

    fn read_u32(&self, at: usize) -> Option<u32> {
        let bytes = self.data.get(at..at.checked_add(4)?)?;
        Some(u32::from_be_bytes([
            *bytes.first()?,
            *bytes.get(1)?,
            *bytes.get(2)?,
            *bytes.get(3)?,
        ]))
    }

    fn read_u64(&self, at: usize) -> Option<u64> {
        let high = u64::from(self.read_u32(at)?);
        let low = u64::from(self.read_u32(at.checked_add(4)?)?);
        Some((high << 32) | low)
    }

    fn parse_at(&mut self) -> Result<Mp4Box<'a>, BoxError> {
        let offset = self.position;
        let available = self.data.len().saturating_sub(offset);

        let declared = self
            .read_u32(offset)
            .ok_or(BoxError::HeaderTruncated { offset, available })?;
        let box_type_bytes = self
            .data
            .get(offset.saturating_add(4)..offset.saturating_add(8))
            .ok_or(BoxError::HeaderTruncated { offset, available })?;
        let mut box_type = [0u8; 4];
        for (slot, &byte) in box_type.iter_mut().zip(box_type_bytes.iter()) {
            *slot = byte;
        }

        let mut header_len = 8usize;
        let total = match declared {
            // size == 1：真实尺寸由随后的 64 位字段给出
            1 => {
                let large = self
                    .read_u64(offset.saturating_add(8))
                    .ok_or(BoxError::HeaderTruncated { offset, available })?;
                header_len = 16;
                large
            }
            // size == 0：该 box 延伸至所在容器末尾
            0 => available as u64,
            other => u64::from(other),
        };

        // uuid box 在标准头之后还带 16 字节 usertype
        if box_type == *b"uuid" {
            header_len = header_len.saturating_add(16);
        }

        let total_len = usize::try_from(total).unwrap_or(usize::MAX);
        if total_len < header_len || total_len > available {
            return Err(BoxError::SizeInvalid {
                offset,
                declared: total,
                available,
            });
        }

        let payload_start = offset.saturating_add(header_len);
        let payload_end = offset.saturating_add(total_len);
        let payload = self
            .data
            .get(payload_start..payload_end)
            .ok_or(BoxError::SizeInvalid {
                offset,
                declared: total,
                available,
            })?;

        self.position = payload_end;
        Ok(Mp4Box {
            box_type,
            offset,
            total_len,
            header_len,
            payload,
        })
    }
}

impl<'a> Iterator for BoxIter<'a> {
    type Item = Result<Mp4Box<'a>, BoxError>;

    fn next(&mut self) -> Option<Self::Item> {
        // 尾部不足一个头部的残余字节视为遍历结束，而非错误：
        // 部分封装器会在容器末尾留下填充。
        if self.finished || self.data.len().saturating_sub(self.position) < 8 {
            return None;
        }
        let result = self.parse_at();
        if result.is_err() {
            self.finished = true;
        }
        Some(result)
    }
}

/// 在一段字节中查找首个指定类型的同级 box。
#[must_use]
pub fn find_box<'a>(data: &'a [u8], name: &[u8; 4]) -> Option<Mp4Box<'a>> {
    BoxIter::new(data).flatten().find(|item| item.is(name))
}

/// 沿路径逐层向下查找 box。
///
/// 每一层都在上一层的 payload 中查找，因此只适用于纯容器 box。
/// `stsd` 之类头部带额外字段的 box 需由调用方自行处理偏移。
#[must_use]
pub fn find_path<'a>(data: &'a [u8], path: &[[u8; 4]]) -> Option<Mp4Box<'a>> {
    let mut current: Option<Mp4Box<'a>> = None;
    let mut scope = data;
    for name in path {
        let found = find_box(scope, name)?;
        scope = found.payload;
        current = Some(found);
    }
    current
}

#[cfg(test)]
#[expect(clippy::arithmetic_side_effects, reason = "测试内构造固定长度的 box")]
mod tests {
    use super::*;

    fn make_box(box_type: &[u8; 4], payload: &[u8]) -> [u8; 16] {
        let mut out = [0u8; 16];
        let total = (8 + payload.len()) as u32;
        out[..4].copy_from_slice(&total.to_be_bytes());
        out[4..8].copy_from_slice(box_type);
        for (index, &byte) in payload.iter().enumerate() {
            if let Some(slot) = out.get_mut(8 + index) {
                *slot = byte;
            }
        }
        out
    }

    #[test]
    fn parses_simple_box() {
        let data = make_box(b"ftyp", &[1, 2, 3, 4]);
        let found = find_box(&data[..12], b"ftyp").unwrap();
        assert_eq!(found.total_len, 12);
        assert_eq!(found.header_len, 8);
        assert_eq!(found.payload, &[1, 2, 3, 4]);
    }

    #[test]
    fn parses_large_size_box() {
        let mut data = [0u8; 20];
        data[..4].copy_from_slice(&1u32.to_be_bytes()); // size == 1 → 64 位尺寸
        data[4..8].copy_from_slice(b"mdat");
        data[8..16].copy_from_slice(&20u64.to_be_bytes());
        data[16..].copy_from_slice(&[9, 9, 9, 9]);
        let found = find_box(&data, b"mdat").unwrap();
        assert_eq!(found.header_len, 16, "64 位尺寸的头部为 16 字节");
        assert_eq!(found.total_len, 20);
        assert_eq!(found.payload, &[9, 9, 9, 9]);
    }

    #[test]
    fn size_zero_extends_to_end() {
        let mut data = [0u8; 12];
        data[..4].copy_from_slice(&0u32.to_be_bytes());
        data[4..8].copy_from_slice(b"free");
        let found = find_box(&data, b"free").unwrap();
        assert_eq!(found.total_len, 12);
        assert_eq!(found.payload.len(), 4);
    }

    #[test]
    fn rejects_size_beyond_container() {
        let mut data = [0u8; 12];
        data[..4].copy_from_slice(&999u32.to_be_bytes());
        data[4..8].copy_from_slice(b"moov");
        let mut iter = BoxIter::new(&data);
        assert!(matches!(
            iter.next().unwrap().unwrap_err(),
            BoxError::SizeInvalid { offset: 0, .. }
        ));
        assert!(iter.next().is_none(), "出错后停止遍历");
    }

    #[test]
    fn rejects_size_smaller_than_header() {
        let mut data = [0u8; 12];
        data[..4].copy_from_slice(&4u32.to_be_bytes()); // 小于 8 字节头部
        data[4..8].copy_from_slice(b"junk");
        let mut iter = BoxIter::new(&data);
        assert!(matches!(
            iter.next().unwrap().unwrap_err(),
            BoxError::SizeInvalid { .. }
        ));
    }

    #[test]
    fn iterates_siblings() {
        let mut data = [0u8; 24];
        data[..12].copy_from_slice(&make_box(b"ftyp", &[1, 2, 3, 4])[..12]);
        data[12..].copy_from_slice(&make_box(b"moov", &[5, 6, 7, 8])[..12]);
        let types: [[u8; 4]; 2] = [*b"ftyp", *b"moov"];
        let found: heapless_vec::Vec = BoxIter::new(&data).flatten().collect();
        assert_eq!(found.len(), 2);
        for (item, expected) in found.iter().zip(types.iter()) {
            assert_eq!(&item.box_type, expected);
        }
    }

    /// 尾部残余不足一个头部时应静默结束，而不是报错
    #[test]
    fn trailing_bytes_end_iteration() {
        let mut data = [0u8; 15];
        data[..12].copy_from_slice(&make_box(b"ftyp", &[1, 2, 3, 4])[..12]);
        let count = BoxIter::new(&data).count();
        assert_eq!(count, 1);
    }

    #[test]
    fn finds_nested_path() {
        // moov > trak > mdia
        let mdia = make_box(b"mdia", &[7]);
        let mut trak_payload = [0u8; 9];
        trak_payload.copy_from_slice(&mdia[..9]);
        let mut trak = [0u8; 17];
        trak[..4].copy_from_slice(&17u32.to_be_bytes());
        trak[4..8].copy_from_slice(b"trak");
        trak[8..].copy_from_slice(&trak_payload);
        let mut moov = [0u8; 25];
        moov[..4].copy_from_slice(&25u32.to_be_bytes());
        moov[4..8].copy_from_slice(b"moov");
        moov[8..].copy_from_slice(&trak);

        let found = find_path(&moov, &[*b"moov", *b"trak", *b"mdia"]).unwrap();
        assert_eq!(found.payload, &[7]);
        assert!(find_path(&moov, &[*b"moov", *b"minf"]).is_none());
    }
}

/// 测试内用的极小定长向量，避免为单元测试引入 alloc。
#[cfg(test)]
mod heapless_vec {
    use super::Mp4Box;

    #[derive(Debug, Default)]
    pub struct Vec<'a> {
        items: [Option<Mp4Box<'a>>; 8],
        len: usize,
    }

    impl<'a> Vec<'a> {
        pub fn len(&self) -> usize {
            self.len
        }

        pub fn iter(&self) -> impl Iterator<Item = &Mp4Box<'a>> {
            self.items.iter().flatten()
        }
    }

    impl<'a> FromIterator<Mp4Box<'a>> for Vec<'a> {
        fn from_iter<T: IntoIterator<Item = Mp4Box<'a>>>(iter: T) -> Self {
            let mut out = Vec::default();
            for item in iter {
                if let Some(slot) = out.items.get_mut(out.len) {
                    *slot = Some(item);
                    out.len = out.len.saturating_add(1);
                }
            }
            out
        }
    }
}
