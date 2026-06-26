# Randomized Stabilization Current Head Status

Date: 2026-06-25

## Repository

- Branch: `final-format-resolution`
- Baseline HEAD inspected before edits: `f8523322ccffbf9a2eb023e4c5f6aecabc3a2605`
- Baseline worktree: clean
- Remote: `origin` configured, no push performed

## Baseline Commands

| Command | Result |
| --- | --- |
| `git status --short` | clean |
| `git rev-parse HEAD` | `f8523322ccffbf9a2eb023e4c5f6aecabc3a2605` |
| `git branch --show-current` | `final-format-resolution` |
| `git diff --check` | pass |
| `cargo metadata --no-deps` | pass |
| `cargo check` | pass |
| `cargo check --examples` | pass |
| `cargo test` | pass |
| `cargo build --release --bin aura-bench` | pass |
| `cargo build --release --bin aura_sdk_bench` | pass |
| `cargo build --release --bin aura-fixture-gen` | pass |

## Binaries Present

- `aura-bench`
- `aura-fixture-gen`
- `aura-json-i64`
- `aura-parquet-ohlcv`
- `aura-size`
- `aura_sdk_bench`
- `aura-verify-random` added during this stabilization pass

## Examples Present

- `columnar_write`
- `convert_aura`
- `read_aura`
- `replay_aura1`
- `roundtrip`
- `stream_batches`
- `write_aura0_compact`
- `write_aura0_hybrid`
- `write_aura1`

## Current Public APIs

- Dynamic schemas: `AuraSchema`, `AuraSchemaBuilder`, `AuraField`, `AuraType`
- Writing: `AuraWriter`, `AuraI64Writer`, `AuraTypedWriter`, `WriterOptions`
- Reading: `AuraReader`, `AuraRecordBatch`, `AuraColumnBatch`, streaming batch APIs
- Conversion: `convert_aura`, `ConvertOptions`
- Replay: `replay_i64`, `replay_fixed_batches`, grouped replay APIs
- Orderbook-compatible replay: `OrderBookDeltaSpec`, `replay_orderbook_deltas`, fused replay/apply APIs
- Plans and metadata: `CompiledAuraPlan`, `AuraMetadata`, `SymbolMap`

## Current Benchmark And Verification Commands

- `cargo test --test randomized_stabilization -- --nocapture`
- `cargo run --bin aura-verify-random -- --cases 1000 --max-records 10000 --formats aura1,aura0 --check-cross-format --check-streaming --check-replay --check-orderbook --seed 176609594574376 --output /tmp/aura-random-verify/report.json`
- `cargo test --test aura_bench_cli -- --nocapture`
- `target/release/aura-bench ...`
- `target/release/aura_sdk_bench ...`
- `target/release/aura-fixture-gen ...`

## Immediate Blockers Found

Fresh randomized tests found and fixed:

- Rough-step stats overflowed on extreme legal i64 deltas.
- Non-slot-0 timestamp fields could be rejected by compact header mapping.
- Wide schemas could exceed the one-byte header comment length because the SDK writer joined all field names into the comment.
- Ingest stamping could fail the whole file when the optional generic compact plan could not represent an extreme legal i64 column.

No remaining randomized blocker is known after the fixes in this pass.
