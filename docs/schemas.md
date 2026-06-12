# Schemas

Aura schemas describe logical records. They are separate from physical layout.

```text
schema descriptor
  name
  schema_id
  fields[]

field descriptor
  index
  name
  field type
  semantic role
  nullable flag
  relationship
```

Collectors should write normalized `.aura` ingest values using generous integer
types. The schema tells Aura what those values mean; ingest stats tell Aura how
small or fast the compiled representation can be.

Starter schema constructors exist for:

```text
book_delta_v1
tick_v1
ohlcv_v1
```

`ohlcv_v1` is intentionally a simple six-field integer schema:

```text
0 ts_open  timestamp
1 open     price_anchor
2 high     price delta_from_field 1
3 low      price delta_from_field 1
4 close    price delta_from_field 1
5 volume   quantity
```

The relationship metadata lets Aura0 test encodings such as `high - open`,
`low - open`, and `close - open` instead of only testing previous-value deltas.
Timestamps can also be proven implicit when every row advances by the same fixed
step, such as one-minute OHLCV bars.

The intended plug-in pattern is:

```text
define logical schema
write generous ingest records
collect stats while writing
seal footer
compile to .aura0 or .aura1
```

Aura0 planners currently represent these field encodings in the footer:

```text
absolute
delta_previous
delta_base
delta_related
implicit_fixed_step
```

Each chosen field plan stores the selected width, optional related field, base
value, fixed step, and estimated encoded bytes.

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
by the schema relationship metadata.

Snapshot-style schemas are expected to be separate logical schemas or explicit
record kinds. They should not be forced into the delta schema just because both
describe a book.
