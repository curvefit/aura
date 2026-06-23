# AURA Benchmarking

Use the release binary:

```text
cargo build --release --bin aura-bench
target/release/aura-bench --help
```

Compatible benchmark fixture metadata can be generated with:

```text
cargo build --release --bin aura-fixture-gen
target/release/aura-fixture-gen --output-dir /tmp/aura-benchmarks/generated-fixtures --zstd-level 3
```

The generator writes `.aura`, `.aura0`, `.aura1`, `.aura1.zst`, and
`fixtures.json` entries for tiny, dense/few-symbol, sparse/many-symbol, nohuff,
and larger generated fixtures. The huff fixture entry is currently marked
`blocked_by_specific_format_issue`: the public writer/planner did not produce a
`HuffmanDictionary` stream under the current 2x Huffman speed gate during
generation, so `grimoire-50mb-huff` remains an external compatible artifact.

Implemented operations:

```text
parse-aura1
decode-aura0
transcode-aura1-to-aura0
transcode-aura0-to-aura1
aura1-scan-fixed
aura1-replay-callback
aura1-parse-to-rows
zstd-decompress-only
zstd-decompress-plus-parse
zstd-decompress-plus-replay
zstd-decompress-plus-aura1-output
aura0-to-aura1-bytes
aura0-to-aura1-bytes-verify
zstd-aura1-to-aura1-bytes
zstd-aura1-to-aura1-bytes-verify
aura0-byte-lane-to-aura1-bytes
aura0-byte-lane-to-aura1-bytes-verify
```

Important flags:

```text
--guard-mode no_guard|fused_output_guard|old_post_output_guard|block_batched_output_guard
--transcode-path auto|materialized|direct
--decode-path materialized|cursor
--encoder-path materialized|direct-streams|column-free
--canonical-hash-mode none|verify
--preserve-output <path>
--verify-output-decodes
--zstd-level <level>
--aura0-profile compact|fast|hybrid
--byte-lane-codec raw|lz4|zstd1|zstd3|zstd9
--use-byte-lane auto|always|never
--reference-aura0 <path>
--reference-aura1 <path>
```

Transcode benchmarks can preserve and verify output bytes. Verification decodes
the source and produced output after timing and reports row equality,
record-count equality, output SHA256, output-byte guard equality, and canonical
hash equality when available.

Aura1 replay operations measure fixed-width scan/replay without Aura0 encoding.
`aura1-parse-to-rows` is intentionally separate and materializes rows.

Zstd baselines compress the input once before timed iterations, then time
decompression and optional parse/replay work. They are labeled by
`baseline_kind=zstd` and `work_included`; do not compare zstd decompress-only to
AURA decode+write as equivalent work.

The fair product comparison is:

```text
aura0-to-aura1-bytes
zstd-aura1-to-aura1-bytes
```

Both write Aura1 bytes into the same in-memory `Vec<u8>` sink. The `*-verify`
variants compare output bytes against `--reference-aura1` and emit
`dataset_sha256_aura0`, `dataset_sha256_aura1`,
`dataset_sha256_aura1_zst`, compressed sizes, uncompressed Aura1 size,
`output_bytes_equal`, and `output_byte_hash`.

The fair benchmark previously overstated production time because `Option::then_some`
eagerly evaluated output equality/hash work even when verification was disabled.
The production path now skips equality/hash work unless a `*-verify` operation
is selected.

The benchmark harness also exposes real Aura0 fast/hybrid byte-lane profiles:

```text
aura0-byte-lane-to-aura1-bytes
aura0-byte-lane-to-aura1-bytes-verify
```

This operation builds a real Aura0 file outside timed iterations with
`--aura0-profile fast|hybrid`, then times production Aura0 reader expansion into
Aura1 bytes. Production mode validates lane structure and lengths but does not
scan the output guard. Verify mode validates output equality and output-byte
guard. The byte lane is serialized in the compiled footer as an `AUBL`
extension.

Materialized paths are retained as reference/correctness fallbacks. Final
candidate path decisions should use direct/profiled paths that emit
`compiled_plan_used=true` and a non-null `conversion_plan_hash`.

`--decode-path cursor` is a benchmarkable `.aura0 -> .aura1` experimental path
for supported plans. It avoids materialized stream vectors, but current measured
grimoire runs are slower than the materialized/profiled path, so it is not a
default candidate.

`--encoder-path column-free` is accepted only to report the current blocker.
It fails before encoding because the current `.aura1 -> .aura0` encoder API
requires Aura1 column buffers for dictionary/Huffman stream construction.

## Default Benchmark Commands

Current default-candidate command shapes:

```text
target/release/aura-bench --operation aura0-to-aura1-bytes --guard-mode no_guard --canonical-hash-mode none
target/release/aura-bench --operation transcode-aura1-to-aura0 --transcode-path direct --encoder-path materialized --guard-mode no_guard --canonical-hash-mode none
target/release/aura-bench --operation aura1-scan-fixed --canonical-hash-mode none
```

Strict verification should preserve outputs and decode them:

