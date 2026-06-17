# Schemas

Aura schemas describe logical records. They are separate from physical layout.
Every file repeats a compact schema map in the front header:

```text
schema_len    u8
comment_len   u8
schema_map    schema_len bytes
comment_utf8  comment_len bytes
```

The current Rust compact schema-map dialect is intentionally small:

```text
255      timestamp slot; currently required at physical slot 0
0        event/root slot with no parent
1-127    event slot parent; parent index = byte - 1
128      repeated child root slot
129-254  repeated child parent; parent index = byte - 129
```

Timestamp is currently expected in physical slot `0`. Parent refs encode an
earlier physical slot, so byte `2` means "delta from slot 1". Repeated slots use
the same parent idea with the `128` offset. This is enough for the current
generic i64 path to expose event roots, event parent relationships, repeated
child roots, and repeated child parent relationships.

The richer target dialect discussed in planning can add overflow derived
references, group array widths, boolean and enum leaves, and opaque streams.
Those target bytes are design notes, not current Rust output.

The mapping is a quick file-shape preview and body-start metadata. It is not the
full footer program. Constants, decimal scales, residual values, bit widths,
varint/bitpack choices, and compression choices live in the footer/body.
`comment_utf8` is optional human-readable text, usually CSV-style labels;
`comment_len = 0` means no comment.

### Current emitted examples

OHLCV-like positional i64:

```text
slots:
0 ts
1 open
2 high  parent=open
3 low   parent=open
4 close parent=open
5 volume

schema map:
255 0 2 2 2 0
```

Repeated child rows flattened into fixed-width logical records:

```text
slots:
0 ts
1 event_value
2 partition
3 child_side repeated root
4 child_price repeated parent=slot3
5 child_qty   repeated parent=slot4
6 child_flag  repeated parent=slot4

schema map:
255 0 0 128 132 133 133
```

### Target extended examples

The following examples show the planned richer header hints. They require an
extended dialect before they are valid emitted bytes in this crate.

```text
100 timestamp axis
101 derived expression reference
200 wrapper for the next group/node
202 repeated element width = 2
241 one-bit boolean stream
242 two-bit enum stream
255 opaque stream
```

```text
schema block
  schema_len       u32 little-endian
  schema_encoding  schema_len bytes
```

The footer schema block does not store a schema ID or schema name. The footer
schema encoding is the archive copy that keeps a file readable without the
external registry and is the authoritative stamped schema.

Schema encoding type `0` is the compact schema-map form for positional generic
i64 records:

```text
descriptor_type  u8 = 0
schema_map_len   u8
schema_map       schema_map_len bytes
```

Slot `0` is the timestamp slot when the map starts with `255`. Slots `1..N` are
generic `i64` values. This uses the same byte convention as the current front
header.

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
  field scope
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
5     flag_a        flag        zero-width or one-bit base-offset bitpack
6     flag_b        flag        zero-width or one-bit base-offset bitpack
```

The front header mapping for this shape is intentionally plain:

```text
[255, 0, 0, 0, 0]
```

or, with flags:

```text
[255, 0, 0, 0, 0, 0]
```

This says slot `0` is time and ordinary numeric slots are roots. Boolean and
enum leaf markers belong to the target extended dialect, not the current Rust
emitted dialect. The useful tick transforms are previous-row and base
transforms, so no fake parent link is needed.

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

## Generic planner

`src/generic_planner.rs` is the executable bridge between dumb header hints and
stamped generic instructions. It does not inspect field names or market-data
schema names. It uses only:

```text
slot position
timestamp marker
parent relationship
group and leaf markers
observed values collected during ingest
```

From that, it can stamp and execute these generic choices:

```text
fixed_step
base_bitpack
prev_delta
patched_bitpack
rle
bitplane_rle
dictionary
block_local
uuid_const_mask
group
partition_runs
derived_stream
```

For OHLC-like data, the parent map is enough for the planner to discover the
shape without a named OHLCV mode:

```text
open   first value, then delta from previous close
close  open + residual
high   max(open, close) + residual
low    min(open, close) - residual
```

For repeated child streams, the current emitted dialect marks repeated roots
with `128` and repeated parent links with `129..254`. The target extended
dialect can add group-width bytes such as `201..239` and wrappers such as
`200`, but those bytes are not emitted by the current Rust helper.

If a future transform cannot be derived from those hints plus observed values,
the schema header needs a generic hint, not a domain-specific one. The current
minimum emitted hint set is timestamp, parent relationships, and repeated
scope. The target hint set also includes generic group, derived-expression,
leaf-size, boolean/enum, and opaque markers.

`generic_i64_parent_schema` is the compact schema-map path for dynamic
OHLCV-like records and repeated child records. The map includes the timestamp
slot. Each byte is self-describing with the same header map convention listed
above.

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

schema map:
255 0 2 2 2 0 6 6
```

This says slot `0` is the timestamp, high/low/close are parented to open, and
taker buy/sell volumes are parented to total volume. The Aura0 planner can still
discover candle residuals generically from those parent relationships:

```text
E1 = max(slot 1, slot 4)
E2 = min(slot 1, slot 4)
```

The codec still chooses the actual physical encoding from stats. The schema map
is only relationship evidence, not a command to force a related delta.

For an orderbook update stream flattened into level rows with the current
emitted dialect:

```text
slots:
0 ts_event
1 sequence_final
2 sequence_primary
3 price
4 qty1
5 qty2
6 delete_flag

schema map:
255 0 0 128 128 128 128
```

Slots `0..2` are event-level fields and slots `3..6` are repeated child fields.
The current dialect repeats the child rows in `.aura1` fixed-width replay. A
future `.aura0` grouping planner can still stamp partition run counts,
event-value streams, segmented child deltas, and sparse/presence streams from
the observed rows and repeated scope. The stamped footer decides exact storage
units, widths, bitpacking, varints, packed dictionaries, and whether each
candidate actually wins.

On the Grimoire Bybit 15-minute fixture, this relationship shape stamps a
generic footer with partition run lengths, grouped event-value streams,
segmented price deltas, a multi-slot presence map, sparse nonzero quantity
streams, packed dictionaries for those quantity values, and a presence-derived
flag. There is no orderbook-specific opcode; the footer only says what to do
with repeated groups, presence bits, dictionaries, and streams.

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

schema_map = [255, 0, 2, 2, 2, 0, 1, 0, 0, 6, 8]
```

This produces:

```text
comment     = open_time_ms,open,high,low,close,volume,...
schema_map  = [255, 0, 2, 2, 2, 0, 1, 0, 0, 6, 8]
```

The generic schema-definition helper validates that slot `0` is the primary
timestamp marker, parent references point to valid earlier physical slots, the
mapping fits in the header `schema_len u8`, and the generated comment fits in
the header. Source adapters still own source-specific mapping, such as JSON
array indexes, decimal scales, missing-field policy, and venue quirks.

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
