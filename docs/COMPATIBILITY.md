# AURA Compatibility

The current format version is checked during footer decode. Unsupported versions
return an error instead of falling back silently.

## Supported Current Paths

- `.aura -> .aura0`
- `.aura -> .aura1`
- `.aura0 -> .aura1`
- `.aura1 -> .aura0`
- `.aura1` fixed replay visitor

Current fast benchmark paths are i64-oriented. Typed wide values can round-trip
through `.aura`, but compiled i64 paths reject schemas with wide fields.

## Guard Modes

Default performance benchmarks use `no_guard` unless a strict mode is requested.
Strict modes preserve output-byte guard semantics:

```text
fused_output_guard
old_post_output_guard
block_batched_output_guard
```

Do not compare guarded and unguarded runs as speedups.

## Experimental Paths

`--transcode-path direct` and `--encoder-path direct-streams` are benchmarkable
direct paths. They are not yet unconditional defaults because wider fixture
coverage and remaining materialization decisions are still open.

## Canonical Hash

`--canonical-hash-mode verify` computes a logical i64 row hash for verification.
It is off by default for transcodes and zstd baselines because it adds extra
work. Parse/decode operations still emit canonical hashes by default for
continuity with existing benchmark output.