```text
target/release/aura-bench --operation transcode-aura0-to-aura1 --guard-mode old_post_output_guard --canonical-hash-mode verify --preserve-output <out.aura1> --verify-output-decodes
target/release/aura-bench --operation transcode-aura1-to-aura0 --transcode-path direct --encoder-path materialized --guard-mode old_post_output_guard --canonical-hash-mode verify --preserve-output <out.aura0> --verify-output-decodes
```

Fresh resolution artifacts used for the default decision:

```text
/tmp/aura-benchmarks/zstd-resolution-lane-5/*.json
/tmp/aura-benchmarks/zstd-resolution-pre-fallback/*.json
/tmp/aura-benchmarks/final-resolution-lane-a/*.json
/tmp/aura-benchmarks/final-resolution-lane-b/*.json
/tmp/aura-benchmarks/final-resolution-lane-c/*.json
/tmp/aura-benchmarks/final-resolution-lane-d/*.json
/tmp/aura-benchmarks/final-resolution-fixtures/fixtures.json
/tmp/aura-benchmarks/real-byte-lane-production-20260623T032512Z/*.json
```

Current fair Aura0-vs-zstd product scoreboard:

| Dataset | Aura0 bytes | Aura1.zst L3 bytes | Aura1 bytes | Aura0 -> Aura1 ms | zstd -> Aura1 ms | Winner |
| --- | ---: | ---: | ---: | ---: | ---: | --- |
| grimoire-50mb-huff | 1,879,040 | 5,542,945 | 36,410,980 | 80.608 | 61.386 | zstd |
| grimoire-50mb-nohuff | 2,246,910 | 5,538,275 | 36,403,133 | 104.173 | 62.798 | zstd |

Current real Aura0-fast/hybrid byte-lane scoreboard:

| Dataset | Profile | Codec | Aura0 profile bytes | Aura1 bytes | Lane -> Aura1 ms | zstd L3 -> Aura1 ms | Winner |
| --- | --- | --- | ---: | ---: | ---: | ---: | --- |
| grimoire-50mb-huff | fast | raw | 36,419,472 | 36,410,980 | 24.753 | 61.386 | byte lane |
| grimoire-50mb-huff | fast | lz4 | 9,795,104 | 36,410,980 | 44.373 | 61.386 | byte lane |
| grimoire-50mb-huff | hybrid | lz4 | 11,665,724 | 36,410,980 | 43.761 | 61.386 | byte lane |
| grimoire-50mb-nohuff | fast | raw | 36,403,778 | 36,403,133 | 25.271 | 62.798 | byte lane |
| grimoire-50mb-nohuff | fast | lz4 | 9,781,060 | 36,403,133 | 43.281 | 62.798 | byte lane |
| grimoire-50mb-nohuff | hybrid | lz4 | 12,027,397 | 36,403,133 | 44.058 | 62.798 | byte lane |

Use verify-mode JSON from
`/tmp/aura-benchmarks/real-byte-lane-production-20260623T032512Z/*_verify.json`
for output equality evidence. Fast lz4 verify runs reported
`output_bytes_equal=true` with output byte hashes
`12194870092346231300` for huff and `10372430540135078667` for nohuff.
# SDK Generic Matrix

For SDK closeout benchmarks, generate fixtures and run the SDK matrix:

```bash
target/release/aura-fixture-gen --output-dir /tmp/aura-sdk-fixtures --zstd-level 3 --sdk-full
target/release/aura_sdk_bench --fixture-dir /tmp/aura-sdk-fixtures --output-dir /tmp/aura-sdk-results --iterations 10 --warmups 2 --batch-size 8192
```

The runner writes one JSON result per dataset/operation and a `sdk_full_matrix_summary.json` aggregate. Results include schema metadata, p95, median, records/sec, streaming flags, materialization counters, command, git commit, and dirty status.

## Aura1 Parse Matrix

`aura_sdk_bench` also emits Aura1 parse-speed rows:

- `aura1-scan-raw`: fixed body byte scan, no value decode.
- `aura1-replay-i64`: visitor replay over fixed-width rows.
- `aura1-read-batches-row`: row-oriented `AuraRecordBatch` materialization.
- `aura1-read-batches-columnar`: direct `AuraColumnBatch` materialization.
- `aura1-grouped-replay-primary`: consecutive-run grouped replay by the first
  timestamp field, or the first field if no timestamp exists.
- `aura1-grouped-replay-symbol`: consecutive-run grouped replay by the first
  symbol/id-like field when present.
- `aura1-grouped-replay-pair`: grouped replay by timestamp plus a symbol/id-like
  field when present.

The generated fixture matrix includes repeated timestamp, repeated symbol,
timestamp+symbol, high-cardinality, and mixed-burst datasets. JSON rows report
`rows_materialized`, `values_materialized`, `group_count`,
`rows_per_group_avg`, `rows_per_group_p95`, and
`callback_count_reduction`.

Do not compare grouped replay directly to row replay unless the callback
semantics are labeled: grouped replay invokes one callback per consecutive run,
not one callback per row.
