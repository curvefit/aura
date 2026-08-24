# Aura container

Aura files use one container shape across all public levels:

```text
Header
Body
Footer
FooterLen
Seal
```

The established SDK writer and generic reader use container V2 by default.
Container V3 is explicitly dispatched and has two complete uncompressed Aura0
SDK profiles: flat event-scoped exact-value blocks and grouped exact-event
chunks. Both are exposed by seekable writer/reader APIs. The `aura v3 aura0
seal` and `aura v3 aura0 verify` developer commands cover the flat profile only;
there is no grouped CLI complete-seal command yet. Neither V3 profile is
selected implicitly by the V2 SDK writer or promised as production/default.

## V2 header

The V2 front header starts at byte zero. Its fixed prefix is 25 bytes; `header_len`
is the total front-header size and is the byte offset where the body starts.

```text
offset  size  field
0       4     magic          AURA
4       2     version        format version
6       1     profile        ingest | Aura0 | Aura1
7       2     header_len     total header bytes before body
9       8     start_time_ns  file-local time anchor
17      2     stream_id      external stream dictionary key
19      2     dictionary_id  external dictionary/version key
21      1     schema_len     compact schema-map byte count
22      1     comment_len    UTF-8 comment byte count
23      2     derived_len    derived-expression table byte count
25      N     schema_map     time marker and parent bytes
25+N    D     derived_exprs  optional generic expression table
25+N+D  M     comment_utf8   optional human-readable field labels
```

`header_len = 25 + schema_len + derived_len + comment_len`. `derived_len = 0`
means no derived-expression table. `comment_len = 0` means the file has no
front-header comment. `header_len` and `derived_len` are little-endian `u16`
fields so derived-expression metadata can exceed 255 bytes.

The header is write-once. When the file is sealed, the writer appends the
footer, writes the footer length, and writes the trailing seal magic. No header
field needs to be patched.

The magic identifies the Aura container family. The version is read immediately
after magic so future versions can define a different header layout before a
reader interprets profile-specific fields. The version field starts at byte `4`
after the four-byte magic and is the escape hatch for future schema-header
dialects. The `profile` byte identifies which public file level the body and
footer use.

The front header intentionally stores compact stream IDs rather than strings or
full schemas. For market data, `stream_id` can resolve through the external
dictionary to the venue, market type, exchange symbol, base, quote, contract
type, tick size, and quantity step. The current Rust implementation emits this
compact schema-map dialect:

```text
0        event/root slot with no parent
1-99     parent slot; parent index = byte - 1
100      timestamp slot; expected at physical slot 0 when present
101-199  derived expression ref; expression id = byte - 100
200      dual-domain repeated group marker
201-239  repeated group width; width = byte - 200 slots
241      1-bit boolean leaf
242      2-bit enum leaf, up to 4 outcomes
243      bitfield leaf, up to 8 flags
255      opaque / do-not-attempt stream
```

Derived expression definitions belong to the schema header and declare generic
same-row calculations such as add/sub/mul/div/min/max residual forms. The table
is byte-aligned:

```text
expr_count u8

entry:
  expression_id u8    1..99, referenced by schema byte 100 + id
  op            u8    add | sub | mul | div | min | max |
                       add_residual | subtract_residual |
                       max_plus_residual | min_minus_residual |
                       first_offset_then_delta
  output_slot   u16
  flags         u8    bit 0 = internally derived output
  input_count   u8
  input_slots   input_count * u16
  literal_count u8
  literals      literal_count * i64
```

The current planner consumes both expression families. Arithmetic definitions
(`add`, `sub`, `mul`, `div`, `min`, `max`) stamp an expression residual footer
instruction carrying the op, input slots, literals, and residual stream. Shape
definitions (`add_residual`, `subtract_residual`, `max_plus_residual`,
`min_minus_residual`, `first_offset_then_delta`) stamp the smaller dedicated
derived-stream footer instruction.

Constants, residual streams, and physical coding choices remain in the
footer/body. Decimal scale metadata is stamped in the footer schema field table,
not the front header.
`comment_utf8` is optional human-facing text, such as CSV-style field labels.
The stamped footer schema remains the authoritative schema copy.

