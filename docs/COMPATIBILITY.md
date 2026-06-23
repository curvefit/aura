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

## Default Path Matrix

| Role | Current default candidate | Reference/experimental alternatives |
| --- | --- | --- |
| `.aura0 -> .aura1` | `--transcode-path auto --decode-path materialized` | compiled generic fallback covers no-Huffman/`PartitionRuns`; `--decode-path cursor` is correct on huff/nohuff but slower; keep behind flag |
| `.aura1 -> .aura0` | `--transcode-path direct --encoder-path materialized` | materialized fallback is reference-only; `direct-streams` is mixed; `column-free` is a diagnostic rejection |
| `.aura1` replay | `aura1-scan-fixed` / fixed replay visitor | `aura1-parse-to-rows` materializes rows for comparison only |
| guard mode | `no_guard` | strict modes for verification only |
| canonical hash | `none` | `verify` for correctness checks |

The default candidates are conservative. They are chosen from current test and
benchmark evidence, not from an assertion that remaining materialization is
unavoidable.

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

`--transcode-path direct`, `--decode-path cursor`,
`--encoder-path direct-streams`, and `--encoder-path column-free` are
benchmarkable or diagnostic direct-path selectors. They are not all
unconditional defaults because wider fixture coverage and remaining
materialization decisions are still open.

`--decode-path cursor` currently removes Aura0 stream-vector materialization for
supported `.aura0 -> .aura1` plans, but measured grimoire runs were slower than
the materialized/profiled path. Treat it as experimental evidence, not the
production default.

The profiled materialized `.aura0 -> .aura1` path now stays compiled-plan-backed
when the specialized partitioned-sparse writer declines and the generic direct
writer can reconstruct all Aura1 fields. This resolved the prior
no-Huffman/`PartitionRuns` fallback gap:

```text
grimoire-50mb-nohuff Aura0 -> Aura1 bytes:
before generic profiled fallback: 154.896 ms, compiled_plan_used=false
after generic profiled fallback:  107.088 ms, compiled_plan_used=true
```

The current path still materializes 12 stream vectors and 2,754,892 stream
values on the grimoire artifacts. It is not the final answer to the zstd target.

`--encoder-path column-free` is currently rejected with a specific error. The
existing `.aura1 -> .aura0` encoder decodes Aura1 into per-field column buffers
before dictionary/Huffman stream construction, so a real column-free path needs
a new row/replay emitter or two-pass stream builder.

Materialized transcode paths remain reference/correctness fallbacks. They may
emit `compiled_plan_used=false` and `conversion_plan_hash=null`; this is
intentional for reference-only paths and should not be treated as final fast
path evidence.

## Canonical Hash

`--canonical-hash-mode verify` computes a logical i64 row hash for verification.
It is off by default for transcodes and zstd baselines because it adds extra
work. Parse/decode operations still emit canonical hashes by default for
continuity with existing benchmark output.

## Generated Fixture Coverage

`aura-fixture-gen` produces current-format tiny, dense/few-symbol,
sparse/many-symbol, nohuff, and larger fixture pairs plus `.aura1.zst`
baselines. The generated `huff` entry is intentionally blocked rather than
silently mislabeled: the current public writer/planner did not select a
`HuffmanDictionary` stream for generated rows under the 2x Huffman speed gate.
Use the external `grimoire-50mb-huff` artifact for Huffman-heavy benchmarking
until a repo-native Huffman fixture generator or fixture blob is added.

## Aura0 Versus Zstd Status

The current fair product comparison is unresolved in favor of zstd:

```text
Aura0: .aura0 -> .aura1 uncompressed bytes
Zstd:  .aura1.zst -> .aura1 uncompressed bytes
```

Fresh 10-run warm results after the generic profiled fallback:

```text
grimoire-50mb-huff:   Aura0 81.971 ms, zstd L3 62.132 ms
grimoire-50mb-nohuff: Aura0 107.088 ms, zstd L3 61.590 ms
```

The compatibility recommendation is to keep the current semantic Aura0 stream
layout as the compact/canonical cold format, and evaluate an optional
footer-described Aura1 byte lane before claiming a faster-than-zstd cold byte
expansion path.
