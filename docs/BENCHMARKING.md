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
