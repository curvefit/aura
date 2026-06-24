# Aura1 Replay Millisecond Breakdown Results

Fresh matrix:

- Main results: `/tmp/aura-benchmarks/aura1-replay-breakdown-20260624T-ms/sdk_full_matrix_summary.json`
- Batch 128 sweep: `/tmp/aura-benchmarks/aura1-replay-breakdown-20260624T-ms-batch128/sdk_full_matrix_summary.json`
- Batch 1024 sweep: `/tmp/aura-benchmarks/aura1-replay-breakdown-20260624T-ms-batch1024/sdk_full_matrix_summary.json`
- Batch 65536 sweep: `/tmp/aura-benchmarks/aura1-replay-breakdown-20260624T-ms-batch65536/sdk_full_matrix_summary.json`
- Whole-file batch sweep: `/tmp/aura-benchmarks/aura1-replay-breakdown-20260624T-ms-batch500000/sdk_full_matrix_summary.json`

## What The Operations Do

Per-row replay calls a consumer callback once per record, in file order. The
selected/all variants read fields through `Aura1RowView` and mix those values
into a checksum.

Batch replay calls a consumer callback once per fixed-width batch. The selected
variant uses field-major selected-field checksums. The all-field variant uses the
type-specialized fixed-width kernel.

Grouped replay detects consecutive group runs, then calls a consumer callback
once per group. High-cardinality datasets still produce one group per row and
therefore behave like a slower per-row replay with extra key detection.

Row materialization builds `AuraRecordBatch` rows and `AuraValue`s. Column
materialization builds typed `AuraColumnBatch` vectors. These are reads, not
replay.

Raw scan and view-only benchmarks are controls. They are not replay because they
do not expose decoded records or touched fields to a consumer.

## sdk-larger Timing Breakdown

Dataset: `sdk-larger`, 500,000 records, 6 fields, 3,000,000 all-field values.

| operation | total ms | setup ms | field/load loop ms | checksum ms | callback ms | allocation/materialization ms | group detection ms | unexplained ms | JSON |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---|
| per-row noop | 1.957 | 0.028 | 0.000 | 0.000 | 0.000 | 0.000 | 0.000 | 0.006 | `/tmp/aura-benchmarks/aura1-replay-breakdown-20260624T-ms/sdk-larger-aura1-replay-per-row-noop.json` |
| per-row selected | 11.597 | 0.054 | 11.527 | 0.000 | 0.000 | 0.000 | 0.000 | 0.014 | `/tmp/aura-benchmarks/aura1-replay-breakdown-20260624T-ms/sdk-larger-aura1-replay-per-row-touch-selected.json` |
| per-row all | 20.957 | 0.058 | 20.880 | 0.000 | 0.000 | 0.000 | 0.000 | 0.018 | `/tmp/aura-benchmarks/aura1-replay-breakdown-20260624T-ms/sdk-larger-aura1-replay-per-row-touch-all.json` |
| batch noop | 1.226 | 0.020 | 0.000 | 0.000 | 0.002 | 0.000 | 0.000 | 0.006 | `/tmp/aura-benchmarks/aura1-replay-breakdown-20260624T-ms/sdk-larger-aura1-replay-batch-noop.json` |
| batch selected | 5.755 | 0.029 | 4.328 | 0.000 | 0.009 | 0.000 | 0.000 | 0.008 | `/tmp/aura-benchmarks/aura1-replay-breakdown-20260624T-ms/sdk-larger-aura1-replay-batch-touch-selected.json` |
| batch all | 3.623 | 0.026 | 2.287 | 0.000 | 0.013 | 0.000 | 0.000 | 0.007 | `/tmp/aura-benchmarks/aura1-replay-breakdown-20260624T-ms/sdk-larger-aura1-replay-batch-touch-all.json` |
| grouped selected | 54.959 | 2.383 | 52.565 | 0.000 | 0.000 | 0.000 | 0.000 | 0.011 | `/tmp/aura-benchmarks/aura1-replay-breakdown-20260624T-ms/sdk-larger-aura1-replay-grouped-touch-selected.json` |
| grouped all | 65.187 | 2.450 | 62.726 | 0.000 | 0.000 | 0.000 | 0.000 | 0.011 | `/tmp/aura-benchmarks/aura1-replay-breakdown-20260624T-ms/sdk-larger-aura1-replay-grouped-touch-all.json` |
| row batch | 75.097 | 0.048 | 0.000 | 13.505 | 0.000 | 61.530 | 0.000 | 0.013 | `/tmp/aura-benchmarks/aura1-replay-breakdown-20260624T-ms/sdk-larger-aura1-read-batches-row-file-range.json` |
| column batch | 24.607 | 0.045 | 19.692 | 4.858 | 0.000 | 0.000 | 0.000 | 0.012 | `/tmp/aura-benchmarks/aura1-replay-breakdown-20260624T-ms/sdk-larger-aura1-read-batches-columnar-file-range.json` |

