//! Presentation 级 EMDF payload substream 的逐帧 census。

use std::collections::{BTreeMap, BTreeSet};

use macindecode_ac4_bitstream::{BitReader, EmdfInfo, EmdfPayloadsSubstream};

use super::Ac4Topology;

const OPAQUE_PREFIX_BYTES: usize = 16;
const FNV1A64_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV1A64_PRIME: u64 = 0x0000_0100_0000_01b3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum RouteKind {
    Primary,
    Additional,
}

impl RouteKind {
    const fn label(self) -> &'static str {
        match self {
            Self::Primary => "primary",
            Self::Additional => "additional",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct RouteSignature {
    kind: RouteKind,
    emdf_version: u32,
    key_id: u32,
    substream_index: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct PayloadSignature {
    id: u32,
    sample_offset: Option<u32>,
    duration: Option<u32>,
    group_id: Option<u32>,
    codec_data: Option<u8>,
    discard_unknown_payload: bool,
    payload_frame_aligned: bool,
    create_duplicate: bool,
    remove_duplicate: bool,
    priority: Option<u8>,
    processing_allowed: Option<u8>,
    size_bytes: u32,
    fnv1a64: u64,
    opaque_prefix: Vec<u8>,
}

/// Presentation 级 `emdf_payloads_substream()` 的累计覆盖。
#[derive(Debug, Default)]
pub(crate) struct EmdfTrace {
    infos: u32,
    routed_infos: u32,
    routed_frames: u32,
    located_substreams: u32,
    parsed_substreams: u32,
    nonempty_substreams: u32,
    empty_substreams: u32,
    payloads: u32,
    payload_bytes: u64,
    max_payload_bytes: u32,
    failures: u32,
    first_error: Option<String>,
    routes: BTreeMap<RouteSignature, u32>,
    signatures: BTreeMap<PayloadSignature, u32>,
    first_detail: Option<String>,
}

impl EmdfTrace {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    fn remember_failure(&mut self, frame: u32, message: &str) {
        self.failures = self.failures.saturating_add(1);
        if self.first_error.is_none() {
            self.first_error = Some(format!("Frame {frame}: {message}"));
        }
    }

    fn record_info(&mut self, info: EmdfInfo, kind: RouteKind, indexes: &mut BTreeSet<u32>) {
        self.infos = self.infos.saturating_add(1);
        let Some(substream_index) = info.payloads_substream_index else {
            return;
        };
        self.routed_infos = self.routed_infos.saturating_add(1);
        indexes.insert(substream_index);
        let route = RouteSignature {
            kind,
            emdf_version: info.emdf_version,
            key_id: info.key_id,
            substream_index,
        };
        let count = self.routes.entry(route).or_default();
        *count = count.saturating_add(1);
    }

    pub(crate) fn observe(&mut self, frame: &[u8], topology: &Ac4Topology, frame_index: u32) {
        let mut indexes = BTreeSet::new();
        for presentation in topology.presentations() {
            if let Some(info) = presentation.emdf {
                self.record_info(info, RouteKind::Primary, &mut indexes);
            }
            for &info in presentation.additional_emdf() {
                self.record_info(info, RouteKind::Additional, &mut indexes);
            }
        }
        if !indexes.is_empty() {
            self.routed_frames = self.routed_frames.saturating_add(1);
        }

        for substream_index in indexes {
            let payload = match topology.substream_payload(frame, substream_index) {
                Ok(payload) => payload,
                Err(error) => {
                    self.remember_failure(
                        frame_index,
                        &format!("EMDF substream {substream_index} location failed: {error}"),
                    );
                    continue;
                }
            };
            self.located_substreams = self.located_substreams.saturating_add(1);

            let mut reader = BitReader::new(payload);
            let parsed = match EmdfPayloadsSubstream::parse(&mut reader) {
                Ok(parsed) if reader.remaining_bits() == 0 => parsed,
                Ok(_) => {
                    self.remember_failure(
                        frame_index,
                        &format!(
                            "EMDF substream {substream_index} has {} trailing bits",
                            reader.remaining_bits()
                        ),
                    );
                    continue;
                }
                Err(error) => {
                    self.remember_failure(
                        frame_index,
                        &format!("EMDF substream {substream_index} parse failed: {error}"),
                    );
                    continue;
                }
            };
            self.parsed_substreams = self.parsed_substreams.saturating_add(1);
            if parsed.payload_count == 0 {
                self.empty_substreams = self.empty_substreams.saturating_add(1);
            } else {
                self.nonempty_substreams = self.nonempty_substreams.saturating_add(1);
            }
            self.payloads = self.payloads.saturating_add(parsed.payload_count);
            self.payload_bytes = self.payload_bytes.saturating_add(parsed.payload_bytes);
            self.max_payload_bytes = self.max_payload_bytes.max(parsed.max_payload_bytes);

            for descriptor in parsed.payloads() {
                let Some(bytes) = descriptor.bytes(payload) else {
                    self.remember_failure(
                        frame_index,
                        &format!(
                            "EMDF payload {} in substream {substream_index} has an invalid byte view",
                            descriptor.id
                        ),
                    );
                    continue;
                };
                let mut hash = FNV1A64_OFFSET;
                let mut opaque_prefix = Vec::new();
                for byte in bytes {
                    hash ^= u64::from(byte);
                    hash = hash.wrapping_mul(FNV1A64_PRIME);
                    if opaque_prefix.len() < OPAQUE_PREFIX_BYTES {
                        opaque_prefix.push(byte);
                    }
                }
                let config = descriptor.config;
                let signature = PayloadSignature {
                    id: descriptor.id,
                    sample_offset: config.sample_offset,
                    duration: config.duration,
                    group_id: config.group_id,
                    codec_data: config.codec_data,
                    discard_unknown_payload: config.discard_unknown_payload,
                    payload_frame_aligned: config.payload_frame_aligned,
                    create_duplicate: config.create_duplicate,
                    remove_duplicate: config.remove_duplicate,
                    priority: config.priority,
                    processing_allowed: config.processing_allowed,
                    size_bytes: descriptor.size_bytes,
                    fnv1a64: hash,
                    opaque_prefix,
                };
                let count = self.signatures.entry(signature).or_default();
                *count = count.saturating_add(1);
            }

            if self.first_detail.is_none() {
                self.first_detail = Some(format!(
                    "{{\"frame\": {frame_index}, \"substream_index\": {substream_index}, \
                     \"substream_bytes\": {}, \"payload_count\": {}, \
                     \"payload_bytes\": {}}}",
                    payload.len(),
                    parsed.payload_count,
                    parsed.payload_bytes
                ));
            }
        }
    }

    pub(crate) fn to_json(&self) -> String {
        let mut routes = String::from("[");
        for (position, (route, count)) in self.routes.iter().enumerate() {
            if position > 0 {
                routes.push_str(", ");
            }
            routes.push_str(&format!(
                concat!(
                    "{{\"kind\": \"{}\", \"emdf_version\": {}, \"key_id\": {}, ",
                    "\"substream_index\": {}, \"count\": {}}}"
                ),
                route.kind.label(),
                route.emdf_version,
                route.key_id,
                route.substream_index,
                count
            ));
        }
        routes.push(']');

        let mut signatures = String::from("[");
        for (position, (signature, count)) in self.signatures.iter().enumerate() {
            if position > 0 {
                signatures.push_str(", ");
            }
            let option = |value: Option<u32>| {
                value.map_or_else(|| "null".to_owned(), |value| value.to_string())
            };
            let opaque_prefix = signature
                .opaque_prefix
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            signatures.push_str(&format!(
                concat!(
                    "{{\"id\": {}, \"count\": {}, \"size_bytes\": {}, ",
                    "\"fnv1a64\": \"{:016x}\", \"opaque_prefix_hex\": \"{}\", ",
                    "\"opaque_prefix_truncated\": {}, \"config\": {{",
                    "\"sample_offset\": {}, \"duration\": {}, \"group_id\": {}, ",
                    "\"codec_data\": {}, \"discard_unknown_payload\": {}, ",
                    "\"payload_frame_aligned\": {}, \"create_duplicate\": {}, ",
                    "\"remove_duplicate\": {}, \"priority\": {}, ",
                    "\"processing_allowed\": {}}}}}"
                ),
                signature.id,
                count,
                signature.size_bytes,
                signature.fnv1a64,
                opaque_prefix,
                usize::try_from(signature.size_bytes).unwrap_or(usize::MAX)
                    > signature.opaque_prefix.len(),
                option(signature.sample_offset),
                option(signature.duration),
                option(signature.group_id),
                option(signature.codec_data.map(u32::from)),
                signature.discard_unknown_payload,
                signature.payload_frame_aligned,
                signature.create_duplicate,
                signature.remove_duplicate,
                option(signature.priority.map(u32::from)),
                option(signature.processing_allowed.map(u32::from)),
            ));
        }
        signatures.push(']');

        format!(
            concat!(
                "{{\"infos\": {}, \"routed_infos\": {}, \"routed_frames\": {}, ",
                "\"located_substreams\": {}, \"parsed_substreams\": {}, ",
                "\"nonempty_substreams\": {}, \"empty_substreams\": {}, ",
                "\"payloads\": {}, \"payload_bytes\": {}, \"max_payload_bytes\": {}, ",
                "\"failures\": {}, \"first_error\": {}, \"routes\": {}, ",
                "\"signatures\": {}, \"first_detail\": {}}}"
            ),
            self.infos,
            self.routed_infos,
            self.routed_frames,
            self.located_substreams,
            self.parsed_substreams,
            self.nonempty_substreams,
            self.empty_substreams,
            self.payloads,
            self.payload_bytes,
            self.max_payload_bytes,
            self.failures,
            self.first_error
                .as_ref()
                .map_or_else(|| "null".to_owned(), |error| format!("{error:?}")),
            routes,
            signatures,
            self.first_detail.as_deref().unwrap_or("null")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::super::testutil::topology_with_presentation_emdf_payload;
    use super::*;

    #[test]
    fn records_nonempty_presentation_payload_and_opaque_signature() {
        let (frame, topology) = topology_with_presentation_emdf_payload(false);
        let mut trace = EmdfTrace::new();
        trace.observe(&frame, &topology, 7);

        assert_eq!(trace.routed_frames, 1);
        assert_eq!(trace.located_substreams, 1);
        assert_eq!(trace.parsed_substreams, 1);
        assert_eq!(trace.nonempty_substreams, 1);
        assert_eq!(trace.payloads, 1);
        assert_eq!(trace.payload_bytes, 1);
        assert_eq!(trace.failures, 0, "{:?}", trace.first_error);
        let json: serde_json::Value = serde_json::from_str(&trace.to_json()).unwrap();
        let route = json
            .get("routes")
            .and_then(serde_json::Value::as_array)
            .and_then(|routes| routes.first())
            .unwrap();
        assert_eq!(route.get("kind"), Some(&serde_json::json!("primary")));
        assert_eq!(route.get("substream_index"), Some(&serde_json::json!(1)));
        let signature = json
            .get("signatures")
            .and_then(serde_json::Value::as_array)
            .and_then(|signatures| signatures.first())
            .unwrap();
        assert_eq!(signature.get("id"), Some(&serde_json::json!(20)));
        assert_eq!(signature.get("size_bytes"), Some(&serde_json::json!(1)));
        assert_eq!(
            signature.get("opaque_prefix_hex"),
            Some(&serde_json::json!("00"))
        );
        assert_eq!(
            signature.get("fnv1a64"),
            Some(&serde_json::json!("af63bd4c8601b7df"))
        );
        assert_eq!(
            signature
                .get("config")
                .and_then(|config| config.get("discard_unknown_payload")),
            Some(&serde_json::json!(true))
        );
    }

    #[test]
    fn rejects_trailing_bytes_in_presentation_payload_substream() {
        let (frame, topology) = topology_with_presentation_emdf_payload(true);
        let mut trace = EmdfTrace::new();
        trace.observe(&frame, &topology, 3);
        assert_eq!(trace.parsed_substreams, 0);
        assert_eq!(trace.failures, 1);
        assert!(
            trace
                .first_error
                .as_deref()
                .is_some_and(|error| error.contains("trailing bits"))
        );
    }
}
