//! A-JOC 矩阵参数的跨帧时间插值。
//!
//! `TS103190-2:v1.3.1:5.7.3.4` 的 `Pseudocode 17` 为每个系数保存上一值与
//! 增量，但 ramp 进度属于整个 A-JOC substream。这里把两类状态显式分开：
//! [`RollingCoefficient`] 只保存 `current`、`target`、`delta`，而
//! [`InterpolationState`] 只保存一份共享 ramp 游标。每个 QMF 时隙必须且只能
//! 调用一次 [`InterpolationState::interpolate_timeslot`]，该入口会统一推进 dry、
//! wet 与 pre 三组系数。

use super::MAX_DATA_POINTS;
use super::syntax::AjocDataPoints;
use core::fmt;

/// 单帧 A-JOC 插值可寻址的 QMF 时隙上界；实际时隙数由帧长决定。
pub const MAX_AJOC_TIMESLOTS: u8 = 32;

/// `ajoc_ramp_len_minus1` 可表达的最大 ramp 长度。
pub const MAX_RAMP_LENGTH: u8 = 64;

/// 插值目标所属的矩阵组。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoefficientGroup {
    /// 对象直达矩阵。
    Dry,
    /// 对象去相关矩阵。
    Wet,
    /// 去相关器输入的 pre 矩阵。
    Pre,
}

/// 一个数据点的插值控制。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RampPoint {
    start_pos: u8,
    ramp_len: u8,
}

impl RampPoint {
    const EMPTY: Self = Self {
        start_pos: 0,
        ramp_len: 1,
    };

    /// 创建一个经过范围校验的数据点。
    ///
    /// # Errors
    ///
    /// `num_qmf_timeslots` 不在 `1..=32`、`start_pos` 不在当前帧时隙内，或
    /// `ramp_len` 不在 `1..=64` 时返回 [`InterpolationError`]。尤其不会把零长度
    /// 悄悄改成一步 ramp。
    pub fn new(
        start_pos: u8,
        ramp_len: u8,
        num_qmf_timeslots: u8,
    ) -> Result<Self, InterpolationError> {
        validate_timeslot_count(num_qmf_timeslots)?;
        validate_start_position(start_pos, num_qmf_timeslots)?;
        if !(1..=MAX_RAMP_LENGTH).contains(&ramp_len) {
            return Err(InterpolationError::RampLengthOutOfRange { ramp_len });
        }
        Ok(Self {
            start_pos,
            ramp_len,
        })
    }

    /// 新 target 在哪个 QMF 时隙之后安装。
    #[must_use]
    pub const fn start_pos(self) -> u8 {
        self.start_pos
    }

    /// 该 target 要执行的增量次数。
    #[must_use]
    pub const fn ramp_len(self) -> u8 {
        self.ramp_len
    }
}

/// 一帧 A-JOC 数据点的固定容量插值日程。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InterpolationSchedule {
    points: [RampPoint; MAX_DATA_POINTS],
    count: u8,
    num_qmf_timeslots: u8,
}

impl InterpolationSchedule {
    /// 没有新数据点的帧；已经开始的 ramp 仍会继续跨帧推进。
    ///
    /// # Errors
    ///
    /// `num_qmf_timeslots` 不在 `1..=32` 时返回 [`InterpolationError`]。
    pub fn empty(num_qmf_timeslots: u8) -> Result<Self, InterpolationError> {
        validate_timeslot_count(num_qmf_timeslots)?;
        Ok(Self {
            points: [RampPoint::EMPTY; MAX_DATA_POINTS],
            count: 0,
            num_qmf_timeslots,
        })
    }

    /// 从已经校验的逐数据点控制创建日程。
    ///
    /// # Errors
    ///
    /// 数据点数量超过 [`MAX_DATA_POINTS`]、时隙数无效，或任一起点不在当前帧
    /// 时隙内时返回 [`InterpolationError`]。
    pub fn new(points: &[RampPoint], num_qmf_timeslots: u8) -> Result<Self, InterpolationError> {
        if points.len() > MAX_DATA_POINTS {
            return Err(InterpolationError::TooManyDataPoints {
                count: points.len(),
                limit: MAX_DATA_POINTS,
            });
        }
        let mut out = Self::empty(num_qmf_timeslots)?;
        for (target, source) in out.points.iter_mut().zip(points) {
            validate_start_position(source.start_pos(), num_qmf_timeslots)?;
            *target = *source;
        }
        out.count = u8::try_from(points.len()).unwrap_or(0);
        Ok(out)
    }

