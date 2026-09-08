//! AC-4 源 AU 元数据会话；无 PCM、播放对齐或 presentation processing。
#![no_std]
extern crate alloc;
mod audio;
mod error;
pub use audio::{AudioMetadataState, PreparedAudioMetadata};
#[cfg(feature = "metadata-decode")]
mod extended;
pub mod group_oamd;
mod input;
mod oamd;
pub mod presentation;
pub mod selection;
mod session;
mod state;
#[cfg(feature = "metadata-decode")]
pub use extended::{AjocMetadataRecord, DeMetadataRecord, DrcMetadataRecord};
pub mod layout;
pub use error::{MetadataError, MetadataErrorContext, MetadataErrorKind};
pub use input::{AccessUnit, AccessUnitContext, PresentationSelection};
pub use presentation::PresentationSubstreamMetadata;
pub use session::*;
pub use state::*;
