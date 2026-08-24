//! 带 CoreAudio 声道布局标签的 Float32 PCM CAF writer。
//!
//! CAF 的 chunk header 与描述字段均为大端；`lpcm` 样本按 `desc` 中的
//! `IsLittleEndian` 标志写成小端 f32。布局 tag 使用 CoreAudio 公布的整数值，
//! writer 本身不链接 AudioToolbox。

use std::io::{self, Seek, SeekFrom, Write};

const FLOAT32_BYTES: u64 = 4;
const DATA_EDIT_COUNT_BYTES: u64 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CafChannelLayout {
    Mpeg51A,
    Atmos512,
    Atmos514,
    Atmos714,
}

impl CafChannelLayout {
    pub(crate) const fn channels(self) -> usize {
        match self {
            Self::Mpeg51A => 6,
            Self::Atmos512 => 8,
            Self::Atmos514 => 10,
            Self::Atmos714 => 12,
        }
    }

    pub(crate) const fn tag(self) -> u32 {
        match self {
            // CoreAudioTypes/CoreAudioBaseTypes.h:
            // kAudioChannelLayoutTag_MPEG_5_1_A
            Self::Mpeg51A => (121u32 << 16) | 6,
            // kAudioChannelLayoutTag_Atmos_5_1_2
            Self::Atmos512 => (194u32 << 16) | 8,
            // kAudioChannelLayoutTag_Atmos_5_1_4
            Self::Atmos514 => (195u32 << 16) | 10,
            // kAudioChannelLayoutTag_Atmos_7_1_4
            Self::Atmos714 => (192u32 << 16) | 12,
        }
    }

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Mpeg51A => "5.1",
            Self::Atmos512 => "5.1.2",
            Self::Atmos514 => "5.1.4",
            Self::Atmos714 => "7.1.4",
        }
    }

    pub(crate) const fn channel_order(self) -> &'static str {
        match self {
            Self::Mpeg51A => "L R C LFE Ls Rs",
            Self::Atmos512 => "L R C LFE Ls Rs Ltm Rtm",
            Self::Atmos514 => "L R C LFE Ls Rs Vhl Vhr Ltr Rtr",
            Self::Atmos714 => "L R C LFE Ls Rs Rls Rrs Vhl Vhr Ltr Rtr",
        }
    }
}

pub(crate) struct FloatCafWriter<W: Write + Seek> {
    inner: W,
    channels: usize,
    data_size_offset: u64,
    pcm_bytes_written: u64,
}

impl<W: Write + Seek> FloatCafWriter<W> {
    pub(crate) fn new(
        mut inner: W,
        sample_rate: u32,
        layout: CafChannelLayout,
    ) -> io::Result<Self> {
        if sample_rate == 0 {
            return Err(invalid_input("CAF sample rate must not be zero"));
        }
        let channels = u32::try_from(layout.channels())
            .map_err(|_| invalid_input("CAF channel count exceeds u32"))?;
        let bytes_per_packet = channels
            .checked_mul(u32::try_from(FLOAT32_BYTES).unwrap_or(4))
            .ok_or_else(|| invalid_input("CAF bytes-per-packet overflow"))?;

        write_fourcc(&mut inner, *b"caff")?;
        write_u16(&mut inner, 1)?;
        write_u16(&mut inner, 0)?;

        write_fourcc(&mut inner, *b"desc")?;
        write_i64(&mut inner, 32)?;
        inner.write_all(&f64::from(sample_rate).to_bits().to_be_bytes())?;
        write_fourcc(&mut inner, *b"lpcm")?;
        write_u32(&mut inner, 0x3)?; // IsFloat | IsLittleEndian
        write_u32(&mut inner, bytes_per_packet)?;
        write_u32(&mut inner, 1)?; // frames per packet
        write_u32(&mut inner, channels)?;
        write_u32(&mut inner, 32)?; // bits per channel

        write_fourcc(&mut inner, *b"chan")?;
        write_i64(&mut inner, 12)?;
        write_u32(&mut inner, layout.tag())?;
        write_u32(&mut inner, 0)?; // bitmap unused for a layout tag
        write_u32(&mut inner, 0)?; // no channel descriptions

        write_fourcc(&mut inner, *b"data")?;
        let data_size_offset = inner.stream_position()?;
        write_i64(&mut inner, -1)?;
        write_u32(&mut inner, 0)?; // edit count

        Ok(Self {
            inner,
            channels: layout.channels(),
            data_size_offset,
            pcm_bytes_written: 0,
        })
    }

