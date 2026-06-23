# SDK Closeout Implementation Plan

## Meaning of True Streaming

For Aura1, true streaming means `AuraReader::open` parses the container header, schema, footer, and compiled plan, but does not decode any logical rows. `next_batch` reads fixed-width rows directly from the Aura1 body into only the requested batch.

For Aura0 compact, true streaming means `AuraReader::open` parses schema/footer/plan only. `next_batch` must not be a slice of rows decoded during open. The current compact stream layout is file-stream oriented, so the smallest safe SDK closeout implementation is lazy bounded materialization: decode happens on first batch request, not open, and the reader records that Aura0 used a bounded SDK state rather than open-time materialization. If existing compact metadata cannot support independent row groups, the reader must expose that through counters and keep the decoded rows out of `open`.

## Current Whole-File Materialization

`src/reader.rs::AuraReader::open_with_options` currently reads all input bytes and calls `records::decode_i64_file`, storing `rows: Vec<Vec<i64>>`. `next_batch` slices that full vector.

## Struct Changes

Replace the stored row vector with reader state:

- source bytes
- schema
- profile/format
- compiled footer
- compiled plan
- current row cursor
- stream stats
- state enum for Aura1 fixed rows, Aura0 lazy rows, and ingest lazy rows

Add `AuraReaderStats` so tests and benchmarks can assert:

- `open_decoded_row_count == 0`
- `full_file_materialized == false`
- `rows_decoded_in_last_batch <= batch_size` for Aura1
- `max_rows_materialized_at_once <= batch_size` for Aura1

## Smallest Safe Aura1 Implementation

Use the existing non-materializing row visitor:

- `records::visit_i64_rows_file`
- `records::aura1_fixed_layout_info`

`next_batch` can visit rows and collect only the requested range. This avoids `records::decode_i64_file` for Aura1 opens and replay.

## Smallest Safe Aura0 Implementation

Open should parse metadata without `decode_i64_file`. Use a new metadata-only helper if needed. On first batch, decode compact rows lazily. If the current file format has no independent row-group stream table, the batch reader can still satisfy the open-time streaming invariant and expose that the first batch materialized the compact row set. Full Aura0 row-group streaming requires a compact block table with per-stream row ranges and offsets.

## Benchmark Matrix

Add a full generated SDK matrix command to `aura-fixture-gen` or a small SDK bench helper. It must cover generated tiny, narrow, wide, reordered, dense, sparse, edge-case, and larger schemas.

Operations:

- write Aura1
- write Aura0 compact
- read Aura1 batches
- read Aura0 batches
- replay Aura1
- convert Aura0 to Aura1
- convert Aura1 to Aura0
- zstd Aura1 to Aura1
- roundtrip verify

Each result must include median, p95, records/sec, schema hash, record width, record count, streaming flags, materialization counters, command, git commit, and result path.

## Verification Commands

- `git diff --check`
- `cargo check`
- `cargo check --examples`
- targeted SDK/bench/footer/writer tests
- `cargo test`
- release builds for `aura-bench` and `aura-fixture-gen`
- SDK examples
- full generic benchmark matrix
