# Aura container

Aura files use one container shape across all public levels:

```text
Header
Body
Footer
FooterLen
Seal
```

## Header

The header is fixed-width and appears at byte zero.

```text
magic           AURA
profile         ingest | Aura0 | Aura1
header_len      fixed header size
version         format version
reserved        zero
reserved        zero
base_time_ns    file-local time anchor
stream_id       external stream dictionary key
dictionary_id   external dictionary/version key
schema_hash     embedded schema guardrail, zero if unavailable
footer_offset   zero while open, patched when sealed
reserved        zero
reserved
```

Open writers use zero footer pointers. When the file is sealed, the writer
appends the footer, patches the header pointer, writes the footer length, and
writes the trailing seal magic.

The magic identifies the Aura container family. The `profile` byte identifies
which public file level the body and footer use.

The fixed header intentionally stores compact stream IDs rather than strings or
variable schemas. For market data, `stream_id` can resolve through the external
dictionary to the venue, market type, exchange symbol, base, quote, contract
type, tick size, and quantity step. Schema identity lives in the footer schema
block; `schema_hash` is only a guardrail for checking that embedded schema copy.

## Body

The body is profile-specific:

```text
.aura   normalized generous ingest records
.aura0  compact compiled records
.aura1  replay compiled blocks
```

The body should not need per-record schema mutation. Schema and layout decisions
are file-level facts recorded in the footer.

## Footer

The footer stores the facts readers and converters need before replaying the
body. Ingest and compiled files intentionally use different footer payloads.

An `.aura` ingest footer keeps the calculation evidence used while sealing:

```text
schema block
ingest stats
compression descriptor
Aura0 physical plan
Aura1 physical plan
chunk table
```

A compiled `.aura0` or `.aura1` footer stores only the selected decode program
and replay metadata, not the ingest stats:

```text
magic AURP
version
compression descriptor
profile
record_count
block_capacity
schema block
decode program
chunk table
```

The schema block is a length-prefixed, self-describing archive copy of the
logical field layout:

```text
schema_len       u32 little-endian
schema_encoding  schema_len bytes
```

The schema encoding does not store a schema ID or schema name. A reader uses the
footer schema block as the durable source of truth for unknown schemas,
validation, and conversion.

The compiled decode program is a field-index ordered list of small instructions.
Each field starts with a `u16` code:

```text
bits 0..4    op: absolute | delta_base | delta_previous | delta_related | fixed_step
bits 5..7    stored value width: zero | i8 | i16 | i32 | i64
bits 8..10   constant width: zero | i8 | i16 | i32 | i64
bits 11..13  aux: inline related field index, or 7 for extended aux
bit 14       has base constant
bit 15       has step constant
```

Optional extras follow only when the code asks for them: an extended `u16`
reference field, a base constant, and a step constant. That keeps common fields
to two bytes of instruction data while still representing base deltas,
previous-value deltas, related-field deltas, and implicit fixed-step timestamps
where the per-row timestamp is reconstructed from `base_value + row_index *
step`.

The footer is what makes conversion deterministic. A converter can read the
header, jump to the footer, run the field program, and then process chunks
without re-reading source payloads to discover ranges or group shapes.

## Footer length and seal

The last twelve bytes of a complete file store the footer length followed by the
seal:

```text
footer_len  u32 little-endian
sealed:)
```

The seal is the final eight bytes. `footer_len` is stored immediately before the
seal and is not part of the footer itself. A reader can reject a file whose last
eight bytes do not match the seal magic, then use the preceding length to find
the footer.
