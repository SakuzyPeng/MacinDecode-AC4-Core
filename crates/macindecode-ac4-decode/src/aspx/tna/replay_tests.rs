//! 固定低带、历史及控制状态的离线重放；只调用现有生产原语。
#![allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "测试入口先验证固定矩阵、频带、状态及区间，再使用有界下标"
)]
extern crate std;
use super::*;
use crate::aspx::bands::AspxBandTables;
use crate::aspx::hfgen::hf_generate;
use crate::aspx::patches::PatchTable;
use crate::aspx::preflatten::{PreFlattenGains, pre_flatten};
use std::collections::BTreeSet;
use std::format;
use std::io::Write;
use std::path::Path;
use std::string::{String, ToString};
use std::vec;
use std::vec::Vec;

struct Parameters {
    first: usize,
    last: usize,
    bands: AspxBandTables,
    patches: PatchTable,
    modes: Vec<u8>,
    previous_modes: Vec<u8>,
    previous: Vec<f32>,
    borders: Vec<u8>,
}

fn parameters(raw: &[u8]) -> Result<Parameters, String> {
    if raw.len() != 176 {
        return Err("parameter length".to_string());
    }
    let word = |i: usize| u32::from_le_bytes(raw[4 * i..4 * i + 4].try_into().unwrap());
    let byte = |i: usize| u8::try_from(word(i)).map_err(|_| "parameter range".to_string());
    let first = word(0) as usize;
    let last = word(1) as usize;
    let groups = word(10) as usize;
    let count = word(11) as usize;
    if word(2) != 16
        || word(3) != 2
        || word(4) > 1
        || word(5) > 1
        || first >= last
        || last > 38
        || !first.is_multiple_of(2)
        || !last.is_multiple_of(2)
        || !(1..=5).contains(&groups)
        || !(1..=5).contains(&count)
    {
        return Err("parameter geometry".to_string());
    }
    let master = word(5) != 0;
    let crossover = byte(14)?;
    let xover = byte(9)?;
    if xover != 255 && xover > 7 {
        return Err("crossover range".to_string());
    }
    let start = byte(6)?;
    let stop = byte(7)?;
    let noise = byte(8)?;
    let candidates: Vec<_> = (0..=7)
        .filter(|&x| xover == 255 || xover == x)
        .filter_map(|x| AspxBandTables::derive(master, start, stop, noise, x).ok())
        .filter(|b| b.sbx() == crossover)
        .collect();
    if candidates.len() != 1 {
        return Err("crossover not unique".to_string());
    }
    let bands = candidates.into_iter().next().unwrap();
    let patches = PatchTable::derive(&bands, master, true).map_err(|e| format!("{e:?}"))?;
    if count != usize::from(patches.count())
        || groups != usize::from(bands.num_sbg_noise())
        || bands.sbz() != byte(15)?
        || crossover > 40
        || byte(13)? > bands.sba()
    {
        return Err("derived table shape".to_string());
    }
    let mut borders = Vec::new();
    for i in 0..=groups {
        let value = byte(16 + i)?;
        if bands.noise_border(i) != Some(value) {
            return Err("noise borders".to_string());
        }
        borders.push(value);
    }
    for i in 0..=count {
        if patches.border(i) != Some(byte(22 + i)?) {
            return Err("patch target".to_string());
        }
    }
    let mut lower = 64;
    let mut upper = 0;
    for i in 0..count {
        let source = byte(28 + i)?;
        if patches.start_sb(i) != Some(source) {
            return Err("patch source".to_string());
        }
        lower = lower.min(source);
        upper = upper.max(source + patches.num_sb(i).unwrap());
    }
    if lower != byte(12)? || upper != byte(13)? {
        return Err("source interval".to_string());
    }
    let packed = u64::from_le_bytes(raw[136..144].try_into().unwrap());
    let previous_packed = u64::from_le_bytes(raw[144..152].try_into().unwrap());
    let mut modes = Vec::new();
    let mut previous_modes = Vec::new();
    let mut previous = Vec::new();
    for g in 0..groups {
        modes.push(((packed >> (62 - 2 * g)) & 3) as u8);
        previous_modes.push(((previous_packed >> (62 - 2 * g)) & 3) as u8);
        let value = f32::from_le_bytes(raw[152 + 4 * g..156 + 4 * g].try_into().unwrap());
        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
            return Err("previous chirp".to_string());
        }
        previous.push(value);
    }
    Ok(Parameters {
        first,
        last,
        bands,
        patches,
        modes,
        previous_modes,
        previous,
        borders,
    })
}

