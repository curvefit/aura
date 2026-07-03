# Benchmarking

## Current `aura-bench` CLI

The repository includes a release benchmark binary named `aura-bench`.
Current operations are:

```text
parse-aura1
decode-aura0
transcode-aura1-to-aura0
transcode-aura0-to-aura1
```

Common options:

```text
--dataset <name>
--input <path>
--iterations <n>
--warmups <n>
--format json|csv
--output <report-path>
--cache-mode warm|cold
--guard-mode no_guard|fused_output_guard|old_post_output_guard|block_batched_output_guard
--transcode-path auto|materialized|direct
--encoder-path materialized|direct-streams
```

For transcode operations, benchmark output bytes can be preserved and decoded
after the measured loop:

```text
--preserve-output <aura-output-path>
--verify-output-decodes
```

`--preserve-output` writes the produced transcode bytes to the requested path.
`--verify-output-decodes` decodes the source and produced output after timing
and reports semantic row equality without adding that decode to `runtime_ns`.
The JSON report includes:

```text
output_preserved
output_path
preserve_output_runtime_ns
verify_output_decodes_requested
conversion_plan_hash
decoded_row_equality
record_count_equality
schema_footer_validation
byte_equality
canonical_hash_equality
output_byte_guard_equality
output_verification_runtime_ns
```

`byte_equality` and `canonical_hash_equality` are `null` unless a benchmark mode
explicitly checks them. `output_byte_guard_equality` is populated when the
selected guard mode produced an output-byte guard. `conversion_plan_hash` is
populated by profiled direct transcodes that build a compiled footer plan.

## Benchmarking Plan

Aura profiles should be compared with synthetic and private real-world inputs,
but public benchmarks should remain source-neutral.

Measure at least:

```text
encoded bytes per event
encoded bytes per changed level
decode events/sec
decode levels/sec
ingest -> compiled conversion throughput
padding overhead by block size
compression ratio by chunk size and zstd level
```

Benchmark matrix:

```text
level: .aura, .aura0, .aura1
Aura1 block capacity: 1, 2, 4, 8, 16, 32
chunk target: 16 MiB, 32 MiB, 64 MiB
compression: none, zstd low, zstd high
```

Important caveats:

- repeated timestamps can make Aura1 block packing smaller,
- one event per header is the simplest max-speed baseline,
- larger block sizes can parse faster but increase padding,
- outlier events should only pay for their own extra blocks,
- whole-file compression is simpler but blocks parallel conversion.