    /// 把 `ajoc_data_point_info()` 转成经过校验的日程。
    ///
    /// # Errors
    ///
    /// 语法对象的数量或逐点字段不完整、越界时返回 [`InterpolationError`]。
    pub fn from_data_points(
        points: &AjocDataPoints,
        num_qmf_timeslots: u8,
    ) -> Result<Self, InterpolationError> {
        let count = usize::from(points.count);
        if count > MAX_DATA_POINTS {
            return Err(InterpolationError::TooManyDataPoints {
                count,
                limit: MAX_DATA_POINTS,
            });
        }
        let mut out = Self::empty(num_qmf_timeslots)?;
        for data_point in 0..count {
            let start_pos = points
                .start_pos(data_point)
                .ok_or(InterpolationError::MissingDataPoint { data_point })?;
            let ramp_len_minus1 = points
                .ramp_len_minus1(data_point)
                .ok_or(InterpolationError::MissingDataPoint { data_point })?;
            let ramp_len =
                ramp_len_minus1
                    .checked_add(1)
                    .ok_or(InterpolationError::RampLengthOutOfRange {
                        ramp_len: ramp_len_minus1,
                    })?;
            let point = RampPoint::new(start_pos, ramp_len, num_qmf_timeslots)?;
            if let Some(slot) = out.points.get_mut(data_point) {
                *slot = point;
            }
        }
        out.count = points.count;
        Ok(out)
    }

    /// 本帧携带的数据点数。
    #[must_use]
    pub const fn len(&self) -> u8 {
        self.count
    }

    /// 本帧是否不携带新参数。
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// 当前帧实际包含的 QMF 时隙数。
    #[must_use]
    pub const fn num_qmf_timeslots(&self) -> u8 {
        self.num_qmf_timeslots
    }

    /// 取出一个数据点。
    #[must_use]
    pub fn point(&self, data_point: usize) -> Option<RampPoint> {
        if data_point >= usize::from(self.count) {
            return None;
        }
        self.points.get(data_point).copied()
    }

    fn iter(&self) -> impl Iterator<Item = (usize, RampPoint)> + '_ {
        self.points
            .iter()
            .copied()
            .take(usize::from(self.count))
            .enumerate()
    }
}

/// 单个 dry/wet/pre 系数的 rolling 状态。
///
/// 它不含时隙游标，因而逐系数 helper 不可能意外把共享 ramp 推进多次。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RollingCoefficient {
    current: f32,
    target: f32,
    delta: f32,
}

impl RollingCoefficient {
    /// 全零初始状态。
    pub const ZERO: Self = Self::new(0.0);

    /// 以一个稳定值开始；初始 target 等于 current，delta 为零。
    #[must_use]
    pub const fn new(initial: f32) -> Self {
        Self {
            current: initial,
            target: initial,
            delta: 0.0,
        }
    }

    /// 当前 QMF 时隙应使用的值。
    #[must_use]
    pub const fn current(&self) -> f32 {
        self.current
    }

    /// 当前 ramp 的目标。
    #[must_use]
    pub const fn target(&self) -> f32 {
        self.target
    }

    /// 每个 QMF 时隙使用的增量。
    #[must_use]
    pub const fn delta(&self) -> f32 {
        self.delta
    }

    /// 丢弃 ramp，并把系数钉到新的稳定值。
    pub fn reset(&mut self, value: f32) {
        *self = Self::new(value);
    }

    fn advance(&mut self, final_increment: bool) {
        self.current += self.delta;
        if final_increment {
            // 重复浮点加法可能在最后留下舍入尾差；规范 target 是精确终点。
            self.current = self.target;
        }
    }

    fn install(&mut self, target: f32, ramp_len: u8) {
        self.target = target;
        self.delta = (target - self.current) / f32::from(ramp_len);
    }
}

impl Default for RollingCoefficient {
    fn default() -> Self {
        Self::ZERO
    }
}