fn floats(path: &Path, count: usize) -> Vec<f32> {
    let raw = std::fs::read(path).unwrap();
    assert_eq!(raw.len(), count * 4);
    let values: Vec<_> = raw
        .as_chunks::<4>()
        .0
        .iter()
        .map(|&v| f32::from_le_bytes(v))
        .collect();
    assert!(values.iter().all(|v| v.is_finite()));
    values
}

fn slots(raw: &[f32], width: usize) -> Vec<QmfSlot> {
    assert_eq!(raw.len() % (width * 2), 0);
    raw.chunks_exact(width * 2)
        .map(|values| {
            let mut slot = QmfSlot::zero();
            for sb in 0..width {
                slot.re[sb] = values[2 * sb];
                slot.im[sb] = values[2 * sb + 1];
            }
            slot
        })
        .collect()
}

fn save(path: &Path, values: impl IntoIterator<Item = f32>) {
    let mut file = std::fs::File::create_new(path).unwrap();
    for value in values {
        assert!(value.is_finite());
        file.write_all(&value.to_le_bytes()).unwrap();
    }
}

fn save_filters(path: &Path, filters: &TnaFilters) {
    save(
        path,
        (0..64).flat_map(|b| {
            let (a0, a1) = filters.coefficients(b).unwrap_or_default();
            [a0.re, a0.im, a1.re, a1.im]
        }),
    );
}

fn from_values(c: &[f32]) -> Covariance {
    assert_eq!(c.len(), 9);
    let c12 = Complex::new(f64::from(c[5]), f64::from(c[6]));
    Covariance {
        entries: [
            [
                Complex::new(f64::from(c[0]), f64::from(c[1])),
                Complex::new(f64::from(c[2]), f64::from(c[3])),
            ],
            [Complex::new(f64::from(c[4]), 0.0), c12],
            [c12.conj(), Complex::new(f64::from(c[7]), 0.0)],
        ],
    }
}

