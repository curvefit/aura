# AURA Format

This document describes the current implementation.

Current production and default SDK writers still emit container V2. Aura also
implements three complete, explicitly selected V3 Aura0 flavors: uncompressed
flat event-scoped files whose body is a concatenation of exact-value
`AURAV3VB` blocks; a development-only planned-flat file with plan-bound fixed,
absolute-varint, exact-byte variable-dictionary lanes, the existing per-chunk
zstd19 wrapper, and a distinct schema-authorized temporal wrapper candidate;
and uncompressed grouped exact-event files whose body is a
concatenation of `AURAV3EB` chunks.
Exact-flat and grouped have seekable
writers/readers. Planned-flat compilation and verification are bounded
all-memory reference paths. The explicit V3 CLI seal command covers all three
and verify auto-dispatches from the held footer tuple. V3 is not
production-ready or the default, and a V3 schema cannot be embedded in a V2
container.

## Standalone V3 exact-value reference block

Aura defines a versioned, uncompressed exact-value block as the V3 value and
null-semantics primitive. The block itself is not a complete `.aura`, `.aura0`,
or `.aura1` file: it has no footer, planner, codec, or compression. The
complete V3 flat Aura0 writer wraps one or more of these blocks with the V3
header, footer, hashes, chunk table, and seal. Version 1 covers only flat
event-scoped schemas (the trade/OI-like subset). It rejects repeated fields and
groups because its single row count cannot represent event-to-child boundaries.
Its magic is `AURAV3VB`; every integer is little-endian.

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

`CanonicalV3RowHasher` is the incremental form of this same contract. The
caller commits the total row count at construction and supplies schema-bound
batches in file order. This permits a complete-file decoder to verify the
global logical hash chunk by chunk without retaining a second concatenated
batch. The one-batch `canonical_v3_batch_sha256` output is unchanged.

## Flat Aura0 V3 container V1

The complete V3 subset has a V3 Aura0 header, a body containing contiguous
positive-row `AURAV3VB` version-1 blocks, an `AURP` V3 flat footer, the common
u32 footer length, and `sealed:)`. Compression kind, level, and flags are zero.
There is no padding between blocks. A zero-row file has an empty body, no
chunks, and one zero-valued stats descriptor per schema field.

This V1 layout accepts only flat event fields. It rejects groups, repeated
scope, derived fields/map bytes 101–239, byte 200, non-Aura0 profiles, and
nonzero base time, stream ID, or dictionary ID. Header and footer schema maps
must agree. If present, byte 100 declares the primary timestamp at slot 0;
schemas with no primary timestamp store `ffff` in the footer and omit all
timestamp chunk bounds. Byte 255 remains valid for auxiliary timestamps. At
most one U64 field with role `sequence` is the primary sequence slot.

The hard supported-subset ceilings are 64 MiB footer bytes, 1 TiB body bytes,
65,536 chunks, 16,777,216 total rows, and 16 MiB for the encoded schema
descriptor. The seekable writer and reader check all lengths and counts before
count-controlled allocation. Their safe defaults are 256 MiB for the body and
each exact-value block, 4,194,304 total rows, and 4,096 chunks;
`V3FlatLimits::HARD` explicitly opts into the absolute 1 TiB body, 1 GiB block,
and 16,777,216-row format ceilings.

## Planned-flat Aura0 V3 development container

Planned-flat is an additive group-free development format for the same logical
flat schema subset. Its `AURP` route tuple is container version `3`, footer
layout `3`, body encoding `4`, followed by three zero reserved bytes. The
footer stamps body/block version `1` for registry 1, version `2` for registry
2, outer version `3` for wrapper v1, outer version `5` for the registry-3
temporal wrapper v2, or outer version `7` for the registry-4 prefix/suffix
wrapper v3, plus the complete canonical schema descriptor and an `AUF2` plan.
A decoder never replans and needs no dataset, venue, symbol, filename, or
sidecar.

Canonical `AUF2` plan version `2`, registry version `1`, starts with magic
`AUF2`, total length, schema ID and fingerprint, field count, and reserved
zero bytes. One physical codec byte follows for every schema slot, then a
domain-separated 32-byte plan hash. Registry 1 permits fixed-width lanes for
all supported flat fields, unsigned canonical ULEB128 for unsigned integer
fields, and signed canonical ZigZag ULEB128 for signed integers and timestamp
fields. The plan decoder verifies its length, hash, schema binding, codec/type
compatibility, and canonical re-encoding.

