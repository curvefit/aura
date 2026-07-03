# Aura Schema API

`AuraSchema` is the SDK schema object. Build one at runtime:

```rust
let schema = aura_codec::AuraSchema::builder()
    .field("ts_event", aura_codec::AuraType::TimestampNanos)
    .field("symbol_id", aura_codec::AuraType::U32)
    .field("price", aura_codec::AuraType::PriceI64Scaled { scale: 9 })
    .build()?;
# Ok::<(), aura_codec::AuraError>(())
```

Supported v1 types are fixed-width scalar values backed by the generic i64 physical engine: booleans, signed/unsigned integer widths, timestamp nanos/micros, scaled i64 values, enum u8, and flags u32.

Unsupported v1 types reject during schema build:

- nullable fields
- `Utf8`
- `Binary`
- `F32`
- `F64`

Schema field order is the canonical storage order. Schema name, schema hash, field names, logical roles, physical types, scale, and nullability flags are preserved through SDK Aura0/Aura1 writes and conversions.