#[test]
#[ignore = "requires normalized local HF fixtures and independent reference primitive outputs"]
fn replay_fixed_hf_inputs() {
    let data = std::env::var("MACINAC4_HF_REPLAY_DATA").unwrap();
    let native = std::env::var("MACINAC4_HF_REPLAY_NATIVE").unwrap();
    let output = std::env::var("MACINAC4_HF_REPLAY_OUTPUT").unwrap();
    let (data, native, output) = (Path::new(&data), Path::new(&native), Path::new(&output));
    std::fs::create_dir(output).unwrap();
    // 缺标记的旧文件固定按旧式解释，不能随着生产变化重释历史实验。
    std::fs::write(
        output.join("replay_metadata.json"),
        "{\"schema\":\"macinac4.hf-replay\",\"version\":1,\"preflatten_gain_convention\":\"slope_minus_mean\"}\n",
    ).unwrap();
    let manifest = std::fs::read_to_string(data.join("hf_cases.txt")).unwrap();
    let mut ids = BTreeSet::new();
    for id in manifest.lines() {
        assert!(!id.is_empty() && id.len() <= 63 && id.bytes().all(|x| x.is_ascii_alphanumeric()));
        assert!(ids.insert(id.to_string()));
        let p = parameters(&std::fs::read(data.join(format!("{id}.param"))).unwrap()).unwrap();
        let low = slots(&floats(&data.join(format!("{id}.qmf.f32")), 40 * 128), 64);
        let history = slots(&floats(&data.join(format!("{id}.history.f32")), 4 * 80), 40);
        let alpha = floats(&native.join(format!("{id}.alpha.f32")), 64 * 4);
        let reference_chirp = floats(&native.join(format!("{id}.chirp.f32")), 5);
        let reference_cov = floats(&native.join(format!("{id}.cov.f32")), 64 * 9);
        let mut supplied_filters = TnaFilters::new();
        supplied_filters.subbands = p.bands.sba();
        for b in 0..usize::from(p.bands.sba()) {
            supplied_filters.alpha0[b] = Coefficient {
                re: alpha[4 * b],
                im: alpha[4 * b + 1],
            };
            supplied_filters.alpha1[b] = Coefficient {
                re: alpha[4 * b + 2],
                im: alpha[4 * b + 3],
            };
        }
        let mut state = ChirpState::new();
        state.previous[..p.previous.len()].copy_from_slice(&p.previous);
        state.modes[..p.previous_modes.len()].copy_from_slice(&p.previous_modes);
        let mut chirp = vec![0.0; p.modes.len()];
        chirp_factors(&p.modes, &mut state, &mut chirp).unwrap();
        save(
            &output.join(format!("{id}.chirp.f32")),
            chirp.iter().copied(),
        );
        save(
            &output.join(format!("{id}.chirp_after.f32")),
            state.previous,
        );
        std::fs::write(output.join(format!("{id}.modes_after.u8")), state.modes).unwrap();
        let mut from_cov = TnaFilters::new();
        from_cov.subbands = p.bands.sba();
        for sb in 0..usize::from(p.bands.sba()) {
            let c = &reference_cov[9 * sb..9 * sb + 9];
            let cov = from_values(c);
            let (a0, a1) = solve(&cov);
            let (a0, a1) = clamp_unstable(a0, a1);
            from_cov.alpha0[sb] = Coefficient::from_complex(a0);
            from_cov.alpha1[sb] = Coefficient::from_complex(a1);
        }
        save_filters(
            &output.join(format!("{id}.alpha_native_cov.f32")),
            &from_cov,
        );
        for (mode, scale) in [("raw", 1.0), ("scaled", 8_388_608.0)] {
            let convert = |input: &QmfSlot| {
                let mut q = *input;
                for b in 0..64 {
                    q.re[b] *= scale;
                    q.im[b] *= scale;
                }
                q
            };
            let low: Vec<_> = low[..38].iter().map(convert).collect();
            let history: Vec<_> = history.iter().map(convert).collect();
            let mut delay = TnaDelay::new();
            delay.advance(&history, 4).unwrap();
            let ext = ExtendedLowBand::new(&delay, &low);
            let mut filters = TnaFilters::new();
            prediction_filters(ext, 32, p.bands.sba(), &mut filters).unwrap();
            save_filters(&output.join(format!("{id}.{mode}.alpha.f32")), &filters);
            if mode == "raw" {
                let mut file =
                    std::fs::File::create_new(output.join(format!("{id}.cov.f64"))).unwrap();
                for sb in 0..64 {
                    let c = covariance(ext, sb, 42);
                    let values = [
                        c.get(0, 1).re,
                        c.get(0, 1).im,
                        c.get(0, 2).re,
                        c.get(0, 2).im,
                        c.get(1, 1).re,
                        c.get(1, 2).re,
                        c.get(1, 2).im,
                        c.get(2, 2).re,
                    ];
                    for value in values {
                        file.write_all(&value.to_le_bytes()).unwrap();
                    }
                }
            }
            let mut gains = PreFlattenGains::new();
            pre_flatten(
                &low,
                usize::from(p.bands.sbx()),
                p.first,
                p.last,
                &mut gains,
            )
            .unwrap();
            save(
                &output.join(format!("{id}.{mode}.gain.f32")),
                (0..64).map(|b| gains.gain(b).unwrap_or(1.0)),
            );
            for (suffix, use_filters, use_chirp, gain) in [
                ("off", &filters, chirp.as_slice(), None),
                ("preflat", &filters, chirp.as_slice(), Some(&gains)),
                (
                    "reference_controls",
                    &supplied_filters,
                    &reference_chirp[..p.modes.len()],
                    None,
                ),
            ] {
                let mut q = vec![QmfSlot::zero(); p.last - p.first];
                hf_generate(
                    ext,
                    &p.patches,
                    use_filters,
                    use_chirp,
                    &p.borders,
                    gain,
                    p.first,
                    p.last,
                    &mut q,
                )
                .unwrap();
                save(
                    &output.join(format!("{id}.{mode}.{suffix}.f32")),
                    q.iter().flat_map(|s| {
                        (0..64).flat_map(move |b| [s.re[b] / scale, s.im[b] / scale])
                    }),
                );
            }
            delay.advance(&low, 32).unwrap();
            save(
                &output.join(format!("{id}.{mode}.history.f32")),
                delay
                    .tail
                    .iter()
                    .flat_map(|s| (0..64).flat_map(move |b| [s.re[b] / scale, s.im[b] / scale])),
            );
        }
    }
    for line in std::fs::read_to_string(data.join("chirp_cases.txt"))
        .unwrap()
        .lines()
    {
        let fields: Vec<_> = line.split_whitespace().collect();
        assert_eq!(fields.len(), 4);
        let id = fields[0];
        assert!(id.bytes().all(|x| x.is_ascii_alphanumeric()) && ids.insert(id.to_string()));
        let current: u8 = fields[1].parse().unwrap();
        let previous: u8 = fields[2].parse().unwrap();
        let value = f32::from_bits(fields[3].parse().unwrap());
        assert!(current <= 3 && previous <= 3 && value.is_finite() && (0.0..=1.0).contains(&value));
        let mut state = ChirpState::new();
        state.modes[0] = previous;
        state.previous[0] = value;
        let mut chirp = [0.0];
        chirp_factors(&[current], &mut state, &mut chirp).unwrap();
        assert_eq!(state.modes[0], current);
        assert_eq!(state.previous[0].to_bits(), chirp[0].to_bits());
        save(&output.join(format!("{id}.local_chirp.f32")), chirp);
    }
    for id in std::fs::read_to_string(data.join("solver_cases.txt"))
        .unwrap()
        .lines()
    {
        assert!(id.bytes().all(|x| x.is_ascii_alphanumeric()) && ids.insert(id.to_string()));
        let values = floats(&data.join(format!("{id}.solver_cov.f32")), 9);
        let (a0, a1) = solve(&from_values(&values));
        let (a0, a1) = clamp_unstable(a0, a1);
        let (a0, a1) = (Coefficient::from_complex(a0), Coefficient::from_complex(a1));
        save(
            &output.join(format!("{id}.local_alpha.f32")),
            [a0.re, a0.im, a1.re, a1.im],
        );
    }
}

