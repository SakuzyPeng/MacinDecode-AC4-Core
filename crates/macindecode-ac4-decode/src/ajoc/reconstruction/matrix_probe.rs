//! 真实生产矩阵边界的测试观察器；所有文件都由显式测试入口选择。
#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::too_many_arguments,
    reason = "测试只遍历已经由 reconstruction 验证的有限形状，不改变数值状态"
)]
extern crate std;
use super::*;
use std::{
    cell::RefCell,
    fs::{File, OpenOptions},
    io::{BufWriter, Write},
    path::PathBuf,
};
use std::{format, vec::Vec};

struct Location {
    directory: PathBuf,
    au: usize,
    selected: bool,
}
std::thread_local! { static LOCATION: RefCell<Option<Location>> = const { RefCell::new(None) }; }
pub(crate) fn select(directory: PathBuf, au: usize, selected: bool) {
    LOCATION.with(|cell| {
        *cell.borrow_mut() = Some(Location {
            directory,
            au,
            selected,
        })
    });
}
pub(super) fn clear() {
    LOCATION.with(|cell| *cell.borrow_mut() = None);
}
fn file(location: &Location, tag: &str) -> PathBuf {
    location
        .directory
        .join(format!("au{:04}.{tag}", location.au))
}
fn floats(location: &Location, tag: &str, append: bool, values: impl IntoIterator<Item = f32>) {
    let path = file(location, tag);
    let f = if append {
        OpenOptions::new().append(true).open(path).unwrap()
    } else {
        File::create_new(path).unwrap()
    };
    let mut stream = BufWriter::new(f);
    for value in values {
        assert!(value.is_finite());
        stream.write_all(&value.to_le_bytes()).unwrap();
    }
    stream.flush().unwrap();
}
fn qmf(slot: &QmfSlot) -> impl Iterator<Item = f32> + '_ {
    (0..64).flat_map(move |b| [slot.re[b], slot.im[b]])
}
fn indices(d: FrameDimensions) -> [Vec<usize>; 3] {
    [
        (0..d.objects)
            .flat_map(|o| {
                (0..d.num_dmx).flat_map(move |c| (0..64).map(move |b| dry_rolling_index(o, c, b)))
            })
            .collect(),
        (0..d.objects)
            .flat_map(|o| {
                (0..d.num_decorr)
                    .flat_map(move |c| (0..64).map(move |b| wet_rolling_index(o, c, b)))
            })
            .collect(),
        (0..d.num_decorr)
            .flat_map(|o| {
                (0..d.num_dmx).flat_map(move |c| (0..64).map(move |b| pre_rolling_index(o, c, b)))
            })
            .collect(),
    ]
}
fn state_values(d: FrameDimensions, state: &AjocReconstructionState) -> Vec<f32> {
    let index = indices(d);
    let groups = [&state.dry, &state.wet, &state.pre];
    let mut out = Vec::new();
    for (ids, group) in index.iter().zip(groups) {
        for &i in ids {
            let v = group[i];
            out.extend([v.current(), v.target(), v.delta()]);
        }
    }
    out
}
pub(super) fn start(
    ajoc: &Ajoc,
    controls: &[AjocObjectControl],
    raw: &[AjocObjectMatrix],
    d: FrameDimensions,
    input: &[QmfChannelFrame],
    state: &AjocReconstructionState,
) {
    LOCATION.with(|cell| { let binding=cell.borrow();let Some(location)=binding.as_ref() else{return};
        let mut objects=Vec::new();
        for o in 0..d.objects {
            let c=&controls[o];let mut kinds=Vec::new();
            for (kind,rows) in [(MatrixKind::Dry,d.num_dmx),(MatrixKind::Wet,d.num_decorr)] {
                let mut points=Vec::new();
                if c.present { for dp in 0..d.num_dpoints {
                    let mut entries=Vec::new();
                    for row in 0..rows {
                        let present=match kind {MatrixKind::Dry=>c.dry_present(row),MatrixKind::Wet=>!c.sparse||c.wet_present(row)};
                        let time=present && match kind { MatrixKind::Dry=>raw[o].dry_time_direction(dp,row).unwrap(),MatrixKind::Wet=>raw[o].wet_time_direction(dp,row).unwrap() };
                        let values:Vec<_>=if present {(0..usize::from(c.num_bands)).map(|b| match kind {MatrixKind::Dry=>raw[o].dry(dp,row,b).unwrap(),MatrixKind::Wet=>raw[o].wet(dp,row,b).unwrap()}).collect()} else {Vec::new()};
                        entries.push(format!("{{\"present\":{present},\"time\":{time},\"values\":{values:?}}}"));
                    }
                    points.push(format!("[{}]",entries.join(",")));
                } }
                kinds.push(format!("[{}]",points.join(",")));
            }
            objects.push(format!("{{\"present\":{},\"bands\":{},\"coarse\":{},\"sparse\":{},\"dry\":{},\"wet\":{}}}",c.present,c.num_bands,c.coarse,c.sparse,kinds[0],kinds[1]));
        }
        let starts:Vec<_>=(0..d.num_dpoints).map(|p| ajoc.data_points.start_pos(p).unwrap()).collect();
        let ramps:Vec<_>=(0..d.num_dpoints).map(|p| ajoc.data_points.ramp_len_minus1(p).unwrap()+1).collect();
        let enabled=&d.decorr_enabled[..d.num_decorr];
        let line=format!("{{\"au\":{},\"slots\":{},\"dmx\":{},\"decorr\":{},\"enabled\":{enabled:?},\"starts\":{starts:?},\"ramps\":{ramps:?},\"selected\":{},\"cursor_before\":[{},{}],\"objects\":[{}]}}\n",location.au,d.timeslots,d.num_dmx,d.num_decorr,location.selected,state.interpolation.completed(),state.interpolation.ramp_len(),objects.join(","));
        let mut stream=OpenOptions::new().create(true).append(true).open(location.directory.join("frames.jsonl")).unwrap();stream.write_all(line.as_bytes()).unwrap();
        if location.selected {
            floats(location,"before.f32",false,state_values(d,state));
            floats(location,"x.f32",false,input.iter().take(d.num_dmx).flat_map(|frame| frame[..d.timeslots].iter().flat_map(qmf)));
        }
    });
}
pub(super) fn targets(
    controls: &[AjocObjectControl],
    d: FrameDimensions,
    maps: &[Option<AjocBandMap>; MAX_RECONSTRUCTED_OBJECTS],
    workspace: &AjocWorkspace,
) {
    LOCATION.with(|cell| {
        let binding = cell.borrow();
        let Some(l) = binding.as_ref().filter(|l| l.selected) else {
            return;
        };
        let mut out = Vec::new();
        let index = indices(d);
        for dp in 0..d.num_dpoints {
            for (group, ids) in [
                CoefficientGroup::Dry,
                CoefficientGroup::Wet,
                CoefficientGroup::Pre,
            ]
            .into_iter()
            .zip(&index)
            {
                for &i in ids {
                    out.push(target_for(
                        group,
                        dp,
                        i,
                        controls,
                        d,
                        maps,
                        &workspace.dry_targets,
                        &workspace.wet_targets,
                        &workspace.pre_targets,
                    ));
                }
            }
        }
        floats(l, "targets.f32", false, out);
    });
}
pub(super) fn slot(
    ts: usize,
    d: FrameDimensions,
    dry: &[RollingCoefficient],
    wet: &[RollingCoefficient],
    pre: &[RollingCoefficient],
    u: &[QmfSlot],
    y: &[QmfSlot],
    out: &[QmfChannelFrame],
) {
    LOCATION.with(|cell| {
        let binding = cell.borrow();
        let Some(l) = binding.as_ref().filter(|l| l.selected) else {
            return;
        };
        let mut values = Vec::new();
        for (ids, group) in indices(d).iter().zip([dry, wet, pre]) {
            for &i in ids {
                values.push(group[i].current());
            }
        }
        floats(l, "coeff.f32", ts != 0, values);
        floats(l, "u.f32", ts != 0, u[..d.num_decorr].iter().flat_map(qmf));
        floats(l, "y.f32", ts != 0, y[..d.num_decorr].iter().flat_map(qmf));
        floats(
            l,
            "z.f32",
            ts != 0,
            out[..d.objects].iter().flat_map(|frame| qmf(&frame[ts])),
        );
    });
}
pub(super) fn finish(d: FrameDimensions, state: &AjocReconstructionState) {
    LOCATION.with(|cell| {
        let binding = cell.borrow();
        let Some(l) = binding.as_ref().filter(|l| l.selected) else {
            return;
        };
        floats(l, "after.f32", false, state_values(d, state));
        std::fs::write(
            file(l, "cursor_after.json"),
            format!(
                "[{},{}]\n",
                state.interpolation.completed(),
                state.interpolation.ramp_len()
            ),
        )
        .unwrap();
    });
}

