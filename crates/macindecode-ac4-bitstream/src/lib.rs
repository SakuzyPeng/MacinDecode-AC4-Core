//! AC-4 比特流解析与音频重建原语。
//!
//! 规范基线 `TS103190:2025-07`，见 `docs/SPEC_TRACEABILITY.md`。
//!
//! 本 crate 负责解析，并提供音频重建原语；它不依赖容器或平台。所有解析入口
//! 都接受有限切片，不会越过传入数据的边界寻找内容。
//! OAMD、substream 与 topology 领域类型分别只以 [`oamd`]、[`substream`]、
//! [`topology`] 为规范公共入口；crate 根不重复批量重导出这些类型。
//!
//! `spec-tables` 消费用户从官方 ETSI PDF 本地生成的静态表；`audio-decode` 在其
//! 基础上再消费规范随附 C 表，启用完整音频重建路径。两类表值均不随仓库或
//! crate 分发，准备方法见 crate README。

#![no_std]

#[cfg(feature = "audio-decode")]
extern crate alloc;

#[cfg(feature = "spec-tables")]
#[allow(
    dead_code,
    reason = "生成文件同时覆盖 spec-tables 与 audio-decode 的规范表"
)]
pub(crate) mod spec_tables {
    include!(concat!(env!("OUT_DIR"), "/ts103190_pdf_tables.rs"));
}

pub mod ajoc;
#[cfg(feature = "audio-decode")]
pub use ajoc::de as ajoc_de;
pub mod asf;
pub mod aspx;
#[cfg(feature = "audio-decode")]
pub mod audio_data;
pub mod audio_substream;
#[cfg(feature = "audio-decode")]
pub mod channel;
#[cfg(feature = "audio-decode")]
pub mod element_drive;
pub mod emdf;
#[cfg(feature = "audio-decode")]
pub mod frame_alignment;
#[cfg(feature = "audio-decode")]
pub mod full_ajoc;
#[cfg(feature = "audio-decode")]
pub mod huffman;
pub mod math;
pub mod oamd;
pub mod presentation;
pub mod presentation_substream;
pub mod reader;
pub mod substream;
#[cfg(feature = "audio-decode")]
pub mod substream_audio;
pub mod syncframe;
#[cfg(all(test, feature = "audio-decode"))]
mod testutil;
pub mod toc;
pub mod topology;
#[cfg(feature = "audio-decode")]
pub mod var_element;

pub use audio_substream::{
    Ac4AudioSubstream, AudioSubstreamError, BasicMetadata, ExtendedMetadata, FurtherLoudnessInfo,
    LoudnessExtensionBits, LoudnessProgrammeBoundary, SubstreamContext,
};
pub use emdf::{EmdfInfo, EmdfPayloadsSubstream};
#[cfg(feature = "audio-decode")]
pub use huffman::{HuffmanError, HuffmanTable};
pub use presentation::{Ac4PresentationV1Info, PresentationSubstreamInfo};
pub use presentation_substream::{
    Ac4PresentationSubstream, Ac4PresentationSubstreamSelection, AdvancedDeConfig, AdvancedDeData,
    AlternativePresentationSelection, PresentationAddDataBits, PresentationAdditionalData,
    PresentationDrcFrameBits, PresentationSubstreamContext, PresentationSubstreamError,
    PresentationSubstreamSelectionContext,
};
pub use reader::{BitReader, ReadError};
#[cfg(feature = "audio-decode")]
pub use substream_audio::{
    Ac4SubstreamAjoc, AjocAudioWorkspace, AjocSubstreamContext, SubstreamAudioError,
    parse_substream_ajoc,
};
pub use syncframe::{SyncFrame, SyncFrameError, SyncFrameIter, SyncWord};
pub use toc::{Ac4Toc, DecodingDelay, SequenceTransition, TocError};