#[test]
fn rejects_truncated_and_invalid_time_geometry() {
    assert!(parameters(&[0; 175]).is_err());
    assert!(parameters(&[0; 176]).is_err());
    let mut bytes = [0; 176];
    bytes[4..8].copy_from_slice(&39u32.to_le_bytes());
    bytes[8..12].copy_from_slice(&16u32.to_le_bytes());
    bytes[12..16].copy_from_slice(&2u32.to_le_bytes());
    assert!(parameters(&bytes).is_err());
}

fn valid_parameters() -> [u8; 176] {
    let mut bytes = [0; 176];
    let fields = [
        0u32, 32, 16, 2, 1, 1, 6, 1, 3, 255, 2, 2, 24, 36, 36, 56, 36, 44, 56, 0, 0, 0, 36, 44, 56,
        0, 0, 0, 28, 24, 0, 0, 0,
    ];
    for (i, value) in fields.iter().enumerate() {
        bytes[4 * i..4 * i + 4].copy_from_slice(&value.to_le_bytes());
    }
    bytes[136..144].copy_from_slice(&(5u64 << 60).to_le_bytes());
    bytes[144..152].copy_from_slice(&(5u64 << 60).to_le_bytes());
    bytes[152..156].copy_from_slice(&0.75f32.to_le_bytes());
    bytes[156..160].copy_from_slice(&0.75f32.to_le_bytes());
    bytes
}

#[test]
fn validates_derived_patch_noise_and_crossover_tables() {
    let valid = valid_parameters();
    let result = parameters(&valid).expect("known geometry");
    assert_eq!(result.bands.sbx(), 36);
    assert_eq!(result.patches.start_sb(0), Some(28));
    for (word, value) in [
        (28, 24u32),
        (23, 46),
        (16, 35),
        (9, 8),
        (5, 2),
        (6, u32::MAX),
    ] {
        let mut bytes = valid;
        bytes[word * 4..word * 4 + 4].copy_from_slice(&value.to_le_bytes());
        assert!(parameters(&bytes).is_err());
    }
}

#[test]
fn rejects_nonfinite_or_out_of_range_chirp_history() {
    for value in [f32::NAN, f32::INFINITY, -0.1, 1.1] {
        let mut bytes = valid_parameters();
        bytes[152..156].copy_from_slice(&value.to_le_bytes());
        assert!(parameters(&bytes).is_err());
    }
}
