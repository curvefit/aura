# Aura1 File-Backed Replay Benchmark Results

Date: 2026-06-24

Fixture directory:

`/tmp/aura-benchmarks/aura1-file-backed-fixtures-20260624T173521Z`

Result directory:

`/tmp/aura-benchmarks/aura1-file-backed-20260624T173521Z-focused`

Summary JSON:

`/tmp/aura-benchmarks/aura1-file-backed-20260624T173521Z-focused/sdk_full_matrix_summary.json`

The focused run used 10 timed iterations, 2 warmups, and `batch_size=8192`.
It selected Aura1 scan, replay, row batch, column batch, and grouped replay
operations only. The generated `huff` fixture entry had no runnable file paths
in this fixture set, so it was skipped by the runner. `nohuff` was included.

## Replay And Batch Rows

| Dataset | Operation | Records | Median ms | P95 ms | Records/sec | Source | Bytes at open | Body at open | Full copy |
| --- | --- | ---: | ---: | ---: | ---: | --- | ---: | ---: | ---: |
| sdk-dense | aura1-replay-i64 | 100,000 | 2.050 | 2.088 | 48,786,723 | memory | 2,000,478 | 2,000,000 | 2,000,478 |
| sdk-dense | aura1-replay-file-range | 100,000 | 2.135 | 2.259 | 46,845,802 | file_range | 503 | 0 | 0 |
| sdk-dense | aura1-read-batches-row-file-range | 100,000 | 12.198 | 16.313 | 8,198,175 | file_range | 503 | 0 | 0 |
| sdk-sparse | aura1-replay-i64 | 100,000 | 1.805 | 1.856 | 55,390,093 | memory | 1,600,458 | 1,600,000 | 1,600,458 |
| sdk-sparse | aura1-replay-file-range | 100,000 | 1.866 | 2.738 | 53,576,614 | file_range | 483 | 0 | 0 |
| sdk-larger | aura1-replay-i64 | 500,000 | 11.219 | 12.000 | 44,568,142 | memory | 10,000,460 | 10,000,000 | 10,000,460 |
| sdk-larger | aura1-replay-file-range | 500,000 | 11.670 | 15.040 | 42,843,404 | file_range | 485 | 0 | 0 |
| sdk-larger | aura1-read-batches-row-file-range | 500,000 | 60.533 | 64.137 | 8,259,922 | file_range | 485 | 0 | 0 |

## Grouped Replay Rows

| Dataset | Operation | Median ms | P95 ms | Groups | Callback reduction | Avg rows/group | Source | Full copy |
| --- | --- | ---: | ---: | ---: | ---: | ---: | --- | ---: |
| repeated-timestamp | grouped primary file-range | 4.999 | 5.138 | 3,125 | 32.00x | 32.00 | file_range | 0 |
| repeated-symbol | grouped symbol file-range | 4.794 | 5.360 | 3,125 | 32.00x | 32.00 | file_range | 0 |
| repeated-timestamp-symbol | grouped pair file-range | 5.361 | 11.048 | 2,084 | 47.98x | 47.98 | file_range | 0 |
| high-cardinality | grouped primary file-range | 12.455 | 13.005 | 100,000 | 1.00x | 1.00 | file_range | 0 |
| mixed-burst | grouped primary file-range | 6.994 | 7.173 | 28,247 | 3.54x | 3.54 | file_range | 0 |

## Decision

Keep file-range as the default file-backed Aura1 backend for v1. It proves
DBN-like input behavior by reading only header/footer metadata at open and
scanning the fixed-width body range during replay. Mmap remains a future
optional backend, not a blocker for file-backed replay.
