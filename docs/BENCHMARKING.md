# AURA Benchmarking

Use the release binary:

```text
cargo build --release --bin aura-bench
target/release/aura-bench --help
```

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
--encoder-path materialized|direct-streams
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