Files without a `100` timestamp marker are treated as non-time-series data.
Group-width bytes mark the current slot and the following `width - 1` slots as
repeated fields.

## V3 relationship/group header

The implemented V3 front-header prefix is 39 bytes:

```text
offset  size  field
0       4     magic          AURA
4       2     version        3
6       1     profile
7       4     header_len     u32 little-endian
11      8     start_time_ns
19      2     stream_id
21      2     dictionary_id
23      4     schema_len     one byte per logical field
27      4     derived_len
31      4     group_len
35      4     comment_len
39      N     schema_map
39+N    D     derived_exprs
39+N+D  G     group_descriptors
39+N+D+G M    comment_utf8
```

The complete V3 header is limited to 16 MiB. Length discovery enforces that
ceiling before a file-backed reader allocates the advertised header. Encoding
and decoding use checked section sums and fallible allocation.

V3 relationship-map bytes are one-to-one with logical fields:

```text
0        root/no direct parent
1-99     parent slot, byte - 1
100      timestamp
101-199  derived-expression reference
200      this field is the dual-domain discriminator
201-239  invalid in V3; V2 structural widths are not reused
241      boolean
242      small enum
243      bitfield
255      opaque/do-not-attempt arithmetic
```

The group table is versioned. Descriptors are serialized canonically by group
ID and carry kind, semantic relationship permissions, dual-domain metadata,
and strictly increasing child slots. The first table version supports repeated
column subsets of one shared child row and exactly two domains. Unknown table
versions, kinds, flags, permissions, overlapping children, mismatched byte 200,
and expression cycles reject.

The V3 header can carry a derived-expression section for dialect recognition,
but both complete V3 Aura0 profiles require it to be empty; derived expressions
are not executed by V3.

The front V3 header authorizes relationships and groups. The full schema
encoding tag 4 is authoritative for field names, exact types, roles, scales,
nullability, and schema identity. For both complete V3 Aura0 profiles, the
validated schema is embedded in the footer. The flat writer rejects groups,
repeated fields, derived expressions, and byte `200`. The grouped writer
accepts only one exact repeated dual-domain group: all repeated child slots,
the non-null U8 `side` discriminator marked by byte `200`, and no derived
expressions. Group/Flag200 execution is exact logical event/child execution;
it does not choose a physical relationship transform, compression, or Plan v2.

The V2 SDK writer remains the production compatibility path. It rejects V3
schemas and emits V2 containers; callers that need V3 must select the explicit
V3 API or CLI.

## Body

The body is profile-specific:

```text
.aura   normalized generous ingest records
.aura0  V2 compact compiled records, or V3 flat exact-value/grouped exact-event blocks
.aura1  V2 replay compiled blocks
```

V2 body/schema and layout decisions are file-level facts recorded in the V2
footer. A V3 flat body is a concatenation of positive-row `AURAV3VB` version-1
exact-value blocks; each block carries its own schema fingerprint and exact
column/value planes. A grouped V3 body is a concatenation of positive-event
`AURAV3EB` version-1 chunks. Grouped chunks carry authoritative child offsets,
event/child counts, and exact scoped column planes. Both V3 bodies are
uncompressed and do not perform physical relationship planning.

The V2 SDK writer body path is lossless for its declared i64-compatible rows.
Its typed boundary validates `i128` and `opaque16` but rejects them before V2
sealing until the V2 body codecs can preserve those fields losslessly. The
separate V3 exact-value block supports the V3 field types listed in
[`SCHEMA.md`](SCHEMA.md), including exact `i128` and `opaque16` values, subject
to the flat-profile limits. Grouped `AURAV3EB` chunks use the same exact value
and null semantics for event and repeated child columns, subject to grouped
event/child limits.

## Footer

The footer stores the facts readers and converters need before replaying the
body. Ingest and compiled V2 files intentionally use different footer payloads.

An `.aura` ingest footer keeps the calculation evidence used while sealing:

```text
schema block
ingest stats
compression descriptor
Aura0 physical plan
Aura1 physical plan
generic Aura0 instruction plan
chunk table
```

