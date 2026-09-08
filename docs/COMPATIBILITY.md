# AURA Compatibility

The current format version is checked during footer decode. Unsupported versions
return an error instead of falling back silently.

`LegacyV1` is recognized for historical front-header parsing only. This crate
has no supported complete V1 container/footer read or write contract; recognition
of its header does not imply a successful complete-file decode.

Current production and default SDK writers emit complete V2 containers. V3 has
two complete, explicitly selected, uncompressed Aura0 SDK compatibility
subsets: flat event-only `AURAV3VB` exact-value blocks, and grouped exact-event
`AURAV3EB` chunks with the AURP V3 grouped footer. V3 also has a separate
planned-flat development route with tuple-specific complete-file receipts;
planned compilation/verification is an all-memory reference path, not a
seekable SDK compatibility subset. Flat and grouped profiles have public
seekable writers/readers and bounded verification, and all three have explicit
V3 CLI seal/held-file auto-dispatch verification routes. V3 adds no ingest/Aura1
layout, stamping, or V2 conversion path, and is not a production/default
replacement. V2 footers still reject V3 schema tag 4. Both V3 front headers
have a normative 16 MiB ceiling enforced before file-backed allocation.

The grouped compatibility subset is exact logical execution of one repeated,
two-domain group: byte-200 marks a non-null U8 `side` discriminator, all
repeated child slots are covered, nullable event/repeated values retain validity
bitmaps, and authoritative event-to-child offsets plus global event/child ranges
are preserved in source order. Grouped encoding is uncompressed and does not
run a physical relationship planner, compression, or Plan v2. Non-empty derived
expression tables are rejected by both complete V3 writers.

The planned-grouped reference compiler and held-ingest writer are an additive
development route, not part of the grouped seekable-writer compatibility promise. Plan-v2
registries/body layouts 1-4 retain their existing dispatch. Registry/body
version 5 adds only schema-authorized, checked cross-domain same-slot residuals
with two serialized orientations and per-event ordinal pairing; unmatched
tails are absolute. Complete schema/plan/footer metadata is charged before
selection, Direct wins ties, and decoding uses only the embedded plan. This
does not change the exact-event fixture, V2 bytes, default writers, or Aura1.
The held writer's final bytes match the reference compiler for the same batch
and chunk contract. Its open scratch has no footer/trailer and is never a
decodable compatibility artifact; publication is synced and create-once.

The `AURAV3VB` exact-value reference block is a separate, explicitly V3 API.
Its 32-bit schema ID is only a routing hint; a SHA-256 fingerprint of the
canonical tag-4 schema is the strong binding. It is not a container or
conversion target. Version 1 supports flat event-scoped schemas only and
rejects repeated fields/groups rather than flattening child rows. V2 schema encode/decode and
V2 ingest/compiled writer paths reject field codes 12
(`TimestampMs`), 13 (`Utf8`), and 14 (`DecimalText`). Codes 1 through 11 and all
checked-in V2 fixture bytes/hashes remain unchanged. Adding the reference block
does not by itself permit a V3 writer, stamp, restamp, or profile conversion.

The versioned fixture under `tests/fixtures/v3/` freezes a canonical schema,
hex-encoded reference-block bytes, schema fingerprint, block SHA-256, logical
SHA-256, and row count. It is explicitly a standalone block fixture, not a
complete Aura-file compatibility fixture. Regeneration is an ignored,
explicitly invoked maintenance test; normal tests only verify checked-in bytes.

`tests/fixtures/v3-container/` separately freezes an empty flat file and an
exact three-row/two-chunk file with timestamp, nullable U64 sequence, nullable
U8 boolean, required UTF-8 (including empty and embedded NUL), and nullable
decimal text with Unicode outer whitespace. Its manifest records exact sizes
and SHA-256 values. Regeneration is an ignored maintenance test only.

`tests/fixtures/v3-grouped-container/` freezes the grouped Aura0 V3 exact-event
v1 fixture bytes, routing tuple, schema identity, event/child/chunk counts, and
global logical hash. Its manifest records body encoding 2, body/footer layout
versions, and the artifact SHA-256. These bytes and hashes are the grouped
compatibility promise; normal tests regenerate the in-memory artifact and
compare it with these checked-in bytes.

## Planned-flat v5 reader compatibility fixture

