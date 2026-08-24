//! bitstream Full A-JOC 帧级 decoder 的 blocker trace 映射。

use super::{AjocTrace, AspxBlocker, SupportedAspxFrame};

impl AjocTrace {
    /// 提交 bitstream 门禁结果，并产出 decoder 唯一接受的凭证。
    pub(in crate::trace) fn commit_aspx_support(
        &mut self,
        support: Result<SupportedAspxFrame, AspxBlocker>,
        substream_index: u32,
        frame_index: u32,
    ) -> Option<SupportedAspxFrame> {
        match support {
            Ok(supported) => Some(supported),
            Err(blocker) => {
                let error = format!("substream {substream_index}: {}", blocker.detail());
                if self.aspx_unsupported_first_error.is_none() {
                    self.aspx_unsupported_first_error =
                        Some(format!("Frame {frame_index}: {error}"));
                }
                self.fail_aspx(substream_index, frame_index, error);
                None
            }
        }
    }

    /// 让该 substream 的 engine 历史失效，并计入 A-SPX 不变量。
    pub(in crate::trace) fn fail_aspx(
        &mut self,
        substream_index: u32,
        frame_index: u32,
        error: String,
    ) {
        if let Some(decoder) = self.full_decoder.as_mut() {
            decoder.reset_substream(substream_index);
        }
        self.aspx_failures = self.aspx_failures.saturating_add(1);
        if self.aspx_first_error.is_none() {
            self.aspx_first_error = Some(format!("Frame {frame_index}: {error}"));
        }
    }
}
