//! 连续解码真实流，在路由前和矩阵消费者处分别观察输入。
//! 大文件只写到显式给定的新目录；公共接口及产品数值路径不变。
#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    reason = "测试先约束真实帧布局、通道数和观察范围，再按有限矩阵访问"
)]
extern crate std;

use super::*;
use crate::full_ajoc::input_probe;
use crate::substream_audio::AjocSubstreamContext;
use macindecode_ac4_bitstream::{
    substream::SubstreamInfo, syncframe::SyncFrameIter, topology::Ac4Topology,
};
use std::{
    collections::BTreeSet,
    fs::File,
    io::{BufWriter, Write},
    path::Path,
    vec,
};

fn write_floats(path: &Path, values: impl IntoIterator<Item = f32>) {
    let mut file = BufWriter::new(File::create_new(path).unwrap());
    append_floats(&mut file, values);
    file.flush().unwrap();
}

fn append_floats(file: &mut impl Write, values: impl IntoIterator<Item = f32>) {
    for value in values {
        assert!(value.is_finite());
        file.write_all(&value.to_le_bytes()).unwrap();
    }
}

fn save_qmf(path: &Path, frame: &[crate::aspx::qmf::QmfSlot]) {
    write_floats(
        path,
        frame
            .iter()
            .flat_map(|q| (0..64).flat_map(move |b| [q.re[b], q.im[b]])),
    );
}

fn equal_qmf(a: &[crate::aspx::qmf::QmfSlot], b: &[crate::aspx::qmf::QmfSlot]) {
    assert_eq!(a.len(), b.len());
    for (left, right) in a.iter().zip(b) {
        for (x, y) in left
            .re
            .iter()
            .chain(&left.im)
            .zip(right.re.iter().chain(&right.im))
        {
            assert_eq!(
                x.to_bits(),
                y.to_bits(),
                "路由到消费者之间不得重缩放或改写 QMF"
            );
        }
    }
}

// P2 5.7.2.1 / P14a 的独立下标公式，不调用 ajoc_input_order、aspx_jobs 或 ajoc_slots。
fn expected_sources(fullband: usize, lfe: bool) -> Vec<usize> {
    let offset = if fullband > 3 { 2 + fullband % 2 } else { 0 };
    (fullband - offset..fullband)
        .chain(0..fullband - offset)
        .map(|index| index + usize::from(lfe))
        .collect()
}

#[test]
fn input_boundary_expected_routes_are_explicit_bijections() {
    assert_eq!(expected_sources(1, false), [0]);
    assert_eq!(expected_sources(3, true), [1, 2, 3]);
    assert_eq!(expected_sources(4, true), [3, 4, 1, 2]);
    assert_eq!(expected_sources(9, true), [7, 8, 9, 1, 2, 3, 4, 5, 6]);
    for n in 1..=16 {
        for lfe in [false, true] {
            assert_eq!(
                expected_sources(n, lfe)
                    .into_iter()
                    .collect::<BTreeSet<_>>(),
                (usize::from(lfe)..n + usize::from(lfe)).collect()
            );
            let element = VarChannelElement::for_test(true, Some(false), n as u8, lfe, &[]);
            let transmitted: Vec<_> = element.signals().collect();
            let actual: Vec<_> = element
                .ajoc_input_order()
                .map(|signal| {
                    transmitted
                        .iter()
                        .position(|candidate| *candidate == signal)
                        .unwrap()
                })
                .collect();
            assert_eq!(actual, expected_sources(n, lfe));
        }
    }
}

#[test]
#[ignore = "requires complete local sync stream and explicit new output directory"]
fn capture_ajoc_input_boundary() {
    std::thread::Builder::new()
        .stack_size(32 * 1024 * 1024)
        .spawn(capture)
        .unwrap()
        .join()
        .unwrap();
}

