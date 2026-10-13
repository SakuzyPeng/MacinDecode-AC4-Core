//! 可公开的源码夹具；只构造语法，不用被测解码器计算预期 PCM。
//! TS103190-2:v1.3.1:6.2.4、6.2.5；TS103190-1:v1.4.1:4.2.8、4.2.12。

use macindecode_ac4_bitstream::{
    Ac4Toc,
    reader::BitReader,
    substream::{Ac4SubstreamGroupInfo, SubstreamInfo},
};
use macindecode_ac4_decode::{
    AjocSubstreamContext, HuffmanError, HuffmanTable,
    ajoc::{
        MatrixKind,
        syntax::{AjocHcbType, table_for as ajoc_table},
    },
    asf::sfb_offsets_48,
    aspx::{
        bands::AspxBandTables,
        codebooks::table_for,
        tables::{EnvelopeKind, HcbType, StereoMode, get_aspx_hcb},
    },
    huffman::tables::{ASF_HCB_1, ASF_HCB_SCALEFAC},
};
use std::sync::{Mutex, OnceLock};

// 生产 API 不暴露编码表；只在测试中枚举合法前缀，避免复制受限 C 表或增加公共 API。
// 输入流自身也冻结 SHA-256，码本变化不能和 PCM 一起静默漂移。
fn codeword(table: &'static HuffmanTable, symbol: u16) -> (u32, u32) {
    type Entry = (&'static HuffmanTable, u16, (u32, u32));
    static CACHE: OnceLock<Mutex<Vec<Entry>>> = OnceLock::new();
    let mut cache = CACHE.get_or_init(|| Mutex::new(Vec::new())).lock().unwrap();
    if let Some((_, _, word)) = cache
        .iter()
        .find(|(t, s, _)| core::ptr::eq(*t, table) && *s == symbol)
    {
        return *word;
    }
    fn search(table: &HuffmanTable, symbol: u16, code: u32, width: u32) -> Option<(u32, u32)> {
        let bytes = code.checked_shl(32 - width).unwrap_or(0).to_be_bytes();
        let mut reader = BitReader::new_bounded(&bytes, 0, u64::from(width)).unwrap();
        match table.decode(&mut reader) {
            Ok(actual) => (actual == symbol).then_some((code, width)),
            Err(HuffmanError::Read(_)) if width < 32 => search(table, symbol, code << 1, width + 1)
                .or_else(|| search(table, symbol, (code << 1) | 1, width + 1)),
            other => panic!("无效 Huffman 前缀：{other:?}"),
        }
    }
    assert!(usize::from(symbol) < table.len());
    let word = search(table, symbol, 0, 0).expect("完整码本必须含有目标符号");
    cache.push((table, symbol, word));
    word
}

pub const FRAME_SAMPLES: usize = 1920;
pub const FRAMES: usize = 8;

#[derive(Clone, Copy, Debug)]
pub struct Case {
    pub name: &'static str,
    pub objects: usize,
    pub lfe: bool,
    pub wet: bool,
    pub tna: u32,
    pub noise: u16,
}

pub const CASES: [Case; 3] = [
    Case {
        name: "odd_dry",
        objects: 3,
        lfe: false,
        wet: false,
        tna: 3,
        noise: 12,
    },
    Case {
        name: "odd_wet",
        objects: 3,
        lfe: false,
        wet: true,
        tna: 3,
        noise: 12,
    },
    Case {
        name: "even_wet_lfe",
        objects: 2,
        lfe: true,
        wet: true,
        tna: 3,
        noise: 12,
    },
];

#[derive(Default)]
struct Bits {
    bytes: Vec<u8>,
    len: usize,
}

impl Bits {
    fn bit(&mut self, value: bool) {
        if self.len.is_multiple_of(8) {
            self.bytes.push(0);
        }
        if value {
            self.bytes[self.len / 8] |= 1 << (7 - self.len % 8);
        }
        self.len += 1;
    }
    fn bits(&mut self, value: u32, width: u32) {
        assert!(width == 32 || value < (1 << width));
        for bit in (0..width).rev() {
            self.bit(value & (1 << bit) != 0);
        }
    }
    fn text(&mut self, text: &str) {
        for c in text.chars().filter(|c| !c.is_whitespace()) {
            assert!(matches!(c, '0' | '1'));
            self.bit(c == '1');
        }
    }
    fn align(&mut self) {
        while !self.len.is_multiple_of(8) {
            self.bit(false);
        }
    }
    fn bytes(&mut self, values: &[u8]) {
        for &value in values {
            self.bits(u32::from(value), 8);
        }
    }
    fn symbol(&mut self, table: &'static HuffmanTable, symbol: u16) {
        let (code, width) = codeword(table, symbol);
        self.bits(code, width);
    }
    fn spectrum(&mut self, lfe: bool, frame: usize) {
        let bands = if lfe { 2 } else { 16 };
        if lfe {
            self.bits(bands, 3);
        } else {
            self.bit(false);
            self.bit(true);
            self.bits(bands, 6);
        }
        self.bits(1, 4); // ASF codebook 1，四元有符号谱线
        self.bits(bands - 1, 5);
        let end = usize::from(sfb_offsets_48(1920).unwrap()[bands as usize]);
        assert_eq!(end % 4, 0);
        for line in 0..end / 4 {
            self.symbol(
                &ASF_HCB_1,
                if (line + frame).is_multiple_of(3) {
                    80
                } else {
                    0
                },
            );
        }
        self.bits(if lfe { 104 } else { 100 + (frame % 3) as u32 }, 8);
        for _ in 1..bands {
            self.symbol(&ASF_HCB_SCALEFAC, 60);
        }
        self.bit(false); // b_snf_data_exists
    }
    fn aspx(&mut self, iframe: bool, tna: u32, noise: u16) {
        let bands = AspxBandTables::derive(true, 0, 0, 1, 0).unwrap();
        if iframe {
            self.bits(0, 3);
        }
        self.text("0 0 0 0"); // FIXFIX，单包络，signal/noise 均走 DF
        for _ in 0..bands.num_sbg_noise() {
            self.bits(tna, 2);
        }
        self.text("0 0 0"); // 无额外谐波、FIC、TIC
        for (kind, count, level) in [
            (EnvelopeKind::Signal, bands.num_sbg_sig_highres(), 8),
            (EnvelopeKind::Noise, bands.num_sbg_noise(), noise),
        ] {
            let f0 = table_for(get_aspx_hcb(kind, StereoMode::Level, false, HcbType::F0));
            let df = table_for(get_aspx_hcb(kind, StereoMode::Level, false, HcbType::Df));
            self.symbol(f0, level);
            for _ in 1..count {
                self.symbol(df, ((df.len() - 1) / 2) as u16);
            }
        }
    }
    fn timing(&mut self) {
        self.text("0 001 000000 00");
    }
    fn inactive_objects(&mut self, count: usize) {
        for _ in 0..count {
            self.text("1 0");
        }
    }
    fn ajoc(&mut self, case: Case, frame: usize) {
        self.bits(u32::from(case.wet), 3);
        if case.wet {
            self.bit(true);
        }
        for _ in 0..case.objects {
            self.bit(true);
        }
        self.text("01 00000 000011"); // 单数据点，四时隙 ramp
        for _ in 0..case.objects {
            self.text("110 1 0");
        } // 三频带，coarse，dense
        self.bit(true); // b_nodt：首个数据点无方向标志
        for object in 0..case.objects {
            for kind in [MatrixKind::Dry, MatrixKind::Wet] {
                if kind == MatrixKind::Wet && !case.wet {
                    continue;
                }
                let code = if kind == MatrixKind::Dry { 29 } else { 12 };
                self.symbol(
                    ajoc_table(kind, true, AjocHcbType::F0),
                    code + object as u16 + (frame % 2) as u16,
                );
                for _ in 1..3 {
                    self.symbol(ajoc_table(kind, true, AjocHcbType::Df), 0);
                }
            }
        }
    }
}

pub struct Frame {
    pub raw: Vec<u8>,
    pub payload: Vec<u8>,
    pub context: AjocSubstreamContext,
}

pub fn frame(case: Case, index: usize) -> Frame {
    frame_with_spectrum(case, index, index)
}

pub fn frame_with_spectrum(case: Case, index: usize, spectrum_index: usize) -> Frame {
    let iframe = index == 0;
    let mut audio = Bits::default();
    audio.text("0 1"); // 全部下混信号活动，A-SPX
    if iframe {
        audio.text("0 000 00 1 0 0 0 01 0 11");
    }
    audio.text("0 0"); // companding off，包括 average
    if case.lfe {
        audio.spectrum(true, spectrum_index);
    }
    audio.spectrum(false, spectrum_index);
    audio.aspx(iframe, case.tna, case.noise);
    audio.bit(true);
    audio.timing();
    audio.inactive_objects(1 + usize::from(case.lfe));
    audio.bit(false); // OAMD extension absent
    audio.ajoc(case, index);
    audio.text("1 1 00"); // DE 配置，保持系数，无主对话对象
    for _ in 0..case.objects {
        audio.bit(false);
    }
    audio.text("0 1"); // 上混时间继承下混
    audio.inactive_objects(case.objects + usize::from(case.lfe));
    audio.align();
    let mut payload = Bits::default();
    payload.bits(audio.bytes.len() as u32, 15);
    payload.bit(false);
    payload.bytes(&audio.bytes);
    payload.text("0 0 0 0 0000001 0 0 0"); // basic/tools metadata
    payload.align();

    let mut group = Bits::default();
    group.text("1 0 1 0 0 1");
    group.bit(case.lfe);
    group.text("0 0000 1 0"); // dynamic downmix，单个 fullband 输入
    group.bits((case.objects - 1) as u32, 4);
    group.text("1 0 0"); // dynamic upmix，无倍频或 bitrate info
    group.bit(iframe);
    group.text("01 0"); // audio substream index 1，无 content type
    let info = Ac4SubstreamGroupInfo::parse(&mut BitReader::new(&group.bytes), 2, 1, 1).unwrap();
    let SubstreamInfo::Ajoc(info) = info.substreams()[0] else {
        panic!("应为 A-JOC");
    };
    let mut raw = Bits::default();
    raw.text("10");
    raw.bits(index as u32, 10);
    raw.text("0 1 0001");
    raw.bit(iframe);
    raw.text("1 0 0");
    raw.text("1 0 000 0 0 00 000 0 00 00 0 000 0 0 0 1 00");
    for i in 0..group.len {
        raw.bit(group.bytes[i / 8] & (1 << (7 - i % 8)) != 0);
    }
    raw.text("10 0 0000000011 0");
    raw.bits(payload.bytes.len() as u32, 10);
    raw.align();
    raw.bytes(&[0x55, 0x04, 0x00]);
    raw.bytes(&payload.bytes);
    let toc = Ac4Toc::parse(&raw.bytes).unwrap();
    let context = AjocSubstreamContext::derive(&toc, &info, 1, 1, false, Some(1)).unwrap();
    Frame {
        raw: raw.bytes,
        payload: payload.bytes,
        context,
    }
}

pub fn sync_stream(case: Case) -> Vec<u8> {
    let mut bytes = Vec::new();
    for index in 0..FRAMES {
        let raw = frame(case, index).raw;
        bytes.extend_from_slice(&[0xac, 0x40]);
        bytes.extend_from_slice(&u16::try_from(raw.len()).unwrap().to_be_bytes());
        bytes.extend_from_slice(&raw);
    }
    bytes
}
