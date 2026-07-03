# Aura1 True Parse Results

Date: 2026-06-24

Final result directory:

`/tmp/aura-benchmarks/aura1-true-parse-c701d16`

Summary JSON:

`/tmp/aura-benchmarks/aura1-true-parse-c701d16/sdk_full_matrix_summary.json`

The benchmark separates view construction from real field access. A row counts
as true parse only when the operation reports non-zero `fields_accessed`,
`values_decoded`, `bytes_touched`, and a value-dependent `checksum`.

## sdk-larger Final Matrix

| Operation | Fields | Values decoded | Callbacks | Median ms | P95 ms | MB/sec |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| raw scan file-range | 0 | 0 | 0 | 1.377 | 1.516 | 6924.0 |
| batch view only file-range | 0 | 0 | 62 | 1.262 | 1.769 | 7557.6 |
| batch touch one field file-range | 1 | 500,000 | 62 | 2.539 | 2.646 | 3756.2 |
| batch touch all fields file-range | 6 | 3,000,000 | 62 | 12.163 | 12.518 | 784.1 |
| row view only file-range | 0 | 0 | 500,000 | 1.980 | 2.153 | 4815.9 |
| row view one field file-range | 1 | 500,000 | 500,000 | 5.737 | 7.892 | 1662.5 |
| row view all fields file-range | 6 | 3,000,000 | 500,000 | 21.276 | 25.136 | 448.3 |
| current replay_i64 file-range | 6 | 3,000,000 | 500,000 | 16.719 | 19.783 | 570.4 |
| column batch file-range | 6 | 3,000,000 | 0 | 26.018 | 30.560 | 366.6 |
| row batch file-range | 6 | 3,000,000 | 0 | 74.246 | 82.065 | 128.5 |

## Grouped Truth

| Dataset | Operation | Groups | Values decoded | Median ms | P95 ms | Callback reduction |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| repeated-timestamp | grouped view only | 3,125 | 0 | 1.432 | 2.182 | 32.00x |
| repeated-timestamp | grouped touch key | 3,125 | 3,125 | 1.456 | 1.525 | 32.00x |
| repeated-timestamp | grouped touch all fields | 3,125 | 500,000 | 2.897 | 3.013 | 32.00x |
| timestamp+symbol | grouped view only | 2,084 | 0 | 1.325 | 1.443 | 47.98x |
| timestamp+symbol | grouped touch all fields | 2,084 | 500,000 | 2.796 | 3.000 | 47.98x |
| high-cardinality | grouped view only | 100,000 | 0 | 10.217 | 14.167 | 1.00x |
| high-cardinality | grouped touch all fields | 100,000 | 500,000 | 11.604 | 12.213 | 1.00x |

## Interpretation

- `batch view only` is not parse throughput. It reports zero decoded values.
- `batch touch all fields` is the fastest all-field true parse path in the
  probe.
- `row view one field` is useful for selected-field ergonomic replay, but
  `row view all fields` is slower than the current `replay_i64` stack-row path.
- `replay_i64` is dominated by all-field decode plus one callback per row. It
  uses bounded stack row buffers, not heap row vectors.
- Row and column batch paths measure materialization cost, not just parsing.
