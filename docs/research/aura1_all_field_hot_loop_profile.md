# Aura1 All-Field Hot Loop Profile

## Probe

Command:

```text
target/release/aura_sdk_bench --fixture-dir /tmp/aura-benchmarks/aura1-file-backed-fixtures-20260624T173521Z --output-dir /tmp/aura-benchmarks/aura1-all-field-parse-22c3c1a --iterations 10 --warmups 2 --batch-size 8192 --datasets sdk-larger,sdk-dense,sdk-sparse,sdk-wide,sdk-reordered,repeated-timestamp,repeated-timestamp-symbol,high-cardinality,nohuff --operations aura1-scan-raw-file-range,aura1-batch-view-only-file-range,aura1-batch-touch-all-fields-file-range,aura1-batch-touch-all-fields-field-major-file-range,aura1-batch-touch-all-fields-unchecked-file-range,aura1-batch-touch-all-fields-type-kernel-file-range,aura1-batch-touch-all-fields-instruction-tape-file-range,aura1-batch-selected-one-field-file-range,aura1-batch-selected-two-fields-file-range,aura1-batch-selected-all-fields-file-range,aura1-row-view-all-fields-file-range,aura1-replay-i64-current-file-range,aura1-read-batches-columnar-file-range,aura1-grouped-touch-all-fields,aura1-grouped-aggregate
```

Summary path:

```text
/tmp/aura-benchmarks/aura1-all-field-parse-22c3c1a/sdk_full_matrix_summary.json
```

## Findings

| operation | values decoded | median ms | p95 ms | MB/s | decision |
|---|---:|---:|---:|---:|---|
| raw scan | 0 | 1.468 | 1.572 | 6494 | control only |
| view only | 0 | 1.364 | 1.474 | 6994 | control only |
| original all-field | 3,000,000 | 11.904 | 12.185 | 801 | baseline |
| field-major safe | 3,000,000 | 11.478 | 11.605 | 831 | rejected as small win |
| checked-once unchecked loads | 3,000,000 | 6.517 | 6.986 | 1464 | kept |
| type-kernel unchecked loads | 3,000,000 | 3.750 | 4.092 | 2543 | kept, passes 2 GB/s |
| parse instruction tape | 3,000,000 | 6.205 | 6.593 | 1537 | kept as evidence, slower than type kernel |
| selected one field | 500,000 | 3.028 | 3.449 | 3150 | kept |
| selected two fields | 1,000,000 | 4.918 | 5.164 | 1939 | kept |
| selected all fields | 3,000,000 | 10.793 | 11.273 | 884 | rejected for all-field speed |
| replay_i64 | 3,000,000 | 14.862 | 15.256 | 642 | ergonomic baseline |
| column batch | 3,000,000 | 24.016 | 24.348 | 397 | materialization baseline |

## Bottleneck decomposition

The previous all-field path was dominated by per-value safe access overhead:
row slice creation, field slice bounds checks, width matching, endian conversion,
and heavy checksum mixing. Field-major alone did not move enough work out of the
hot loop. Checked-once loads removed most repeated range checks and improved the
path from about 801 MB/s to about 1464 MB/s. Width-grouped kernels reduced width
dispatch to one dispatch per group and reached about 2543 MB/s.

The remaining bottleneck is scalar strided AoS loads plus checksum arithmetic. The
raw scan control remains much faster because it does not decode values.
