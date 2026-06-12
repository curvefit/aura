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
body:

```text
schema descriptor
ingest stats
compression descriptor
Aura0 physical plan
Aura1 physical plan
chunk table
```

The schema descriptor is the self-describing archive copy of the raw logical
schema. A reader should use the header `schema_id` for fast-path parser lookup
when its registry is available, then use the footer schema as the durable source
of truth for unknown schemas, validation, and conversion.

Aura0 field plans are footer-visible. A field plan records:

```text
field_index
encoding
width
reference_field_index
base_value
step
estimated_bytes
```

That is enough to represent base deltas for stable values, related-field deltas
for schemas such as OHLCV, and implicit fixed-step timestamps where the per-row
timestamp is reconstructed from `base_value + row_index * step`.

The footer is what makes conversion deterministic. A converter can read the
header, jump to the footer, select the compiled physical plan, and then process
chunks without re-reading source payloads to discover ranges or group shapes.