`tests/fixtures/v3-planned-v5/` freezes one small, complete registry-4/layout-7
planned-flat Aura0 artifact: a two-field schema (`TimestampNs` plus nullable
`Utf8`) in two 256-row batches with deterministic common-prefix/suffix text.
Its manifest binds the complete artifact, schema and plan IDs/fingerprints,
header/body/footer/global-logical hashes, chunk descriptors, route tuple,
`AUFPZB03`/`AUFPVB04`/`AUF2` markers, and registry-4 codec class. The normal
reader test verifies those fields, exact decoded values, and canonical hashes.

This is a reader-compatibility promise only. It deliberately does not freeze
the seven-candidate planner cost table, planner tie behavior, future writer
selection, schema-label invariance, or a production/default path. Registry-1,
-2, and -3 route coverage remains in the existing generated planned-flat CLI
tests referenced by the fixture manifest; no duplicate binaries are retained.

Regeneration is an explicit maintenance action and is not part of normal CI:

```text
AURA_PLANNED_V5_FIXTURE_GENERATE=1 cargo test --offline --test planned_v5_compatibility -- --ignored generate_planned_v5_compatibility_fixture
```

`AnyCompiledFooter` provides version-aware AURP routing. It delegates V2 bytes
to the unchanged `CompiledFooter` codec, routes V3 body encoding 1 to the flat
footer decoder, and routes V3 body encoding 2 to the grouped footer decoder.
Checked-in V2 fixture routing and hashes are unchanged; V3 layouts are never
silently reinterpreted as one another.

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
- V3 flat Aura0 SDK writer/reader (`AURAV3VB`)
- V3 grouped Aura0 SDK writer/reader (`AURAV3EB`)

Current fast benchmark paths are i64-oriented. Typed wide values can round-trip
through `.aura`, but compiled i64 paths reject schemas with wide fields.

## V3 Limits and failure boundary

Flat V3 hard ceilings are a 64 MiB footer, 1 TiB body, 65,536 chunks,
16,777,216 rows, 1 GiB per exact-value block, 16 MiB per variable value, and
16 MiB schema descriptor. Its default in-memory limits are 256 MiB body/block,
4,194,304 rows, and 4,096 chunks. Grouped V3 hard ceilings are a 64 MiB
footer, 1 TiB body, 65,536 chunks, 16,777,216 events, 67,108,864 children,
and 16 MiB schema descriptor. Its defaults are 256 MiB body, 4,096 chunks,
1,048,576 events, and 4,194,304 children. Grouped event-block defaults are
256 MiB, 1,048,576 events, 4,194,304 children, and 16,777,216 values; hard
limits are 1 GiB, 4,194,304 events, 16,777,216 children, and 67,108,864
values. `V3FlatLimits::HARD` and `V3GroupedLimits::HARD` are explicit opt-ins;
caller limits cannot raise the format ceilings.

V3 readers require the complete header/body/footer/trailer envelope and reject
truncated or invalid u32 footer lengths, missing seals, unsupported versions,
schema/map disagreement, non-contiguous event/child/body ranges, malformed
null/value planes, stored/logical/body/global hash mismatches, footer self-hash
failures, and trailing bytes. The seekable grouped reader treats open-time
metadata as provisional until `verify_all`/`verify_with` completes its bounded
passes and envelope recheck. These checks detect corruption and observed
mutation; they do not promise concurrent-mutation exclusion or recovery.

For both V3 SDK writers, a write before the complete seal, or any writer,
flush, sync, or second-pass error, is a failed/uncommitted result that callers
must discard and must not publish even if bytes happen to end in a valid seal.
The flat and grouped CLI paths share one synced mode-0600 temporary-file,
create-once publication and recovery state machine. Verification boundedly
routes from the held footer tuple, then fully verifies and hashes that exact
held file without an external schema.

## Execution and storage choices

| Role | Current default candidate | Reference/experimental alternatives |
| --- | --- | --- |
| `.aura0 -> .aura1` | `--transcode-path auto --aura1-body-path stable-auto` | compiled generic fallback covers no-Huffman/`PartitionRuns`; `streaming-cursor` is correct where supported but slower; keep explicit |
| `.aura1 -> .aura0` | `--transcode-path direct --encoder-path materialized` | materialized fallback is reference-only; `direct-streams` is mixed; `column-free` is a diagnostic rejection |
| `.aura1` replay | `aura1-scan-fixed` / fixed replay visitor | `aura1-parse-to-rows` materializes rows for comparison only |
| guard mode | `no_guard` | strict modes for verification only |
| canonical hash | `none` | `verify` for correctness checks |
| Aura0 profile | `compact` is the SDK writer/converter default | `hybrid` adds a byte lane with semantic fallback; `fast` is byte-lane-only |
| V3 flat/grouped SDK | no production/default candidate; select the explicit API or CLI | `V3FlatLimits::HARD` / `V3GroupedLimits::HARD` for explicit hard-envelope tests |

