# Schemas

Aura schemas describe logical records. They are separate from physical layout.
Every file repeats a compact schema map in the front header:

```text
schema_len    u8
comment_len   u8
derived_len   u16
schema_map    schema_len bytes
derived_exprs derived_len bytes
comment_utf8  comment_len bytes
```

The current Rust compact schema-map dialect is intentionally small:

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

Timestamp is expected in physical slot `0` when present. Files without byte
`100` are treated as non-time-series data. Parent refs encode an earlier
physical slot, so byte `2` means "delta from slot 1". A group-width byte marks
the current slot and the following `width - 1` slots as repeated child fields.
The mapping exposes event roots, parent relationships, derived expression refs,
repeated groups, boolean/enum/bitfield leaves, and opaque streams without
naming a market-data schema.

The mapping is a quick file-shape preview and body-start metadata. It is not the
full footer program. Constants, decimal scales, residual values, bit widths,
varint/bitpack choices, and compression choices live in the footer/body.
Derived expression definitions belong to the schema header, not the footer,
because they decide which same-row calculations are legal during ingestion.
`comment_utf8` is optional human-readable text, usually CSV-style labels;
`comment_len = 0` means no comment.

### Derived Expression Header

Bytes `101-199` are not literal parent slots. They reference schema-header
derived expressions by id:

```text
101 -> expression 1
102 -> expression 2
...
199 -> expression 99
```

A derived expression is generic arithmetic metadata in the schema header. It
must define at least:

```text
expression_id
output_slot
op              add | sub | mul | div | min | max | add_residual |
                subtract_residual | max_plus_residual | min_minus_residual |
                first_offset_then_delta
input_slots
literal constants, if any
residual direction, if the op emits a residual stream
```

The serialized v2 table is:

```text
derived_len u16
expr_count  u8

entry:
  expression_id u8
  op            u8
  output_slot   u16
  flags         u8
  input_count   u8
  input_slots   input_count * u16
  literal_count u8
  literals      literal_count * i64
```

`derived_len` is a two-byte little-endian length because expression definitions
can exceed 255 bytes.

The compact byte only says "this slot uses expression N." The expression table
says what calculation is legal. For example, a high-like field can be represented
without schema names as `max_plus_residual(output=slot2, inputs=[slot1, slot4])`,
and a low-like field as `min_minus_residual(output=slot3, inputs=[slot1, slot4])`.
Those calculations are allowed only because the header expression table declares
them; a parent-only map such as `100 0 2 2 2 0` does not authorize max/min
shape discovery.

The footer later stamps the chosen round-trip instruction and residual stream
layout. The footer may choose direct storage if the declared expression does not
win on measured size, but it must not invent a new derived expression that was
not present in the schema header.

Current Rust decodes `101-199` as expression refs, preserves those bytes in the
compact map, and round-trips the v2 header expression table. The planner
execution path that turns those refs into max/min candidates is still the next
required implementation step.

### Current emitted examples

OHLCV-like positional i64 with direct parent deltas only:

```text
slots:
0 ts
1 open
2 high  parent=open
3 low   parent=open
4 close parent=open
5 volume

schema map:
100 0 2 2 2 0
```

That map authorizes `slot2 - slot1`, `slot3 - slot1`, and `slot4 - slot1`
candidate streams. It does not authorize `max(slot1, slot4)` or
`min(slot1, slot4)` calculations.

The same logical fields with declared min/max expression refs would use
derived-expression bytes for the slots that need shape math:

```text
slots:
0 ts
1 open
2 high  expr2 = max(slot1, slot4) + residual
3 low   expr3 = min(slot1, slot4) - residual
4 close parent=open
5 volume

schema map:
100 0 102 103 2 0
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
100 0 0 204 4 5 5
```

### Extended examples

These richer hints are valid compact schema bytes in the current crate:

