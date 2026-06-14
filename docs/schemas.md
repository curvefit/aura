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

## Tick Data

Trade ticks usually do not need same-row parent relationships. Their compact
shape comes from row-local deltas and small enums:

```text
slot  field         role        common Aura0 result
0     ts_event_ns   timestamp   previous-offset bitpack, or grouping later
1     seq           sequence    previous-offset bitpack
2     price         price       previous-offset bitpack
3     size          quantity    base-offset bitpack
4     side          side        base-offset bitpack
5     is_block      flag        zero-width or one-bit base-offset bitpack
6     is_rpi        flag        zero-width or one-bit base-offset bitpack
```

The front header mapping for this shape is intentionally plain:

```text
[255, 0, 0, 0, 0]
```

or, with flags:

```text
[255, 0, 0, 0, 0, 0, 0]
```

This says only that slot `0` is time and the other slots are roots. The useful
tick transforms are previous-row and base transforms, so no fake parent link is
needed.

Bybit spot trades expose a numeric `execId`, which can be modeled as an
`Identifier` and compressed with previous/base bitpacking when it is monotonic.
Bybit derivatives expose UUID `execId` values; those are not losslessly
representable by the current generic i64 writer. For those streams, `seq` is the
compact int64 ordering key today. A future UUID/fixed-bytes or dictionary field
family should carry the derivative `execId` when trade-id preservation is
required.

Typed field roles matter. `Identifier` fields may use base/previous delta
bitpacking when numeric. `Side` and `Flag` fields may use base bitpacking, but
they are not candidates for product or proportional residual transforms. That
keeps the schema reader simple: arithmetic residuals are reserved for measurable
quantity/value fields, while enums stay enum-like.

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

The intended module pattern is:

```text
source adapter owns parsing and scaling
adapter hardcodes an Aura schema definition
adapter passes normalized rows into the generic Aura writer
writer collects stats and stamps .aura
compiler follows the stamped plans into .aura0 or .aura1
```

## Code-defined schema modules

Schema relationship definitions live in code beside the source adapter, the same
way a Parquet writer is given a schema before it receives typed column values.
The schema definition is not needed to decode a sealed file because the emitted
file carries the compact schema map in its header and the stamped schema in its
footer.

Example:

```text
field_names = [
  "open_time_ns", "open", "high", "low", "close", "volume",
  "close_time_ns", "quote_asset_volume", "number_of_trades",
  "taker_buy_base_asset_volume", "taker_buy_quote_asset_volume",
]

parent_slots = [255, 0, 2, 2, 2, 0, 1, 0, 0, 6, 8]
```

This produces:

```text
comment     = open_time_ms,open,high,low,close,volume,...
schema_map  = [255, 0, 2, 2, 2, 0, 1, 0, 0, 6, 8]
```

The generic schema-definition helper validates that slot `0` is the primary
timestamp marker, parent references point backward, the mapping fits in the
header `schema_len u8`, and the generated comment fits in the header. Source
adapters still own source-specific mapping, such as JSON array indexes, decimal
scales, missing-field policy, and venue quirks.

Schemas declare candidate transforms. Aura0 currently compiles this implemented
subset into field-program instructions:

```text
absolute
delta_previous
delta_base
delta_related
implicit_fixed_step
bitpacked_delta_previous
bitpacked_delta_base
bitpacked_delta_related
derived_offset
bitpacked_delta_related_offset
bitpacked_delta_previous_offset
bitpacked_delta_previous_field_offset
bitpacked_candle_max_offset
bitpacked_candle_min_offset
bitpacked_product_residual
bitpacked_proportional_residual
```

The schema may also allow candidate families that are tracked as stats before
the physical writer grows an emitted representation, such as midpoint,
delta-of-delta, rough fixed steps, zigzag varints, and exception tables.
Declaring a candidate is permission to test it; the planner still chooses
empirically from the supported reversible encodings.

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
candle open/close/high/low residual ranges when relationships imply a shape
product residual ranges such as quote = quantity * price / divisor
proportional residual ranges such as child_quote = quote * child_qty / qty
```

These calculations are mostly column-local. Related-field, candle-shape, product,
and proportional residuals are row scans driven by schema/order evidence. They
are inputs to a seal-time planner, not state that every compiled file must
retain.

Snapshot-style schemas are expected to be separate logical schemas or explicit
record kinds. They should not be forced into the delta schema just because both
describe a book.