Note: fine-grained per-value timers were tested and rejected because they changed
per-row selected replay from roughly 11 ms to roughly 256 ms. The JSON therefore
keeps clean runtimes and reports coarse non-overlapping buckets for hot loops.
Fine-grained field-load/checksum cost should be inferred from operation deltas.

## Controls

| operation | total ms | MB/s | meaning | JSON |
|---|---:|---:|---|---|
| raw file-range scan | 1.443 | 6609 | body read plus minimal byte touch, not replay | `/tmp/aura-benchmarks/aura1-replay-breakdown-20260624T-ms/sdk-larger-aura1-scan-raw-file-range.json` |
| batch view only | 1.247 | 7646 | borrowed batch view construction and callbacks, not parse | `/tmp/aura-benchmarks/aura1-replay-breakdown-20260624T-ms/sdk-larger-aura1-batch-view-only-file-range.json` |
| type-kernel parse control | 3.622 | 2633 | all-field checksum kernel inside batch callback shape | `/tmp/aura-benchmarks/aura1-replay-breakdown-20260624T-ms/sdk-larger-aura1-batch-touch-all-fields-type-kernel-file-range.json` |

## Cost Per Unit

For `sdk-larger`:

| operation | ns/row | ns/value | ns/callback | MB/s | records/sec |
|---|---:|---:|---:|---:|---:|
| per-row noop | 3.91 | n/a | 3.91 | 4874 | 255.55M |
| per-row selected | 23.19 | 7.73 | 23.19 | 822 | 43.12M |
| per-row all | 41.91 | 6.99 | 41.91 | 455 | 23.86M |
| batch selected | 11.51 | 3.84 | 92,823 | 1657 | 86.88M |
| batch all | 7.25 | 1.21 | 58,435 | 2632 | 138.01M |
| row batch | 150.19 | 25.03 | n/a | 127 | 6.66M |
| column batch | 49.21 | 8.20 | n/a | 388 | 20.32M |

## Repeated/Grouped Dataset

Dataset: `repeated-timestamp`, 100,000 records.

| operation | total ms | callbacks/groups | callback reduction | avg rows/group | p95 rows/group | MB/s | JSON |
|---|---:|---:|---:|---:|---:|---:|---|
| per-row selected | 2.091 | 100,000 | 0.0 | 0.0 | 0 | 775 | `/tmp/aura-benchmarks/aura1-replay-breakdown-20260624T-ms/repeated-timestamp-aura1-replay-per-row-touch-selected.json` |
| batch selected | 1.096 | 13 | 0.0 | 0.0 | 0 | 1480 | `/tmp/aura-benchmarks/aura1-replay-breakdown-20260624T-ms/repeated-timestamp-aura1-replay-batch-touch-selected.json` |
| grouped selected | 2.301 | 3,125 | 32.0 | 32.0 | 32 | 705 | `/tmp/aura-benchmarks/aura1-replay-breakdown-20260624T-ms/repeated-timestamp-aura1-replay-grouped-touch-selected.json` |
| grouped all | 2.892 | 3,125 | 32.0 | 32.0 | 32 | 561 | `/tmp/aura-benchmarks/aura1-replay-breakdown-20260624T-ms/repeated-timestamp-aura1-replay-grouped-touch-all.json` |
| grouped aggregate | 2.240 | 3,125 | 32.0 | 32.0 | 32 | 724 | `/tmp/aura-benchmarks/aura1-replay-breakdown-20260624T-ms/repeated-timestamp-aura1-grouped-aggregate.json` |

