# Aura1 Parse Speed Results

Date: 2026-06-24

Fixture directory:

`/tmp/aura-benchmarks/aura1-file-backed-fixtures-20260624T173521Z`

Result directory:

`/tmp/aura-benchmarks/aura1-parse-speed-clean-a2a0c71`

Summary JSON:

`/tmp/aura-benchmarks/aura1-parse-speed-clean-a2a0c71/sdk_full_matrix_summary.json`

The focused run used 10 timed iterations, 2 warmups, and `batch_size=8192`.
It selected Aura1 raw scan, per-row replay, batch-callback replay, row batches,
column batches, and grouped replay in memory and file-range modes.

## Replay And Parse

| Dataset | Operation | Records | Median ms | P95 ms | Records/sec | MB/sec | Visitor calls |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| sdk-larger | raw scan file-range | 500,000 | 1.450 | 1.567 | 344.92M | 6579.2 | 0 |
| sdk-larger | per-row replay file-range | 500,000 | 14.550 | 15.059 | 34.36M | 655.5 | 500,000 |
| sdk-larger | batch-callback replay file-range | 500,000 | 1.244 | 1.375 | 401.85M | 7664.9 | 62 |
| sdk-larger | row batches file-range | 500,000 | 62.284 | 65.685 | 8.03M | 153.1 | 0 |
| sdk-larger | column batches file-range | 500,000 | 21.959 | 23.114 | 22.77M | 434.3 | 0 |

Batch-callback replay is not equivalent to per-row replay. It is a lower-level
fixed-width batch view for callers that can process row ranges and pull only
the fields they need.

## Grouped Replay

| Dataset | Operation | Median ms | P95 ms | Groups | Callback reduction | Field decodes |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| repeated-timestamp | grouped primary file-range | 1.534 | 1.650 | 3,125 | 32.00x | 3,125 |
| repeated-symbol | grouped symbol file-range | 1.530 | 1.765 | 3,125 | 32.00x | 3,125 |
| repeated-timestamp-symbol | grouped pair file-range | 1.889 | 2.773 | 2,084 | 47.98x | 4,168 |
| high-cardinality | grouped primary file-range | 8.989 | 9.366 | 100,000 | 1.00x | 100,000 |
| mixed-burst | grouped primary file-range | 3.802 | 4.710 | 28,247 | 3.54x | 28,247 |

The byte-key grouped path compares fixed-width key bytes in the hot loop and
materializes typed `AuraValue` keys only at group boundaries. This removes the
previous per-row key `Vec<i64>` allocation.

## Decisions

- Keep file-range as the v1 file-backed source backend.
- Keep `replay_i64` as the per-row compatibility API.
- Add `replay_fixed_batches` for callers that can process fixed-width row
  ranges with one callback per batch.
- Keep grouped replay as opt-in consecutive-run grouping.
- No footer group index for v1. On-the-fly byte-key grouping is already fast on
  repeated-run datasets and high-cardinality inputs correctly show 1.00x
  callback reduction.
- Reject mmap for this sprint: file-range raw scan and batch-callback replay
  are already around 7 GB/s, so the current bottleneck is API work, not file
  range I/O.
