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

## Grouped Aura0 V3 writer

`V3GroupedAura0Writer<W>` writes the complete grouped, uncompressed V3 Aura0
SDK profile to a new empty `Read + Write + Seek` stream. It accepts
`AuraV3EventBatch` values with authoritative `event_count + 1` child offsets,
event-scoped and repeated columns, nullable validity bitmaps, and the exact
one-group/two-domain schema with the non-null U8 `side` discriminator marked by
relationship byte `200`. Grouped `AURAV3EB` chunks preserve source order and
global event/child ranges; their canonical logical hash includes both ranges,
field slots/types, presence, and exact values, so the hash is stable across
rechunking. Derived expressions are not supported.

`finish` performs a bounded second pass over the emitted chunks. It validates
every block and range again, recomputes stored/logical hashes, statistics,
timestamp/sequence bounds, body hash, and the global logical hash, self-decodes
the 184-byte-prefix `AURP` footer with 152-byte chunk descriptors, then writes
the footer, u32 footer length, and `sealed:)` trailer. `finish_and_sync` is the
`File` specialization. A write, flush, sync, or second-pass error is a failed
and uncommitted result; callers must discard it and must not publish it, even if
the underlying bytes happen to end in a seal. The grouped SDK has no CLI
complete-seal command yet.

`V3GroupedAura0Reader<R>` is a bounded seekable reader. It can locate chunks by
global event or child, read one checked chunk, and run `verify_all`/`verify_with`
for complete body, range, statistics, hash, and envelope verification. Opening
checks the header/footer envelope; body-dependent claims remain provisional
until verification succeeds. `V3GroupedLimits::default()` uses 256 MiB body,
4,096 chunks, 1,048,576 events, and 4,194,304 children. The hard envelope is
64 MiB footer, 1 TiB body, 65,536 chunks, 16,777,216 events, 67,108,864
children, and 16 MiB schema. Per event-block defaults are 256 MiB,
1,048,576 events, 4,194,304 children, and 16,777,216 values; hard limits are
1 GiB, 4,194,304 events, 16,777,216 children, and 67,108,864 values.
`V3GroupedLimits::HARD` is an explicit opt-in and all supplied limits are
clamped to those ceilings. The developer CLI exposes this writer through
`aura v3 aura0 seal --protocol aura-logical-arrow-ipc-v2`; it decodes the
strict nested Arrow stream, seals empty input as an empty complete file, and
uses the same create-once held-file publication state machine as the flat V3
command. `aura v3 aura0 verify` routes exact-flat, planned-flat, and grouped
files from their bounded footer tuple and verifies the embedded schema and
exact held file. Explicit flat `--mode planned` is a bounded all-memory
development path and may select the existing exact file; it is not a
streaming-writer claim. Its one registry-2 extension uses exact-byte,
chunk-local dictionaries with minimal bitpacked present-only indices for
Utf8/DecimalText; it performs no normalization or entropy compression.
One additional body-layout-3 candidate wraps the preselected registry1/2 block
independently per chunk with the canonical bounded zstd19 profile. This does
not make the complete planned reader streaming or production-ready. A sixth
complete candidate inherits that selected registry1/2 plan, replaces only a
strictly smaller schema-authorized primary timestamp lane with checked
per-chunk previous-delta (or explicitly authorized delta-of-delta), encodes an
`AUFPVB03` inner block, and stores it in the distinct `AUFPZB02` wrapper-v2
layout 5. The compiler retains all six complete artifacts while scoring; this
is a bounded all-memory development limitation, not a measured-size claim.
