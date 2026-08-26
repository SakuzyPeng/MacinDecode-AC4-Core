//! A-JOC 量化矩阵侧信息的 trace 语料 census。
//!
//! 这里记录的是逐物理 substream 的编码分支分布，不参与 DSP。它回答「M6 的真实
//! 基线究竟覆盖了什么」，不能把未出现的分支解释为规范不允许。

use super::{Ajoc, AjocObjectControl, AjocObjectMatrix};

const PARAMETER_BANDS: [u8; 8] = [1, 3, 5, 7, 9, 12, 15, 23];

#[derive(Debug, Default)]
struct U8Range {
    count: u64,
    min: Option<u8>,
    max: Option<u8>,
}

impl U8Range {
    fn observe(&mut self, value: u8) {
        self.count = self.count.saturating_add(1);
        self.min = Some(self.min.map_or(value, |current| current.min(value)));
        self.max = Some(self.max.map_or(value, |current| current.max(value)));
    }

    fn to_json(&self) -> String {
        format!(
            "{{\"count\": {}, \"min\": {}, \"max\": {}}}",
            self.count,
            option_u8(self.min),
            option_u8(self.max)
        )
    }
}

#[derive(Debug, Default)]
struct I16Range {
    count: u64,
    min: Option<i16>,
    max: Option<i16>,
}

impl I16Range {
    fn observe(&mut self, value: i16) {
        self.count = self.count.saturating_add(1);
        self.min = Some(self.min.map_or(value, |current| current.min(value)));
        self.max = Some(self.max.map_or(value, |current| current.max(value)));
    }

    fn to_json(&self) -> String {
        format!(
            "{{\"count\": {}, \"min\": {}, \"max\": {}}}",
            self.count,
            option_i16(self.min),
            option_i16(self.max)
        )
    }
}

#[derive(Debug, Default)]
struct MatrixPathCensus {
    carried_segments: u64,
    omitted_segments: u64,
    frequency_segments: u64,
    time_segments: u64,
    values: I16Range,
}

impl MatrixPathCensus {
    fn observe_segment(&mut self, carried: bool, time_direction: Option<bool>) {
        if !carried {
            self.omitted_segments = self.omitted_segments.saturating_add(1);
            return;
        }
        self.carried_segments = self.carried_segments.saturating_add(1);
        if time_direction == Some(true) {
            self.time_segments = self.time_segments.saturating_add(1);
        } else {
            self.frequency_segments = self.frequency_segments.saturating_add(1);
        }
    }

    fn to_json(&self) -> String {
        format!(
            "{{\"carried_segments\": {}, \"omitted_segments\": {}, \
             \"frequency_segments\": {}, \"time_segments\": {}, \"raw_values\": {}}}",
            self.carried_segments,
            self.omitted_segments,
            self.frequency_segments,
            self.time_segments,
            self.values.to_json()
        )
    }
}

/// `ajoc()` 矩阵语法与 M6 full 支持边界的累积统计。
#[derive(Debug, Default)]
pub(in crate::trace) struct AjocMatrixCensus {
    substreams: u64,
    supported_full_substreams: u64,
    unsupported_full_substreams: u64,
    first_unsupported: Option<String>,
    data_points: [u64; 4],
    start_positions: U8Range,
    ramp_lengths: U8Range,
    nodt_false: u64,
    nodt_true: u64,
    objects_present: u64,
    objects_absent: u64,
    objects_with_parameters: u64,
    parameter_bands: [u64; 8],
    fine_objects: u64,
    coarse_objects: u64,
    dense_objects: u64,
    sparse_objects: u64,
    decorrelator_counts: [u64; 8],
    decorrelators_enabled: u64,
    decorrelators_disabled: u64,
    dry: MatrixPathCensus,
    wet: MatrixPathCensus,
}