fn capture() {
    let input = std::env::var("MACINAC4_AJOC_INPUT_STREAM").unwrap();
    let destination = std::env::var("MACINAC4_AJOC_INPUT_OUTPUT").unwrap();
    let selected: BTreeSet<usize> = std::env::var("MACINAC4_AJOC_INPUT_AUS")
        .unwrap()
        .split(',')
        .map(|part| part.parse().unwrap())
        .collect();
    assert!(!selected.is_empty() && !selected.contains(&0));
    let destination = Path::new(&destination);
    std::fs::create_dir(destination).unwrap();
    let matrix_capture = std::env::var_os("MACINAC4_AJOC_MATRIX").is_some();
    if matrix_capture {
        std::fs::create_dir(destination.join("matrix")).unwrap();
    }
    let data = std::fs::read(input).unwrap();
    let mut decoder = FullAjocDecoder::new();
    let mut group_oamd = OamdState::new();
    let mut group_common = None;
    let mut core_file = BufWriter::new(File::create_new(destination.join("core.f32")).unwrap());
    let mut object_file =
        BufWriter::new(File::create_new(destination.join("objects.f32")).unwrap());
    let mut rows = BufWriter::new(File::create_new(destination.join("frames.jsonl")).unwrap());
    let mut identities = VecDeque::new();
    let mut previous_pcm: Vec<Vec<f32>> = Vec::new();
    let mut count = 0;
    let mut shape = None;
    let mut sample_start = 0i64;
    let mut observed = BTreeSet::new();
    let mut previous_state = String::new();
    write_floats(
        &destination.join("window.f32"),
        crate::aspx::qmf::QMF_WINDOW,
    );
    for (index, result) in SyncFrameIter::new(&data).enumerate() {
        let frame = result.unwrap();
        assert_ne!(frame.verify_crc(&data), Some(false));
        let topology = Ac4Topology::parse(frame.raw_frame).unwrap();
        assert_eq!(topology.presentations().len(), 1);
        assert!(
            !topology.presentations()[0]
                .substream
                .is_some_and(|s| s.alternative),
            "此调查入口不推断 alternative presentation 的上下文"
        );
        assert_eq!(topology.groups().len(), 1);
        let group = &topology.groups()[0];
        let mut timing_updated = false;
        let mut common_updated = false;
        if let Some(reference) = group.oamd_substream {
            use macindecode_ac4_bitstream::oamd::{
                OamdContext, OamdSubstreamPayload, ObjectDescriptors,
            };
            let descriptors = ObjectDescriptors::from_group(group).unwrap();
            let metadata = OamdSubstreamPayload::parse(
                topology
                    .substream_payload(frame.raw_frame, reference.substream_index.unwrap())
                    .unwrap(),
                OamdContext {
                    objects: descriptors.as_slice(),
                    b_alternative: false,
                    b_oamd_ndot: reference.ndot,
                    previous_num_obj_info_blocks: group_oamd.previous_num_obj_info_blocks(),
                },
            )
            .unwrap();
            timing_updated = metadata.timing.is_some();
            common_updated = metadata.common.is_some();
            if let Some(common) = metadata.common {
                group_common = Some(common);
            }
            group_oamd.apply(&metadata).unwrap();
        }
        let mut infos = topology.groups().iter().flat_map(|g| {
            assert_eq!(g.frame_rate_factor, 1);
            g.substreams().iter().filter_map(|info| match info {
                SubstreamInfo::Ajoc(value) => Some(value),
                _ => None,
            })
        });
        let info = infos.next().unwrap();
        assert!(infos.next().is_none());
        let substream = info.substream_index().unwrap();
        if let Some(common) = info.oamd_common_data {
            group_common = Some(common);
            common_updated = true;
        }
        let timing = group_oamd.effective_timing();
        let context = AjocSubstreamContext::derive(
            &topology.toc,
            info,
            1,
            1,
            false,
            timing.map(|t| t.num_obj_info_blocks),
        )
        .unwrap();
        let length = usize::from(context.params.context.frame_len_base);
        let (delay, control_delay) = match length {
            2048 => (352, 1),
            1920 => (288, 1),
            1536 => (96, 1),
            _ => panic!("本调查仅覆盖 Full 已支持的长帧"),
        };
        let before = decoder
            .substreams
            .get(substream as usize)
            .map(|s| format!("{:?}", s.states))
            .unwrap_or_default();
        assert_eq!(
            before, previous_state,
            "连续帧入口必须保留上一帧的完整 A-SPX 历史"
        );
        input_probe::begin();
        if matrix_capture {
            crate::ajoc::reconstruction::matrix_probe::select(
                destination.join("matrix"),
                index + 1,
                selected.contains(&(index + 1)),
            );
        }
        let decoded = decoder
            .decode_complete_audio_frame(FullAjocAudioFrameInput {
                syntax: FullAjocSyntaxFrameInput {
                    payload: topology
                        .substream_payload(frame.raw_frame, substream)
                        .unwrap(),
                    context,
                    substream_index: substream,
                    physical_substreams: 1,
                },
                provenance: FullAjocFrameProvenance::new(index as u64)
                    .with_source_sample_start(sample_start)
                    .try_with_group_oamd_state(FullAjocGroupOamdState::new(
                        0,
                        group_common,
                        common_updated,
                        timing,
                        timing_updated,
                    ))
                    .unwrap(),
                lfe_position: info.lfe_reinsertion_position(),
                mode: FullAjocDecodeMode::RequireFull,
            })
            .unwrap();
        let syntax = decoded.frontend().syntax().observation();
        let parsed = syntax.parsed();
        let element = parsed.audio.var_element;
        let fullband = usize::from(element.n_dmx_signals);
        let channels = fullband + usize::from(element.b_has_lfe);
        let current_shape = (length, channels, fullband, substream);
        assert!(shape.is_none() || shape == Some(current_shape));
        shape = Some(current_shape);
        let raw_aspx =
            input_probe::aspx_identity(&element, syntax.aspx(), syntax.aspx_config().as_ref());
        let raw_ajoc = input_probe::ajoc_identity(
            &parsed.audio.ajoc,
            syntax.object_controls(),
            syntax.matrices(),
        );
        let raw_bands: Vec<_> = element
            .signals()
            .map(|signal| {
                signal.aspx.map_or(64, |(element, _)| {
                    usize::from(syntax.aspx()[usize::from(element)].bands.sbx())
                })
            })
            .collect();
        identities.push_back((index, raw_aspx, raw_ajoc, raw_bands));
        let core: Vec<Vec<f32>> = decoded.frontend().asf().pcm().to_vec();
        assert_eq!(core.len(), channels);
        append_floats(&mut core_file, core.iter().flatten().copied());
        for sample in 0..length {
            for channel in 0..decoded.output().reconstructed_channels() {
                let pcm = decoded.output().reconstructed_channel(channel).unwrap();
                append_floats(&mut object_file, [pcm.samples()[sample]]);
            }
        }
        let source_au = decoded
            .output()
            .aligned_side_information()
            .map(|s| s.provenance().access_unit_index());
        let reconstructed = decoded.output().observation().reconstructed;
        let wet = decoded.output().observation().wet;
        let record = input_probe::take();
        let scratch = &decoder.workspace;
        for channel in 0..channels {
            let mut expected = if previous_pcm.is_empty() {
                vec![0.0; delay]
            } else {
                previous_pcm[channel][length - delay..].to_vec()
            };
            expected.extend_from_slice(&core[channel][..length - delay]);
            assert_eq!(
                expected.iter().map(|x| x.to_bits()).collect::<Vec<_>>(),
                scratch.aligned[channel]
                    .iter()
                    .map(|x| x.to_bits())
                    .collect::<Vec<_>>(),
                "表 188 必须按样本延迟当前 ASF 输出"
            );
        }
        assert_eq!(record.signals.len(), channels);
        let mut signals = record.signals;
        signals.sort_by_key(|s| s.0);
        assert_eq!(
            signals.iter().map(|s| s.0).collect::<Vec<_>>(),
            (0..channels).collect::<Vec<_>>()
        );
        let order = expected_sources(fullband, element.b_has_lfe);
        for (target, source) in order.iter().enumerate() {
            equal_qmf(&signals[*source].1, &scratch.out[target][..length / 64]);
        }
        if element.b_has_lfe {
            equal_qmf(&signals[0].1, &scratch.lfe[..length / 64]);
        }
        let due = if index >= control_delay {
            Some(identities.pop_front().unwrap())
        } else {
            None
        };
        if let Some((origin, expected_aspx, expected_ajoc, _)) = &due {
            assert_eq!(*origin, index - control_delay);
            assert_eq!(source_au, Some(*origin as u64));
            assert_eq!(record.applied.as_ref(), Some(expected_aspx));
            let (consumed, slots, input) = record.consumed.as_ref().unwrap();
            assert_eq!(
                consumed, expected_ajoc,
                "矩阵消费者必须使用到期帧的完整控制和量化矩阵"
            );
            assert_eq!(usize::from(*slots), length / 64);
            assert_eq!(input.len(), fullband);
            for (target, source) in order.iter().enumerate() {
                equal_qmf(&signals[*source].1, &input[target][..length / 64]);
            }
            assert!(reconstructed);
        } else {
            assert!(
                source_au.is_none()
                    && record.applied.is_none()
                    && record.consumed.is_none()
                    && !reconstructed
            );
        }
        let drive = &decoder.substreams[substream as usize];
        let after = format!("{:?}", drive.states);
        if selected.contains(&(index + 1)) {
            assert!(due.is_some(), "选定窗口必须有到期控制");
            observed.insert(index + 1);
            let prefix = format!("au{:04}", index + 1);
            for (source, qmf) in &signals {
                save_qmf(
                    &destination.join(format!("{prefix}.source{source}.f32")),
                    qmf,
                );
            }
            for (target, input) in record.consumed.as_ref().unwrap().2.iter().enumerate() {
                save_qmf(
                    &destination.join(format!("{prefix}.input{target}.f32")),
                    &input[..length / 64],
                );
            }
            if element.b_has_lfe {
                save_qmf(
                    &destination.join(format!("{prefix}.lfe.f32")),
                    &scratch.lfe[..length / 64],
                );
            }
            std::fs::write(
                destination.join(format!("{prefix}.state_before.txt")),
                &before,
            )
            .unwrap();
            std::fs::write(
                destination.join(format!("{prefix}.state_after.txt")),
                &after,
            )
            .unwrap();
            let (_, expected_aspx, expected_ajoc, _) = due.as_ref().unwrap();
            for (tag, value) in [
                ("raw_aspx", expected_aspx),
                ("applied_aspx", record.applied.as_ref().unwrap()),
                ("raw_ajoc", expected_ajoc),
                ("consumed_ajoc", &record.consumed.as_ref().unwrap().0),
            ] {
                std::fs::write(destination.join(format!("{prefix}.{tag}.txt")), value).unwrap();
            }
        }
        let bands = due.as_ref().map(|v| &v.3);
        writeln!(rows, "{{\"au\":{},\"source_au\":{},\"samples\":{},\"channels\":{},\"fullband\":{},\"lfe\":{},\"substream\":{},\"pcm_delay\":{},\"control_delay\":{},\"source_order\":{:?},\"crossover\":{},\"selected\":{},\"reconstructed\":{},\"wet\":{}}}", index+1, source_au.map_or_else(|| String::from("null"), |i| format!("{}", i+1)), length, channels, fullband, element.b_has_lfe, substream, delay, control_delay, order, bands.map_or_else(|| String::from("null"), |v| format!("{v:?}")), selected.contains(&(index+1)), reconstructed, wet).unwrap();
        previous_state = after;
        previous_pcm = core;
        sample_start += length as i64;
        count += 1;
    }
    assert_eq!(selected, observed);
    assert!(count > 1);
    core_file.flush().unwrap();
    object_file.flush().unwrap();
    rows.flush().unwrap();
    std::println!(
        "A-JOC input boundary: {count} complete AUs, {} selected",
        selected.len()
    );
}