```text
100 timestamp axis
101 derived expression ref 1
200 dual-domain repeated group marker
202 repeated group width = 2 slots
241 one-bit boolean leaf
242 two-bit enum leaf
243 bitfield leaf, up to 8 flags
255 opaque / do-not-attempt stream
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

Slot `0` is the timestamp slot when the map starts with `100`. Slots without
leaf markers are generic `i64` values. This uses the same byte convention as the
current front header.

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

Phase 3 supports a typed writer boundary in addition to the existing
`Vec<Vec<i64>>` compatibility path:

```text
default numeric value  i64
explicit wide number   i128
opaque identifier      opaque16
```

Explicit `i128` is for true numeric overflow or arithmetic fields that cannot
fit in `i64`. `opaque16` is for fixed 16-byte identifiers and is not treated as
an arithmetic number. Opaque fields default to absolute-only candidates and
cannot be parent-delta fields.

Typed ingest can seal declared `i128` and `opaque16` values losslessly in
`.aura` files. The typed body stores ordinary i64-compatible slots as i64,
`i128` slots as 16-byte little-endian integers, and `opaque16` slots as fixed
16-byte payloads. The i64 reader and i64 compiler reject wide typed files
instead of silently downcasting. Strict mode never silently downcasts and never
mutates the schema. Overflow and width mismatches report the row, slot,
declared type, observed type/value class, and suggested upgrade.

Derived slots have one owner for a given ingest path. A slot may be supplied by
the caller as an ordinary logical field or reserved for an internal derivation.
The typed writer can accept rows with internal-derived slots omitted, compute
the slot from the schema expression, and then seal the full logical row. It
rejects supplying values for a slot also marked internally derived.

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
[100, 0, 0, 0, 0]
```

or, with flags:

```text
[100, 0, 0, 0, 0, 241]
```

This says slot `0` is time and ordinary numeric slots are roots. Boolean and
enum leaf markers are compact generic hints; the useful tick transforms are
previous-row and base transforms, so no fake parent link is needed.

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
derived expression reference
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

For OHLC-like data, a parent map is enough to test direct related deltas such as
`close - open`. It is not enough to discover shape math. Min/max residuals must
be declared through derived expression refs:

```text
100 0 102 103 2 0

expr2: output slot 2 = max(slot 1, slot 4) + residual
expr3: output slot 3 = min(slot 1, slot 4) - residual
```

For repeated child streams, the current emitted dialect marks the first repeated
slot with a group-width byte such as `204`, which means "this slot and the next
three slots are repeated." Slots inside the group still use ordinary root bytes
or `1..99` parent bytes.

If a future transform cannot be represented by parent refs, group refs, leaf
markers, or derived expression refs, the schema header needs another generic
hint, not a domain-specific one. The current minimum emitted hint set is
timestamp, parent relationships, derived expression refs, repeated groups,
boolean/enum/bitfield leaves, and opaque markers.

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
100 0 2 2 2 0 6 6
```

This says slot `0` is the timestamp, high/low/close are parented to open, and
taker buy/sell volumes are parented to total volume. The Aura0 planner may score
related residuals from those parent relationships. If high/low should use
`max(open, close)` or `min(open, close)` residuals, the schema header must use
`101-199` expression refs for those output slots and include the expression
definitions.

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
100 0 0 204 0 0 0
```

Slots `0..2` are event-level fields and slots `3..6` are repeated child fields.
The current dialect repeats the child rows in `.aura1` fixed-width replay. A
future `.aura0` grouping planner can still stamp partition run counts,
event-value streams, segmented child deltas, and sparse/presence streams from
the observed rows and repeated scope. The stamped footer decides exact storage
units, widths, bitpacking, varints, packed dictionaries, and whether each
candidate actually wins.

On the 15-minute orderbook fixture, this relationship shape stamps a
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

schema_map = [100, 101, 102, 103, 2, 0, 1, 107, 0, 6, 110]

expr1  = first_offset_then_delta(output=1, input=4)
expr2  = max_plus_residual(output=2, inputs=[1,4])
expr3  = min_minus_residual(output=3, inputs=[1,4])
expr7  = mul(output=7, inputs=[4,5])
expr10 = mul(output=10, inputs=[4,9])
```

This produces:

```text
comment     = open_time_ms,open,high,low,close,volume,...
schema_map  = [100, 101, 102, 103, 2, 0, 1, 107, 0, 6, 110]
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
bitpacked_max_plus_residual
bitpacked_min_minus_residual
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
header-declared derived expression residual ranges
product residual ranges such as quote = quantity * price / divisor
proportional residual ranges such as child_quote = quote * child_qty / qty
```

These calculations are mostly column-local. Related-field, header-declared
derived expressions, product, and proportional residuals are row scans driven by
schema/order evidence. They are inputs to a seal-time planner, not state that
every compiled file must retain.

Snapshot-style schemas are expected to be separate logical schemas or explicit
record kinds. They should not be forced into the delta schema just because both
describe a book.
