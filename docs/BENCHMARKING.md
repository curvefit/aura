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
/tmp/aura-benchmarks/final-resolution-lane-a/*.json
/tmp/aura-benchmarks/final-resolution-lane-b/*.json
/tmp/aura-benchmarks/final-resolution-lane-c/*.json
/tmp/aura-benchmarks/final-resolution-lane-d/*.json
/tmp/aura-benchmarks/final-resolution-fixtures/fixtures.json
```
