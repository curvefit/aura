# Aura SDK

Aura exposes a small Rust library facade for dynamic fixed-width schemas.

```rust
use aura_codec::{
    AuraRecordBatch, AuraSchema, AuraType, AuraValue, AuraWriter, WriterOptions,
};

let schema = AuraSchema::builder()
    .field("ts_event", AuraType::TimestampNanos)
    .field("symbol_id", AuraType::U32)
    .field("price", AuraType::PriceI64Scaled { scale: 9 })
    .field("size", AuraType::U64)
    .field("side", AuraType::EnumU8)
    .field("flags", AuraType::FlagsU32)
    .build()?;

let batch = AuraRecordBatch::new(schema.clone(), vec![
    vec![
        AuraValue::I64(1_700_000_000_000_000_000),
        AuraValue::U64(101),
        AuraValue::I64(42_100_000_000),
        AuraValue::U64(10),
        AuraValue::U64(1),
        AuraValue::U64(0),
    ],
])?;

let mut bytes = Vec::new();
let mut writer = AuraWriter::try_new(&mut bytes, schema, WriterOptions::aura0_compact())?;
writer.write_batch(batch)?;
writer.finish()?;
# Ok::<(), aura_codec::AuraError>(())
```

## Public Types

- `AuraSchema`, `AuraSchemaBuilder`, `AuraField`, `AuraType`
- `AuraRecordBatch`, `AuraColumnBatch`, `AuraColumn`, `AuraValue`
- `AuraWriter`, `AuraReader`
- `WriterOptions`, `ReaderOptions`, `ConvertOptions`
- `AuraFormat`, `AuraProfile`
- `CompiledAuraPlan`, `CompiledAuraField`
- `convert_aura`

## Supported Schema Types

The SDK currently supports fixed-width scalar fields that can be represented by the existing generic i64 physical engine:

- `Bool`
- `U8`, `U16`, `U32`, `U64` values that fit in the current signed physical lane
- `I8`, `I16`, `I32`, `I64`
- `TimestampNanos`, `TimestampMicros`
- `I64Scaled { scale }`, `PriceI64Scaled { scale }`
- `EnumU8`
- `FlagsU32`

Unsupported types are rejected during schema build:

- `F32`
- `F64`
- `Binary`
- `Utf8`
- nullable fields

Values are range-checked against the declared type before writing.

## Writing

Use `AuraWriter::try_new(output, schema, options)` and `write_batch`.

Supported output formats:

- `WriterOptions::new(AuraFormat::Aura)` for sealed ingest
- `WriterOptions::aura0_compact()` for compact `.aura0`
- `WriterOptions::aura1()` for fixed-width `.aura1`

Aura0 fast and hybrid profiles are available through `WriterOptions::profile`, but compact remains the default SDK Aura0 writer profile.

`write_batch` accepts both row batches and column batches:

```rust
let batch = aura_codec::AuraColumnBatch::builder(schema.clone())
    .i64("ts_event", ts_values)
    .u32("symbol_id", symbol_values)
    .i64("price", price_values)
    .u64("size", size_values)
    .u8("side", side_values)
    .u32("flags", flag_values)
    .build()?;
writer.write_batch(batch)?;
# Ok::<(), aura_codec::AuraError>(())
```

## Reading

Use `AuraReader::open(input)` to detect the sealed profile, recover the schema, and read rows:

```rust
let reader = aura_codec::AuraReader::open(std::io::Cursor::new(bytes))?;
let schema = reader.schema();
let batches = reader.read_batches()?;
# Ok::<(), aura_codec::AuraError>(())
```

For Aura1 files on disk, prefer the file-backed APIs:

```rust
let mut reader = aura_codec::AuraReader::open_path("ticks.aura1")?;
while let Some(batch) = reader.next_batch(8192)? {
    println!("rows={}", batch.row_count());
}
assert_eq!("file_range", reader.stats().source_kind.as_str());
# Ok::<(), aura_codec::AuraError>(())
```

`open_path` and `open_file` read only the Aura1 header/trailer/footer at open,
compile the `CompiledAuraPlan`, and range-read Aura1 body rows as replay or
batches request them. The generic `open(Read)` API remains memory-backed
because a plain `Read` source cannot seek to the footer without buffering.

