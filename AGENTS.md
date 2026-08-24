# Repository Guidelines

## Project Structure & Module Organization

This Rust 2024 workspace has an MSRV of 1.85. `macindecode-ac4-bitstream` owns syntax and reconstruction, `macindecode-ac4-mp4` owns containers and timing, and `macindecode-ac4-cli` owns trace, validation, and exports. Tests sit beside implementations. Use `docs/` for design records, `scripts/` for utilities, and `vectors/<case_id>/` for case metadata. `spec/MANIFEST.json` pins specification sources.

## Build, Test, and Development Commands

```bash
cargo build --workspace                         # compile every crate
cargo test --workspace                          # run tests without licensed tables
cargo test --workspace --features audio-decode  # licensed-table regressions; real vectors stay ignored
cargo test -p macindecode-ac4-cli --features audio-decode -- --ignored  # require local real vectors
cargo fmt --all -- --check                      # verify rustfmt output
cargo clippy --workspace --all-targets --features audio-decode -- -D warnings
cargo run --bin macinac4 -- trace path/to/input.m4a
./scripts/cross_check.sh path/to/input.m4a       # compare trace fields with installed tools
./scripts/audio_check.sh path/to/input.m4a       # require full A-JOC parsing to land exactly
./scripts/trajectory_check.py vectors/<case_id>  # compare decoded object tracks with case.json
./scripts/decode_check.py                        # local bit-exact PCM baselines (core + A-SPX stages)
python3 -m unittest scripts/test_trajectory_check.py scripts/test_decode_check.py scripts/test_ajoc_census.py scripts/test_check_patch_tables.py scripts/test_dme_ac4.py scripts/test_dee_ims.py
./scripts/check_transform_tables.py              # audit transform constants without the PDF
./scripts/check_sfb_tables.py                    # verify Annex B against the PDF
./scripts/check_aspx_tables.py                   # verify the A-SPX static tables against the PDF
./scripts/check_ajoc_tables.py                   # verify the A-JOC band map against both PDFs
./scripts/check_patch_tables.py --sweep           # cross-check all HF patch/limiter configurations
./scripts/check_spec_distribution.py --generated # audit local-only tables and crate packages
```

Install `scripts/requirements-spec.txt`, then run `./scripts/fetch_specs.py` and `./scripts/generate_spec_tables.py` before feature-gated or PDF-backed checks. `decode_check.py` runs two stages against separate baselines (`--stage core|aspx`) and requires every ignored medium named by each; CI tests only the checker logic. SFB/A-SPX/A-JOC audits need the PDF and `pdfplumber`; the transform audit uses the standard library. Check the default production chain with `./scripts/check_tools.sh`, DME A-JOC with `--profile dme_ac4`, or every configured backend with `--profile all`.

## Coding Style & Naming Conventions

Use `rustfmt` defaults (four spaces), `snake_case` for functions/tests, `UpperCamelCase` for types, and `SCREAMING_SNAKE_CASE` for constants. Unsafe Rust is forbidden. Validate all bitstream sizes and indices, return structured errors, and use integer timelines. Cite logic as `TS103190-1:v1.4.1:<clause>` or its Part 2 equivalent. Keep identifiers in English.

## Testing Guidelines

Keep tests in local `#[cfg(test)] mod tests` modules and name behavior, for example `rejects_truncated_payload`. Cover valid input, truncation, bad lengths, overflow, reserved values, and timeline boundaries. Treat `case.json` as authoritative; regenerate provenance with `scripts/record_provenance.py`.

## Commit & Pull Request Guidelines

Use concise Conventional Commit-style subjects, often in Chinese: `feat(bitstream): ...`, `fix: ...`, `test: ...`, `docs: ...`, or `chore: ...`. Keep one logical change per commit. PRs must explain impact, link issues or clauses, list checks, and identify changed vectors or CLI JSON.

## Security & Local Configuration

Copy `.env.local.example` to `.env.local`; never commit it, proprietary binaries, specification PDFs, customer media, or generated artifacts. Verify specifications with `./scripts/fetch_specs.py --verify`.

## Agent-Specific Instructions

除非用户明确要求其他语言，代理回复、贡献说明、代码评审和仓库文档优先使用中文；代码标识符与公共 API 保持英文，并沿用仓库现有术语。
