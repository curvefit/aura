# AURA Format

This document describes the current implementation.

Current production writers still emit container V2. The V3 relationship/group
front header and full schema encoding are implemented and testable as standalone
metadata, but complete V3 ingest/compiled footers and bodies are intentionally
unsupported until Aura Plan V2 and the bounded writer path land. A V3 schema
cannot be embedded in a V2 container.

## Standalone V3 exact-value reference block

Aura defines a versioned, uncompressed exact-value block as a reference for V3
value and null semantics. It is not a complete `.aura`, `.aura0`, or `.aura1`
file, has no footer, planner, codec, or compression, and does not enable V3 file
writing or conversion. Version 1 covers only flat event-scoped schemas (the
trade/OI-like subset). It rejects repeated fields and groups because its single
row count cannot represent event-to-child boundaries. Its magic is `AURAV3VB`;
every integer is little-endian.

The fixed 64-byte header is:

| Offset | Width | Meaning |
| ---: | ---: | --- |
| 0 | 8 | magic `AURAV3VB` |
| 8 | 2 | reference-block version, exactly `1` |
| 10 | 2 | block flags, exactly zero |
| 12 | 4 | V3 `schema_id` routing hint |
| 16 | 32 | canonical V3 schema SHA-256 fingerprint |
| 48 | 4 | row count |
| 52 | 4 | exact column count |
| 56 | 8 | total block byte length, including header |

The schema fingerprint is SHA-256 over
`aura-v3-schema-fingerprint-v1\0`, the canonical tag-4 descriptor length as
u64 LE, and the exact canonical descriptor bytes (including their outer u32
length). Descriptor complexity is checked without cloning owned schema data
before encoding, and the encoded descriptor is capped at the standalone
16 MiB schema envelope. The decoder verifies this fingerprint before any value
plane allocation. The 32-bit `schema_id` remains a routing hint and is not the
strong identity boundary.

Columns immediately follow in stable schema-slot order. Each has this 20-byte
header, followed immediately by its planes:

| Offset | Width | Meaning |
| ---: | ---: | --- |
| 0 | 2 | stable schema slot |
| 2 | 1 | exact `FieldType` code |
| 3 | 1 | flags: bit 0 validity, bit 1 variable-width; all others zero |
| 4 | 4 | validity byte length |
| 8 | 4 | fixed-value byte length |
| 12 | 4 | offset-plane byte length |
| 16 | 4 | variable-data byte length |

Plane order is validity, then fixed values, or validity, u32 offsets, and
variable data. Nullable columns carry exactly `ceil(row_count / 8)` validity
bytes; nonnullable columns carry none. Validity is LSB-first: row `r` uses bit
`r % 8` of byte `r / 8`, where one means present. Unused high bits in the final
byte are zero.

Fixed values use their exact signed/unsigned little-endian width. Null fixed
slots contain all-zero bytes, including `Opaque16`. Variable columns have
exactly `row_count + 1` u32 offsets: first zero, monotonic, and final equal to
the data length. A null repeats its preceding offset. Present empty text and
null therefore have the same adjacent offsets but different validity bits.
Every slice is independently valid UTF-8.

Decoding is canonical and fail-closed: magic, version, schema identity, slots,
types, flags, plane lengths, bitmap padding, placeholders, offsets, UTF-8,
decimal grammar, total length, and trailing bytes must all agree. Hard ceilings
are 1 GiB per block, 16 MiB of raw stored UTF-8 bytes per value, and 16,777,216
rows. `V3ValueLimits` may lower but never raise these ceilings.

The exact-value SHA-256 domain is
`aura-v3-canonical-exact-values-v1\0`. Framing includes the canonical encoded
schema fingerprint and routing ID, row count and column count, then supplied
row order. Within each row, fields occur in stable schema-slot order and
contribute slot, exact type tag, presence tag, and—only when present—the exact
fixed LE payload or a u32 length plus original variable bytes. Hashing never
sorts rows, narrows U64, uses Rust `Hash`, or includes physical null
placeholders.

## Roles

- `.aura`: ingest/preservation file. It stores logical i64 or typed rows plus
  the ingest footer (`AURF`).
- `.aura0`: compiled cold file. It stores stream/delta/codec bodies plus the
  compiled footer (`AURP`).
- `.aura1`: compiled fixed-width replay file. It stores fixed-width i64 rows
  plus the compiled footer (`AURP`).

All profiles use the same container shape:

```text
header
body
footer
u32 little-endian footer length
seal magic "sealed:)"
```