The execution candidates describe benchmark paths, not a universal speed
promise. Optional storage profiles must be chosen explicitly. Historical
workload results are labeled in [BENCHMARKING.md](BENCHMARKING.md#historical-benchmark-evidence).

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

Codec implementation strategy is explicit and process-local. Library callers
may pass `Aura1ExecutionOptions`; ordinary writer/conversion APIs always use
`StableAuto`. Environment variables are not part of Aura's execution contract
and cannot switch production codec paths. Explicit strategies affect neither
wire bytes nor compatibility promises: supported strategies must emit the same
Aura1 bytes as `StableAuto`, and unsupported strategies either return an Aura
error or report a deliberate fallback according to the caller's policy.
The typed API defaults to `UnsupportedPathBehavior::Error`; callers must opt in
to `FallbackToStable` explicitly.

The established `ProfiledCompileOutput` remains unchanged. The explicit-options
API returns it together with a separate `Aura1ExecutionTrace`, so requested and
effective paths, embedded-byte-lane dispatch, column-path applicability, and
fallback reasons remain auditable without expanding the compatibility surface
of existing profiled callers.

The strategy enums are not serialized. Adding, removing, or tuning an execution
strategy therefore does not create a format version. V2 compatibility fixtures
remain the byte-level authority for stable writer output.

The materialized Aura0-to-Aura1 reference compiler is a separate API and
execution path. Its counters prove row/field/value materialization, and its
output must remain byte-identical to stable Aura1 output. Embedded AUBL bytes
may be used only as an input decoder for pure-fast Aura0; they are never copied
as the materialized output path.

V2 row-materializing readers enforce non-configurable hard ceilings before
allocating: 4,194,304 rows, 256 fields, 67,108,864 logical values, and 512 MiB
of body bytes. Counts use checked `u64`/`usize` conversion and checked products;
raw, fixed Aura0, and Aura1 bodies must prove their declared dimensions from
actual bytes before row matrices are reserved. Generic streams additionally
cap aggregate declared values and use fallible reservations. These ceilings
apply to the public decoder and the materialized reference compiler; callers
cannot raise them.

The same admission gate applies to column readers, profiled/direct compilers,
typed execution paths, byte-lane expansion, metadata-only readers, file-backed
readers, batches, visitors, and replay. Attacker-controlled dictionary,
Huffman, RLE, bitplane, UUID, schema, and program table counts are capped and
checked against remaining bytes before fallible allocation. Lane-only wrapper
footers may have no local fields, but their declared rows remain bounded and
their AUBL descriptors must pass the existing lane limits; non-lane zero-width
row claims are rejected.

`--transcode-path direct`, `--aura1-body-path streaming-cursor`,
`--encoder-path direct-streams`, and `--encoder-path column-free` are
benchmarkable or diagnostic direct-path selectors. They are not all
unconditional defaults because wider fixture coverage and remaining
materialization decisions are still open.

`--aura1-body-path streaming-cursor` currently removes Aura0 stream-vector materialization for
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

`aura-fixture-gen` produces current-format fixture pairs plus `.aura1.zst`
baselines. Coverage includes tiny, dense/sparse, reordered/wide/edge-case SDK,
repeated-key, high-cardinality, nohuff, and larger inputs. `fixtures.json` is
the generated inventory; `tiny` (16-row OHLCV) and `sdk-tiny` (4-row SDK) are
distinct datasets. The generated `huff` entry is intentionally blocked rather than
silently mislabeled: the current public writer/planner did not select a
`HuffmanDictionary` stream for generated rows under the 2x Huffman speed gate.
Use the external `grimoire-50mb-huff` artifact for Huffman-heavy benchmarking
until a repo-native Huffman fixture generator or fixture blob is added.

## Aura0 profile reader compatibility

The workload-specific compact/fast/hybrid versus zstd results are retained in
[BENCHMARKING.md](BENCHMARKING.md#historical-benchmark-evidence). They do not
change the compact SDK default or establish a universal compression/speed win.
Choose profiles with the required semantic fallback and reader versions in mind.

Old readers may reject fast/hybrid files because their `AURP` footer has an
optional trailing `AUBL` extension. Current readers decode old compact files
without that extension through the semantic path. A fast-only file has no
semantic lane; requesting `--use-byte-lane never` cannot decode it.
