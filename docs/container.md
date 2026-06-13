# Aura container

Aura files use one container shape across all public levels:

```text
Header
Body
Footer
```

## Header

The header is fixed-width and appears at byte zero.

```text
magic           AURA | AUR0 | AUR1
version         format version
profile         ingest | Aura0 | Aura1
header_len      fixed header size
schema_id       logical schema registry key
flags           sealed/open state
base_time_ns    file-local time anchor
stream_id       external stream dictionary key
dictionary_id   external dictionary/version key
schema_hash     resolved schema guardrail, zero if unavailable
footer_offset   zero while open, patched when sealed
footer_len      zero while open, patched when sealed
reserved
```

Open writers use zero footer pointers. When the file is sealed, the writer
appends the footer and patches the header pointer.

The fixed header intentionally stores compact registry IDs rather than strings
or variable schemas. For market data, `stream_id` can resolve through the
external dictionary to the venue, market type, exchange symbol, base, quote,
contract type, tick size, and quantity step. `schema_id` is the fast parser
lookup. `schema_hash` lets a reader reject a stale registry entry if the schema
ID resolves to different fields than the file was written with.

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
schema descriptor
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
schema descriptor
decode program
chunk table
```

The schema descriptor is the self-describing archive copy of the raw logical
schema. A reader should use the header `schema_id` for fast-path parser lookup
when its registry is available, then use the footer schema as the durable source
of truth for unknown schemas, validation, and conversion.

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