The trailer is discovered from EOF by checking the seal magic, reading the
footer length immediately before it, and locating the footer before that length.

## Header

V2 and V3 use explicitly dispatched header layouts. Both contain the profile,
stream and dictionary IDs, base timestamp, relationship map, derived expression
table, and optional comment. V3 additionally carries canonical group
descriptors and uses slot-level byte 200 for the actual dual-domain
discriminator. See `docs/container.md` for exact layouts.

The V3 front header is authoritative for relationship and group permissions.
The versioned full schema descriptor is authoritative for names, types, roles,
scales, and nullability. When complete V3 containers are enabled, header and
full-schema maps, expressions, groups, and schema dialect must agree exactly.
Relationship byte 100 uniquely marks the primary event timestamp at slot 0.
Additional event-scoped timestamp-role fields use byte 255 in the front map;
their nanosecond, millisecond, or scaled-i64 units and nullability remain in the
authoritative tag-4 schema. Byte 100 at any later slot is invalid. V2 mapping
semantics and bytes are unchanged.

## Metadata

SDK writers can store an `AuraMetadata` v1 block in the header comment using
the `AURAMETA1|` tag. This block can carry optional dataset/source/venue
strings, writer version, a static numeric `SymbolMap`, and safe custom
key/value pairs. Readers expose it through `AuraReader::metadata()` before
replay starts.

The v1 metadata contract is intentionally static. Aura order-book replay uses
numeric symbol or instrument IDs in the hot loop and does not resolve symbol
strings while replaying. Time-ranged symbology, corporate-action-aware symbol
history, and dataset registry semantics remain out of scope for this metadata
block.

## Footers

`.aura` uses the ingest footer magic `AURF`. The current order is:

```text
AURF
format version u16
compression kind u8
compression level u8
schema block
ingest statistics
compiled plan slots
chunk descriptors
```

`.aura0` and `.aura1` use the compiled footer magic `AURP`. The current order is:

```text
AURP
format version u16
compression kind u8
compression level u8
record count u64
Aura1 block capacity u16
schema block
Aura0 decode program
Aura1 decode program
optional generic Aura0 instruction plan
chunk descriptors
optional Aura1 byte-lane descriptor table (`AUBL`)
```

Unsupported versions reject during footer decode. Complete V3 footer decode is
not enabled merely because V3 front-header metadata can be decoded.

## Aura1 Body

Aura1 rows are fixed-width i64 records. Field order is schema/index order.
Each field uses the width stamped by the Aura1 decode program. Values are
little-endian. There is no per-record allocation in the Aura1 visitor path.

## End-to-End Source Model

Aura's DBN-like hot path is the Aura1 body plus its schema-derived
`CompiledAuraPlan`, not DBN records or DBN metadata. On disk, the Aura1 body is
wrapped by the common Aura header/footer container. In memory, the SDK can
borrow fixed-width Aura1 body ranges as `Aura1FixedBatchView`. For transport or
live use, `AuraLiveSource<R>` consumes the same aligned Aura1 body records when
the caller supplies the schema/compiled plan out of band.

`AuraEventSource` is the common event-loop interface for this model:

- `AuraMemorySource` replays sealed Aura1 bytes from memory.
- `AuraFileSource` replays sealed Aura1 files with bounded range reads.
- `AuraLiveSource<R>` replays an incoming stream of fixed-width Aura1 body
  records.

This intentionally differs from DBN's single binary format. Aura keeps `.aura`
as dumb ingest/preservation, `.aura0` as compact semantic cold storage, and
`.aura1` as the fixed-width replay/transport candidate. Aura defines only
static metadata/symbology in v1; time-aware DBN-style symbology mappings remain
a future format extension.

## Aura0 Body

Aura0 uses the generic instruction plan when present. The body contains stream
payloads referenced by that plan. Current stream operations include fixed-step,
delta, varint, bitpack, RLE, dictionary, packed dictionary, block-local, and
Huffman dictionary variants. The current direct paths preserve the footer plan
and do not change binary layout.

Aura0 can now be written with three profiles:

- `compact`: semantic stream lane only. This is the smallest current profile
  and keeps the full semantic decode path.
- `fast`: Aura1 byte lane only. The body contains compressed Aura1-compatible
  bytes and the compiled footer carries an `AUBL` descriptor table.
- `hybrid`: semantic stream lane followed by an Aura1 byte lane. Readers can
  use the byte lane for byte-output expansion or force the semantic lane for
  verification/fallback.