impl AjocMatrixCensus {
    pub(super) fn observe(
        &mut self,
        ajoc: &Ajoc,
        controls: &[AjocObjectControl],
        matrices: &[AjocObjectMatrix],
        unsupported_detail: Option<String>,
    ) {
        self.substreams = self.substreams.saturating_add(1);
        if let Some(detail) = unsupported_detail {
            self.unsupported_full_substreams = self.unsupported_full_substreams.saturating_add(1);
            if self.first_unsupported.is_none() {
                self.first_unsupported = Some(detail);
            }
        } else {
            self.supported_full_substreams = self.supported_full_substreams.saturating_add(1);
        }

        let dpoints = usize::from(ajoc.data_points.count);
        if let Some(slot) = self.data_points.get_mut(dpoints) {
            *slot = slot.saturating_add(1);
        }
        for dp in 0..dpoints {
            if let Some(start) = ajoc.data_points.start_pos(dp) {
                self.start_positions.observe(start);
            }
            if let Some(ramp) = ajoc.data_points.ramp_len_minus1(dp) {
                self.ramp_lengths.observe(ramp.saturating_add(1));
            }
        }
        if ajoc.b_nodt {
            self.nodt_true = self.nodt_true.saturating_add(1);
        } else {
            self.nodt_false = self.nodt_false.saturating_add(1);
        }

        let decorr = usize::from(ajoc.num_decorr);
        if let Some(slot) = self.decorrelator_counts.get_mut(decorr) {
            *slot = slot.saturating_add(1);
        }
        for de in 0..decorr {
            if ajoc.decorr_enable(de) == Some(true) {
                self.decorrelators_enabled = self.decorrelators_enabled.saturating_add(1);
            } else {
                self.decorrelators_disabled = self.decorrelators_disabled.saturating_add(1);
            }
        }

        let objects = usize::try_from(ajoc.num_umx_signals).unwrap_or(usize::MAX);
        for object in 0..objects {
            let Some(control) = controls.get(object) else {
                break;
            };
            if !control.present {
                self.objects_absent = self.objects_absent.saturating_add(1);
                continue;
            }
            self.objects_present = self.objects_present.saturating_add(1);
            // `num_bands`、quant 与 sparse 只在至少一个数据点时传输。
            if dpoints == 0 {
                continue;
            }
            self.objects_with_parameters = self.objects_with_parameters.saturating_add(1);
            if let Some(index) = PARAMETER_BANDS
                .iter()
                .position(|&bands| bands == control.num_bands)
                && let Some(slot) = self.parameter_bands.get_mut(index)
            {
                *slot = slot.saturating_add(1);
            }
            if control.coarse {
                self.coarse_objects = self.coarse_objects.saturating_add(1);
            } else {
                self.fine_objects = self.fine_objects.saturating_add(1);
            }
            if control.sparse {
                self.sparse_objects = self.sparse_objects.saturating_add(1);
            } else {
                self.dense_objects = self.dense_objects.saturating_add(1);
            }

            let Some(matrix) = matrices.get(object) else {
                continue;
            };
            let bands = usize::from(control.num_bands);
            for dp in 0..dpoints {
                for ch in 0..usize::from(ajoc.num_dmx_signals) {
                    let carried = control.dry_present(ch);
                    self.dry
                        .observe_segment(carried, matrix.dry_time_direction(dp, ch));
                    if carried {
                        for band in 0..bands {
                            if let Some(value) = matrix.dry(dp, ch, band) {
                                self.dry.values.observe(value);
                            }
                        }
                    }
                }
                for de in 0..decorr {
                    // P2 6.2.5.3：dense 模式无条件携带全部 wet 段；只有 sparse
                    // 模式才由逐去相关器存在标志裁剪。
                    let carried = !control.sparse || control.wet_present(de);
                    self.wet
                        .observe_segment(carried, matrix.wet_time_direction(dp, de));
                    if carried {
                        for band in 0..bands {
                            if let Some(value) = matrix.wet(dp, de, band) {
                                self.wet.values.observe(value);
                            }
                        }
                    }
                }
            }
        }
    }

