# Chunked storage

`.aura0` files should use independently encoded and compressed chunks rather
than a single whole-file compression stream. `.aura1` files may use the same
chunk table when the replay profile is compressed, but uncompressed `.aura1`
remains valid when maximum replay speed matters more than disk.

A chunk directory stores:

```text
chunk_id
first_event_index
event_count
compressed_offset
compressed_len
uncompressed_len
first_ts_event
last_ts_event
first_sequence
last_sequence
checksum
```

This enables parallel conversion:

```text
worker 0: decode chunk 0 -> aura1 chunk 0
worker 1: decode chunk 1 -> aura1 chunk 1
worker 2: decode chunk 2 -> aura1 chunk 2
```

Chunk-local bases are important. Aura0 records should be encoded so a worker can
materialize a chunk without decoding the previous chunk. Avoid quantity deltas
that depend on prior book state unless the chunk also carries the required
checkpoint state.

Recommended starting point:

```text
uncompressed chunk target: 16-64 MiB
initial default: 32 MiB
Aura0 compression: high zstd level
Aura1 compression: low zstd level or none
```
