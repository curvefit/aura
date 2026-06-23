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
- `AuraRecordBatch`, `AuraValue`
- `AuraWriter`, `AuraReader`
- `WriterOptions`, `ReaderOptions`, `ConvertOptions`
- `AuraFormat`, `AuraProfile`
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

## Reading

Use `AuraReader::open(input)` to detect the sealed profile, recover the schema, and read rows:

```rust
let reader = aura_codec::AuraReader::open(std::io::Cursor::new(bytes))?;
let schema = reader.schema();
let batches = reader.read_batches()?;
# Ok::<(), aura_codec::AuraError>(())
```

`AuraReader` currently returns one in-memory `AuraRecordBatch`. Streaming batch iteration is a remaining SDK improvement.

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

The SDK surface does not assume a fixed row width, a fixed field count, grimoire field names, or grimoire field order. Layout still compiles through the existing generic i64 engine, so unsupported physical types reject explicitly.

One current format constraint remains: the legacy compact timestamp-role shortcut only applies when a nanosecond timestamp is field 0. Reordered nanosecond timestamp fields still roundtrip as `TimestampNanos`, but the SDK stores them as normal fixed-width timestamp values instead of using that first-field shortcut.