Registry version `2` has a distinct plan-hash domain and adds physical codec
code `3` for exact-byte dictionary plus minimal bitpacked indices. It is legal
only for Utf8 and DecimalText and requires at least one such lane; the other
codec bytes retain registry-1 meaning. One registry covers both types because
the schema binds the logical type and the codec preserves already-validated
bytes without DecimalText normalization.

Registry version `3` has another distinct plan-hash domain and inherits the
registry-2 codecs. Physical code `4` means previous-delta ZigZag ULEB128 and
code `5` means delta-of-delta ZigZag ULEB128. These codes are legal only for
slot 0 when it is the event-scope, non-null TimestampNs/TimestampMs primary
field whose compact schema map byte is `100`. Absolute and the chosen temporal
transform must both be present in the field's schema candidates. The standard
timestamp contract authorizes `DeltaPrevious`; it does not authorize `Delta2`
unless that candidate is explicitly added. Each chunk starts with one absolute
signed value. Previous-delta stores each following checked difference;
delta-of-delta stores the first difference then checked differences of
differences. Forward and inverse arithmetic use checked i128 intermediates and
must fit i64 exactly. Auxiliary timestamp byte `255`, nullable timestamps, and
arbitrary integer fields cannot use either codec.

Registry version `4` has its own plan-hash domain, inherits legal registry-3
codecs, and adds physical code `6` only for Utf8 and DecimalText. At least one
code-6 lane is required. Every chunk stores the schema-required validity bitmap
followed by a fixed 16-byte lane header: lane version 1, zero reserved flags,
row count, present count, and record-stream byte length. Each present record is
canonical ULEB128 `prefix_len`, `suffix_len`, and `middle_len`, then exactly the
literal middle. Prefix and suffix are maximal bytewise matches against the
previous present value and cannot overlap. Previous resets to empty per chunk;
a null emits no record and does not update it, while a present empty value emits
zero lengths and updates it to empty. Decode checks bounds and canonical
re-encoding before accepting exact Utf8 or DecimalText bytes.

Each planned body chunk starts with `AUFPVB01`, block version `1`, zero flags,
schema ID/fingerprint, row and field counts, zero reserved bytes, and the full
block length. Lanes then occur in schema-slot order. Fixed lanes reuse the
exact-value plane payload without its 20-byte per-column header. Varint lanes
store the exact validity bitmap when nullable followed by one canonical value
per row. The footer's 208-byte prefix binds record/body totals, schema and
plan lengths, schema/plan/header/body/global-logical hashes, then stores the
schema and plan. Its version-1 chunk table uses 104-byte descriptors carrying
contiguous row/body ranges plus stored and logical SHA-256 values, followed by
the footer hash and common trailer.

Registry-2 chunks instead use `AUFPVB02`, body layout `2`, and block version
`2`. Non-dictionary lanes keep the registry-1 payload. Each dictionary lane
stores its normal validity bitmap followed by `u32 entry_count`, `u32 exact
dictionary-data bytes`, minimal index bit width, three reserved zero bytes,
`entry_count + 1` little-endian u32 offsets, lexicographically sorted unique
entry bytes, and LSB-first packed indices for present rows only. Null rows have
no index and reconstruct the required empty placeholder; a present empty value
is a real dictionary entry. Width and packed length are minimal, unused high
bits are zero, every entry is referenced, and offsets plus decoded output are
checked against caller bounds before allocation.

Body-layout-3 chunks store an independently decodable `AUFPZB01` wrapper around
one unchanged registry1/2 block. Its fixed 68-byte header stamps wrapper
version 1, zstd codec, level 19, window log 23, required content-size/checksum
flags, zero reserved bytes, inner body/block versions, uncompressed and
compressed lengths, and SHA-256 of the exact inner block. One canonical zstd
frame follows with standard magic, exact pledged/content size, checksum, no
dictionary ID, no long-distance matching, and zero workers. The decoder
requires one exact frame, enforces WindowLogMax 23 before bounded output,
checks length/SHA/inner plan decode, then recompresses with the pinned profile
and requires exact frame equality. Footer layout 3/body encoding 4 remain the
planned-flat family route; old readers fail closed on unknown body layout 3.

