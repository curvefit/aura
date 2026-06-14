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

The front header starts at byte zero. Its fixed prefix is 22 bytes; `header_len`
is the total front-header size and is the byte offset where the body starts.

```text
offset  size  field
0       4     magic          AURA
4       2     version        format version
6       1     profile        ingest | Aura0 | Aura1
7       1     header_len     total header bytes before body
8       8     start_time_ns  file-local time anchor
16      2     stream_id      external stream dictionary key
18      2     dictionary_id  external dictionary/version key
20      1     schema_len     time/parent mapping byte count
21      1     comment_len    UTF-8 comment byte count
22      N     schema_map     time marker and parent bytes
22+N    M     comment_utf8   optional human-readable field labels
```

`header_len = 22 + schema_len + comment_len`. `comment_len = 0` means the file
has no front-header comment.

The header is write-once. When the file is sealed, the writer appends the
footer, writes the footer length, and writes the trailing seal magic. No header
field needs to be patched.

The magic identifies the Aura container family. The version is read immediately
after magic so future versions can define a different header layout before a
reader interprets profile-specific fields. The `profile` byte identifies which
public file level the body and footer use.

The front header intentionally stores compact stream IDs rather than strings or
full schemas. For market data, `stream_id` can resolve through the external
dictionary to the venue, market type, exchange symbol, base, quote, contract
type, tick size, and quantity step. `schema_map` is a small time/parent mapping
so the file shape is visible at the front: `255` marks the primary timestamp
slot, `0` means no parent, and `1..254` means parent slot `value - 1`.
`comment_utf8` is optional human-facing text, such as CSV-style field labels.
The stamped footer schema remains the authoritative schema copy.

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
deltas, implicit fixed-step timestamps, constant offsets, candle wick residuals,
and product/proportional residuals.

Aura0 bodies are columnar by decode-program order. Aura1 bodies are row-major
fixed-width replay data. Converting Aura0 to Aura1 decodes logical rows from the
Aura0 field program, then emits a fresh Aura1 fixed-width program from those
rows.

The footer is what makes conversion deterministic. A converter can read the
trailer to locate the footer, run the field program, and then process chunks
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

```text
seal_offset       = file_len - 8
footer_len_offset = seal_offset - 4
footer_start      = footer_len_offset - footer_len
body_start        = header_len
body_end          = footer_start
```
