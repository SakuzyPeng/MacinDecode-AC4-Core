//! AC-4 在 ISO base media file format 中的承载。
//!
//! 对应 `TS103190-1:v1.4.1:Annex E`。
//!
//! 本 crate 只负责 access unit 定界与时间线，不解释音频工具语义。它向上
//! 提供每个 sample 的字节范围与时间信息，音频语义由比特流层处理。

#![no_std]

pub mod boxes;
pub mod dsi;
pub mod samples;
pub mod timeline;

pub use boxes::{BoxError, BoxIter, Mp4Box, find_box, find_path};
pub use dsi::{
    Ac4BitrateDsi, Ac4BitrateMode, Ac4Dsi, Ac4DsiPresentation, Ac4DsiPresentationIter, Ac4DsiV1,
    Ac4ProgramId, BaseSamplingFrequency, DsiError, FrameRate, MediaTimeline, SampleDelta,
};
pub use samples::{SampleInfo, SampleIter, SampleTable, SampleTableError};
pub use timeline::{
    EditListEntry, HeaderTiming, PresentationTiming, TimelineError, media_time_to_presentation,
    parse_edit_list, parse_header_timing, presentation_timing,
};