Registry-3 inner chunks use magic `AUFPVB03`, body layout `4`, and block
version `4`. They retain registry-2 lane framing and stamp every selected
physical codec in the registry-3 plan. The reference compiler emits them only
inside the new `AUFPZB02` wrapper-v2 candidate. That wrapper retains the
canonical 68-byte zstd19 profile but is stored as outer body layout `5` and
block version `5`, so no wrapper-v1 artifact or receipt is reinterpreted.

Registry-4 inner chunks use `AUFPVB04`, body layout/block version `6`, and are
never emitted as a raw complete candidate. They exist only inside the distinct
`AUFPZB03` wrapper version 3 stored as outer body layout/block version `7`.
Wrapper v3 retains the canonical bounded 68-byte zstd19 framing without
reinterpreting wrapper v1 or v2.

The checked-in reader compatibility fixture under
`tests/fixtures/v3-planned-v5/` freezes one registry-4/layout-7 winner and its
schema, plan, chunk, artifact, and logical hash identities. The fixture covers
two 256-row chunks with required timestamps and nullable all-present UTF-8
values. It freezes reader routing, bounds, canonical re-encoding, and logical
reconstruction only; it does not freeze planner candidate costs, tie rules,
future writer choice, or production/default behavior. Existing generated
planned-flat tests retain registry-1/2/3 route coverage. Fixture regeneration
is an ignored, environment-gated maintenance test.

The reference compiler scores seven complete artifacts: legacy exact flat,
planned all-fixed, planned per-field fixed/varint, and one registry-2 candidate
starting from the mixed plan and choosing a dictionary per eligible field only
on a strict aggregate actual-lane byte win across chunks, followed by one
zstd19 candidate, one registry-3 temporal-zstd19 candidate, and one registry-4
prefix/suffix-zstd19 candidate. The wrapper-v1
base is chosen before compression: registry2
when applicable, otherwise registry1 mixed; both bases are never compressed
and compared retrospectively. Registry 3 inherits that chosen base and changes
only authorized primary timestamp lanes that strictly reduce aggregate raw
lane bytes; its resulting inner plan is wrapped once. Registry 4 inherits that
preselected temporal plan when available, otherwise the dictionary/mixed plan,
and replaces only Utf8/DecimalText lanes that strictly reduce bounded aggregate
raw bytes. Raw and complete-file ties retain the earlier codec/candidate. It
charges header,
body, plan, schema, footer, descriptors, hashes, footer length, and seal, and
uses stable candidate order for complete-file ties. Therefore an explicit
planned request may truthfully publish the byte-identical legacy exact
fallback. The CLI receipt records requested mode, every candidate's
cost/applicability/rejection, selected candidate, actual route tuple, and
bounded per-slot baseline/dictionary/prefix-suffix/count/index attribution,
validity bytes, inherited complete-candidate identity, derived raw-plan
identity, inner, compressed, and overhead bytes. This reference compiler
retains up to seven complete candidates plus lane/dictionary/temporal/
prefix-suffix/compression scratch; it
is not the streaming ingest writer or a readiness claim. The additive
registries use no RLE, Huffman, provider identity, or inferred economic
semantics. Layouts 3, 5, and 7 are distinct wrapper versions; none is a
streaming/readiness or measured-size claim.

## Grouped Aura0 V3 container V1

The complete grouped V3 SDK profile has the same V3 header/trailer envelope as
the flat profile, but its body is a gapless sequence of positive-event
`AURAV3EB` version-1 chunks. The `AURP` V3 grouped footer uses body encoding
`2`, zero compression kind/level/flags, a 184-byte fixed prefix, 36-byte
per-field statistics descriptors, and 152-byte chunk descriptors. The file
ends with the common little-endian u32 footer length and `sealed:)` trailer.
There is no physical compression in this profile.

