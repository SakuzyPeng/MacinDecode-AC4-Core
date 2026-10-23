//! 显式测试入口的实际去相关消费者观察；产品构建不包含此模块。
#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    reason = "测试协议先约束实例、时隙及固定大小的观察矩阵"
)]
extern crate std;
use super::*;
use std::{
    boxed::Box,
    cell::RefCell,
    collections::BTreeMap,
    format,
    fs::{File, OpenOptions},
    io::{BufWriter, Write},
    path::PathBuf,
    vec,
    vec::Vec,
};

struct Location {
    root: PathBuf,
    au: usize,
    selected: bool,
    slots: usize,
    count: usize,
    ts: usize,
    de: usize,
    calls: usize,
    writers: BTreeMap<&'static str, BufWriter<File>>,
}
impl Location {
    fn bytes(&mut self, tag: &'static str, bytes: &[u8]) {
        let writer = self.writers.entry(tag).or_insert_with(|| {
            BufWriter::new(
                File::create_new(self.root.join(format!("au{:04}.{tag}", self.au))).unwrap(),
            )
        });
        writer.write_all(bytes).unwrap();
    }
    fn f32(&mut self, tag: &'static str, values: impl IntoIterator<Item = f32>) {
        for x in values {
            assert!(x.is_finite());
            self.bytes(tag, &x.to_le_bytes());
        }
    }
    fn f64(&mut self, tag: &'static str, values: impl IntoIterator<Item = f64>) {
        for x in values {
            assert!(x.is_finite());
            self.bytes(tag, &x.to_le_bytes());
        }
    }
}
std::thread_local! { static LOCATION: RefCell<Option<Location>> = const { RefCell::new(None) }; }

pub(crate) fn select(root: PathBuf, au: usize, selected: bool) {
    finish_frame();
    LOCATION.with(|v| {
        *v.borrow_mut() = Some(Location {
            root,
            au,
            selected,
            slots: 0,
            count: 0,
            ts: 0,
            de: 0,
            calls: 0,
            writers: BTreeMap::new(),
        });
    });
}
pub(crate) fn frame(slots: usize, enabled: &[bool]) {
    LOCATION.with(|v| {
        if let Some(l) = v.borrow_mut().as_mut() {
            assert_eq!(l.slots, 0);
            l.slots = slots;
            l.count = enabled.len();
            let mut file = OpenOptions::new().create(true).append(true)
                .open(l.root.join("frames.jsonl")).unwrap();
            writeln!(file, "{{\"au\":{},\"slots\":{slots},\"instances\":{},\"enabled\":{enabled:?},\"selected\":{}}}",
                l.au, l.count, l.selected).unwrap();
        }
    });
}
pub(crate) fn instance(ts: usize, de: usize) {
    LOCATION.with(|v| {
        if let Some(l) = v.borrow_mut().as_mut() {
            assert!(ts < l.slots && de < l.count);
            l.ts = ts;
            l.de = de;
        }
    });
}
pub(crate) fn finish_frame() {
    LOCATION.with(|v| {
        if let Some(mut l) = v.borrow_mut().take() {
            assert_eq!(l.calls, l.slots * l.count);
            for writer in l.writers.values_mut() {
                writer.flush().unwrap();
            }
        }
    });
}