    pub(crate) fn write_interleaved(&mut self, samples: &[f32]) -> io::Result<()> {
        if samples.len().checked_rem(self.channels) != Some(0) {
            return Err(invalid_input(format!(
                "CAF interleaved sample count {} does not form complete {}-channel frames",
                samples.len(),
                self.channels
            )));
        }
        if let Some(sample) = samples.iter().find(|sample| !sample.is_finite()) {
            return Err(invalid_input(format!(
                "CAF PCM contains a non-finite sample: {sample:?}"
            )));
        }
        let added = u64::try_from(samples.len())
            .ok()
            .and_then(|count| count.checked_mul(FLOAT32_BYTES))
            .ok_or_else(|| invalid_input("CAF PCM byte-count overflow"))?;
        let new_total = self
            .pcm_bytes_written
            .checked_add(added)
            .ok_or_else(|| invalid_input("CAF cumulative PCM byte-count overflow"))?;
        checked_data_chunk_size(new_total)?;

        let capacity = samples
            .len()
            .checked_mul(usize::try_from(FLOAT32_BYTES).unwrap_or(4))
            .ok_or_else(|| invalid_input("CAF temporary PCM buffer length overflow"))?;
        let mut bytes = Vec::with_capacity(capacity);
        for sample in samples {
            bytes.extend_from_slice(&sample.to_bits().to_le_bytes());
        }
        self.inner.write_all(&bytes)?;
        self.pcm_bytes_written = new_total;
        Ok(())
    }

    pub(crate) fn finish(mut self) -> io::Result<W> {
        let end = self.inner.stream_position()?;
        let data_size = checked_data_chunk_size(self.pcm_bytes_written)?;
        self.inner.seek(SeekFrom::Start(self.data_size_offset))?;
        write_i64(&mut self.inner, data_size)?;
        self.inner.seek(SeekFrom::Start(end))?;
        self.inner.flush()?;
        Ok(self.inner)
    }
}

fn checked_data_chunk_size(pcm_bytes: u64) -> io::Result<i64> {
    let size = pcm_bytes
        .checked_add(DATA_EDIT_COUNT_BYTES)
        .ok_or_else(|| invalid_input("CAF data-chunk length overflow"))?;
    i64::try_from(size).map_err(|_| invalid_input("CAF data chunk exceeds i64"))
}