The first implemented byte lane is single-block and stores full Aura1 file
bytes. Descriptor fields include lane version, codec id/level, row range,
Aura1 output offset, uncompressed length, compressed body offset/length,
checksum kind, checksum, and flags. Implemented codecs are raw, lz4, zstd1,
zstd3, and zstd9.

## Compiled Plan

`CompiledAuraPlan` is a runtime execution plan built from `CompiledFooter`.
It validates program field coverage and precomputes:

```text
record count
field count
Aura0 physical plan
Aura1 physical plan
generic Aura0 plan
Aura1 record width
Aura1 body size
canonical field order
conversion_plan_hash
```

Profiled direct `.aura0 -> .aura1`, profiled direct `.aura1 -> .aura0`, and the
Aura1 fixed replay benchmark use this plan. Some fallback/materialized paths
still use older helper flows.

## Hashes And Guards

The output-byte guard is a byte-level FNV-style guard over encoded output bytes.
Strict guard modes keep this as verification/integrity work separate from
canonical logical hashing.

Canonical hash mode is benchmark-selectable:

```text
--canonical-hash-mode none|verify
```

The current canonical hash is an i64 logical-row hash with stable row delimiters,
schema/index field order, and little-endian value encoding. It is intended to
compare equivalent logical i64 streams across `.aura0`, `.aura1`, and transcode
outputs without materializing CanonicalRecord objects.

## Determinism

Compiled output is deterministic for the current tested i64 paths when the same
footer program, encoder path, and guard mode are selected. Direct and
materialized `.aura1 -> .aura0` outputs are tested for byte equality on fixtures.

## Current Default Recommendations

These defaults are benchmark recommendations for the current implementation,
not a claim that the format has reached its final speed limit.

- `.aura0 -> .aura1`: use the compiled/profiled materialized decode path
  (`--transcode-path auto --decode-path materialized`) for production bytes.
  The profiled path now uses the shared compiled plan for the specialized
  partitioned-sparse writer and for the generic direct writer fallback. The
  fallback covers current no-Huffman/`PartitionRuns` artifacts instead of
  falling back to the unprofiled helper.
  The cursor decode path removes stream-vector materialization on supported
  plans, but fresh grimoire huff/nohuff runs were slower, so it remains
  experimental.
- `.aura1 -> .aura0`: use the direct fixed-row scanner with the materialized
  encoder (`--transcode-path direct --encoder-path materialized`) as the current
  default candidate. It avoids `Vec<Vec<i64>>` row materialization, uses
  `CompiledAuraPlan`, and was faster than the full materialized fallback. The
  direct-streams encoder remains behind a flag because fresh huff/nohuff
  results were mixed.
- `.aura1` replay: use `aura1-scan-fixed` or the fixed replay visitor.
  `aura1-parse-to-rows` is a materialization benchmark, not the replay default.
- Guard mode: use `no_guard` for production timing. Use strict guard modes only
  for verification/integrity runs and compare them only with other strict runs.
- Canonical hash: keep `--canonical-hash-mode none` by default and enable
  `verify` only for correctness checks.

The fair product benchmark shows compact semantic `.aura0 -> .aura1` bytes
losing to `.aura1.zst -> .aura1` bytes on the grimoire huff/nohuff artifacts:

```text
grimoire-50mb-huff:   compact Aura0 80.608 ms, zstd L3 61.386 ms
grimoire-50mb-nohuff: compact Aura0 104.173 ms, zstd L3 62.798 ms
```

The current Aura0 stream layout is compact but requires stream decode, semantic
field reconstruction, partition/presence handling, and fixed-row Aura1 writes.
Whole-file zstd inflates already-formed Aura1 bytes. Until the product
benchmark is reversed, AURA0 should be documented as a compact cold format, not
as faster than zstd for the cold decode-to-Aura1 byte path.

The speed target is reversed by real Aura0 fast/hybrid byte-lane files expanded
through the production Aura0 reader path into the same `memory_vec` sink as the
zstd baseline.

```text
grimoire-50mb-huff:   fast raw 24.753 ms, fast lz4 44.373 ms, hybrid lz4 43.761 ms
grimoire-50mb-nohuff: fast raw 25.271 ms, fast lz4 43.281 ms, hybrid lz4 44.058 ms
```

Raw is a copy-only speed limit and stores the full Aura1 byte payload. The lz4
lane is the current compressed speed-profile candidate: it beats zstd L3 on the
tested datasets but is larger than `.aura1.zst` and much larger than compact
semantic Aura0. The default recommendation is hybrid + lz4 for users who want
both compact semantic interchange and faster-than-zstd byte expansion; compact
remains the smallest archival profile.
