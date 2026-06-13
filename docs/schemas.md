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
  transform candidates
```

`schema_id` is the compact registry key used by the fixed file header. The full
schema descriptor is still written into the footer so a file remains readable
without the external registry and so registry lookups can be checked against the
embedded schema.

Collectors should write normalized `.aura` ingest values using generous integer
types. The schema tells Aura what those values mean; ingest stats tell Aura how
small or fast the compiled representation can be.

Starter schema constructors exist for:

```text
book_delta_v1
tick_v1
ohlcv_v1
generic_i64_schema
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

The intended plug-in pattern is:

```text
define logical schema
write generous ingest records
collect stats while writing
seal footer
compile to .aura0 or .aura1
```

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
