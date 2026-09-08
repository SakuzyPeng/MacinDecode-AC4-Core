//! Presentation 解析历史与借用侧车。
use alloc::vec::Vec;
use macindecode_ac4_bitstream::{
    Ac4PresentationSubstream, PresentationDrcConfiguration, PresentationDrcState,
    PresentationSubstreamContext, PresentationSubstreamError, PresentationSubstreamGroupGainCodes,
    PresentationSubstreamGroupGainState,
};

/// 所选 presentation 当前 AU 的完整 processing-metadata 侧车。
///
/// 该视图只观察码流原值，不执行 DRC、gain、pan、downmix、loudness correction 或任何
/// renderer 处理。`payload` 借用 Session 自持的已验证副本，因此与
/// `MetadataAccessUnit` 一样只在下一次 Session 可变调用前有效。
#[derive(Debug, Clone, Copy)]
pub struct PresentationSubstreamMetadata<'a> {
    payload: &'a [u8],
    record: PresentationSubstreamMetadataRecord,
}

impl<'a> PresentationSubstreamMetadata<'a> {
    /// 供已经验证该 payload/record 的共享会话适配器恢复视图。
    pub fn from_record(payload: &'a [u8], record: PresentationSubstreamMetadataRecord) -> Self {
        Self { payload, record }
    }
    /// 产生该 metadata 的 AU 下标。
    #[must_use]
    pub const fn access_unit_index(self) -> u64 {
        self.record.access_unit_index
    }

    /// Session 已选择的零基 presentation 下标。
    #[must_use]
    pub const fn presentation_index(self) -> u32 {
        self.record.presentation_index
    }

    /// 所选 presentation 的码流标识；码流未声明时为 `None`。
    #[must_use]
    pub const fn presentation_id(self) -> Option<u32> {
        self.record.presentation_id
    }

    /// 该 presentation substream 在 TOC index table 中的物理下标。
    #[must_use]
    pub const fn substream_index(self) -> u32 {
        self.record.substream_index
    }

    /// 从同一 AU 拓扑派生、且已经用于验证 payload 的完整解析上下文。
    #[must_use]
    pub const fn context(self) -> PresentationSubstreamContext {
        self.record.context
    }

    /// Session 自持的完整有界 presentation substream payload。
    #[must_use]
    pub const fn payload(self) -> &'a [u8] {
        self.payload
    }

    /// 已按 `TS103190-2:v1.3.1:6.2.2.3` 的 `ac4_presentation_substream()` 语法严格验证的
    /// payload 前缀。
    ///
    /// 通常与 [`payload`](Self::payload) 相同。已观测的 object/A-JOC 独立 DRC 帧会在规范
    /// 末尾之后额外携带一个 `0x00`、`0x80` 或 `0xd8`；会话为回放互操作性只接受这三种精确形态，
    /// 并把该字节排除在本切片之外。
    #[must_use]
    pub fn syntax_payload(self) -> &'a [u8] {
        self.payload
            .get(..self.record.syntax_payload_len)
            .unwrap_or(&[])
    }

    /// 规范语法之后、为已知回放兼容形态保留的原始尾部。
    ///
    /// 当前返回值只可能为空、单字节 `[0x00]`、`[0x80]` 或 `[0xd8]`。该字节没有被赋予 AC-4
    /// processing 语义，调用方不得把它解释为 DRC、gain 或 loudness-correction 字段。
    #[must_use]
    pub fn compatibility_tail(self) -> &'a [u8] {
        self.payload
            .get(self.record.syntax_payload_len..)
            .unwrap_or(&[])
    }

    /// 当前 AU 成功提交后有效的 DRC 配置。
    ///
    /// 这与 [`parsed_substream`](Self::parsed_substream) 中“本帧是否传输配置”保持区分：
    /// dependent frame 可以不重新传配置，但仍使用前一有效配置解析当前 DRC data。
    #[must_use]
    pub const fn effective_drc_configuration(self) -> Option<PresentationDrcConfiguration> {
        self.record.effective_drc_configuration
    }

    /// 当前 AU 生效的逐 substream-group 六比特 gain 码值。
    ///
    /// 返回值仍是原始码值，不换算为 dB，也不应用到 PCM。当前帧的 absent/keep/new 传输形态
    /// 保留在 [`Ac4PresentationSubstream::substream_group_gain_update`] 中。
    #[must_use]
    pub const fn effective_group_gain_codes(self) -> PresentationSubstreamGroupGainCodes {
        self.record.effective_group_gain_codes
    }

    /// 从 Session 自持 payload 重建完整、借用的 presentation metadata 视图。
    ///
    /// Session 在发布本侧车前已经用相同上下文和 DRC 起始状态完成过一次严格验证；这里对
    /// [`syntax_payload`](Self::syntax_payload) 重放 parser，是为了在禁止自引用存储的安全 Rust
    /// 中恢复所有 opaque bit view。已知的非规范兼容尾部不参与语法解析。
    ///
    /// # Errors
    ///
    /// 若 Session 自持数据不再满足先前验证过的不变量，则返回底层解析错误；正常使用中不应
    /// 出现该情况。
    pub fn parsed_substream(
        self,
    ) -> Result<Ac4PresentationSubstream<'a>, PresentationSubstreamError> {
        let mut drc_state = self.record.replay_drc_state;
        Ac4PresentationSubstream::parse_with_drc_state(
            self.syntax_payload(),
            self.record.context,
            &mut drc_state,
        )
    }
}

