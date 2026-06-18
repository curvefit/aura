# Ingest Inference and Calculation Contract

This document defines what the `.aura` ingestor should infer from the schema
header and what simple calculations it should collect before the seal-time
Aura0 planner stamps final decode instructions.

The key rule is:

```text
Header bytes provide generic relationship evidence.
Ingest collects reversible statistics and candidate streams.
Footer stamps the final binary decode instructions.
```

The ingestor must not hardcode "OHLCV", "tick", "orderbook", "Bybit", or any
other named schema as a special case. It may recognize generic structures
described by the header: timestamps, parent relationships, derived expression
refs, dual-domain groups, repeated groups, booleans/enums/bitfields, and opaque
streams.

## Production Header

All production Aura files use the `AURA` container magic.

```text
offset  size  field
0       4     magic          AURA
4       2     version
6       1     profile        ingest | Aura0 | Aura1
7       2     header_len
9       8     start_time_ns
17      2     stream_id
19      2     dictionary_id
21      1     schema_len
22      1     comment_len
23      2     derived_len
25      N     schema_map
25+N    D     derived_exprs
25+N+D  M     comment_utf8
```

The compact schema bytes are:

```text
0        root slot / no parent
1-99     parent slot reference, parent index = byte - 1
100      timestamp slot
101-199  derived expression ref, expression id = byte - 100
200      dual-domain repeated group marker
201-239  repeated group width, width = byte - 200
241      1-bit boolean leaf
242      2-bit enum leaf
243      bitfield leaf, up to 8 flags
255      opaque / do-not-attempt stream
```

The header does not store chosen physical transforms. It does store generic
relationship evidence, including derived expression refs. A `101-199` byte must
resolve to a schema-header expression definition before the ingestor may compute
`+`, `-`, `*`, `/`, `min`, `max`, or residual streams for that slot.

`header_len` and `derived_len` are little-endian `u16` values. The schema map
and comment remain length-prefixed by one byte each; the expression table gets
the wider length because rich derived definitions can exceed 255 bytes.

## What the Ingestor Infers From the Header

For each file, the ingestor should build a `SchemaMap`-like view with these
facts.

### Slot Role

For every physical slot:

```text
slot index
header byte
root / timestamp / parented / derived expression / group / leaf / opaque
parent slot if byte is 1-99
derived expression id if byte is 101-199
leaf width if byte is 241, 242, or 243
opaque flag if byte is 255
```

### Time-Series Axis

If any slot has byte `100`:

```text
timestamp_slot = that slot, expected physical slot 0
file is time-series
start_time_ns candidate = first timestamp
```

If no slot has byte `100`:

```text
file is non-time-series
start_time_ns = 0
do not run timestamp fixed-step assumptions
```

### Parent Relationships

For bytes `1-99`, the ingestor infers:

```text
child slot may be tested against parent slot
child - parent residual is legal
child - previous(parent) residual is legal where row order exists
same-row related residual candidates are legal
```

The parent byte is permission to score related transforms. It is not a command
to force related deltas.

### Derived Expressions

For bytes `101-199`, the ingestor infers:

```text
slot references a header-declared derived expression
expression id = byte - 100
slot must have exactly one owner for the ingest path
```

The expression definition lives in the schema header and declares the operation,
output slot, input slots, literals, and residual direction. Valid generic
operations include:

```text
add
sub
mul
div
min
max
add_residual
subtract_residual
max_plus_residual
min_minus_residual
first_offset_then_delta
```

The writer must reject an ingest path that both supplies a slot externally and
also marks the same slot as internally computed.

Current Rust decodes and preserves `101-199` expression refs in the compact
schema map, validates the full schema-header expression table, and scores those
declared expressions as executable Aura0 candidates. The planner may still choose
direct storage if a declared expression's residual stream plus footer instruction
overhead is larger than the direct stream.

### Repeated Groups

For bytes `201-239`, the ingestor infers:

```text
group starts at this physical slot
group width = byte - 200
current slot and next width - 1 slots are repeated children
slots before group are event-level slots
```

For byte `200`, the ingestor infers:

```text
the following group has two domains
physical streams may be split by domain
domain names are not assumed
domain values/order must be observed from data or explicit slot values
```

In orderbook data those domains may be bid/ask, but the ingestor must treat
them as generic domain `0` and domain `1`.

### Leaf and Opaque Slots

For bytes `241`, `242`, and `243`:

```text
241 -> boolean, 1 bit logical value
242 -> enum, up to 2 bits
243 -> bitfield, up to 8 flags
```

For byte `255`:

```text
do not attempt arithmetic deltas
do not use signed i64 previous deltas
test only opaque-safe candidates such as constant masks or raw bytes
```

