# Field programs

Field programs are the compact instructions stored in compiled `.aura0` and
`.aura1` footers. The ingest footer may keep stats and candidate plans, but a
compiled footer should keep the result: one decode instruction per logical
field, plus only the constants needed to replay that field.

## Field code

Each field starts with one little-endian `u16` code.

```text
bits 0..4    op
bits 5..7    value width
bits 8..10   constant width
bits 11..13  aux
bit 14       base constant present
bit 15       step constant present
```

The current operation codes are:

```text
0 absolute        store the value directly
1 delta_base      store value - base
2 delta_previous  store value - previous; row 0 comes from base
3 delta_related   store value - another field in the same row
4 fixed_step      derive base + row_index * step
```

Widths use this codebook:

```text
0 zero
1 i8
2 i16
3 i32
4 i64
```

`aux` stores a related field index when it fits in three bits. `aux = 7` means
the next two bytes are an extended `u16` related field index. Constants follow
only when their flags are set, encoded with `constant width`.

## Example

An OHLCV-like positional integer schema can compile to these instructions
without naming OHLCV in the schema:

```text
ts  op=fixed_step     width=zero  const=i64  base,step
v1  op=delta_previous width=i16   const=i64  base
v2  op=delta_related  width=i16   aux=1
v3  op=delta_related  width=i16   aux=1
v4  op=delta_related  width=i16   aux=1
v5  op=delta_base     width=i32   const=i64  base
```

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
forced; if a previous-value delta is smaller and the schema allows it, the
planner can choose it.
