//! 配置指纹、随机访问判定与跨帧状态机。

use super::*;

/// 解码器配置的指纹。
///
/// 两帧的指纹相同，意味着解码器无需重新配置即可继续处理；不同则必须重建
/// 工作区。指纹保守地包含完整配置与 substream 映射，不包含
/// `sequence_counter` 这类逐帧变化的值。
///
/// 这不是规范定义的结构，而是本实现对「配置代次」的表达。故意归一化
/// ndot 和 EMDF 保留填充，并不含 `payload_base` 与各 substream 尺寸：
/// 它们逐帧变化但不影响配置。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConfigFingerprint {
    /// 比特流版本。
    pub bitstream_version: u32,
    /// 采样频率索引。
    pub fs_index: u8,
    /// 帧率索引。
    pub frame_rate_index: u8,
    /// presentation 数。
    pub n_presentations: u32,
    /// substream group 数。
    pub n_groups: u32,
    /// 编码路径。
    pub scene_path: ScenePath,
    /// 对象总数。
    pub total_objects: u32,
    /// 固定配置引用到的 substream 下标跨度。
    ///
    /// 逐帧出现的 EMDF payload substream 不计入；音频、OAMD、HSF 与
    /// presentation substream 的映射仍会进入指纹。
    pub n_substreams: u32,
    pub(super) program_id: Option<ProgramId>,
    pub(super) presentations: [Ac4PresentationV1Info; MAX_PRESENTATIONS],
    pub(super) groups: [Ac4SubstreamGroupInfo; MAX_SUBSTREAM_GROUPS],
}

/// 一帧作为随机访问点的可用程度。
///
/// `b_iframe_global` 单独不足以判定：`TS103190-2:v1.3.1:6.3.2.1.3` 只要求
/// **每个 presentation 的第一个** substream 无时间依赖，而 OAMD、presentation
/// substream 以及 group 内其余 substream 各自还有独立的 ndot 标志
/// （`4.5.2`）。从只满足前者的帧起解，音频可以出声，但对象元数据可能仍在
/// 延续前序帧的状态。
///
/// 注意 `TS103190-1:v1.4.1:4.3.3.2.7` 对同一标志的表述是「所有 presentation
/// 中的所有 substream」，与 Part 2 不一致。本实现按 Part 2 处理，因为它针对
/// `bitstream_version == 2`，也正是本项目覆盖的范围。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RandomAccess {
    /// 全部 substream 均无时间依赖，可从本帧完整重建场景。
    Full,
    /// `b_iframe_global` 为真，但至少一个 substream 依赖前序帧。
    AudioOnly,
    /// 依赖前序帧，不可作为起解点。
    None,
}

impl RandomAccess {
    /// 用于序列化的稳定名称。
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match *self {
            RandomAccess::Full => "full",
            RandomAccess::AudioOnly => "audio_only",
            RandomAccess::None => "none",
        }
    }
}

impl fmt::Display for RandomAccess {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// 解码器重置的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetReason {
    /// 首次观察到可解码的配置。
    Initial,
    /// `sequence_counter` 表明流来源发生变化。
    SourceChange,
    /// 规范化配置指纹发生变化。
    ConfigurationChange,
    /// 上一帧的 TOC 或拓扑解析失败。
    ParseFailure,
    /// 容器或调用方报告了外部不连续。
    ExternalDiscontinuity,
}

/// 观察一帧后解码器应采取的操作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecoderAction {
    /// 配置和流连续，直接继续。
    Continue,
    /// 当前帧是完整随机访问点，应在此重置后起解。
    Reset {
        /// 触发重置的原因。
        reason: ResetReason,
    },
    /// 已需重置，但当前帧不能完整重建场景。
    WaitForRandomAccess {
        /// 触发等待的原因。
        reason: ResetReason,
    },
}

/// 一次帧观察的状态转移结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TopologyTransition {
    /// 当前配置代次，从 1 开始。
    pub generation: u32,
    /// 序列计数器与前一个成功解析帧的关系。
    pub sequence: SequenceTransition,
    /// 本帧是否开启了新的配置代次。
    pub config_changed: bool,
    /// 调用方对当前帧应采取的操作。
    pub action: DecoderAction,
}

/// 跨帧跟踪配置代次、来源变化与随机访问门禁。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TopologyStateMachine {
    fingerprint: Option<ConfigFingerprint>,
    previous_sequence: Option<u16>,
    generation: u32,
    pending_reset: Option<ResetReason>,
}

impl Default for TopologyStateMachine {
    fn default() -> Self {
        Self::new()
    }
}

impl TopologyStateMachine {
    /// 创建未观察任何帧的状态机。
    #[must_use]
    pub const fn new() -> Self {
        Self {
            fingerprint: None,
            previous_sequence: None,
            generation: 0,
            pending_reset: None,
        }
    }

    /// 观察一个已成功解析的帧。
    #[must_use]
    pub fn observe(&mut self, topology: &Ac4Topology) -> TopologyTransition {
        let sequence = topology.toc.sequence_transition(self.previous_sequence);
        self.previous_sequence = Some(topology.toc.sequence_counter);

        let fingerprint = topology.config_fingerprint();
        let config_changed = self.fingerprint != Some(fingerprint);
        if config_changed {
            let reason = if self.fingerprint.is_none() {
                ResetReason::Initial
            } else {
                ResetReason::ConfigurationChange
            };
            self.fingerprint = Some(fingerprint);
            self.generation = self.generation.saturating_add(1);
            self.pending_reset = Some(reason);
        } else if sequence == SequenceTransition::SourceChange && self.pending_reset.is_none() {
            self.pending_reset = Some(ResetReason::SourceChange);
        }

        let action = if let Some(reason) = self.pending_reset {
            if topology.random_access() == RandomAccess::Full {
                self.pending_reset = None;
                DecoderAction::Reset { reason }
            } else {
                DecoderAction::WaitForRandomAccess { reason }
            }
        } else {
            DecoderAction::Continue
        };

        TopologyTransition {
            generation: self.generation,
            sequence,
            config_changed,
            action,
        }
    }

    /// 报告解析失败或容器级不连续，并等待下一个完整随机访问点。
    pub fn mark_discontinuity(&mut self, reason: ResetReason) {
        self.pending_reset = Some(reason);
        self.previous_sequence = None;
    }

    /// 当前配置代次；尚未观察到成功解析帧时为 0。
    #[must_use]
    pub const fn generation(&self) -> u32 {
        self.generation
    }

    /// 是否已经需要重置，但仍在等待完整随机访问点。
    #[must_use]
    pub const fn is_waiting_for_random_access(&self) -> bool {
        self.pending_reset.is_some()
    }
}

/// 单个 presentation 可引用的 group 数上限对外可见，便于调用方预估容量。
pub use crate::presentation::MAX_GROUPS_PER_PRESENTATION as MAX_PRESENTATION_GROUPS;

const _: () = assert!(MAX_GROUPS_PER_PRESENTATION <= MAX_SUBSTREAM_GROUPS);
const _: () = assert!(MAX_SUBSTREAMS <= 32);
