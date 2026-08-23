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

## Flat Aura0 V3 writer

`V3FlatAura0Writer<W>` writes the frozen flat, event-only V3 Aura0 layout to a
new empty `Read + Write + Seek` stream. Each positive-row `AuraV3Batch` is fully
validated and encoded before append. A write error permanently poisons the
writer; later calls reject. Dropping the writer does not seal it.

`finish` performs a bounded second pass over the already-written exact-value
blocks after the total row count is known. It recomputes stored and logical
chunk hashes, aggregate stats, bounds, body hash, and the total-row-prefix
global logical hash, self-decodes the constructed footer, and writes the
footer, length, and `sealed:)` last. Empty writers produce the checked zero-row
file. `finish_and_sync` is the `File` specialization and syncs completed bytes;
generic `finish` proves bytes and returns the stream plus `V3FlatWriteSummary`.

The safe default limits are 256 MiB body/block, 4,194,304 rows, and 4,096
chunks. `V3FlatLimits::HARD` is an explicit larger-envelope opt-in.
