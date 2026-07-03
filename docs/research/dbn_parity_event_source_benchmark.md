# DBN-Parity Event Source Benchmark

Date: 2026-06-25

Commit: `ad4446625c68`

Artifacts:

- fixtures: `/tmp/aura-benchmarks/dbn-parity-event-source-20260625/fixtures/fixtures.json`
- event-source results: `/tmp/aura-benchmarks/dbn-parity-event-source-20260625/results-clean/sdk_full_matrix_summary.json`
- specialized baseline: `/tmp/aura-benchmarks/dbn-parity-event-source-20260625/baseline-orderbook-clean/sdk_full_matrix_summary.json`

Commands:

```bash
target/release/aura-fixture-gen \
  --output-dir /tmp/aura-benchmarks/dbn-parity-event-source-20260625/fixtures \
  --sdk-full

target/release/aura_sdk_bench \
  --fixture-dir /tmp/aura-benchmarks/dbn-parity-event-source-20260625/fixtures \
  --output-dir /tmp/aura-benchmarks/dbn-parity-event-source-20260625/results-clean \
  --iterations 10 \
  --warmups 2 \
  --batch-size 8192 \
  --datasets sdk-dense,sdk-sparse,sdk-larger \
  --operations aura1-event-source-file-orderbook-apply,aura1-event-source-memory-orderbook-apply,aura1-event-source-live-orderbook-apply

target/release/aura_sdk_bench \
  --fixture-dir /tmp/aura-benchmarks/dbn-parity-event-source-20260625/fixtures \
  --output-dir /tmp/aura-benchmarks/dbn-parity-event-source-20260625/baseline-orderbook-clean \
  --iterations 10 \
  --warmups 2 \
  --batch-size 8192 \
  --datasets sdk-dense,sdk-sparse,sdk-larger \
  --operations aura1-replay-orderbook-deltas-apply-batch
```

## What This Measures

The benchmark runs one generic event loop over `AuraEventSource` and applies
book updates through the benchmark order-book structure. The same code consumes:

- `AuraFileSource`: sealed Aura1 file with bounded range reads.
- `AuraMemorySource`: sealed Aura1 bytes owned in memory.
- `AuraLiveSource<R>`: Aura1 body stream with schema and compiled plan supplied
  out of band.

This is not a raw scan benchmark. Every row decodes the resolved order-book
payload fields and mutates the book structure.

## Results

| Dataset | Source | Rows | Fields/row | Median ms | P95 ms | Records/sec | MB/sec | Rows materialized | Full file bytes copied |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| sdk-dense | file | 100,000 | 6 | 5.978 | 11.441 | 16.73M | 319.13 | 0 | 0 |
| sdk-dense | memory | 100,000 | 6 | 5.266 | 12.302 | 18.99M | 362.27 | 0 | 2,000,478 |
| sdk-dense | live | 100,000 | 6 | 7.707 | 8.251 | 12.97M | 247.47 | 0 | 0 |
| sdk-sparse | file | 100,000 | 5 | 8.105 | 8.762 | 12.34M | 188.33 | 0 | 0 |
| sdk-sparse | memory | 100,000 | 5 | 9.784 | 13.423 | 10.22M | 156.00 | 0 | 1,600,458 |
| sdk-sparse | live | 100,000 | 5 | 10.580 | 15.946 | 9.45M | 144.22 | 0 | 0 |
| sdk-larger | file | 500,000 | 6 | 35.448 | 43.921 | 14.11M | 269.04 | 0 | 0 |
| sdk-larger | memory | 500,000 | 6 | 40.632 | 45.417 | 12.31M | 234.72 | 0 | 10,000,460 |
| sdk-larger | live | 500,000 | 6 | 50.872 | 52.710 | 9.83M | 187.47 | 0 | 0 |

All file/memory/live rows for a dataset produced the same checksum:

- `sdk-dense`: `14671305852831295695`
- `sdk-sparse`: `9903779750498015101`
- `sdk-larger`: `7006162731148306001`

## Specialized Baseline Comparison

The existing `aura1-replay-orderbook-deltas-apply-batch` path uses
`OrderBookDeltaBatch` directly. The event-source benchmark uses the public
generic `AuraEventBatch::value_i64` field access surface.

| Dataset | Specialized median ms | Event source file | Event source memory | Event source live |
|---|---:|---:|---:|---:|
| sdk-dense | 5.285 | 1.13x | 1.00x | 1.46x |
| sdk-sparse | 8.439 | 0.96x | 1.16x | 1.25x |
| sdk-larger | 31.222 | 1.14x | 1.30x | 1.63x |

## Interpretation

The file-backed event-source path is within roughly 0.96x to 1.14x of the
specialized order-book batch path on these fixtures. That is good enough to say
the common historical event-source abstraction is not just a benchmark-specific
view path.

The live source is slower, roughly 1.25x to 1.63x of the specialized baseline.
The current live benchmark reads from `std::io::Read` into an internal batch
buffer each iteration. That is an honest first implementation, but not a final
zero-copy live transport design.

The memory source owns sealed Aura1 bytes and therefore reports full-file bytes
copied at open. This is an API ownership artifact, not per-row materialization;
rows materialized remained zero in all event-source operations.