struct Details {
    before: Box<DecorrelatorState>,
    filtered: [[f64; 2]; SUBBANDS],
}
pub(super) struct Observation(Option<Details>);
impl Observation {
    pub(super) fn new(state: &DecorrelatorState) -> Self {
        Self(LOCATION.with(|v| {
            v.borrow().as_ref().filter(|l| l.selected).map(|_| Details {
                before: Box::new(state.clone()),
                filtered: [[0.0; 2]; SUBBANDS],
            })
        }))
    }
    pub(super) fn filtered(&mut self, sb: usize, value: Complex64) {
        if let Some(d) = self.0.as_mut() {
            d.filtered[sb] = [value.re, value.im];
        }
    }
    pub(super) fn finish(
        self,
        kind: DecorrelatorKind,
        input: &QmfSlot,
        energy: &[f32; DUCKER_BANDS],
        gain: &[f32; DUCKER_BANDS],
        state: &DecorrelatorState,
        output: &QmfSlot,
    ) {
        LOCATION.with(|v| {
            let mut binding = v.borrow_mut();
            let Some(l) = binding.as_mut() else { return };
            assert_eq!(l.calls, l.ts * l.count + l.de);
            l.calls += 1;
            l.bytes("kinds.u8", &[kind.column() as u8]);
            l.f32("u.f32", (0..64).flat_map(|b| [input.re[b], input.im[b]]));
            l.f32("y.f32", (0..64).flat_map(|b| [output.re[b], output.im[b]]));
            if let Some(d) = self.0 {
                l.f32("energy.f32", energy.iter().copied());
                l.f32("gain.f32", gain.iter().copied());
                l.f32(
                    "ducker_before.f32",
                    d.before
                        .ducker
                        .iter()
                        .flat_map(|s| [s.peak_decay, s.smooth, s.smooth_peak_diff]),
                );
                l.f32(
                    "ducker_after.f32",
                    state
                        .ducker
                        .iter()
                        .flat_map(|s| [s.peak_decay, s.smooth, s.smooth_peak_diff]),
                );
                l.f64("filtered.f64", d.filtered.into_iter().flatten());
                if l.ts == 0 {
                    l.f64(
                        "filter_before.f64",
                        d.before
                            .filters
                            .iter()
                            .flat_map(|s| s.cells.iter().flat_map(|x| [x.re, x.im])),
                    );
                }
                if l.ts + 1 == l.slots {
                    l.f64(
                        "filter_after.f64",
                        state
                            .filters
                            .iter()
                            .flat_map(|s| s.cells.iter().flat_map(|x| [x.re, x.im])),
                    );
                }
            }
        });
    }
}

#[test]
#[ignore = "owned u.f32 and frame sizes in an explicit external directory"]
fn replay_ajoc_decorrelator() {
    let root = PathBuf::from(std::env::var("MACINAC4_DECORR_REPLAY").unwrap());
    let count: usize = std::env::var("MACINAC4_DECORR_INSTANCES").map_or(7, |v| v.parse().unwrap());
    assert!((1..=7).contains(&count));
    let selected: Option<Vec<usize>> = std::env::var("MACINAC4_DECORR_DETAIL_AUS")
        .ok()
        .map(|v| v.split(',').map(|s| s.parse().unwrap()).collect());
    let frames: Vec<usize> = std::fs::read_to_string(root.join("frames.txt"))
        .unwrap()
        .split_whitespace()
        .map(|v| v.parse().unwrap())
        .collect();
    assert!(!frames.is_empty() && frames.iter().all(|n| (1..=32).contains(n)));
    let bytes = std::fs::read(root.join("input.f32")).unwrap();
    assert_eq!(
        bytes.len(),
        frames.iter().sum::<usize>() * count * 64 * 2 * 4
    );
    let input: Vec<f32> = bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect();
    let output = root.join("observed");
    std::fs::create_dir(&output).unwrap();
    let mut states = vec![DecorrelatorState::new(); count];
    let mut time = 0;
    for (index, &slots) in frames.iter().enumerate() {
        select(
            output.clone(),
            index + 1,
            selected
                .as_ref()
                .is_none_or(|values| values.contains(&(index + 1))),
        );
        frame(slots, &vec![true; count]);
        for ts in 0..slots {
            for (de, state) in states.iter_mut().enumerate() {
                let mut q = QmfSlot::zero();
                for b in 0..64 {
                    let p = ((time * count + de) * 64 + b) * 2;
                    q.re[b] = input[p];
                    q.im[b] = input[p + 1];
                }
                instance(ts, de);
                process_timeslot(
                    kind_for_ajoc_index(de).unwrap(),
                    &q,
                    state,
                    &mut QmfSlot::zero(),
                )
                .unwrap();
            }
            time += 1;
        }
        finish_frame();
    }
}
