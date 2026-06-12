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

The intended plug-in pattern is:

```text
define logical schema
write generous ingest records
collect stats while writing
seal footer
compile to .aura0 or .aura1
```

Snapshot-style schemas are expected to be separate logical schemas or explicit
record kinds. They should not be forced into the delta schema just because both
describe a book.
