# Aura1 All-Field Hot Loop Profile

## Probe

Command:

```text
target/release/aura_sdk_bench --fixture-dir /tmp/aura-benchmarks/aura1-file-backed-fixtures-20260624T173521Z --output-dir /tmp/aura-benchmarks/aura1-all-field-probe --iterations 10 --warmups 2 --batch-size 8192 --datasets sdk-larger --operations aura1-scan-raw-file-range,aura1-batch-view-only-file-range,aura1-batch-touch-all-fields-file-range,aura1-batch-touch-all-fields-field-major-file-range,aura1-batch-touch-all-fields-unchecked-file-range,aura1-batch-touch-all-fields-type-kernel-file-range,aura1-batch-touch-all-fields-instruction-tape-file-range,aura1-batch-selected-one-field-file-range,aura1-batch-selected-two-fields-file-range,aura1-batch-selected-all-fields-file-range,aura1-read-batches-columnar-file-range,aura1-replay-i64-current-file-range
```

Summary path:

```text
/tmp/aura-benchmarks/aura1-all-field-probe/sdk_full_matrix_summary.json
```

## Findings

| operation | values decoded | median ms | p95 ms | MB/s | decision |
|---|---:|---:|---:|---:|---|
| raw scan | 0 | 1.468 | 1.569 | 6495 | control only |
| view only | 0 | 1.393 | 1.574 | 6848 | control only |
| original all-field | 3,000,000 | 11.990 | 13.294 | 795 | baseline |
| field-major safe | 3,000,000 | 11.465 | 12.110 | 832 | rejected as small win |
| checked-once unchecked loads | 3,000,000 | 7.102 | 7.667 | 1343 | kept |
| type-kernel unchecked loads | 3,000,000 | 4.001 | 4.865 | 2384 | kept, passes 2 GB/s |
| parse instruction tape | 3,000,000 | 6.600 | 6.956 | 1445 | kept as evidence, slower than type kernel |
| selected one field | 500,000 | 2.986 | 3.504 | 3194 | kept |
| selected two fields | 1,000,000 | 4.745 | 4.943 | 2010 | kept |
| selected all fields | 3,000,000 | 10.689 | 11.335 | 892 | rejected for all-field speed |
| replay_i64 | 3,000,000 | 15.077 | 15.448 | 633 | ergonomic baseline |
| column batch | 3,000,000 | 24.483 | 25.459 | 390 | materialization baseline |

## Bottleneck decomposition

The previous all-field path was dominated by per-value safe access overhead:
row slice creation, field slice bounds checks, width matching, endian conversion,
and heavy checksum mixing. Field-major alone did not move enough work out of the
hot loop. Checked-once loads removed most repeated range checks and improved the
path from about 795 MB/s to about 1343 MB/s. Width-grouped kernels reduced width
dispatch to one dispatch per group and reached about 2384 MB/s.

The remaining bottleneck is scalar strided AoS loads plus checksum arithmetic. The
raw scan control remains much faster because it does not decode values.
