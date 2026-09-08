//! 将已解码帧的内容策略交给调用方音频调度器。
//! 调用方先根据 frame.diagnostics() 处理 reset/discontinuity，并自行选择未指定策略的默认值。

use macindecode_ac4_scene::{Ac4SceneFrame, HeadphonePolicyState, MetadataFields, SceneElementId};

/// `schedule` 的 offset 相对本帧 PCM；不得在 decode 时直接提前渲染未来事件。
pub fn schedule_headphone_policies(
    frame: Ac4SceneFrame<'_>,
    mut schedule: impl FnMut(SceneElementId, u32, HeadphonePolicyState),
) {
    for object in frame.objects() {
        if let Some(state) = object.initial_state() {
            schedule(object.element_id(), 0, state.headphone_policy());
        }
    }
    for bed in frame.beds() {
        if let Some(state) = bed.initial_state() {
            schedule(bed.element_id(), 0, state.headphone_policy());
        }
    }
    for update in frame.metadata_updates() {
        // offset 0 已合并到起点快照，不重复调度。
        if update.offset_samples() != 0
            && update
                .changed_fields()
                .contains(MetadataFields::HEADPHONE_POLICY)
        {
            schedule(
                update.element_id(),
                update.offset_samples(),
                update.state().headphone_policy(),
            );
        }
    }
}

fn main() {
    println!(
        "将 Ac4DecoderSession 输出的帧传给 schedule_headphone_policies，并提供自己的音频调度回调。"
    );
}
