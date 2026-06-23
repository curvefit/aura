# AURA Format

This document describes the current implementation.

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

The header contains the profile, stream and dictionary IDs, base timestamp,
schema parent mapping, derived expression table, and optional comment. The
compiled profiles preserve the schema mapping and derived expression table from
the logical schema.

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
```

Unsupported versions reject during footer decode.

## Aura1 Body

Aura1 rows are fixed-width i64 records. Field order is schema/index order.
Each field uses the width stamped by the Aura1 decode program. Values are
little-endian. There is no per-record allocation in the Aura1 visitor path.

## Aura0 Body

Aura0 uses the generic instruction plan when present. The body contains stream
payloads referenced by that plan. Current stream operations include fixed-step,
delta, varint, bitpack, RLE, dictionary, packed dictionary, block-local, and
Huffman dictionary variants. The current direct paths preserve the footer plan
and do not change binary layout.

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