    pub(super) fn to_json(&self) -> String {
        let first_unsupported = self
            .first_unsupported
            .as_ref()
            .map_or_else(|| "null".to_owned(), |detail| format!("{detail:?}"));
        format!(
            "{{\"substreams\": {}, \
             \"full_support\": {{\"supported\": {}, \"unsupported\": {}, \
             \"first_unsupported\": {first_unsupported}}}, \
             \"data_points\": {}, \"start_positions\": {}, \"ramp_lengths\": {}, \
             \"b_nodt\": {{\"false\": {}, \"true\": {}}}, \
             \"objects\": {{\"present\": {}, \"absent\": {}, \
             \"with_parameters\": {}, \"parameter_bands\": {}, \
             \"quantization\": {{\"fine\": {}, \"coarse\": {}}}, \
             \"sparsity\": {{\"dense\": {}, \"sparse\": {}}}}}, \
             \"decorrelators\": {{\"counts\": {}, \"enabled\": {}, \"disabled\": {}}}, \
             \"dry\": {}, \"wet\": {}}}",
            self.substreams,
            self.supported_full_substreams,
            self.unsupported_full_substreams,
            histogram_json(&[0, 1, 2, 3], &self.data_points),
            self.start_positions.to_json(),
            self.ramp_lengths.to_json(),
            self.nodt_false,
            self.nodt_true,
            self.objects_present,
            self.objects_absent,
            self.objects_with_parameters,
            histogram_json(&PARAMETER_BANDS, &self.parameter_bands),
            self.fine_objects,
            self.coarse_objects,
            self.dense_objects,
            self.sparse_objects,
            histogram_json(&[0, 1, 2, 3, 4, 5, 6, 7], &self.decorrelator_counts),
            self.decorrelators_enabled,
            self.decorrelators_disabled,
            self.dry.to_json(),
            self.wet.to_json(),
        )
    }
}