This is important for UUID/exec ID tick fields.

## Column-Local Calculations During Ingest

For every numeric non-opaque slot or candidate stream, collect:

```text
count
first value
last value
min
max
integer GCD storage unit
unsigned bit width after min/base normalization
signed bit width for deltas
null/presence count if nullable
zero count
distinct count
frequency histogram or top-K counts
RLE run count and longest run
fixed-width dictionary estimate
canonical Huffman estimate if distinct count is reasonable
optional ANS/FSE estimate only after Huffman exists and proves insufficient
```

For ordered rows, also collect:

```text
previous delta min/max
previous delta GCD
previous delta bit width
zigzag varint byte estimate
delta-of-delta min/max
delta-of-delta bit width
exact fixed-step validity
fixed-step base and step if exact
rough fixed-step residual min/max if not exact
gap count from expected step where timestamp-like
```

These are simple scans. They do not require schema names.

## Parent/Related Calculations

For every header-declared parent relationship, collect candidate residuals:

```text
child - parent
child - parent - min_residual
child - previous(parent) where row order exists
child - previous(child)
child - previous(parent field chosen by sibling relationship)
```

For header-declared derived expressions, the ingestor computes only the
residual stream required by that expression:

```text
add_residual        -> output - input
subtract_residual   -> input - output
max_plus_residual   -> output - max(input_a, input_b)
min_minus_residual  -> min(input_a, input_b) - output
first_offset_then_delta -> first output, then output - previous(input)
add/sub/mul/div/min/max -> output - expression(input slots, literals)
```

Parent-child bytes alone do not authorize min/max shape discovery. A map such as
`100 0 2 2 2 0` only authorizes parent residuals against slot `1`. To test
high/low-like min/max residuals, the relevant output slots must use `101-199`
expression refs and the header must define the corresponding `max_plus_residual`
or `min_minus_residual` expressions.

## Product and Proportional Calculations

The compact one-byte map alone does not identify price/quantity semantics.
Product and proportional relationships must be authorized by either a
header-declared derived expression ref (`101-199`) or a richer schema descriptor
with generic transform-candidate flags. Where that evidence exists, the ingestor
may test:

```text
value - quantity * price / divisor
value - total_value * child_quantity / total_quantity
parent - child
```

For product relationships representable without a divisor, the compact form is
just a generic `mul` expression:

```text
slot byte = 100 + expression id
expression op = mul
residual stream = output - input_a * input_b
```

If fixed-point decimal scales require division, the source adapter must either
normalize values so the product lands in the output scale or declare a richer
expression with the required divisor before that transform is legal.

## Group Calculations During Ingest

For every repeated group:

```text
event count
total child count
child count per event
runs per event
run lengths
event boundary reconstruction data
event-level streams that can be stored once per event
child-level streams that must be stored per child
```

For dual-domain groups:

```text
domain count per event, e.g. domain0_count and domain1_count
domain order per event if order is not fixed
per-domain child stream statistics
per-domain first value statistics
per-domain inside-run delta statistics
```

The prototype showed that storing `bid_count` and `ask_count` as ordinary
encoded streams beats raw group-count metadata. Production should generalize
that as:

```text
domain0_count stream
domain1_count stream
optional domain_order stream
```

## Segmented Child Delta Calculations

For child numeric slots inside repeated groups, collect per partition/domain:

```text
partition/domain storage unit
partition/domain base
first value per run as offset from partition/domain base
inside-run deltas from previous child value
bit width and varint estimates for both streams
frequency histogram for both streams
```

For orderbook-like price levels, this becomes:

```text
first price offset per side/domain run
inside-run price deltas
```

No orderbook opcode is required.

## Sparse Presence Calculations

For repeated child slots and nullable/zero-heavy numeric slots:

```text
presence bit per slot
combined presence map for multiple slots
nonzero value stream per sparse slot
presence-derived boolean candidate
zero-width constant candidate
dictionary/Huffman estimates for nonzero streams
```

A larger combined presence map may win if it removes multiple direct value
streams. The planner should compare total body plus footer bytes.

## Huffman Calculations

Huffman is binary entropy coding over integer symbols, not text. During ingest,
the planner can estimate and later stamp a `huffman_dictionary` candidate when:

```text
distinct count is bounded enough for footer metadata
symbol distribution is skewed
dictionary entries can be encoded compactly
Huffman body + code-length footer beats fixed-width dictionary and bitpack
```

Required facts:

```text
sorted unique values
dictionary entry unit/base/entry width
frequency count for each dictionary ID
canonical Huffman code length per dictionary ID
encoded bit count
footer metadata byte estimate
```

The compiled footer should store code lengths in binary form. It should not
store a tree.

## ANS/FSE Calculations