#[derive(Debug, Clone, Copy)]
pub struct PresentationSubstreamMetadataRecord {
    pub access_unit_index: u64,
    pub presentation_index: u32,
    pub presentation_id: Option<u32>,
    pub substream_index: u32,
    pub context: PresentationSubstreamContext,
    pub syntax_payload_len: usize,
    pub replay_drc_state: PresentationDrcState,
    pub effective_drc_configuration: Option<PresentationDrcConfiguration>,
    pub effective_group_gain_codes: PresentationSubstreamGroupGainCodes,
}

/// Session 拥有的 presentation payload 副本与当前可见记录。
#[derive(Debug, Default)]
pub struct PresentationSubstreamMetadataStorage {
    payload: Vec<u8>,
    record: Option<PresentationSubstreamMetadataRecord>,
}

impl PresentationSubstreamMetadataStorage {
    pub fn clear(&mut self) {
        self.payload.clear();
        self.record = None;
    }

    pub fn try_reserve_payload(
        &mut self,
        payload_len: usize,
    ) -> Result<(), alloc::collections::TryReserveError> {
        let additional = payload_len.saturating_sub(self.payload.len());
        self.payload.try_reserve(additional)
    }

    pub fn publish(&mut self, payload: &[u8], record: PresentationSubstreamMetadataRecord) {
        self.payload.clear();
        self.payload.extend_from_slice(payload);
        self.record = Some(record);
    }

    pub fn view(&self) -> Option<PresentationSubstreamMetadata<'_>> {
        Some(PresentationSubstreamMetadata {
            payload: &self.payload,
            record: self.record?,
        })
    }
}

#[derive(Debug, Default)]
pub struct PresentationMetadataState {
    pub drc: PresentationDrcState,
    pub group_gain: PresentationSubstreamGroupGainState,
    pub storage: PresentationSubstreamMetadataStorage,
}
impl PresentationMetadataState {
    pub fn clear_view(&mut self) {
        self.storage.clear();
    }
    pub fn reset(&mut self) {
        self.drc.reset();
        self.group_gain.reset();
        self.storage.clear();
    }
    /// 在状态副本上验证完整 payload；失败不提交 DRC 或 group-gain 历史。
    pub fn prepare<'a>(
        &self,
        payload: &'a [u8],
        context: PresentationSubstreamContext,
        source: crate::MetadataErrorContext,
        reset: bool,
    ) -> Result<PreparedPresentationMetadata<'a>, crate::MetadataError> {
        let replay = if reset {
            PresentationDrcState::new()
        } else {
            self.drc
        };
        let mut drc = replay;
        let (parsed, syntax_payload_len) =
            Ac4PresentationSubstream::parse_with_drc_state_compat(payload, context, &mut drc)
                .map_err(|e| {
                    crate::MetadataError::new(crate::MetadataErrorKind::Presentation(e), source)
                })?;
        let mut gains = if reset {
            PresentationSubstreamGroupGainState::new()
        } else {
            self.group_gain
        };
        let effective = gains
            .apply(parsed.substream_group_gain_update, context)
            .map_err(|e| {
                crate::MetadataError::new(crate::MetadataErrorKind::GroupGain(e), source)
            })?;
        Ok(PreparedPresentationMetadata {
            payload,
            record: PresentationSubstreamMetadataRecord {
                access_unit_index: source.access_unit_index,
                presentation_index: source.presentation_index.unwrap_or(0),
                presentation_id: source.presentation_id,
                substream_index: source.substream_index.unwrap_or(0),
                context,
                syntax_payload_len,
                replay_drc_state: replay,
                effective_drc_configuration: drc.configuration(),
                effective_group_gain_codes: effective,
            },
            next_drc_state: drc,
            next_group_gain_state: gains,
        })
    }
    pub fn commit(&mut self, prepared: PreparedPresentationMetadata<'_>) {
        self.drc = prepared.next_drc_state;
        self.group_gain = prepared.next_group_gain_state;
        self.storage.publish(prepared.payload, prepared.record);
    }
}

#[derive(Debug, Clone, Copy)]
pub struct PreparedPresentationMetadata<'a> {
    pub payload: &'a [u8],
    pub record: PresentationSubstreamMetadataRecord,
    pub next_drc_state: PresentationDrcState,
    pub next_group_gain_state: PresentationSubstreamGroupGainState,
}