`AuraReader` supports whole-file batches and true batch iteration:

```rust
let mut reader = aura_codec::AuraReader::open(std::io::Cursor::new(bytes))?;
while let Some(batch) = reader.next_batch(1024)? {
    println!("rows={}", batch.row_count());
}
# Ok::<(), aura_codec::AuraError>(())
```

Aura1 reads stream fixed-width rows directly from the Aura1 body. Aura0 compact reads parse metadata during open and lazily build bounded row batches from compact stream columns. `read_batches()` is a convenience collector implemented on top of the batch reader.

For faster Aura1 parsing into SDK batches, use column batches:

```rust
let mut reader = aura_codec::AuraReader::open(std::io::Cursor::new(bytes))?;
while let Some(batch) = reader.next_column_batch(8192)? {
    println!("columns={} rows={}", batch.columns().len(), batch.row_count());
}
# Ok::<(), aura_codec::AuraError>(())
```

`next_column_batch` avoids row-vector materialization and per-cell `AuraValue`
construction. Use row batches when callers need `AuraRecordBatch`; use column
batches for scan/analytics-style access.

For low-level fixed-width Aura1 replay, use batch callbacks:

```rust
let reader = aura_codec::AuraReader::open_path("ticks.aura1")?;
reader.replay_fixed_batches(8192, |batch| {
    println!("rows={} width={}", batch.row_count(), batch.record_width());
    Ok(())
})?;
# Ok::<(), aura_codec::AuraError>(())
```

Batch callback replay is not the same work as `replay_i64`: it avoids a
callback per row and lets callers pull selected fields from the fixed-width
batch view.

Aura1 grouped replay is available for consecutive runs:

```rust
let stats = reader.grouped_replay(
    &aura_codec::GroupBy::fields(["ts_event"]),
    |group| {
        println!("run rows={}", group.row_count());
        Ok(())
    },
)?;
# Ok::<(), aura_codec::AuraError>(())
```

Grouped replay is opt-in and does not change row semantics. It is most useful
for repeated timestamp, symbol, or event-type bursts; high-cardinality data
falls back to one callback per row. Aura1 grouped replay is implemented with
fixed-width key-byte comparison and materializes group key values only at group
boundaries.

## Conversion

Use `convert_aura(input, output, ConvertOptions::new(target_format))`.

Supported conversions:

- `.aura` to `.aura0`
- `.aura` to `.aura1`
- `.aura0` to `.aura1`
- `.aura1` to `.aura0`
- conversion back to sealed `.aura`

`ConvertOptions::verify(true)` decodes the output and checks row equality against the input.

## Generic-Schema Guarantees

The SDK tests cover:

- market-data-like schema names
- reordered fields
- narrow schemas
- wide schemas
- non-grimoire field names
- unsigned and signed width validation
- Aura0 compact roundtrip
- Aura1 roundtrip
- Aura0 to Aura1 conversion
- Aura1 to Aura0 conversion
- column batches
- batch iteration
- columnar read batches
- grouped Aura1 replay
- streaming reader stats
- schema name and schema hash preservation
- public compiled plan inspection

The SDK surface does not assume a fixed row width, a fixed field count, grimoire field names, or grimoire field order. Layout still compiles through the existing generic i64 engine, so unsupported physical types reject explicitly.

One current format constraint remains: the legacy compact timestamp-role shortcut only applies when a nanosecond timestamp is field 0. Reordered nanosecond timestamp fields still roundtrip as `TimestampNanos`, but the SDK stores them as normal fixed-width timestamp values instead of using that first-field shortcut.

## Defaults

- Aura0 writer profile: compact
- Aura1 writer behavior: fixed-width compiled profile
- Batch API: prefer `AuraColumnBatch` for larger writes; `AuraRecordBatch` remains available for simple examples
- Reader mode: schema-first reader with batch iteration
- File reader mode: use `open_path`/`open_file` for range-read Aura1 replay
- Fast Aura1 replay mode: use `replay_fixed_batches` when batch callbacks fit
- Conversion: explicit target format through `ConvertOptions`
- Guard mode: off unless using lower-level strict verification tools
- Canonical hash mode: off by default at the SDK facade
- Unsupported types: reject clearly before writing