/// 同一 substream 内由一个游标统一推进的三组 rolling 系数。
#[derive(Debug)]
pub struct RollingCoefficients<'a> {
    dry: &'a mut [RollingCoefficient],
    wet: &'a mut [RollingCoefficient],
    pre: &'a mut [RollingCoefficient],
}

impl<'a> RollingCoefficients<'a> {
    /// 把调用方持有的 dry/wet/pre 状态借给一个时隙入口。
    #[must_use]
    pub fn new(
        dry: &'a mut [RollingCoefficient],
        wet: &'a mut [RollingCoefficient],
        pre: &'a mut [RollingCoefficient],
    ) -> Self {
        Self { dry, wet, pre }
    }

    /// dry rolling 状态。
    #[must_use]
    pub fn dry(&self) -> &[RollingCoefficient] {
        self.dry
    }

    /// wet rolling 状态。
    #[must_use]
    pub fn wet(&self) -> &[RollingCoefficient] {
        self.wet
    }

    /// pre rolling 状态。
    #[must_use]
    pub fn pre(&self) -> &[RollingCoefficient] {
        self.pre
    }

    /// 把全部系数和未完成 ramp 清零；共享游标由调用方另行 reset。
    pub fn clear(&mut self) {
        self.dry.fill(RollingCoefficient::ZERO);
        self.wet.fill(RollingCoefficient::ZERO);
        self.pre.fill(RollingCoefficient::ZERO);
    }

    fn advance(&mut self, final_increment: bool) {
        for coefficient in self.dry.iter_mut() {
            coefficient.advance(final_increment);
        }
        for coefficient in self.wet.iter_mut() {
            coefficient.advance(final_increment);
        }
        for coefficient in self.pre.iter_mut() {
            coefficient.advance(final_increment);
        }
    }

    fn install<F>(&mut self, data_point: usize, ramp_len: u8, target_for: &F)
    where
        F: Fn(CoefficientGroup, usize, usize) -> f32,
    {
        for (index, coefficient) in self.dry.iter_mut().enumerate() {
            coefficient.install(
                target_for(CoefficientGroup::Dry, data_point, index),
                ramp_len,
            );
        }
        for (index, coefficient) in self.wet.iter_mut().enumerate() {
            coefficient.install(
                target_for(CoefficientGroup::Wet, data_point, index),
                ramp_len,
            );
        }
        for (index, coefficient) in self.pre.iter_mut().enumerate() {
            coefficient.install(
                target_for(CoefficientGroup::Pre, data_point, index),
                ramp_len,
            );
        }
    }
}

/// 一个 A-JOC substream 共享的 ramp 游标。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InterpolationState {
    completed: u8,
    ramp_len: u8,
}

impl InterpolationState {
    /// 尚未收到数据点的游标。
    #[must_use]
    pub const fn new() -> Self {
        Self {
            completed: 0,
            ramp_len: 0,
        }
    }

    /// 丢弃未完成的跨帧 ramp。
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// 当前 ramp 已经执行的增量次数。
    #[must_use]
    pub const fn completed(&self) -> u8 {
        self.completed
    }

    /// 当前 ramp 的总增量次数；零表示尚无 ramp。
    #[must_use]
    pub const fn ramp_len(&self) -> u8 {
        self.ramp_len
    }