#[test]
#[ignore = "exports owned mixed-grid and cross-frame matrix fixtures to an explicit new directory"]
fn replay_ajoc_matrix_synthetic() {
    std::thread::Builder::new()
        .stack_size(32 * 1024 * 1024)
        .spawn(|| {
            let root = PathBuf::from(std::env::var("MACINAC4_MATRIX_SYNTHETIC_OUTPUT").unwrap());
            std::fs::create_dir(&root).unwrap();
            for case in 0..24usize {
                let directory = root.join(format!("case{case:02}"));
                std::fs::create_dir(&directory).unwrap();
                let slots = [24usize, 30, 32][case % 3];
                let objects = 3 + case % 2;
                let inputs = 2 + case % 2;
                let decorr = if case % 8 == 0 { 0 } else { 2 };
                let ramp = [1, 3, 7, 31, 32, 63, 64, 5][case % 8];
                let mut state = AjocReconstructionState::new();
                let mut workspace = AjocWorkspace::new();
                let mut history =
                    std::collections::BTreeMap::<(usize, usize, usize), Vec<i16>>::new();
                for frame in 0..5 {
                    let points: Vec<(u8, u8)> = match frame {
                        0 => std::vec![(0, ramp), (slots as u8 - 1, 64)],
                        1 => Vec::new(),
                        2 => std::vec![(1, 3)],
                        3 => std::vec![(0, 7)],
                        _ => std::vec![(0, 32)],
                    };
                    let mut ajoc = Ajoc::for_test(
                        decorr as u8,
                        points.len() as u8,
                        objects as u32,
                        inputs as u8,
                    );
                    ajoc.data_points =
                        crate::ajoc::syntax::AjocDataPoints::with_points_for_test(&points);
                    for de in 0..decorr {
                        ajoc.set_decorr_enable_for_test(de, (de + frame + case) % 3 != 0);
                    }
                    let mut controls = Vec::new();
                    let mut raw = Vec::new();
                    for o in 0..objects {
                        let grids = [1u8, 3, 5, 7, 9, 12, 15, 23];
                        let bands =
                            grids[(case + o + usize::from(frame >= 2 && case % 4 == 0)) % 8];
                        let present = !(o == 1 && (frame == 1 || frame == 3));
                        let coarse = (case + o) % 2 == 0;
                        let sparse = case % 2 == 0;
                        let dry_mask: Vec<_> =
                            (0..inputs).map(|c| (c + o + frame) % 3 != 0).collect();
                        let wet_mask: Vec<_> =
                            (0..decorr).map(|c| (c + o + frame) % 3 != 1).collect();
                        let control = AjocObjectControl::for_test(
                            present, bands, coarse, sparse, &dry_mask, &wet_mask,
                        );
                        let mut matrix = AjocObjectMatrix::for_test(
                            bands,
                            points.len() as u8,
                            inputs as u8,
                            decorr as u8,
                        );
                        if present {
                            for dp in 0..points.len() {
                                for (kind, nrows, mid) in [
                                    (0, inputs, if coarse { 25 } else { 50 }),
                                    (1, decorr, if coarse { 10 } else { 20 }),
                                ] {
                                    for ch in 0..nrows {
                                        let transmitted = if kind == 0 {
                                            control.dry_present(ch)
                                        } else {
                                            !sparse || control.wet_present(ch)
                                        };
                                        let key = (o, kind, ch);
                                        let time = (frame == 0 && dp == 1) || frame == 4;
                                        let values: Vec<i16> = (0..usize::from(bands))
                                            .map(|b| {
                                                mid + ((case + o * 7 + ch * 3 + b * 2 + frame + dp)
                                                    % 9)
                                                    as i16
                                                    - 4
                                            })
                                            .collect();
                                        let symbols = if !transmitted {
                                            std::vec![0;usize::from(bands)]
                                        } else if time {
                                            let previous = history.get(&key).unwrap();
                                            assert_eq!(previous.len(), values.len());
                                            values
                                                .iter()
                                                .zip(previous)
                                                .map(|(a, b)| a - b)
                                                .collect()
                                        } else {
                                            values
                                                .iter()
                                                .enumerate()
                                                .map(|(b, v)| {
                                                    if b == 0 {
                                                        *v
                                                    } else {
                                                        (*v - values[b - 1]).rem_euclid(2 * mid + 1)
                                                    }
                                                })
                                                .collect()
                                        };
                                        if kind == 0 {
                                            matrix.set_dry_for_test(dp, ch, &symbols, time)
                                        } else {
                                            matrix.set_wet_for_test(dp, ch, &symbols, time)
                                        };
                                        history.insert(
                                            key,
                                            if transmitted {
                                                values
                                            } else {
                                                std::vec![mid;usize::from(bands)]
                                            },
                                        );
                                    }
                                }
                            }
                        }
                        controls.push(control);
                        raw.push(matrix);
                    }
                    let mut input = std::vec![empty_channel_frame();inputs];
                    let mut random = 193u32 + (case as u32) * 101 + frame as u32;
                    for channel in &mut input {
                        for q in channel.iter_mut().take(slots) {
                            for value in q.re.iter_mut().chain(&mut q.im) {
                                random = random.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                                *value = ((random >> 8) as f32 / 16_777_216.0 - 0.5) * 0.25;
                            }
                        }
                    }
                    let mut output = std::vec![empty_channel_frame();objects];
                    select(directory.clone(), frame + 1, true);
                    reconstruct_frame(
                        &ajoc,
                        &controls,
                        &raw,
                        slots as u8,
                        &input,
                        &mut state,
                        &mut workspace,
                        &mut output,
                    )
                    .unwrap();
                }
            }
            LOCATION.with(|cell| *cell.borrow_mut() = None);
        })
        .unwrap()
        .join()
        .unwrap();
}
