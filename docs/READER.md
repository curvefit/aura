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

Current v1 limitation: `AuraReader` decodes through the existing in-memory i64 row engine, then serves batches. This gives callers a stable streaming-shaped API, but it is not yet a zero-copy or block-streaming decoder.

`ReaderOptions::default()` uses byte-lane selection `Auto`. `Aura0ByteLaneUse::Always` rejects compact Aura0 files that do not contain a byte lane.
