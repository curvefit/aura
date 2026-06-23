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
- `batches(batch_size)`
- `replay_i64(visitor)`

Aura1 reads stream fixed-width rows directly from the Aura1 body. Aura0 compact opens by parsing metadata only, then lazily builds bounded row batches from compact stream columns. `read_batches()` is a convenience collector over `next_batch`.

`ReaderOptions::default()` uses byte-lane selection `Auto`. `Aura0ByteLaneUse::Always` rejects compact Aura0 files that do not contain a byte lane.

Use `reader.stats()` in tests or diagnostics to inspect `open_decoded_row_count`, `full_file_materialized`, `rows_decoded_in_last_batch`, and `max_rows_materialized_at_once`.