The prototype tested an order-0 rANS-style candidate. It beat Huffman by:

```text
Grimoire: 586 bytes
OHLCV:    3655 bytes
Tick:     5 bytes
```

That is not enough to justify production complexity yet. Production should
implement Huffman before ANS/FSE. If ANS/FSE is tested later, ingestion must
track:

```text
symbol frequencies
normalized frequency table
scale bits
frequency table footer cost
body byte estimate
decode table construction cost
```

## Data-Family Examples

These examples explain what the generic rules produce. They are not hardcoded
schemas.

### OHLCV-Like Data

Header evidence:

```text
100 0 102 103 2 0

expr2: output slot 2 = max(slot 1, slot 4) + residual
expr3: output slot 3 = min(slot 1, slot 4) - residual
```

If the source can legally declare open as a previous-close relationship, the
generic expression form is:

```text
100 101 102 103 2 0

expr1: output slot 1 = first value, then slot 1 - previous(slot 4)
expr2: output slot 2 = max(slot 1, slot 4) + residual
expr3: output slot 3 = min(slot 1, slot 4) - residual
```

Ingestor calculations:

```text
timestamp fixed step
open - previous close where declared with first_offset_then_delta
close - open
slot2 - max(slot1, slot4)
min(slot1, slot4) - slot3
volume direct/base/block candidates
frequency histograms for declared derived residuals
```

For kline-like rows that also contain quote notional fields, the schema can
declare product residuals without naming Binance or any exchange:

```text
100 101 102 103 2 0 1 107 0 6 110

expr1:  output slot 1  = first value, then slot 1 - previous(slot 4)
expr2:  output slot 2  = max(slot 1, slot 4) + residual
expr3:  output slot 3  = min(slot 1, slot 4) - residual
expr7:  output slot 7  = slot 4 * slot 5 + residual
expr10: output slot 10 = slot 4 * slot 9 + residual
```

Ingestor calculations for the product fields:

```text
slot7 residual  = slot7 - slot4 * slot5
slot10 residual = slot10 - slot4 * slot9
```

Prototype winners:

```text
close_minus_open -> Huffman dictionary
max residual     -> Huffman dictionary
min residual     -> Huffman dictionary
open_delta       -> block-local/basic stream
volume           -> block-local/basic stream
```

### Tick Data

Header evidence:

```text
timestamp slot
root price
root size
small enum/boolean side and flags
opaque UUID/exec ID as 255
```

Ingestor calculations:

```text
timestamp runs and block-local candidates
price previous/base/block candidates
size dictionary/Huffman candidates
side enum/RLE candidates
flag const/bit candidates
UUID constant-mask opaque candidate
```

Do not compute signed previous deltas for UUIDs.

Prototype winners:

```text
timestamp -> block-local
price     -> block-local
size      -> Huffman dictionary
side      -> RLE
flag      -> constant
UUID      -> constant-mask packed variable bits
```

### Grimoire/Orderbook-Like Repeated Data

Header evidence:

```text
timestamp slot
event-level sequence roots
200 dual-domain group marker
201-239 repeated group width
child slots for price, quantities, flags
```

Ingestor calculations:

```text
domain0_count/domain1_count streams
optional domain_order stream
event kind/timestamp/sequence event-level streams
per-domain first price offsets
per-domain inside-run price deltas
presence flags for delete and quantity presence
sparse nonzero quantity streams
dictionary/Huffman candidates for counts, flags, deltas, quantities
```

Prototype winners included:

```text
bid_count/ask_count -> Huffman or ANS dictionary
inside-run deltas   -> Huffman or ANS dictionary
quantity flags      -> Huffman or ANS dictionary on some streams
large quantities    -> block-local/basic streams on some streams
```

## Seal-Time Planner Responsibilities

At seal time, the planner should compare candidates by:

```text
body bytes
footer metadata bytes
decode instruction bytes
chunking overhead
optional compression wrapper bytes if enabled
```

It then stamps:

```text
stream repr
value count
body length
aux constants
dictionary metadata
code lengths or frequency tables if entropy-coded
group reconstruction instructions
derived stream instructions
```

The compiled Aura0 file should not need to rerun inference. It should only
execute the stamped footer instructions.

## What the Ingestor Must Not Do

The ingestor must not:

```text
hardcode OHLCV, tick, orderbook, Bybit, Binance, OKX, or Grimoire schemas
force parent deltas just because a parent byte exists
perform arithmetic on opaque 255 streams
silently downcast i128 or opaque fields to i64
store AI/planning evidence in compiled Aura0 footers
require field names to decode a compiled file
```

The ingestor may use source adapters to map external JSON/CSV names into the
generic schema. After that point, candidate scoring should be driven by header
relationships and observed integer values.
