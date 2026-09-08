//! 容器无关的有界 AU 与选择策略。

/// Session 的 presentation 选择策略。
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PresentationSelection {
    /// 只有一个可解码 presentation 时自动选择；多于一个时返回歧义错误。
    #[default]
    AutoUnique,
    /// 按零基 presentation 下标选择。
    Index(u32),
    /// 按码流声明的 `presentation_id` 选择。
    Id(u32),
}

/// 调用方提供的通用外部采样时间。
///
/// 会话只透传这些值，不解释它们来自哪一种容器。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccessUnitContext {
    index: u64,
    source_sample_start: Option<i64>,
    presentation_sample_start: Option<i64>,
    priming_samples: Option<u64>,
    random_access_hint: Option<bool>,
    discontinuity: bool,
}

impl AccessUnitContext {
    /// 为一个已定界的 access unit 创建上下文。
    #[must_use]
    pub const fn new(index: u64) -> Self {
        Self {
            index,
            source_sample_start: None,
            presentation_sample_start: None,
            priming_samples: None,
            random_access_hint: None,
            discontinuity: false,
        }
    }

    /// 设置调用方媒体时间轴中的起始采样。
    #[must_use]
    pub const fn with_source_sample_start(mut self, value: i64) -> Self {
        self.source_sample_start = Some(value);
        self
    }

    /// 设置调用方应用 edit 后的呈现起始采样。
    #[must_use]
    pub const fn with_presentation_sample_start(mut self, value: i64) -> Self {
        self.presentation_sample_start = Some(value);
        self
    }

    /// 设置容器单独声明的 priming，不把它混入其他起点字段。
    #[must_use]
    pub const fn with_priming_samples(mut self, value: u64) -> Self {
        self.priming_samples = Some(value);
        self
    }

    /// 设置容器或传输层的随机访问提示。
    #[must_use]
    pub const fn with_random_access_hint(mut self, value: bool) -> Self {
        self.random_access_hint = Some(value);
        self
    }

    /// 标记 access unit 前存在外部不连续。
    #[must_use]
    pub const fn with_discontinuity(mut self, value: bool) -> Self {
        self.discontinuity = value;
        self
    }

    /// 原始 access unit 下标。
    #[must_use]
    pub const fn index(&self) -> u64 {
        self.index
    }

    /// 调用方媒体时间轴中的起始采样。
    #[must_use]
    pub const fn source_sample_start(&self) -> Option<i64> {
        self.source_sample_start
    }

    /// 应用外部 edit 后的呈现起始采样。
    #[must_use]
    pub const fn presentation_sample_start(&self) -> Option<i64> {
        self.presentation_sample_start
    }

    /// 容器 priming 采样数。
    #[must_use]
    pub const fn priming_samples(&self) -> Option<u64> {
        self.priming_samples
    }

    /// 容器或传输层的随机访问提示。
    #[must_use]
    pub const fn random_access_hint(&self) -> Option<bool> {
        self.random_access_hint
    }

    /// access unit 前是否存在外部不连续。
    #[must_use]
    pub const fn discontinuity(&self) -> bool {
        self.discontinuity
    }
}

impl Default for AccessUnitContext {
    fn default() -> Self {
        Self::new(0)
    }
}

/// 一个已经剥离 sync wrapper、由调用方定界的 `raw_ac4_frame`。
#[derive(Debug, Clone, Copy)]
pub struct AccessUnit<'a> {
    raw_frame: &'a [u8],
    context: AccessUnitContext,
}

impl<'a> AccessUnit<'a> {
    /// 创建 access unit；本构造器不复制输入切片。
    #[must_use]
    pub const fn new(raw_frame: &'a [u8], context: AccessUnitContext) -> Self {
        Self { raw_frame, context }
    }

    /// 完整 `raw_ac4_frame` 字节。
    #[must_use]
    pub const fn raw_frame(&self) -> &'a [u8] {
        self.raw_frame
    }

    /// 调用方提供的时间与连续性上下文。
    #[must_use]
    pub const fn context(&self) -> AccessUnitContext {
        self.context
    }
}