    /// 推进一个 QMF 时隙的全部 dry/wet/pre 系数。
    ///
    /// 先用进入该时隙时已有的 delta 更新所有系数，再把共享游标推进**一次**。
    /// 当前时隙命中的数据点随后安装新 target/delta 并把游标重置为零，所以新
    /// ramp 从下一个时隙开始。完成第 `ramp_len` 次增量时会把每个系数精确钉到
    /// target。
    ///
    /// `target_for(group, data_point, coefficient)` 只在当前时隙命中数据点时调用。
    /// 调用方须为三组中的每个系数返回对应数据点的反量化目标。
    ///
    /// # Errors
    ///
    /// `timeslot` 不在日程绑定的当前帧实际 QMF 时隙内时返回
    /// [`InterpolationError`]；返回前不会推进游标或系数。
    pub fn interpolate_timeslot<F>(
        &mut self,
        timeslot: u8,
        schedule: &InterpolationSchedule,
        coefficients: &mut RollingCoefficients<'_>,
        target_for: F,
    ) -> Result<(), InterpolationError>
    where
        F: Fn(CoefficientGroup, usize, usize) -> f32,
    {
        if timeslot >= schedule.num_qmf_timeslots() {
            return Err(InterpolationError::TimeslotOutOfRange {
                timeslot,
                num_qmf_timeslots: schedule.num_qmf_timeslots(),
            });
        }
        if self.completed < self.ramp_len {
            let next = self.completed.saturating_add(1);
            coefficients.advance(next == self.ramp_len);
            // 游标只在这里、且每个时隙至多推进一次。
            self.completed = next;
        }

        for (data_point, point) in schedule.iter() {
            if timeslot != point.start_pos() {
                continue;
            }
            coefficients.install(data_point, point.ramp_len(), &target_for);
            self.completed = 0;
            self.ramp_len = point.ramp_len();
        }
        Ok(())
    }
}

impl Default for InterpolationState {
    fn default() -> Self {
        Self::new()
    }
}

/// A-JOC 插值控制无效。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterpolationError {
    /// 数据点数量超过固定容量。
    TooManyDataPoints { count: usize, limit: usize },
    /// 当前帧的 QMF 时隙数不是 `1..=32`。
    TimeslotCountOutOfRange { num_qmf_timeslots: u8 },
    /// `ajoc_start_pos` 越出当前帧的实际 QMF 时隙范围。
    StartPositionOutOfRange {
        start_pos: u8,
        num_qmf_timeslots: u8,
    },
    /// 调用方试图推进当前帧不存在的 QMF 时隙。
    TimeslotOutOfRange { timeslot: u8, num_qmf_timeslots: u8 },
    /// ramp 长度不是 `1..=64`；零长度在这里直接拒绝。
    RampLengthOutOfRange { ramp_len: u8 },
    /// 语法对象声明了数据点，但对应字段不可取出。
    MissingDataPoint { data_point: usize },
}

impl fmt::Display for InterpolationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooManyDataPoints { count, limit } => {
                write!(
                    f,
                    "A-JOC interpolation data-point count {count} exceeds limit {limit}"
                )
            }
            Self::TimeslotCountOutOfRange { num_qmf_timeslots } => write!(
                f,
                "A-JOC current-frame QMF timeslot count {num_qmf_timeslots} is outside 1..={MAX_AJOC_TIMESLOTS}"
            ),
            Self::StartPositionOutOfRange {
                start_pos,
                num_qmf_timeslots,
            } => write!(
                f,
                "A-JOC interpolation start {start_pos} is outside current frame range 0..{num_qmf_timeslots}"
            ),
            Self::TimeslotOutOfRange {
                timeslot,
                num_qmf_timeslots,
            } => write!(
                f,
                "A-JOC interpolation timeslot {timeslot} is outside current frame range 0..{num_qmf_timeslots}"
            ),
            Self::RampLengthOutOfRange { ramp_len } => {
                write!(
                    f,
                    "A-JOC interpolation ramp length {ramp_len} is outside 1..=64"
                )
            }
            Self::MissingDataPoint { data_point } => {
                write!(
                    f,
                    "A-JOC interpolation lacks control fields for data point {data_point}"
                )
            }
        }
    }
}

impl core::error::Error for InterpolationError {}

fn validate_timeslot_count(num_qmf_timeslots: u8) -> Result<(), InterpolationError> {
    if !(1..=MAX_AJOC_TIMESLOTS).contains(&num_qmf_timeslots) {
        return Err(InterpolationError::TimeslotCountOutOfRange { num_qmf_timeslots });
    }
    Ok(())
}

fn validate_start_position(start_pos: u8, num_qmf_timeslots: u8) -> Result<(), InterpolationError> {
    if start_pos >= num_qmf_timeslots {
        return Err(InterpolationError::StartPositionOutOfRange {
            start_pos,
            num_qmf_timeslots,
        });
    }
    Ok(())
}

#[cfg(test)]
#[expect(
    clippy::indexing_slicing,
    reason = "独立展开 oracle 使用由同一长度构造的 Vec，下标越界应直接暴露测试错误"
)]
mod tests {
    extern crate std;