fn invalid_input(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

fn write_fourcc<W: Write>(writer: &mut W, value: [u8; 4]) -> io::Result<()> {
    writer.write_all(&value)
}

fn write_u16<W: Write>(writer: &mut W, value: u16) -> io::Result<()> {
    writer.write_all(&value.to_be_bytes())
}

fn write_u32<W: Write>(writer: &mut W, value: u32) -> io::Result<()> {
    writer.write_all(&value.to_be_bytes())
}

fn write_i64<W: Write>(writer: &mut W, value: i64) -> io::Result<()> {
    writer.write_all(&value.to_be_bytes())
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::indexing_slicing,
        reason = "CAF 固定二进制布局测试按规范偏移核对字段"
    )]

    use super::*;
    use std::io::Cursor;

    #[test]
    fn writes_all_supported_layout_tags_and_channel_counts() {
        for (layout, channels, tag) in [
            (CafChannelLayout::Mpeg51A, 6u32, (121u32 << 16) | 6),
            (CafChannelLayout::Atmos512, 8, (194u32 << 16) | 8),
            (CafChannelLayout::Atmos514, 10, (195u32 << 16) | 10),
            (CafChannelLayout::Atmos714, 12, (192u32 << 16) | 12),
        ] {
            let cursor = Cursor::new(Vec::new());
            let writer = FloatCafWriter::new(cursor, 48_000, layout).unwrap();
            let data = writer.finish().unwrap().into_inner();
            assert_eq!(&data[0..4], b"caff");
            assert_eq!(
                u32::from_be_bytes(data[44..48].try_into().unwrap()),
                channels
            );
            assert_eq!(&data[52..56], b"chan");
            assert_eq!(u32::from_be_bytes(data[64..68].try_into().unwrap()), tag);
            assert_eq!(&data[76..80], b"data");
            assert_eq!(i64::from_be_bytes(data[80..88].try_into().unwrap()), 4);
        }
    }

    #[test]
    fn writes_float32_little_endian_and_preserves_over_one() {
        let cursor = Cursor::new(Vec::new());
        let mut writer = FloatCafWriter::new(cursor, 44_100, CafChannelLayout::Mpeg51A).unwrap();
        writer
            .write_interleaved(&[1.25, -1.5, 0.0, 0.5, -0.25, 2.0])
            .unwrap();
        let data = writer.finish().unwrap().into_inner();

        assert_eq!(u16::from_be_bytes(data[4..6].try_into().unwrap()), 1);
        assert_eq!(&data[8..12], b"desc");
        assert_eq!(i64::from_be_bytes(data[12..20].try_into().unwrap()), 32);
        assert_eq!(
            f64::from_bits(u64::from_be_bytes(data[20..28].try_into().unwrap())),
            44_100.0
        );
        assert_eq!(&data[28..32], b"lpcm");
        assert_eq!(u32::from_be_bytes(data[32..36].try_into().unwrap()), 0x3);
        assert_eq!(i64::from_be_bytes(data[80..88].try_into().unwrap()), 28);
        assert_eq!(u32::from_be_bytes(data[88..92].try_into().unwrap()), 0);
        assert_eq!(&data[92..96], &1.25f32.to_bits().to_le_bytes());
        assert_eq!(&data[112..116], &2.0f32.to_bits().to_le_bytes());
    }

    #[test]
    fn rejects_zero_rate_partial_frames_and_nonfinite_samples() {
        assert!(
            FloatCafWriter::new(Cursor::new(Vec::new()), 0, CafChannelLayout::Mpeg51A).is_err()
        );
        let mut writer =
            FloatCafWriter::new(Cursor::new(Vec::new()), 48_000, CafChannelLayout::Mpeg51A)
                .unwrap();
        assert_eq!(
            writer.write_interleaved(&[0.0; 5]).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            writer
                .write_interleaved(&[0.0, 0.0, 0.0, 0.0, 0.0, f32::NAN])
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn rejects_data_chunk_size_overflow() {
        assert!(checked_data_chunk_size(u64::MAX).is_err());
        assert!(checked_data_chunk_size(i64::MAX as u64).is_err());
    }

    #[derive(Debug)]
    struct FlushFailure(Cursor<Vec<u8>>);

    impl Write for FlushFailure {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            self.0.write(buffer)
        }

        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::other("injected flush failure"))
        }
    }

    impl Seek for FlushFailure {
        fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
            self.0.seek(position)
        }
    }

    #[test]
    fn finish_propagates_io_failure() {
        let writer = FloatCafWriter::new(
            FlushFailure(Cursor::new(Vec::new())),
            48_000,
            CafChannelLayout::Mpeg51A,
        )
        .unwrap();
        assert_eq!(writer.finish().unwrap_err().kind(), io::ErrorKind::Other);
    }

    #[derive(Debug)]
    struct SeekFailure {
        inner: Cursor<Vec<u8>>,
        calls: usize,
        fail_at: usize,
    }

    impl Write for SeekFailure {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            self.inner.write(buffer)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.inner.flush()
        }
    }

    impl Seek for SeekFailure {
        fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
            self.calls = self.calls.saturating_add(1);
            if self.calls == self.fail_at {
                return Err(io::Error::other("injected seek failure"));
            }
            self.inner.seek(position)
        }
    }

    #[test]
    fn finish_propagates_seek_failure() {
        let writer = FloatCafWriter::new(
            SeekFailure {
                inner: Cursor::new(Vec::new()),
                calls: 0,
                // `new` 和 `finish` 各先查询一次当前位置，第三次 seek 是回填 data 大小。
                fail_at: 3,
            },
            48_000,
            CafChannelLayout::Mpeg51A,
        )
        .unwrap();
        assert_eq!(writer.finish().unwrap_err().kind(), io::ErrorKind::Other);
    }
}
