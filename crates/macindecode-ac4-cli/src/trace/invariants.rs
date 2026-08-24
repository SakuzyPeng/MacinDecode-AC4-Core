//! A-JOC trace 重建链的完整性判据。
//!
//! 这些计数来自统一 Full engine 的结构化 observation，并形成稳定的 trace JSON；
//! `audio_check.sh` 对真实向量执行 fail-closed 门禁。Scene/PCM 制品出口另由 Session
//! 的逐 AU 事务与结构化错误负责，不再复用这里的整文件收尾判据。

#[cfg(feature = "audio-decode")]
use super::AjocTrace;

/// 声明 A-JOC 重建链的完整性不变量。
///
/// 枚举、[`ReconstructionInvariant::ALL`] 与 [`ReconstructionInvariant::name`]
/// 由同一份变体列表生成，因此加一条不可能只加进其中之一。手写三份时，只把变体
/// 加进枚举、补齐 `name` 与 `violation`、却漏掉 `ALL` 的写法能编译通过——实测
/// 如此，遗漏的那条从此不再被求值。
#[cfg(feature = "audio-decode")]
macro_rules! reconstruction_invariants {
    ($( $(#[$doc:meta])* $variant:ident => $name:literal, )+) => {
        /// A-JOC 重建链上的一条完整性不变量。
        ///
        /// **这是该清单的唯一声明。** `scripts/audio_check.sh` 只消费由它生成的
        /// `reconstruction_invariants` JSON。此前脚本与旧场景收尾各抄一份清单，
        /// `synthesis_failures` 只进了 shell 一侧；旧收尾现已随迁移 oracle 删除。
        ///
        /// 脚本另有非空、非静音与 `fill_bits` 上限等**依赖具体测试向量**的条
        /// 件仍留在 shell 侧。
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub(crate) enum ReconstructionInvariant {
            $( $(#[$doc])* $variant, )+
        }

        impl ReconstructionInvariant {
            /// 全部不变量，与枚举同源。
            pub(crate) const ALL: &'static [Self] = &[ $( Self::$variant, )+ ];

            /// JSON 与门禁输出共用的稳定名字。
            pub(crate) const fn name(self) -> &'static str {
                match self { $( Self::$variant => $name, )+ }
            }
        }
    };
}

#[cfg(feature = "audio-decode")]
reconstruction_invariants! {
    /// 逐对象状态延续不得引用不存在的历史。
    State => "state_failures",
    /// 含 A-JOC substream 的每一帧都必须完整落地。
    ///
    /// `failures` 是帧级总括计数，既包含解析与上下文失败，也包含另行统计的状态
    /// 延续失败；具体类别必须排在本项之前，避免被总括说明掩盖。
    Frame => "failures",
    /// 还原出的绝对标度因子必须落在 `5.1.3.2` 的 `0…255`。
    ScaleFactor => "scale_factor_failures",
    /// 每个声道都必须完成反量化与增益。
    Scale => "scale_failures",
    /// 每个声道都必须完成 `5.1.5` 解组。
    Ungroup => "ungroup_failures",
    /// 解组是排列，非零谱线数逐声道不变。
    UngroupCountMismatch => "ungroup_count_mismatch",
    /// 解组只搬运不计算，能量只应差 f64 求和顺序。
    UngroupEnergyDrift => "ungroup_energy_drift",
    /// 缩放后的谱线全部有限。
    ScaledNonFinite => "scaled_nonfinite",
    /// 每个声道帧都必须完成 `5.5` 帧级合成。
    Synthesis => "synthesis_failures",
    /// 合成出的 PCM 全部有限。
    PcmNonFinite => "pcm_nonfinite",
    /// 每条解组谱线恰好生成一个 PCM 样本。
    PcmSampleConservation => "pcm_sample_conservation",
    /// full A-JOC 帧级矩阵事务必须成功。
    AjocReconstruction => "ajoc_reconstruction_failures",
    /// 重建对象与插回 LFE 的终端 PCM 必须全部有限。
    ObjectsNonFinite => "objects_nonfinite",
    /// full 输出拓扑、缓冲与逐路样本形状必须闭合。
    ObjectShapeMismatch => "object_shape_mismatches",
    /// 已请求的 A-SPX 帧必须全部完成驱动与终端合成。
    AspxDrive => "aspx_failures",
}

#[cfg(feature = "audio-decode")]
impl ReconstructionInvariant {
    /// 违反时给出说明，满足时返回 `None`。
    pub(super) fn violation(self, trace: &AjocTrace) -> Option<String> {
        let stats = &trace.scaled_stats;
        let counted = |count: u64, detail: Option<&str>, label: &str| {
            (count != 0).then(|| {
                detail.map_or_else(
                    || format!("{label} {count} 次"),
                    |text| format!("{label} {count} 次：{text}"),
                )
            })
        };
        match self {
            Self::State => counted(
                u64::from(trace.state_failures),
                trace.first_error.as_deref(),
                "状态延续失败",
            ),
            // `failures` 是帧级总括计数；状态失败也会让 FrameTally 失败，因此把
            // 更具体的 State 排在它之前。
            Self::Frame => counted(
                u64::from(trace.failures),
                trace.first_error.as_deref(),
                "A-JOC 帧未完整落地",
            ),
            Self::ScaleFactor => counted(
                u64::from(trace.scale_factor_failures),
                trace.scale_factor_first_error.as_deref(),
                "标度因子越界",
            ),
            Self::Scale => counted(
                stats.scale_failures,
                stats.scale_first_error.as_deref(),
                "缩放失败",
            ),
            Self::Ungroup => counted(
                stats.ungroup_failures,
                stats.ungroup_first_error.as_deref(),
                "解组失败",
            ),
            Self::UngroupCountMismatch => {
                counted(stats.ungroup_count_mismatch, None, "解组前后非零谱线数不符")
            }
            Self::UngroupEnergyDrift => (stats.ungroup_energy_drift >= 1e-12).then(|| {
                format!(
                    "解组能量漂移 {:e}，超出 f64 求和顺序的量级",
                    stats.ungroup_energy_drift
                )
            }),
            Self::ScaledNonFinite => counted(stats.nonfinite, None, "缩放后出现非有限谱线"),
            Self::Synthesis => counted(
                stats.synthesis_failures,
                stats.synthesis_first_error.as_deref(),
                "IMDCT 合成失败",
            ),
            Self::PcmNonFinite => counted(stats.pcm_nonfinite, None, "PCM 出现非有限样本"),
            Self::PcmSampleConservation => {
                (stats.pcm_samples != stats.ungrouped_lines).then(|| {
                    format!(
                        "PCM 样本数 {} 与解组谱线数 {} 不符",
                        stats.pcm_samples, stats.ungrouped_lines
                    )
                })
            }
            Self::AjocReconstruction => counted(
                u64::from(trace.ajoc_reconstruction_failures),
                trace.ajoc_reconstruction_first_error.as_deref(),
                "A-JOC 对象重建失败",
            ),
            Self::ObjectsNonFinite => counted(
                trace.objects_nonfinite,
                trace.objects_nonfinite_first_error.as_deref(),
                "对象 PCM 出现非有限样本",
            ),
            Self::ObjectShapeMismatch => counted(
                u64::from(trace.object_shape_mismatches),
                trace.object_shape_first_error.as_deref(),
                "对象输出形状不匹配",
            ),
            Self::AspxDrive => counted(
                u64::from(trace.aspx_failures),
                trace.aspx_first_error.as_deref(),
                "A-SPX PCM 驱动失败",
            ),
        }
    }
}
