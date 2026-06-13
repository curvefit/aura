# Schemas

Aura schemas describe logical records. They are separate from physical layout.

Every file repeats a compact time/parent mapping in the front header:

```text
schema_len    u8
comment_len   u8
schema_map    schema_len bytes
comment_utf8  comment_len bytes
```

Each byte is self-describing: `255` marks the primary timestamp slot, `0` means
no parent, and `1..254` means the parent is slot `value - 1`. This header
mapping is a quick file-shape preview and body-start metadata. It is not the
full schema. `comment_utf8` is optional human-readable text, usually CSV-style
labels; `comment_len = 0` means no comment.

```text
schema block
  schema_len       u32 little-endian
  schema_encoding  schema_len bytes
```

The footer schema block does not store a schema ID or schema name. The footer
schema encoding is the archive copy that keeps a file readable without the
external registry and is the authoritative stamped schema.

Schema encoding type `0` is the compact parent-vector form for positional
generic i64 records:

```text
descriptor_type  u8 = 0
slot_count       u8
parent_slots     slot_count bytes
```

Slot `0` is the timestamp slot and is marked `255`. Slots `1..N` are generic
`i64` values. This uses the same byte convention as the front header.

Schema encoding type `1` is the full field-descriptor fallback for richer
schemas:

```text
descriptor_type  u8 = 1
field_count      u16 little-endian

field descriptor
  index
  name
  field type
  semantic role
  nullable flag
  relationship
  transform candidates
```

Collectors should write normalized `.aura` ingest values using generous integer
types. The schema tells Aura what those values mean; ingest stats tell Aura how
small or fast the compiled representation can be.

Starter schema constructors exist for:

```text
book_delta_v1
tick_v1
ohlcv_v1
generic_i64_schema
generic_i64_parent_schema
```

`generic_i64_schema` is the dumb positional path. It treats field `0` as the
timestamp by convention, then creates `v1..vN` as generic `i64` values. The
schema does not need to know that `v1..v5` are open, high, low, close, and
volume. It only says which positional values are allowed to reference other
positional values.

```text
0 ts  timestamp
1 v1  value
2 v2  value delta_from_field 1
3 v3  value delta_from_field 1
4 v4  value delta_from_field 1
5 v5  value
```

The relationship metadata lets Aura0 test encodings such as `v2 - v1`, `v3 -
v1`, and `v4 - v1` instead of only testing previous-value deltas. Fields without
relationships can still test self transforms such as previous-value deltas,
base deltas, midpoint deltas, delta-of-delta statistics, varints, and bit widths
when their candidate flags allow those calculations. Timestamps can also be
proven implicit when every row advances by the same fixed step, such as
one-minute bars.

`generic_i64_parent_schema` is the compact parent-vector path for dynamic
OHLCV-like records. The vector includes the timestamp slot. Each byte is
self-describing: `255` means primary timestamp, `0` means no parent, and
`1..254` means the parent is slot `value - 1`.

```text
slots:
0 ts
1 open
2 high
3 low
4 close
5 volume
6 taker_buy
7 taker_sell

parents:
255 0 2 2 2 0 6 6
```

This says high, low, and close may delta from open; taker buy and taker sell may
delta from volume. The codec still chooses the actual physical encoding from
stats. The parent vector is only relationship evidence, not a command to force
a related delta.

The intended plug-in pattern is:

```text
define logical schema
write generous ingest records
collect stats while writing
seal footer
compile to .aura0 or .aura1
```

## .auraschema files

Ingestion-side schema relationship definitions use the `.auraschema` extension.
They are line-oriented, human-readable source files used before writing an Aura
file. They are not needed to handle or decode a sealed file because the emitted
file carries the compact schema map in its header and the stamped schema in its
footer. `.auraschema` files do not define footer compression instructions.

```text
// comments and blank lines are ignored
"comment_utf8 inserted into the Aura header"
schema_map bytes...
```

The file name identifies the reusable ingest format, such as
`binance_spot_kline_v1.auraschema`; that name does not go into the file body.
The first non-comment line inside the file must be quoted; its unquoted contents
become the header `comment_utf8`. Remaining non-comment content is parsed as
decimal `schema_map` bytes separated by spaces, commas, or newlines. `//` starts
a comment outside quoted text.

Example:

```text
// Binance spot kline array without the final ignore field
"open_time_ms,open,high,low,close,volume,close_time_ms,quote_asset_volume,number_of_trades,taker_buy_base_asset_volume,taker_buy_quote_asset_volume"

255 0 2 2 2 0
1 0 0 6 8
```

This produces:

```text
comment     = open_time_ms,open,high,low,close,volume,...
schema_map  = [255, 0, 2, 2, 2, 0, 1, 0, 0, 6, 8]
```

The parser validates that slot `0` is the primary timestamp marker, parent
references point backward, the mapping fits in the header `schema_len u8`, and
the quoted comment fits in the header.

Schemas declare candidate transforms. Aura0 currently compiles this implemented
subset into field-program instructions:

```text
absolute
delta_previous
delta_base
delta_related
implicit_fixed_step
```

The schema may also allow candidate families that are tracked as stats before
the physical writer grows an emitted representation, such as midpoint,
delta-of-delta, rough fixed steps, zigzag varints, and bitpacking. Declaring a
candidate is permission to test it; the planner still chooses empirically from
the supported reversible encodings.

The `.aura` footer may keep the candidate plan and estimated bytes for audit.
The compiled `.aura0` or `.aura1` footer stores the smaller decode instruction:
operation code, value width, optional related field, and optional constants.

During `.aura` ingest, each integer column keeps the calculation facts needed to
score more Aura0 candidates later:

```text
absolute min/max
first-value base deltas
min/max midpoint deltas
previous-value deltas
delta-of-delta ranges
perfect fixed-step validity
rough fixed-step residuals
gap counts from missed expected steps
zigzag varint byte estimates
signed bit widths for bitpacking
related-field delta ranges
```

These calculations are column-local except related-field deltas, which are driven
by the schema relationship metadata. They are inputs to a seal-time planner, not
state that every compiled file must retain.

Snapshot-style schemas are expected to be separate logical schemas or explicit
record kinds. They should not be forced into the delta schema just because both
describe a book.