fn histogram_json(labels: &[u8], counts: &[u64]) -> String {
    let entries = labels
        .iter()
        .zip(counts)
        .map(|(label, count)| format!("\"{label}\": {count}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("{{{entries}}}")
}

fn option_u8(value: Option<u8>) -> String {
    value.map_or_else(|| "null".to_owned(), |value| value.to_string())
}

fn option_i16(value: Option<i16>) -> String {
    value.map_or_else(|| "null".to_owned(), |value| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trace::testutil::{pack_bits, shortest_codeword};
    use macindecode_ac4_bitstream::BitReader;
    use macindecode_ac4_bitstream::ajoc::{AjocHcbType, MatrixKind, cb_off, parse_ajoc, table_for};

    fn codeword(kind: MatrixKind, coarse: bool, hcb: AjocHcbType, value: i16) -> String {
        let symbol = value.saturating_add(cb_off(kind, coarse, hcb));
        let symbol = u16::try_from(symbol).expect("测试符号应落在码本内");
        shortest_codeword(
            |reader| table_for(kind, coarse, hcb).decode(reader).ok(),
            symbol,
        )
    }

    fn fixture() -> (Ajoc, Vec<AjocObjectControl>, Vec<AjocObjectMatrix>) {
        let mut source = String::from("010 1 0 1 0 10 00011 000100 10001 111111 110 0 1 1 0 1 0 ");
        // dp 0 dry：频率方向 [3, 1, 2]。
        source.push_str("0 ");
        for (hcb, value) in [
            (AjocHcbType::F0, 3),
            (AjocHcbType::Df, 1),
            (AjocHcbType::Df, 2),
        ] {
            source.push_str(&codeword(MatrixKind::Dry, false, hcb, value));
            source.push(' ');
        }
        // dp 0 wet：时间方向 [-2, 0, 2]。
        source.push_str("1 ");
        for value in [-2, 0, 2] {
            source.push_str(&codeword(MatrixKind::Wet, false, AjocHcbType::Dt, value));
            source.push(' ');
        }
        // dp 1 dry：时间方向 [-1, 0, 1]。
        source.push_str("1 ");
        for value in [-1, 0, 1] {
            source.push_str(&codeword(MatrixKind::Dry, false, AjocHcbType::Dt, value));
            source.push(' ');
        }
        // dp 1 wet：频率方向 [4, 1, 2]。
        source.push_str("0 ");
        for (hcb, value) in [
            (AjocHcbType::F0, 4),
            (AjocHcbType::Df, 1),
            (AjocHcbType::Df, 2),
        ] {
            source.push_str(&codeword(MatrixKind::Wet, false, hcb, value));
            source.push(' ');
        }

        let (data, bits) = pack_bits(&source);
        let mut controls = vec![AjocObjectControl::default(); 2];
        let mut matrices = vec![AjocObjectMatrix::new(); 2];
        let mut reader = BitReader::new(&data);
        let ajoc = parse_ajoc(&mut reader, 2, 2, &mut controls, &mut matrices)
            .expect("census fixture 应能解析");
        assert_eq!(reader.bit_position(), u64::try_from(bits).unwrap_or(0));
        (ajoc, controls, matrices)
    }

    #[test]
    fn census_counts_sparse_segments_directions_and_raw_ranges() {
        let (ajoc, controls, matrices) = fixture();
        let mut census = AjocMatrixCensus::default();
        census.observe(&ajoc, &controls, &matrices, None);

        assert_eq!(census.substreams, 1);
        assert_eq!(census.supported_full_substreams, 1);
        assert_eq!(census.data_points, [0, 0, 1, 0]);
        assert_eq!(
            (census.start_positions.min, census.start_positions.max),
            (Some(3), Some(17))
        );
        assert_eq!(
            (census.ramp_lengths.min, census.ramp_lengths.max),
            (Some(5), Some(64))
        );
        assert_eq!((census.objects_present, census.objects_absent), (1, 1));
        assert_eq!(census.parameter_bands, [0, 1, 0, 0, 0, 0, 0, 0]);
        assert_eq!((census.fine_objects, census.coarse_objects), (1, 0));
        assert_eq!((census.dense_objects, census.sparse_objects), (0, 1));
        assert_eq!(census.decorrelator_counts, [0, 0, 1, 0, 0, 0, 0, 0]);
        assert_eq!(
            (census.decorrelators_enabled, census.decorrelators_disabled),
            (1, 1)
        );
        assert_eq!(
            (census.dry.carried_segments, census.dry.omitted_segments),
            (2, 2)
        );
        assert_eq!(
            (census.dry.frequency_segments, census.dry.time_segments),
            (1, 1)
        );
        assert_eq!(
            (
                census.dry.values.count,
                census.dry.values.min,
                census.dry.values.max
            ),
            (6, Some(-1), Some(3))
        );
        assert_eq!(
            (census.wet.carried_segments, census.wet.omitted_segments),
            (2, 2)
        );
        assert_eq!(
            (census.wet.frequency_segments, census.wet.time_segments),
            (1, 1)
        );
        assert_eq!(
            (
                census.wet.values.count,
                census.wet.values.min,
                census.wet.values.max
            ),
            (6, Some(-2), Some(4))
        );

        let json: serde_json::Value =
            serde_json::from_str(&census.to_json()).expect("应输出合法 JSON");
        assert_eq!(json.pointer("/full_support/supported"), Some(&1.into()));
        assert_eq!(json.pointer("/objects/parameter_bands/3"), Some(&1.into()));
    }

    #[test]
    fn census_preserves_the_first_unsupported_reason() {
        let (ajoc, controls, matrices) = fixture();
        let mut census = AjocMatrixCensus::default();
        census.observe(&ajoc, &controls, &matrices, Some("第一条".to_owned()));
        census.observe(&ajoc, &controls, &matrices, Some("第二条".to_owned()));
        assert_eq!(census.unsupported_full_substreams, 2);
        assert_eq!(census.first_unsupported.as_deref(), Some("第一条"));
    }
}