    use super::*;
    use std::vec;
    use std::vec::Vec;

    #[derive(Debug, Clone)]
    struct Targets {
        dry: Vec<Vec<f32>>,
        wet: Vec<Vec<f32>>,
        pre: Vec<Vec<f32>>,
    }

    impl Targets {
        fn value(&self, group: CoefficientGroup, data_point: usize, index: usize) -> f32 {
            match group {
                CoefficientGroup::Dry => self.dry[data_point][index],
                CoefficientGroup::Wet => self.wet[data_point][index],
                CoefficientGroup::Pre => self.pre[data_point][index],
            }
        }

        fn flat_value(
            &self,
            data_point: usize,
            index: usize,
            dry_len: usize,
            wet_len: usize,
        ) -> f32 {
            if index < dry_len {
                return self.dry[data_point][index];
            }
            let wet_index = index.saturating_sub(dry_len);
            if wet_index < wet_len {
                return self.wet[data_point][wet_index];
            }
            self.pre[data_point][wet_index.saturating_sub(wet_len)]
        }
    }

    #[derive(Debug, Clone, Copy)]
    struct ReferenceCoefficient {
        current: f32,
        target: f32,
        delta: f32,
    }

    impl ReferenceCoefficient {
        fn new(value: f32) -> Self {
            Self {
                current: value,
                target: value,
                delta: 0.0,
            }
        }
    }

    /// 仅供测试的 32 时隙展开实现；它不调用任何 production rolling helper。
    struct ExpandedReference {
        coefficients: Vec<ReferenceCoefficient>,
        completed: u8,
        ramp_len: u8,
        dry_len: usize,
        wet_len: usize,
    }

    impl ExpandedReference {
        fn new(dry: &[f32], wet: &[f32], pre: &[f32]) -> Self {
            let coefficients = dry
                .iter()
                .chain(wet)
                .chain(pre)
                .copied()
                .map(ReferenceCoefficient::new)
                .collect();
            Self {
                coefficients,
                completed: 0,
                ramp_len: 0,
                dry_len: dry.len(),
                wet_len: wet.len(),
            }
        }

        fn expand_frame(
            &mut self,
            schedule: &InterpolationSchedule,
            targets: &Targets,
        ) -> Vec<Vec<u32>> {
            let mut expanded = Vec::with_capacity(usize::from(schedule.num_qmf_timeslots()));
            for timeslot in 0..schedule.num_qmf_timeslots() {
                if self.completed < self.ramp_len {
                    let next = self.completed.saturating_add(1);
                    for coefficient in &mut self.coefficients {
                        coefficient.current += coefficient.delta;
                        if next == self.ramp_len {
                            coefficient.current = coefficient.target;
                        }
                    }
                    self.completed = next;
                }

                expanded.push(
                    self.coefficients
                        .iter()
                        .map(|coefficient| coefficient.current.to_bits())
                        .collect(),
                );

                for (data_point, point) in schedule.iter() {
                    if timeslot != point.start_pos() {
                        continue;
                    }
                    for (index, coefficient) in self.coefficients.iter_mut().enumerate() {
                        let target =
                            targets.flat_value(data_point, index, self.dry_len, self.wet_len);
                        coefficient.target = target;
                        coefficient.delta =
                            (target - coefficient.current) / f32::from(point.ramp_len());
                    }
                    self.completed = 0;
                    self.ramp_len = point.ramp_len();
                }
            }
            expanded
        }
    }

    fn rolling(value: &[f32]) -> Vec<RollingCoefficient> {
        value.iter().copied().map(RollingCoefficient::new).collect()
    }

    fn rolling_frame(
        state: &mut InterpolationState,
        schedule: &InterpolationSchedule,
        targets: &Targets,
        dry: &mut [RollingCoefficient],
        wet: &mut [RollingCoefficient],
        pre: &mut [RollingCoefficient],
    ) -> Vec<Vec<u32>> {
        let mut expanded = Vec::with_capacity(usize::from(schedule.num_qmf_timeslots()));
        let mut coefficients = RollingCoefficients::new(dry, wet, pre);
        for timeslot in 0..schedule.num_qmf_timeslots() {
            state
                .interpolate_timeslot(timeslot, schedule, &mut coefficients, |group, dp, index| {
                    targets.value(group, dp, index)
                })
                .expect("日程内时隙应合法");
            expanded.push(
                coefficients
                    .dry()
                    .iter()
                    .chain(coefficients.wet())
                    .chain(coefficients.pre())
                    .map(|coefficient| coefficient.current().to_bits())
                    .collect(),
            );
        }
        expanded
    }

