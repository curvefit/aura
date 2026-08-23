# AURA Compatibility

The current format version is checked during footer decode. Unsupported versions
return an error instead of falling back silently.

Current writers emit complete V2 containers. The V3 front-header and schema
descriptor codecs are implemented for development and validation, but complete
V3 footers/bodies remain unsupported. V2 footers reject V3 schema tag 4, and V3
footers reject until their plan/body contract is implemented. The V3 front
header has a normative 16 MiB ceiling enforced before file-backed allocation.

`CompiledAuraPlan` now exposes typed `container_version` and a numeric
`format_version()` compatibility accessor. Direct field access through the old
experimental `format_version` member is an intentional 0.1 API break ahead of
the V3 SDK freeze.

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
| Aura0 profile | `hybrid` for speed + semantic fallback, `compact` for smallest archive | `fast` for byte-lane-only speed files |

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

The current fair product comparison for compact semantic Aura0 remains in favor
of zstd:

```text
Aura0: .aura0 -> .aura1 uncompressed bytes
Zstd:  .aura1.zst -> .aura1 uncompressed bytes
```

Fresh 10-run warm results after real byte-lane integration:

```text
grimoire-50mb-huff:   compact Aura0 80.608 ms, zstd L3 61.386 ms
grimoire-50mb-nohuff: compact Aura0 104.173 ms, zstd L3 62.798 ms
```

Real Aura0 fast/hybrid byte-lane files reverse that product target:

```text
grimoire-50mb-huff:   fast raw 24.753 ms, fast lz4 44.373 ms, hybrid lz4 43.761 ms
grimoire-50mb-nohuff: fast raw 25.271 ms, fast lz4 43.281 ms, hybrid lz4 44.058 ms
```

Compatibility recommendation:

- Keep the current semantic Aura0 stream layout as the stable compact/canonical
  cold format.
- Use Aura0-hybrid + lz4 when the product requirement is faster-than-zstd
  Aura1 byte expansion while retaining the semantic lane for verification or
  fallback.
- Use Aura0-fast + lz4 when byte-output speed and smaller-than-raw size matter
  more than semantic-lane fallback.
- Do not claim current compact `.aura0` files are faster than zstd for byte
  expansion; claim that fast/hybrid byte-lane profiles beat external zstd L3 in
  the measured huff/nohuff runs.

Old readers may reject new fast/hybrid files because the `AURP` footer has an
optional trailing `AUBL` extension. New readers read old compact files because
the extension is omitted when no byte lanes are present.

## Byte-lane safety limits

Fast and hybrid byte lanes use all-memory expansion in the current reader, so
the supported V2 byte-lane subset has fixed fail-closed limits:

- at most 65,536 byte-lane descriptors;
- at most 1 GiB (`1 << 30` bytes) of compressed payload per lane;
- at most 1 GiB of uncompressed output per lane; and
- at most 1 GiB of total expanded output across all lanes.

Descriptor tables, offsets, lengths, row ranges, and cumulative output are
checked before allocation or decompression. Output allocation is fallible, and
raw, LZ4, and Zstd payloads must produce the exact declared bounded length.
These are normative security ceilings for the current all-memory byte-lane
implementation, not benchmark tuning parameters. Experimental V2 fast/hybrid
artifacts above these ceilings are outside the promised compatibility subset
and reject with a typed error. Compact semantic Aura0 files do not use this
byte-lane output limit.

Ingest and compiled V2 footers also support at most 65,536 chunk descriptors.
Each chunk descriptor is a fixed 76-byte record; decoders validate the count,
checked table size, and remaining footer bytes before fallible allocation.