Grouped replay helps callback count on repeated-run data but is not faster than
batch replay for this synthetic fixture because group detection and group object
work dominate at this scale. On `sdk-larger`, grouping is high-cardinality:
500,000 records produce 500,000 callbacks, so it is expectedly slow.

## Batch Size Sweep

For `sdk-larger`:

| batch size | noop ms | selected ms | all ms | selected MB/s | all MB/s |
|---:|---:|---:|---:|---:|---:|
| 128 | 11.526 | 13.371 | 11.648 | 713 | 819 |
| 1024 | 3.123 | 5.984 | 4.435 | 1594 | 2151 |
| 8192 | 1.226 | 5.755 | 3.623 | 1657 | 2632 |
| 65536 | 1.222 | 5.759 | 3.720 | 1656 | 2564 |
| 500000 | 1.810 | 6.687 | 4.770 | 1426 | 1999 |

Batch size 8192 remains a good default. 65536 is similar, while whole-file
batches regress because each batch range read/view is too large.

## Bottleneck Ranking

- Per-row all: row-view `get_i64` loop plus callback-per-row shape. The 500k
  callback count is cheap by itself (~1.96 ms noop), but field access through the
  row-view path brings total to ~20.96 ms.
- Per-row selected: same shape, fewer fields. The selected delta over noop is
  about 9.64 ms for 1.5M values.
- Batch all: top cost is the all-field type-kernel loop (~2.29 ms), then file
  range/batch view setup (~1.29 ms). Callback overhead is negligible.
- Batch selected: selected field-major loop (~4.33 ms) is slower than all-field
  type-kernel because it uses the selected-field path rather than the optimized
  grouped width kernel.
- Row batch: `AuraValue`/row materialization dominates (~61.53 ms), then checksum
  over materialized rows (~13.51 ms).
- Column batch: field-major decode/vector write dominates (~19.69 ms), then
  checksum (~4.86 ms).
- Grouped high-cardinality: group detection plus group callbacks remain one per
  row, so it is not a throughput path for high-cardinality data.

## Main Answers

Per-row replay is slower than batch replay because it uses one callback per row
and field access through `Aura1RowView`. Batch replay amortizes callbacks and
uses field-major/type-kernel loops over fixed-width batches.

Selected-field replay is faster than all-field replay in per-row mode because it
reads fewer values. In batch mode, all-field replay is faster than selected
replay because the all-field path uses the optimized type kernel while selected
fields currently use the more generic selected-field checksum loop.

Row batch is much slower because it allocates and materializes `AuraValue`s.
Column batch is slower than the parse kernel because it writes typed vectors and
then checksums materialized data.

The next optimization target is clear: implement a selected-field type-kernel
batch replay path. That should attack the current 4.33 ms selected loop and make
selected-field batch replay behave like the all-field type-kernel path instead
of the generic selected-field path.

## API Recommendation

- Max throughput, no materialization: batch replay.
- Actual per-record replay: per-row replay, but use it for ergonomics, not
  maximum throughput.
- Selected-field replay: batch selected replay, with selected type-kernel work as
  the next optimization target.
- Grouped replay: use only when the consumer benefits from fewer callbacks or
  group-level semantics; do not use it as a high-cardinality scan path.
- Materialized analytics: column batches are preferable to row batches.
- Do not call raw scan, view-only construction, or parse-kernel controls replay.
