# Aura Writer API

Use `AuraWriter::try_new(output, schema, options)` to write Aura files from dynamic schemas.

```rust
let mut writer = aura_codec::AuraWriter::try_new(
    output,
    schema.clone(),
    aura_codec::WriterOptions::aura0_compact(),
)?;
writer.write_batch(batch)?;
writer.finish()?;
# Ok::<(), aura_codec::AuraError>(())
```

`write_batch` accepts:

- `AuraRecordBatch` for row-oriented writes
- `AuraColumnBatch` for column-oriented writes

Use `AuraColumnBatch` for larger batches:

```rust
let batch = aura_codec::AuraColumnBatch::builder(schema.clone())
    .i64("ts_event", ts)
    .u32("symbol_id", symbols)
    .i64("price", prices)
    .build()?;
writer.write_batch(batch)?;
# Ok::<(), aura_codec::AuraError>(())
```

Defaults:

- `WriterOptions::default()` is compact Aura0
- `WriterOptions::aura1()` writes Aura1
- `WriterOptions::new(AuraFormat::Aura)` writes sealed ingest
- Aura0 fast/hybrid profiles are explicit opt-ins

The writer exposes `compiled_plan()` for schema-derived layout inspection before bytes are finished.
