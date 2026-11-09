//! ADR-0025 的绝对时隙端点契约；验证工程选择，不裁决官方规范或物理音频原点。
//!
//! 预期值直接由分段直线求得，不重放 production 的 delta/cursor 更新循环。
//! 斜率采用二进制有理数，使全部预期与生产加法精确可表示，隔离时序与舍入策略。

#![expect(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    reason = "有限的构造时轴和固定三组数组；所有样本值均为精确可表示的二进制有理数"
)]

extern crate std;

use super::*;
use std::vec::Vec;

#[derive(Clone, Copy)]
struct Event {
    slot: u16,
    length: u8,
    target: [f32; 3],
}

fn check_curve(
    frames: &[u8],
    initial: [f32; 3],
    events: &[Event],
    expected: impl Fn(u16) -> [f32; 3],
) {
    let mut dry = [RollingCoefficient::new(initial[0])];
    let mut wet = [RollingCoefficient::new(initial[1])];
    let mut pre = [RollingCoefficient::new(initial[2])];
    let mut coefficients = RollingCoefficients::new(&mut dry, &mut wet, &mut pre);
    let mut state = InterpolationState::new();
    let mut base = 0u16;

    for &slots in frames {
        let end = base + u16::from(slots);
        let in_frame: Vec<_> = events
            .iter()
            .filter(|event| (base..end).contains(&event.slot))
            .collect();
        let points: Vec<_> = in_frame
            .iter()
            .map(|event| {
                RampPoint::new(
                    u8::try_from(event.slot - base).expect("帧内时隙"),
                    event.length,
                    slots,
                )
                .expect("合法数据点")
            })
            .collect();
        let schedule = InterpolationSchedule::new(&points, slots).expect("合法日程");

        for slot in 0..slots {
            state
                .interpolate_timeslot(slot, &schedule, &mut coefficients, |group, point, _| {
                    let index = match group {
                        CoefficientGroup::Dry => 0,
                        CoefficientGroup::Wet => 1,
                        CoefficientGroup::Pre => 2,
                    };
                    in_frame[point].target[index]
                })
                .expect("合法时隙");
            let absolute = base + u16::from(slot);
            let actual = [
                coefficients.dry()[0].current(),
                coefficients.wet()[0].current(),
                coefficients.pre()[0].current(),
            ];
            assert_eq!(
                actual.map(f32::to_bits),
                expected(absolute).map(f32::to_bits),
                "absolute slot {absolute}, frame base {base}, frame slots {slots}"
            );

            let (completed, length) = events
                .iter()
                .rev()
                .find(|event| event.slot <= absolute)
                .map_or((0, 0), |event| {
                    (
                        (absolute - event.slot).min(u16::from(event.length)),
                        event.length,
                    )
                });
            assert_eq!(u16::from(state.completed()), completed);
            assert_eq!(state.ramp_len(), length);
        }
        base = end;
    }
    assert!(
        events
            .iter()
            .all(|event| { event.slot + u16::from(event.length) < base }),
        "必须实际消费每个未中断 ramp 的终点及之后的保持时隙"
    );
}

#[test]
fn all_starts_and_lengths_follow_absolute_endpoint_contract() {
    // 24/30/32 的全部合法起点 × 1..=64 长度，共 5,504 条完整链。
    // N=1、末时隙安装、64 槽无中断终点及无新数据点的跨帧保持均实际执行。
    for first_frame_slots in [24, 30, 32] {
        for start in 0..first_frame_slots {
            for length in 1..=MAX_RAMP_LENGTH {
                let initial = [1.0, -0.5, 0.25];
                let slope = [1.0 / 128.0, -1.0 / 64.0, 1.0 / 32.0];
                let line = |steps: u16| {
                    core::array::from_fn(|group| initial[group] + f32::from(steps) * slope[group])
                };
                let event = Event {
                    slot: u16::from(start),
                    length,
                    target: line(u16::from(length)),
                };
                check_curve(
                    &[first_frame_slots, 24, 30, 32],
                    initial,
                    &[event],
                    |absolute| line(absolute.saturating_sub(event.slot).min(u16::from(length))),
                );
            }
        }
    }
}

#[test]
fn interrupted_ramps_start_at_the_old_curve_value_at_the_same_slot() {
    // 从首/末时隙开始，在旧 64 槽 ramp 的每个内部端点中断，再遍历所有新长度。
    // 2 × 63 × 64 = 8,064 条链，含帧内两点、帧首/帧末中断和混合帧长。
    for start in [0u16, 23] {
        for elapsed in 1..64u16 {
            for length in 1..=MAX_RAMP_LENGTH {
                let first_line = |steps: u16| {
                    let steps = f32::from(steps);
                    [1.0 + steps, -1.0 - 2.0 * steps, 0.5 + 0.5 * steps]
                };
                let knot = first_line(elapsed);
                let second_line = |steps: u16| {
                    let steps = f32::from(steps);
                    [
                        knot[0] + 2.0 * steps,
                        knot[1] - steps,
                        knot[2] + 0.25 * steps,
                    ]
                };
                let interruption = start + elapsed;
                let events = [
                    Event {
                        slot: start,
                        length: 64,
                        target: first_line(64),
                    },
                    Event {
                        slot: interruption,
                        length,
                        target: second_line(u16::from(length)),
                    },
                ];
                check_curve(
                    &[24, 30, 32, 24, 30, 32],
                    first_line(0),
                    &events,
                    |absolute| {
                        if absolute < interruption {
                            first_line(absolute.saturating_sub(start))
                        } else {
                            second_line((absolute - interruption).min(u16::from(length)))
                        }
                    },
                );
            }
        }
    }
}
