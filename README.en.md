# MacinDecode-AC4-Core

English | [中文](README.md)

Dolby AC-4 object audio decoding core · Rust 2024 · MSRV 1.98 · `unsafe` forbidden

## What is this?

AC-4 is Dolby's next-generation audio codec supporting object-based immersive audio (Dolby Atmos). MacinDecode-AC4-Core aims to reconstruct AC-4 bitstreams as **pre-render audio scenes** — containing beds, object PCM, Object Audio Metadata (OAMD), and sample-accurate timelines — for consumption by an external renderer without performing the final render itself.

This project does **not** handle speaker/headphone rendering, loudness management, AC-4 encoding, or Dolby product certification.

This is an independent open-source implementation and is not affiliated with, sponsored by,
or endorsed by Dolby Laboratories. Dolby, Dolby Atmos, and AC-4 are trademarks of their
respective owners and are referenced only to describe compatibility.

## Features

- **Container & sync**: MP4 (`ac-4` sample entry / `dac4`) and raw AC-4 sync frame parsing
- **Scene topology**: Presentation / Group / Substream relationships, random access & configuration generation state machine
- **OAMD timeline**: Cross-frame state continuity, intra-frame updates, ramps, and post-seek integrity marking
- **Audio core decoding**: Dequantization, IMDCT, joint channel matrix, A-SPX spectral extension & QMF synthesis
- **A-JOC full reconstruction**: Object matrix, wet/decorrelation, LFE reinsertion, terminal QMF synthesis
- **Scene Rust API**: Container-independent borrowed views, session control plane, presentation selection, and structured errors
- **Multi-format export**: PCM WAVE, ADM BWF (BW64/RF64), DAMF (0.5.1/0.6.0), Apple CAF
- **Spec traceability**: Specification-derived logic uses the `TS103190-1:v1.4.1:<clause>` / `TS103190-2:v1.3.1:<clause>` citation format, with a maintained clause-to-implementation-to-test traceability matrix
- **`#![no_std]` core**: Decoding core has no platform dependencies, `unsafe` forbidden

## Quick Start

### Prerequisites

