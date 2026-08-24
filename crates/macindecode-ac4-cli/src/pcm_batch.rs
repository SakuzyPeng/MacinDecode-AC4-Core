//! Scene Session 借用 PCM 到 artifact writer 所需 owned 轨道的批量适配。
//!
//! 公共 SceneFrame 的 PCM 只借用到下一次 Session 可变调用；CLI 要跨 AU 应用
//! MP4 edit 并原子写出制品，因此必须拥有一份整批样本。这里仅保存这种制品级
//! 所有权，不复制 Scene 元数据模型。Scene 出口的 normalized `f32` 在采集时按
//! 精确的 `2^15` 恢复为既有诊断 WAVE 使用的 `±32 768` 量级。

use macindecode_ac4_scene::SceneElementId;

/// 一层 A-JOC 音频链在呈现时间线上的 owned PCM。
#[derive(Debug)]
pub(crate) struct PcmBatch {
    pub(crate) sample_rate: u32,
    /// 按最终 WAVE/CAF 交织顺序排列的轨道。
    pub(crate) tracks: Vec<PcmTrack>,
}

/// 一条制品 PCM 轨道。
#[derive(Debug)]
pub(crate) struct PcmTrack {
    pub(crate) substream_index: u32,
    /// 在当前批次交织输出中的零基位置，始终与 `tracks` 下标一致。
    pub(crate) output_index: usize,
    /// Scene 产生的轨道保留配置代次内稳定的元素 ID；传输侧诊断轨没有 Scene 元素。
    pub(crate) scene_element_id: Option<SceneElementId>,
    pub(crate) source: PcmTrackSource,
    pub(crate) samples: Vec<f32>,
}

/// 一条 PCM 轨道在对应解码阶段的来源身份。
///
/// 来源自身携带命名空间内的下标；`PcmTrack::output_index` 只表达制品交织位置，
/// 不再同时充当 transport channel、A-JOC input 或重建对象下标。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PcmTrackSource {
    TransportChannel {
        element_index: usize,
        channel_index: usize,
    },
    AjocInput {
        input_index: usize,
    },
    AjocObject {
        object_index: usize,
    },
    Lfe,
}