    fn targets(seed: f32, points: usize, dry: usize, wet: usize, pre: usize) -> Targets {
        let rows = |width: usize, group_offset: f32| {
            (0..points)
                .map(|data_point| {
                    (0..width)
                        .map(|index| {
                            seed + group_offset
                                + (data_point as f32) * 0.375
                                + (index as f32) * 0.0625
                        })
                        .collect()
                })
                .collect()
        };
        Targets {
            dry: rows(dry, 0.0),
            wet: rows(wet, 1.25),
            pre: rows(pre, -0.625),
        }
    }

    #[test]
    fn rejects_zero_and_out_of_range_ramps_before_state_changes() {
        assert_eq!(
            RampPoint::new(0, 0, 32),
            Err(InterpolationError::RampLengthOutOfRange { ramp_len: 0 })
        );
        assert_eq!(
            RampPoint::new(0, 65, 32),
            Err(InterpolationError::RampLengthOutOfRange { ramp_len: 65 })
        );
        assert_eq!(
            RampPoint::new(32, 1, 32),
            Err(InterpolationError::StartPositionOutOfRange {
                start_pos: 32,
                num_qmf_timeslots: 32,
            })
        );

        let point = RampPoint::new(0, 1, 32).expect("合法数据点");
        assert!(matches!(
            InterpolationSchedule::new(&[point; MAX_DATA_POINTS + 1], 32),
            Err(InterpolationError::TooManyDataPoints { .. })
        ));

        let syntax_points = AjocDataPoints::with_count_for_test(2);
        let from_syntax = InterpolationSchedule::from_data_points(&syntax_points, 24)
            .expect("语法中的 minus1 必须转换成非零 ramp");
        assert_eq!(from_syntax.len(), 2);
        assert_eq!(from_syntax.num_qmf_timeslots(), 24);
        assert_eq!(
            from_syntax.point(0),
            Some(RampPoint::new(0, 1, 24).expect("合法数据点"))
        );
        assert_eq!(from_syntax.point(1), from_syntax.point(0));
    }

    #[test]
    fn actual_frame_timeslots_bound_starts_and_driver_calls() {
        for num_qmf_timeslots in [24u8, 30, 32] {
            let point = RampPoint::new(num_qmf_timeslots.saturating_sub(1), 1, num_qmf_timeslots)
                .expect("当前帧末时隙应合法");
            let schedule =
                InterpolationSchedule::new(&[point], num_qmf_timeslots).expect("实际帧边界应合法");
            assert_eq!(schedule.num_qmf_timeslots(), num_qmf_timeslots);
        }

        let point_for_32 = RampPoint::new(24, 1, 32).expect("时隙 24 在 32 时隙帧内");
        assert_eq!(
            InterpolationSchedule::new(&[point_for_32], 24),
            Err(InterpolationError::StartPositionOutOfRange {
                start_pos: 24,
                num_qmf_timeslots: 24,
            })
        );
        assert_eq!(
            InterpolationSchedule::empty(0),
            Err(InterpolationError::TimeslotCountOutOfRange {
                num_qmf_timeslots: 0,
            })
        );
        assert_eq!(
            InterpolationSchedule::empty(33),
            Err(InterpolationError::TimeslotCountOutOfRange {
                num_qmf_timeslots: 33,
            })
        );

        let schedule =
            InterpolationSchedule::new(&[RampPoint::new(0, 4, 24).expect("合法 ramp")], 24)
                .expect("24 时隙帧应合法");
        let mut state = InterpolationState::new();
        let mut dry = [RollingCoefficient::new(1.0)];
        let mut wet = [];
        let mut pre = [];
        let mut coefficients = RollingCoefficients::new(&mut dry, &mut wet, &mut pre);
        state
            .interpolate_timeslot(0, &schedule, &mut coefficients, |_, _, _| 5.0)
            .expect("时隙 0 应安装活动 ramp");
        let before_state = state;
        let before_dry = coefficients.dry()[0];
        assert_eq!(
            state.interpolate_timeslot(24, &schedule, &mut coefficients, |_, _, _| 0.0),
            Err(InterpolationError::TimeslotOutOfRange {
                timeslot: 24,
                num_qmf_timeslots: 24,
            })
        );
        assert_eq!(state, before_state);
        assert_eq!(coefficients.dry()[0], before_dry);
    }

