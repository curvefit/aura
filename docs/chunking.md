# Chunked cold storage

Cold files should use independently encoded and compressed chunks rather than a
single whole-file compression stream.

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
worker 0: decode chunk 0 -> hot chunk 0
worker 1: decode chunk 1 -> hot chunk 1
worker 2: decode chunk 2 -> hot chunk 2
```

Chunk-local bases are important. Cold records should be encoded so a worker can
materialize a chunk without decoding the previous chunk. Avoid quantity deltas
that depend on prior book state unless the chunk also carries the required
checkpoint state.

Recommended starting point:

```text
uncompressed chunk target: 16-64 MiB
initial default: 32 MiB
cold compression: high zstd level
hot compression: low zstd level or none
```