A V2 compiled `.aura0` or `.aura1` footer stores both compiled profile
programs and replay metadata, not the ingest stats:

```text
magic AURP
version
record_count
block_capacity
schema block
aura0 decode program
aura1 decode program
generic Aura0 instruction plan
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
bits 0..4    op: 5-bit ProgramOp code; see docs/field-programs.md
bits 5..7    stored value width: zero | i8 | i16 | i32 | i64 | i128
bits 8..10   constant width: zero | i8 | i16 | i32 | i64 | i128
bits 11..13  aux: inline related field index, or 7 for extended aux
bit 14       has base constant
bit 15       has step constant
```

Optional extras follow only when the code asks for them: an extended `u16`
reference field, a base constant, a step constant, and one bit-width byte for
bitpacked streams. That keeps common fields to two bytes of instruction data
while still representing base deltas, previous-value deltas, related-field
deltas, implicit fixed-step timestamps, constant offsets, header-declared
max/min residuals, and product/proportional residuals.

Aura0 bodies are columnar by decode-program order. Aura1 bodies are row-major
fixed-width replay data. Both use the same compiled footer bytes; converting
between compiled profiles changes the body and front profile byte, then copies
the footer unchanged.

The footer is what makes conversion deterministic. A converter can read the
trailer to locate the footer, run the field program, and then process chunks
without re-reading source payloads to discover ranges or group shapes.

### V3 flat Aura0 footer

The V3 flat footer also uses `AURP`, but its version is `3` and its layout
version is `1`. It records the exact-block body encoding, record and body
lengths, column/chunk counts, schema ID and fingerprint, primary timestamp and
sequence slots, header/body/global logical SHA-256 values, a per-column stats
table, a per-chunk descriptor table, and a footer self-hash. The embedded tag-4
schema is length-prefixed and is checked against the schema ID, fingerprint,
header map, and every exact-value block. Each chunk descriptor records its row
range, body offset/length, stored-byte hash, logical-byte hash, and timestamp or
sequence bounds when present.

The V3 writer emits body blocks first, then performs a second pass over the
body to recompute hashes, statistics, and exact logical values before it writes
the footer, footer length, and final seal. A reader opens the envelope and
footer before trusting body-dependent claims; `verify_all` rechecks every block
and global hash. The flat CLI publishes only a newly created, synced,
atomically renamed destination and never replaces an existing path.

### V3 grouped Aura0 footer

The V3 grouped footer also uses `AURP` version `3`, layout version `1`, and
uncompressed body encoding `2`. Its fixed prefix is 184 bytes. It records event
count, child count, body length, schema identity/fingerprint, primary timestamp
and sequence slots, header/body/global logical SHA-256 values, then a stats
table with 36-byte descriptors, a chunk table with 152-byte descriptors, and a
footer self-hash. Each chunk descriptor records contiguous global event and
child ranges, body offset/length, stored-byte hash, logical-event hash, and
timestamp/sequence bounds when present.

The grouped SDK writer streams `AURAV3EB` chunks and performs a bounded second
pass before appending the grouped footer, u32 footer length, and final seal. The
seekable grouped reader can locate chunks by global event or child, read an
individual checked chunk, and run full verification with a second envelope/body
check. It has no grouped CLI seal or production/default status. A failed write,
flush, sync, or verification is a failed/uncommitted result that callers must
discard and must not publish, even if bytes happen to end in a seal.

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

```text
seal_offset       = file_len - 8
footer_len_offset = seal_offset - 4
footer_start      = footer_len_offset - footer_len
body_start        = header_len
body_end          = footer_start
```

An incomplete or partially written file before its complete seal is rejected
as incomplete. A truncated footer, invalid footer length, mismatched header or
body hash, corrupted block, malformed value plane, unsupported version, or
trailing bytes is rejected as an invalid file. Readers use checked lengths and
bounded allocations. Flat and grouped profiles have separate hard/default
ceilings; see [FORMAT.md](FORMAT.md) for the complete tables and
[COMPATIBILITY.md](COMPATIBILITY.md) for the promised compatibility boundary.
SDK writer/flush/sync errors are failed, uncommitted results rather than a
recovery mechanism; callers must discard them even if a seal was written.