    #[test]
    fn rolling_storage_is_three_scalars_not_a_timeslot_matrix() {
        assert_eq!(core::mem::size_of::<RollingCoefficient>(), 3 * 4);
        assert_eq!(core::mem::size_of::<InterpolationState>(), 2);
    }

    #[test]
    fn new_target_is_installed_after_the_old_delta_for_that_slot() {
        let schedule = InterpolationSchedule::new(
            &[
                RampPoint::new(0, 4, 32).expect("首 ramp"),
                RampPoint::new(2, 2, 32).expect("重定向 ramp"),
            ],
            32,
        )
        .expect("合法日程");
        let targets = Targets {
            dry: vec![vec![4.0], vec![10.0]],
            wet: vec![vec![], vec![]],
            pre: vec![vec![], vec![]],
        };
        let mut state = InterpolationState::new();
        let mut dry = [RollingCoefficient::ZERO];
        let mut wet = [];
        let mut pre = [];
        let mut coefficients = RollingCoefficients::new(&mut dry, &mut wet, &mut pre);
        let mut observed = [0.0; 5];

        for timeslot in 0..5u8 {
            state
                .interpolate_timeslot(
                    timeslot,
                    &schedule,
                    &mut coefficients,
                    |group, dp, index| targets.value(group, dp, index),
                )
                .expect("合法时隙");
            observed[usize::from(timeslot)] = coefficients.dry()[0].current();
        }

        assert_eq!(observed, [0.0, 1.0, 2.0, 6.0, 10.0]);
        assert_eq!(coefficients.dry()[0].target(), 10.0);
        assert_eq!(coefficients.dry()[0].delta(), 4.0);
        assert_eq!(state.completed(), 2);
        assert_eq!(state.ramp_len(), 2);
    }

    #[test]
    fn one_shared_cursor_advances_all_three_groups_once_per_slot() {
        let schedule =
            InterpolationSchedule::new(&[RampPoint::new(0, 2, 32).expect("合法 ramp")], 32)
                .expect("合法日程");
        let targets = Targets {
            dry: vec![vec![2.0, 4.0, 6.0]],
            wet: vec![vec![-2.0, -4.0]],
            pre: vec![vec![8.0]],
        };
        let mut state = InterpolationState::new();
        let mut dry = [RollingCoefficient::ZERO; 3];
        let mut wet = [RollingCoefficient::ZERO; 2];
        let mut pre = [RollingCoefficient::ZERO; 1];
        let mut coefficients = RollingCoefficients::new(&mut dry, &mut wet, &mut pre);

        state
            .interpolate_timeslot(0, &schedule, &mut coefficients, |group, dp, index| {
                targets.value(group, dp, index)
            })
            .expect("时隙 0");
        state
            .interpolate_timeslot(1, &schedule, &mut coefficients, |group, dp, index| {
                targets.value(group, dp, index)
            })
            .expect("时隙 1");

        assert_eq!(state.completed(), 1, "游标不得按六个系数各推进一次");
        assert_eq!(
            coefficients
                .dry()
                .iter()
                .map(RollingCoefficient::current)
                .collect::<Vec<_>>(),
            vec![1.0, 2.0, 3.0]
        );
        assert_eq!(
            coefficients
                .wet()
                .iter()
                .map(RollingCoefficient::current)
                .collect::<Vec<_>>(),
            vec![-1.0, -2.0]
        );
        assert_eq!(coefficients.pre()[0].current(), 4.0);
    }

