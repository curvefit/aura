# Field programs

Field programs are the compact instructions stored in compiled `.aura0` and
`.aura1` footers. The ingest footer may keep stats and candidate plans, but a
compiled footer should keep the result: one decode instruction per logical
field, plus only the constants needed to replay that field.

## Field code

Each compiled field starts with one little-endian `u16` code.

```text
bits 0..4    op
bits 5..7    value width
bits 8..10   constant width
bits 11..13  aux
bit 14       base constant present
bit 15       step constant present
```

The current compiled operation codes are:

```text
0  absolute            store the value directly
1  delta_base          store value - base
2  delta_previous      store value - previous; row 0 comes from base
3  delta_related       store value - another field in the same row
4  fixed_step          derive base + row_index * step
5  bitpack_previous    bitpack value - previous; row 0 comes from base
6  bitpack_base        bitpack value - base
7  bitpack_related     bitpack value - another earlier field in the same row
8  derived_offset      derive related field + base; body stores no values
9  bitpack_rel_offset  bitpack (value - related - min_delta)
10 bitpack_prev_offset bitpack (value - previous - min_delta)
11 bitpack_prev_field  bitpack (value - previous related field - min_delta)
12 max_plus_residual   bitpack (value - max(ref_a, ref_b) - min_residual)
13 min_minus_residual  bitpack (min(ref_a, ref_b) - value - min_residual)
14 product_residual    bitpack (value - quantity * price / divisor - min_residual)
15 proportional_resid  bitpack (value - total_value * child_qty / total_qty - min_residual)
```

Widths use this codebook:

```text
0 zero
1 i8
2 i16
3 i32
4 i64
5 i128
```

`aux` stores a reference field index when it fits in three bits. `aux = 7` means
the next two bytes are an extended `u16` reference field index. Constants follow
only when their flags are set, encoded with `constant width`. Bitpacked ops then
store one extra `u8` bit width.

Plain bitpacked deltas use fixed-width two's-complement signed bitpacking.
Offset and residual ops store unsigned bitpacked spans after subtracting the
minimum observed delta or residual. The minimum is kept in `base`; for
previous-field and legacy max/min residual ops, `step` stores the second
reference or the minimum previous delta. Product residuals pack
`(price_ref, divisor)` into `step`, and proportional residuals pack
`(child_qty_ref, total_qty_ref)` into `step`.

Aura0 bodies are columnar by field program order. This keeps each slot's stream
contiguous, so a bitpacked slot does not need row-level byte padding between
unrelated fields. Aura1 bodies remain fixed-width row-major replay data.

## Example

An OHLCV-like positional integer schema can compile to these instructions only
when the schema header declares the min/max derived expressions:

```text
ts     op=fixed_step          width=zero  const=i64  base,step
open   op=bitpack_prev_field  bits=N      ref=close base=open0 step=min(open - prev_close)
high   op=max_plus_residual   bits=N      ref=open  step=close min_residual
low    op=min_minus_residual   bits=N      ref=open  step=close min_residual
close  op=bitpack_rel_offset  bits=N      ref=open  base=min(close - open)
volume op=bitpack_base        bits=N      const=i64  base
```

For a Binance kline-style schema, the same row scan can also choose:

```text
quote_volume     op=product_residual  ref=volume         aux=mean_floor(high, low)
taker_buy_quote  op=product_residual  ref=taker_buy_base aux=mean_floor(high, low)
```

These are still generic field programs. The planner derives them from declared
schema/header relationships and only keeps them when their bitpacked residual
stream is smaller than the existing candidate. Scale constants and residual
streams are footer/body facts; permission to calculate a derived expression is a
schema-header fact.

The implemented generic planner consumes header/schema expression definitions
for `add_residual`, `subtract_residual`, `max_plus_residual`,
`min_minus_residual`, and `first_offset_then_delta`. It validates that
`101-199` schema-map bytes reference matching expression IDs and output slots,
rejects duplicate outputs and expression dependency cycles, and stamps the
selected round-trip `DerivedStream` instruction in the footer plan. It does not
look at OHLCV field names.

The file no longer needs to keep min/max ranges or candidate scoring tables in
the compiled footer. Those were ingest-time evidence. A reader only needs the
instruction, the schema order, the record count, and any constants referenced by
the instruction.

## Planner inputs

The ingest writer tracks cheap facts per column while it writes records:

```text
absolute min/max
base and midpoint deltas
previous-value deltas
delta-of-delta ranges
perfect fixed steps
rough fixed steps and gap counts
zigzag varint byte estimates
bit widths for packed integers
schema-declared related-field deltas
```

The planner scores candidates from those facts and picks the smallest reversible
instruction. Schema relationships make related deltas possible, but they are not
forced. If a previous-value delta, constant offset, header-declared max/min
residual, or product residual is smaller and reversible, the planner can choose
it.

When a full typed schema is available, residual search is role-gated. Product
residuals are only searched with quantity and price-like operands, and
proportional residuals are only searched across value/quantity fields. Side,
flag, sequence, timestamp, and identifier fields stay on direct/base/previous
encodings. Generic positional i64 schemas keep the broad empirical search
because every non-time slot is only known as a generic value.

For trade ticks this usually compiles to:

```text
ts_event_ns  op=bitpack_prev_offset  bits=N  base=first_ts  step=min_dt
seq          op=bitpack_prev_offset  bits=N  base=first_seq step=min_dseq
exec_id      op=bitpack_prev_offset  bits=N  when numeric and monotonic
price        op=bitpack_prev_offset  bits=N  base=first_px  step=min_dpx
size         op=bitpack_base         bits=N  base=min_size
side         op=bitpack_base         bits=1..2
flags        op=bitpack_base         bits=0..1
```

Aura0 decoding is columnar, so some fields are delayed until their dependencies
exist. The decoder resolves previous-field pairs first, then related offsets,
product residuals, proportional residuals, and header-declared max/min
residuals. Aura1 uses its own stamped fixed-width field program from the same
compiled footer; it is not derived by replanning an Aura0 file.
