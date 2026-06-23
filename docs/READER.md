# Aura Reader API

Use `AuraReader::open(input)` to read any supported sealed Aura profile.

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
- `next_column_batch(batch_size)`
- `batches(batch_size)`
- `replay_i64(visitor)`
- `grouped_replay(group_by, visitor)`

Aura1 reads stream fixed-width rows directly from the Aura1 body. Aura0 compact opens by parsing metadata only, then lazily builds bounded row batches from compact stream columns. `read_batches()` is a convenience collector over `next_batch`.

For Aura1 parse speed, prefer `next_column_batch` when callers want typed
columns rather than row-oriented `AuraValue` batches. It builds
`AuraColumnBatch` directly from fixed-width row scans and avoids the
intermediate `Vec<Vec<i64>>` plus per-cell `AuraValue` path used by
`next_batch`.

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
groups.

`ReaderOptions::default()` uses byte-lane selection `Auto`. `Aura0ByteLaneUse::Always` rejects compact Aura0 files that do not contain a byte lane.

Use `reader.stats()` in tests or diagnostics to inspect `open_decoded_row_count`, `full_file_materialized`, `rows_decoded_in_last_batch`, and `max_rows_materialized_at_once`.
