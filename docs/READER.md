# Aura Reader API

Use `AuraReader::open(input)` to read any supported sealed Aura profile from
an in-memory or generic `Read` source. Use `AuraReader::open_path(path)` or
`AuraReader::open_file(file)` for file-backed Aura1 replay.

```rust
let mut reader = aura_codec::AuraReader::open(input)?;
let schema = reader.schema();
while let Some(batch) = reader.next_batch(1024)? {
    println!("rows={}", batch.row_count());
}
# Ok::<(), aura_codec::AuraError>(())
```

Available reader methods:

- `schema()`
- `format()`
- `compiled_plan()`
- `read_batches()`
- `next_batch(batch_size)`
- `next_fixed_batch(batch_size)`
- `next_column_batch(batch_size)`
- `batches(batch_size)`
- `replay_i64(visitor)`
- `replay_fixed_batches(batch_size, visitor)`
- `replay_row_views(visitor)`
- `grouped_replay(group_by, visitor)`

Aura1 reads stream fixed-width rows directly from the Aura1 body. Aura0 compact opens by parsing metadata only, then lazily builds bounded row batches from compact stream columns. `read_batches()` is a convenience collector over `next_batch`.

For Aura1 files on disk, `open_path` and `open_file` use the existing
header/trailer/footer metadata to avoid copying the whole file at open. The
reader reads the header prefix, footer trailer, and footer, builds
`CompiledAuraPlan`, then range-reads only the Aura1 body rows requested by
`next_batch`, `next_column_batch`, or `replay_i64`.

```rust
let reader = aura_codec::AuraReader::open_path("ticks.aura1")?;
assert_eq!("file_range", reader.stats().source_kind.as_str());
reader.replay_i64(|row| {
    let _ = row;
    Ok(())
})?;
# Ok::<(), aura_codec::AuraError>(())
```

The file-backed backend is range-read based. Mmap is intentionally not part of
the v1 SDK backend; it can be added later behind an explicit source mode if a
benchmark shows it beats bounded range reads on target platforms.

For the fastest fixed-width Aura1 scan shape, use batch-callback replay and
access only the fields needed by the caller:

```rust
reader.replay_fixed_batches(8192, |batch| {
    for row in 0..batch.row_count() {
        let ts = batch.value_i64(row, 0)?;
        let _ = ts;
    }
    Ok(())
})?;
# Ok::<(), aura_codec::AuraError>(())
```

This API invokes one callback per fixed-width batch, not one callback per row.
It is intentionally lower-level than `replay_i64` and is best for callers that
can process row ranges or pull only selected fields.

Do not treat a view-only batch callback as parse throughput. The true parse
benchmark is the path that calls `value_i64`, `field_i64`, `checksum_field`, or
`checksum_all_fields` for the selected fields. View construction is measured
separately in `aura_sdk_bench`.

For ergonomic per-row borrowed access without the temporary full-row i64
buffer used by `replay_i64`, use row views:

```rust
reader.replay_row_views(|row| {
    let ts = row.get_i64(0)?;
    let _ = ts;
    Ok(())
})?;
# Ok::<(), aura_codec::AuraError>(())
```

`replay_row_views` still invokes one callback per row. It is useful for
selected-field per-row access; `replay_fixed_batches` is the faster API when
callers can process a batch at a time.

For Aura1 parse speed, prefer `next_column_batch` when callers want typed
columns rather than row-oriented `AuraValue` batches. It builds
`AuraColumnBatch` directly from fixed-width row scans and avoids the
intermediate `Vec<Vec<i64>>` plus per-cell `AuraValue` path used by
`next_batch`.

For generic event-driven code, use `AuraEventSource` instead of choosing a
reader backend directly. `AuraMemorySource` and `AuraFileSource` wrap sealed
Aura1 bytes/files; `AuraLiveSource<R>` wraps a live stream of Aura1 body
records. All three expose the same schema, compiled plan, and borrowed
`AuraEventBatch` batches:

```rust
fn consume<S>(source: &mut S) -> aura_codec::Result<usize>
where
    S: aura_codec::AuraEventSource,
    for<'a> S::Batch<'a>: aura_codec::AuraEventBatch,
{
    let mut rows = 0usize;
    while let Some(batch) = source.next_batch()? {
        rows += batch.row_count();
    }
    Ok(rows)
}
# Ok::<(), aura_codec::AuraError>(())
```

The event source API is Aura1-only in this version. `.aura` and `.aura0`
sources must be converted or expanded to Aura1 before they can participate in
the shared historical/live event loop.

Grouped replay is an opt-in consecutive-run API:

```rust
let stats = reader.grouped_replay(
    &aura_codec::GroupBy::fields(["ts_event", "symbol_id"]),
    |group| {
        println!("start={} rows={}", group.row_start(), group.row_count());
        Ok(())
    },
)?;
# Ok::<(), aura_codec::AuraError>(())
```

Grouping is schema-driven. Field names or field IDs are resolved once before
replay; row order is preserved; high-cardinality inputs degrade to one-row
groups. Aura1 grouping compares fixed-width key bytes in the hot loop and
materializes typed group key values only when a group is emitted.

`ReaderOptions::default()` uses byte-lane selection `Auto`. `Aura0ByteLaneUse::Always` rejects compact Aura0 files that do not contain a byte lane.

Use `reader.stats()` in tests or diagnostics to inspect
`source_kind`, `replay_backend`, `bytes_read_at_open`,
`body_bytes_read_at_open`, `full_file_bytes_copied`,
`open_decoded_row_count`, `full_file_materialized`,
`rows_decoded_in_last_batch`, and `max_rows_materialized_at_once`.

## Flat Aura0 V3 reader

`V3FlatAura0Reader<R: Read + Seek>::open` reads only the bounded EOF trailer,
footer, and V3 header from one held stream. Open validates metadata and ranges,
but its state is `Opened`, not a complete integrity claim. Call `verify_all`
to stream through one chunk at a time and validate stored/chunk/global/body
hashes, exact values, bounds, stats, and the final stream length. Only success
changes the observable state to `Verified`.

`verify_with` first completes verification without invoking user code, then
performs a second checked callback pass. Callback effects are provisional until
the method returns success: a final envelope check and full-body rehash follow
the callbacks. `read_all` is the explicit collecting convenience.

Generic `Read + Seek` cannot lock an arbitrary producer, so callers must supply
an immutable snapshot for the full operation. The reader performs envelope
checks around a second full-body hash to detect mutations observed during its
passes, but does not claim categorical concurrent-mutation exclusion. Default
and caller-lowered `V3FlatLimits` apply before count-controlled allocation.
