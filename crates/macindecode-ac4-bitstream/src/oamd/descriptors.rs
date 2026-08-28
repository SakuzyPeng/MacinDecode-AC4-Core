//! TOC 对象 assignment 到 OAMD 描述符的适配。

use super::*;

/// 逐对象描述的固定容量数组。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObjectDescriptors {
    items: [ObjectDescriptor; MAX_OAMD_OBJECTS],
    written: usize,
}

impl ObjectDescriptors {
    /// 已填充的对象描述。
    #[must_use]
    pub fn as_slice(&self) -> &[ObjectDescriptor] {
        self.items.get(..self.written).unwrap_or(&[])
    }

    /// 复制一组调用方已经按码流顺序排列的对象描述。
    ///
    /// # Errors
    ///
    /// 对象数超过 [`MAX_OAMD_OBJECTS`] 时返回 [`OamdError::TooManyObjects`]。
    pub fn try_from_slice(objects: &[ObjectDescriptor]) -> Result<Self, OamdError> {
        let mut out = Self::empty();
        for &object in objects {
            out.push(object)?;
        }
        Ok(out)
    }

    pub(super) fn empty() -> Self {
        Self {
            items: [ObjectDescriptor {
                obj_type: ObjectType::Dynamic,
                b_lfe: false,
                b_ajoc_coded: false,
            }; MAX_OAMD_OBJECTS],
            written: 0,
        }
    }

    pub(super) fn push(&mut self, item: ObjectDescriptor) -> Result<(), OamdError> {
        let slot = self
            .items
            .get_mut(self.written)
            .ok_or(OamdError::TooManyObjects {
                limit: MAX_OAMD_OBJECTS,
            })?;
        *slot = item;
        self.written = self.written.saturating_add(1);
        Ok(())
    }

    /// 由一侧 A-JOC 对象分配构造按音频信号顺序排列的描述符。
    ///
    /// 调用方可分别传入 `SubstreamInfoAjoc::dmx_assignment` 与
    /// `SubstreamInfoAjoc::upmix_assignment`，为 `audio_data_ajoc()` 构造 core/full
    /// 两套描述符。按 `6.2.3.4`，存在 LFE 时它固定占据索引 0。
    ///
    /// # Errors
    ///
    /// 对象数超过 [`MAX_OAMD_OBJECTS`] 时返回 [`OamdError::TooManyObjects`]。
    pub fn from_ajoc_assignment(
        assignment: crate::substream::ObjectAssignment,
        b_lfe: bool,
    ) -> Result<Self, OamdError> {
        let mut out = Self::empty();
        out.append_ajoc_assignment(assignment, b_lfe)?;
        Ok(out)
    }

    /// 由一个 direct-object audio substream 的 info 元素推导对象描述。
    ///
    /// 与 [`Self::from_group`] 不同，这里只返回该物理 audio substream 内的对象；
    /// `metadata()` 中的 `oamd_dyndata_single()` 正是以这组局部顺序为上下文。
    ///
    /// # Errors
    ///
    /// 对象数超过 [`MAX_OAMD_OBJECTS`] 时返回 [`OamdError::TooManyObjects`]。
    pub fn from_object_substream(
        info: &crate::substream::SubstreamInfoObj,
    ) -> Result<Self, OamdError> {
        let mut out = Self::empty();
        out.append_object_substream(info)?;
        Ok(out)
    }

    pub(super) fn append_ajoc_assignment(
        &mut self,
        assignment: crate::substream::ObjectAssignment,
        b_lfe: bool,
    ) -> Result<(), OamdError> {
        if b_lfe {
            self.push(ObjectDescriptor {
                obj_type: ObjectType::Bed,
                b_lfe: true,
                b_ajoc_coded: true,
            })?;
        }
        for _ in 0..assignment.n_bed {
            self.push(ObjectDescriptor {
                obj_type: ObjectType::Bed,
                b_lfe: false,
                b_ajoc_coded: true,
            })?;
        }
        for _ in 0..assignment.n_isf {
            self.push(ObjectDescriptor {
                obj_type: ObjectType::Isf,
                b_lfe: false,
                b_ajoc_coded: true,
            })?;
        }
        for _ in 0..assignment.n_dynamic() {
            self.push(ObjectDescriptor {
                obj_type: ObjectType::Dynamic,
                b_lfe: false,
                b_ajoc_coded: true,
            })?;
        }
        Ok(())
    }

    fn append_object_substream(
        &mut self,
        obj: &crate::substream::SubstreamInfoObj,
    ) -> Result<(), OamdError> {
        for index in 0..obj.n_bed {
            self.push(ObjectDescriptor {
                obj_type: ObjectType::Bed,
                b_lfe: obj.bed_object_is_lfe(index),
                b_ajoc_coded: false,
            })?;
        }
        for _ in 0..obj.n_isf {
            self.push(ObjectDescriptor {
                obj_type: ObjectType::Isf,
                b_lfe: false,
                b_ajoc_coded: false,
            })?;
        }
        let dynamic = obj
            .n_objects
            .saturating_sub(obj.n_bed)
            .saturating_sub(obj.n_isf)
            .saturating_sub(u32::from(obj.b_lfe));
        // `ac4_substream_info_obj()` 在动态对象路径中固定先写 LFE（i == 0），
        // 再写其余 DYN 对象；OAMD 必须沿用相同顺序。LFE 不携带动态位置，
        // 因而以床对象描述可与既有 OAMD 状态模型共用同一 gate。
        if obj.b_lfe {
            self.push(ObjectDescriptor {
                obj_type: ObjectType::Bed,
                b_lfe: true,
                b_ajoc_coded: false,
            })?;
        }
        for _ in 0..dynamic {
            self.push(ObjectDescriptor {
                obj_type: ObjectType::Dynamic,
                b_lfe: false,
                b_ajoc_coded: false,
            })?;
        }
        Ok(())
    }

    /// 由一个 substream group 推导对象描述。
    ///
    /// A-JOC substream 的全部对象 `b_ajoc_coded` 为真，因此不在
    /// `oamd_dyndata_multi` 中出现；直接编码的对象 substream 则相反。
    /// `bed_dyn_obj_assignment()` 对其写入的每个条目都置 `b_lfe = 0`
    /// （`6.2.1.10`），A-JOC 的床对象因此不含 LFE。
    ///
    /// 直接编码路径当前没有真实样本，其顺序按 `6.2.1.11` 与表 60 实现，只有
    /// 构造码流的分支覆盖。
    ///
    /// # Errors
    ///
    /// 对象数超过 [`MAX_OAMD_OBJECTS`] 时返回 [`OamdError::TooManyObjects`]。
    pub fn from_group(group: &crate::substream::Ac4SubstreamGroupInfo) -> Result<Self, OamdError> {
        let mut out = Self::empty();

        for info in group.substreams() {
            match *info {
                crate::substream::SubstreamInfo::Ajoc(ref ajoc) => {
                    out.append_ajoc_assignment(ajoc.upmix_assignment, ajoc.b_lfe)?;
                }
                crate::substream::SubstreamInfo::Obj(ref obj) => {
                    out.append_object_substream(obj)?;
                }
                // 声道编码的 substream 不参与 OAMD 对象计数。
                crate::substream::SubstreamInfo::Chan(_) => {}
            }
        }
        Ok(out)
    }
}
