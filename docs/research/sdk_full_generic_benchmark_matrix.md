# SDK Full Generic Benchmark Matrix

Fresh closeout run:

- result directory: `/tmp/aura-benchmarks/sdk-full-generic-20260623T225031Z/`
- fixture metadata: `/tmp/aura-benchmarks/sdk-full-generic-20260623T225031Z/fixtures/fixtures.json`
- summary JSON: `/tmp/aura-benchmarks/sdk-full-generic-20260623T225031Z/results/sdk_full_matrix_summary.json`
- per-operation JSON files: `/tmp/aura-benchmarks/sdk-full-generic-20260623T225031Z/results/*.json`
- result count: 130
- iterations: 10
- warmups: 2
- batch size: 8192

Runnable datasets:

| Dataset | Records | Fields | Purpose |
|---|---:|---:|---|
| `tiny` | 16 | 6 | baseline compact fixture |
| `dense-few-symbol` | 4,096 | 6 | dense compatibility fixture |
| `sparse-many-symbol` | 1,430 | 8 | sparse compatibility fixture |
| `sdk-tiny` | 4 | 3 | fixed overhead sanity |
| `sdk-narrow` | 10,000 | 2 | fewer fields than grimoire |
| `sdk-wide` | 10,000 | 9 | more fields than grimoire |
| `sdk-reordered` | 10,000 | 4 | noncanonical field order |
| `sdk-dense` | 100,000 | 6 | repeated symbols, dense market-like rows |
| `sdk-sparse` | 100,000 | 5 | many symbols and sparse quantities |
| `sdk-edge-case` | 10,000 | 4 | integer min/max and duplicate timestamps |
| `sdk-larger` | 500,000 | 6 | larger generated SDK workload |
| `nohuff` | 30 | 1 | no-Huffman compact fixture |
| `larger` | 32,768 | 6 | larger baseline compact fixture |

The generated `huff` metadata row remains marked
`blocked_by_specific_format_issue`: the public fixture generator did not produce
a HuffmanDictionary stream under the current speed gate, so that row has no
runnable file paths and is skipped by the SDK matrix runner.

Operations:

- `sdk-write-aura1`
- `sdk-write-aura0-compact`
- `sdk-write-aura0-hybrid`
- `sdk-read-aura1-batches`
- `sdk-read-aura0-batches`
- `sdk-replay-aura1`
- `sdk-convert-aura0-to-aura1`
- `sdk-convert-aura1-to-aura0`
- `sdk-zstd-aura1-to-aura1`
- `sdk-roundtrip-verify`

Selected scoreboard:

| Dataset | Operation | Median ms | P95 ms | Records/sec | Max Rows Materialized | Full File Materialized |
|---|---|---:|---:|---:|---:|---|
| `sdk-dense` | read Aura1 batches | 12.358 | 12.470 | 8.09M | 8192 | false |
| `sdk-dense` | read Aura0 batches | 15.442 | 16.166 | 6.48M | 8192 | false |
| `sdk-dense` | replay Aura1 | 2.561 | 2.616 | 39.04M | 0 | false |
| `sdk-sparse` | read Aura1 batches | 11.984 | 12.392 | 8.34M | 8192 | false |
| `sdk-sparse` | read Aura0 batches | 14.070 | 14.143 | 7.11M | 8192 | false |
| `sdk-larger` | read Aura1 batches | 62.457 | 66.185 | 8.01M | 8192 | false |
| `sdk-larger` | read Aura0 batches | 79.180 | 84.747 | 6.31M | 8192 | false |
| `sdk-larger` | replay Aura1 | 13.471 | 14.251 | 37.12M | 0 | false |
| `larger` | read Aura1 batches | 3.978 | 4.034 | 8.24M | 8192 | false |
| `larger` | read Aura0 batches | 10.650 | 11.299 | 3.08M | 8192 | false |

Interpretation:

- The SDK benchmark matrix now covers generated non-grimoire schemas rather than a smoke-only tiny fixture.
- `AuraReader` reports `streaming_reader_used=true`, `full_file_materialized=false`, and bounded `max_rows_materialized_at_once` for read batch operations.
- Aura0 compact SDK reads still decode stream columns lazily because the compact v1 stream lane is not row-grouped, but they do not materialize full-file row vectors and open does not decode rows.