- Rust ≥ 1.98 ([install](https://rustup.rs/))

Prebuilt `macinac4` binaries with full audio decoding and ADM/DAMF export are also available
from [GitHub Releases](https://github.com/SakuzyPeng/MacinDecode-AC4-Core/releases). Automated
releases cover x86_64 and ARM64 on Linux, macOS, and Windows; see
[Multi-platform Binary Releases](docs/BINARY_RELEASE.md) for targets and SHA-256
verification. The required static tables are generated from the pinned official specifications
on each build runner, so end users do not need to download the specifications at runtime.

### Build & Test

```bash
cargo build --workspace
cargo test --workspace
```

### Run a trace

```bash
cargo run --bin macinac4 -- trace path/to/input.m4a
```

### Inspect bitstream metadata

```bash
cargo run --bin macinac4 -- inspect path/to/input.m4a
cargo run --bin macinac4 -- inspect path/to/input.m4a --format json
```

Rust applications can obtain the same owned report without spawning the CLI:

```rust
use macindecode_ac4_inspect::{InspectSourceHint, inspect_bytes, inspect_path};

fn inspect_inputs() -> Result<(), Box<dyn std::error::Error>> {
    let file_report = inspect_path("path/to/input.m4a")?;
    let bytes = std::fs::read("path/to/input.ac4")?;
    let memory_report = inspect_bytes(&bytes, InspectSourceHint::default())?;
    println!("{}", file_report.render_text());
    println!("memory frames: {}", memory_report.source.frame_count);
    Ok(())
}
```

Serializing the report directly produces the shape found at CLI `result.inspectResult`.

### Full audio decoding

When building from source, full audio support requires generating static tables locally from the
official ETSI specifications and fetching their companion C tables. Neither the inputs nor
generated files are tracked by Git or included in crates.io packages:

```bash
python3 -m pip install -r scripts/requirements-spec.txt
./scripts/fetch_specs.py
./scripts/generate_spec_tables.py
cargo test --workspace --features audio-decode

# Conditional local real-vector tests; the default suite reports them as ignored.
cargo test -p macindecode-ac4-cli --features audio-decode -- --ignored
```

## Basic Usage

Here are the three most commonly used commands. See the [CLI Usage Guide](docs/CLI_USAGE.md) for the full reference of all 10 subcommands.

**Inspect a bitstream** — output structured JSON of container, topology, and syntax:

```bash
cargo run --bin macinac4 -- trace path/to/input.m4a
```

**Export full object PCM** — all A-JOC upmixed objects with LFE:

```bash
cargo run -p macindecode-ac4-cli --features audio-decode --bin macinac4 -- \
  export-objects-pcm path/to/input.m4a --output path/to/objects.wav
```

**Export ADM BWF** — full objects and OAMD packaged as standard BW64:

```bash
cargo run -p macindecode-ac4-cli --features audio-decode --bin macinac4 -- \
  export-full-adm-bwf path/to/input.m4a --output path/to/full-adm.wav
```

On success, stdout contains a JSON v1 envelope with `schema`/`version`, except that `inspect`
defaults to stable English text; on failure, stdout is empty (exit code 2 for argument errors,
1 for runtime errors).

## Project Structure

```text
macindecode-ac4-cli ──→ macindecode-ac4-inspect ──→ macindecode-ac4-mp4
                         │                          │
                         └──────────────────────────┴→ macindecode-ac4-bitstream
macindecode-ac4-scene ──────────────────────────────→ macindecode-ac4-bitstream
macindecode-ac4-perf ──→ macindecode-ac4-scene / macindecode-ac4-mp4 (internal)
```

| Crate | Responsibility | `no_std` |
|---|---|---|
| [`macindecode-ac4-bitstream`](crates/macindecode-ac4-bitstream) | Bitstream parsing, TOC/OAMD/EMDF, ASF/A-SPX/A-JOC audio reconstruction | ✅ |
| [`macindecode-ac4-inspect`](crates/macindecode-ac4-inspect) | File-level MP4/raw AC-4 aggregation, JSON DTOs, and stable text rendering | — |
| [`macindecode-ac4-scene`](crates/macindecode-ac4-scene) | `Ac4SceneFrame` contract and streaming Rust API for A-JOC Core/Full | ✅ |
| [`macindecode-ac4-mp4`](crates/macindecode-ac4-mp4) | ISO BMFF boxes, `dac4`, sample table, edit/priming timeline | ✅ |
| [`macindecode-ac4-cli`](crates/macindecode-ac4-cli) | `macinac4` tool: inspect, trace, PCM/ADM/DAMF/CAF export | — |
| `macindecode-ac4-perf` | Unpublished Session timing, allocation, and hotspot-sampling harness | — |

## Data Flow

```text
MP4 / raw AC-4
    → Container & sync layer
    → TOC / presentation / substream
    → Audio core decoding (dequant → IMDCT → PCM)
    → A-SPX spectral extension (QMF)
    → A-JOC full object reconstruction
    → OAMD timeline
    → [M5] Ac4SceneFrame     ← Borrowed Core/Full PCM/OAMD scene entry available
    → External renderer
```

## Current Status

| Milestone | Status | Summary |
|---|---|---|
| M0 Documentation & toolchain | ✅ | Spec versions/hashes/vector provenance/tool fingerprints frozen |
| M1 Container & sync | ✅ | MP4/raw framing, cross-checked with ffprobe/Bento4/MediaInfo |
| M2 TOC & topology | ✅ | Presentation/Group/Substream, random access state machine |
| M3 OAMD & timeline | ✅ | Cross-frame state, intra-frame updates, post-seek integrity |
| M4 Audio core baseline | ✅ | Dequant→IMDCT→A-SPX, 12 A-JOC media bit-exact core/A-SPX baselines frozen |
| M4.5 Presentation/Metadata | ✅ (limited) | Read-only parsing and DE/EMDF real-media gates complete; alternative, non-empty DE bodies, and other EMDF types still await samples |
| M5 Scene API | 🚧 | A-JOC Core/Full borrowed Rust API, core/A-SPX baselines, CoreCAF, ADM/DAMF diagnostic renderers, and Full batch exports integrated; direct-object support pending |
| M6 Full A-JOC reconstruction | ✅ | Object matrix/wet/LFE/terminal QMF synthesis, third bit-exact baseline frozen |
| M7 Architecture, ABI & robustness | 🚧 | ARM64 performance baselines and QMF optimizations complete; responsibility cleanup, C ABI, fuzzing, and x86-64 measurements pending |

For detailed progress, known limitations, and the audio reconstruction support matrix, see the [Roadmap](docs/ROADMAP.md).

## Design Principles

1. Correctness over optimization; establish a traceable scalar baseline before optimizing hot paths.
2. Bitstream input is untrusted by default; parsers must not rely on unchecked indices or lengths.
3. Decoding timelines use integer sample positions, not floating-point seconds.
4. Container time, codec time, and render time must be represented in separate layers.
5. A-JOC is lossy object reconstruction; validation cannot simply compare sample-by-sample against master PCM.
6. Spec clauses, implementation modules, and test cases must be mutually traceable.
7. The repository must not contain proprietary binaries, licensed SDKs, customer media, or non-redistributable test assets.

## Documentation

| Document | Description |
|---|---|
| [Architecture](docs/ARCHITECTURE.md) | Target boundaries, dependency direction, time model, numeric strategy |
| [CLI Usage Guide](docs/CLI_USAGE.md) | Complete reference for all 10 subcommands |
| [CLI Output Contract v1](docs/CLI_OUTPUT_CONTRACT.md) | Machine-readable JSON stdout/stderr specification |
| [Multi-platform Binary Releases](docs/BINARY_RELEASE.md) | Six-target builds, GitHub Releases, and SHA-256 verification |
| [crates.io Release Checklist](docs/CRATES_IO_RELEASE.md) | Package gates, manual release order, and post-release checks |
| [Pre-render Output Contract](docs/OUTPUT_CONTRACT.md) | Scene frame semantics and pre-render boundary |
| [Roadmap](docs/ROADMAP.md) | Milestone details, support matrix, known limitations |
| [Test Vector Strategy](docs/TEST_VECTOR_STRATEGY.md) | Vector production chain, verification tiers, external references |
| [Spec Traceability](docs/SPEC_TRACEABILITY.md) | Clause ↔ implementation ↔ test traceability matrix |
| [ADR Decision Records](docs/decisions/) | Language, numeric, transform, Scene API, and responsibility-layering decisions (11 records) |

## License

[MIT](LICENSE)