The grouped exact subset accepts a V3 tag-4 schema with exactly one repeated
group, whose child slots are exactly all repeated fields and whose dual-domain
descriptor has two domains. Its discriminator is the repeated, non-null `U8`
field with role `side` and compact relationship-map byte `200`; group/Flag200
execution is exact logical execution, not a physical transform. Event and
repeated columns may be nullable and preserve their validity bitmaps. Each
batch carries monotonic `event_count + 1` child offsets, and each chunk/footer
descriptor carries contiguous global event and child ranges. The canonical
logical hash commits event and child indices, boundaries, field slots, exact
types, presence, and values in source/schema order, so rechunking preserves
logical identity. Non-empty derived-expression tables are rejected.

`V3GroupedAura0Writer<W>` streams validated batches to a seekable output. Its
`finish` second pass reads every `AURAV3EB` chunk, rechecks decoding, ranges,
stored/logical hashes and statistics, computes the body/global hashes, verifies
the footer, then appends the footer, u32 length, and seal. `V3GroupedAura0Reader`
opens the bounded envelope, supports event/child chunk lookup and individual
chunk reads, and `verify_all` performs full body/hash/statistics verification
with a second envelope/body check before marking the reader verified. A write
that fails before the complete seal, or any writer/flush/sync or verification
failure, is a failed/uncommitted result that callers must discard and must not
publish, even if bytes happen to end in a seal. The grouped CLI accepts strict
nested Arrow protocol v2, including a zero-record-batch stream as an empty
complete file, and feeds the decoded exact event batch to this same writer. It
shares the flat complete-file publication state machine. This path does not
provide a physical relationship planner, compression, Plan v2, or Aura1.

The grouped hard ceilings are a 64 MiB footer, 1 TiB body, 65,536 chunks,
16,777,216 events, 67,108,864 children, and 16 MiB schema descriptor. Default
in-memory limits are 256 MiB body, 4,096 chunks, 1,048,576 events, and
4,194,304 children. `V3GroupedLimits::HARD` is an explicit opt-in; supplied
limits are clamped to the hard envelope. Grouped event-block limits are a 1 GiB
hard/256 MiB default block, 4,194,304 hard/1,048,576 default events,
16,777,216 hard/4,194,304 default children, and 67,108,864 hard/16,777,216
default values; variable values retain the 16 MiB hard ceiling.

## Roles

- `.aura`: ingest/preservation file. It stores logical i64 or typed rows plus
  the ingest footer (`AURF`).
- `.aura0`: V2 compiled cold file with stream/delta/codec bodies plus the
  compiled footer (`AURP`), or an explicit V3 exact-flat `AURAV3VB`,
  planned-flat `AUFPVB01`, or grouped `AURAV3EB` file with its V3 `AURP`
  footer (version 3).
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
stream and dictionary IDs, base timestamp, relationship map, derived-expression
section, and optional comment. V3 additionally carries canonical group
descriptors and uses slot-level byte 200 for the actual dual-domain
discriminator. The complete V3 flat and grouped profiles require an empty
derived-expression section; V3 does not execute derived expressions. See
`docs/container.md` for exact layouts.

The V3 front header is authoritative for relationship and group permissions.
The versioned full schema descriptor is authoritative for names, types, roles,
scales, and nullability. In the flat Aura0 V3 subset, header and full-schema
maps agree exactly and expressions/groups are empty because the current writer
rejects them.
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

V2 `.aura0` and `.aura1` use the compiled footer magic `AURP`. The current V2
order is:

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

Unsupported versions reject during footer decode. `AnyCompiledFooter` routes
the unchanged V2 compiled footer to `CompiledFooter`, V3 exact-flat
layout-1/encoding-1 bytes to the flat footer decoder, V3 grouped
layout-1/encoding-2 bytes to the grouped footer decoder, and V3 planned-flat
layout-3/encoding-4 bytes to the planned footer decoder. It does not
reinterpret one V3 layout as another.

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

V2 Aura0 uses the generic instruction plan when present. Its body contains
stream payloads referenced by that plan. Current V2 stream operations include
fixed-step, delta, varint, bitpack, RLE, dictionary, packed dictionary,
block-local, and Huffman dictionary variants. The current direct paths preserve
the V2 footer plan and do not change binary layout. The V3 exact profiles above
are uncompressed and do not use a physical relationship planner, compression,
or Plan v2. The additive planned-flat development profile uses the
self-contained `AUF2` physical-codec plan described above; it currently
performs no delta or related-domain math.

V2 Aura0 can be written with three profiles:

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