    #[test]
    fn rolling_is_bit_exact_with_expanded_reference_across_frames() {
        // dry 的 6 个系数表示 2 对象 × 3 参数带；wet 表示 2 对象 × 2 项。
        // pre 单列成第三组，迫使三组共享同一 ramp 游标。
        let dry_initial = [-0.75, 0.1, 0.625, -0.2, 0.03125, 1.0];
        let wet_initial = [0.5, -0.5, 0.2, -0.125];
        let pre_initial = [0.0, 0.75, -0.25];
        let mut dry = rolling(&dry_initial);
        let mut wet = rolling(&wet_initial);
        let mut pre = rolling(&pre_initial);
        let mut rolling_state = InterpolationState::new();
        let mut reference = ExpandedReference::new(&dry_initial, &wet_initial, &pre_initial);

        let empty_24 = InterpolationSchedule::empty(24).expect("24 时隙空日程");
        let two_30 = InterpolationSchedule::new(
            &[
                RampPoint::new(3, 5, 30).expect("数据点 0"),
                RampPoint::new(19, 9, 30).expect("数据点 1"),
            ],
            30,
        )
        .expect("两数据点日程");
        let one_at_boundary =
            InterpolationSchedule::new(&[RampPoint::new(0, 32, 32).expect("跨帧 ramp")], 32)
                .expect("单数据点日程");

        let frame_controls = [
            (empty_24, targets(0.0, 0, 6, 4, 3)),
            (two_30, targets(1.125, 2, 6, 4, 3)),
            (one_at_boundary, targets(-0.375, 1, 6, 4, 3)),
            (empty_24, targets(0.0, 0, 6, 4, 3)),
        ];

        for (frame, (schedule, frame_targets)) in frame_controls.iter().enumerate() {
            let rolling_expanded = rolling_frame(
                &mut rolling_state,
                schedule,
                frame_targets,
                &mut dry,
                &mut wet,
                &mut pre,
            );
            let reference_expanded = reference.expand_frame(schedule, frame_targets);
            assert_eq!(
                rolling_expanded, reference_expanded,
                "frame {frame} 的 rolling 与展开 oracle 不逐位一致"
            );
            let boundary_target = frame_controls[2].1.dry[0][0].to_bits();
            if frame == 2 {
                assert_ne!(
                    rolling_expanded[31][0], boundary_target,
                    "start_pos=0 后，本帧末尾只应完成 31/32 次增量"
                );
            } else if frame == 3 {
                assert_eq!(
                    rolling_expanded[0][0], boundary_target,
                    "下一帧 ts=0 必须执行第 32 次增量并钉到 target"
                );
            }
        }

        // start_pos=0 的新 ramp 在该时隙之后安装：第一帧只执行 31 次，下一帧
        // ts=0 执行第 32 次并精确钉到 target。
        let expected = frame_controls[2].1.dry[0][0].to_bits();
        assert_eq!(dry[0].current().to_bits(), expected);
        assert_eq!(rolling_state.completed(), 32);
        assert_eq!(rolling_state.ramp_len(), 32);
    }

    #[test]
    fn clear_and_reset_discard_coefficients_and_shared_cursor() {
        let schedule =
            InterpolationSchedule::new(&[RampPoint::new(0, 4, 32).expect("合法 ramp")], 32)
                .expect("合法日程");
        let targets = Targets {
            dry: vec![vec![4.0]],
            wet: vec![vec![2.0]],
            pre: vec![vec![-2.0]],
        };
        let mut state = InterpolationState::new();
        let mut dry = [RollingCoefficient::new(1.0)];
        let mut wet = [RollingCoefficient::new(1.0)];
        let mut pre = [RollingCoefficient::new(1.0)];
        let mut coefficients = RollingCoefficients::new(&mut dry, &mut wet, &mut pre);
        state
            .interpolate_timeslot(0, &schedule, &mut coefficients, |group, dp, index| {
                targets.value(group, dp, index)
            })
            .expect("时隙 0");
        state
            .interpolate_timeslot(1, &schedule, &mut coefficients, |group, dp, index| {
                targets.value(group, dp, index)
            })
            .expect("时隙 1");

        coefficients.clear();
        state.reset();
        for coefficient in coefficients
            .dry()
            .iter()
            .chain(coefficients.wet())
            .chain(coefficients.pre())
        {
            assert_eq!(*coefficient, RollingCoefficient::ZERO);
        }
        assert_eq!(state, InterpolationState::new());
    }
}
