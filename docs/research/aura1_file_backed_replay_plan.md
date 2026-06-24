# Aura1 File-Backed Replay Plan

Date: 2026-06-24

## Goal

Implement DBN-like Aura1 fixed-width replay without changing the Aura1 binary
layout. The format already stores enough metadata in the existing header,
trailer, and compiled footer. The implementation gap is that the SDK reader
currently copies the entire file into memory before replay.

## Implementation Status

Implemented with a range-read file backend. `AuraReader::open_path` and
`AuraReader::open_file` parse Aura1 header/trailer/footer metadata, compile the
existing `CompiledAuraPlan`, and store a file-backed body range. They do not
read Aura1 body bytes at open and do not copy the full file into a `Vec<u8>`.

Mmap is not implemented in this pass. The accepted v1 backend is explicit
file-range replay because it satisfies the no-full-copy requirement without
adding unsafe mapping behavior or a platform-specific dependency.

## Current Input Path

`AuraReader::open` accepts `Read` and delegates to `open_with_options`.
`open_with_options` calls `read_to_end` into `Vec<u8>`, then calls
`records::decode_i64_file_metadata`. This means an Aura1 file body is copied
into RAM at open, even though the open step only needs the header and footer.

`replay_i64` for Aura1 currently calls `records::visit_i64_rows_file(&self.bytes,
visitor)`. That helper parses the front header, discovers the footer from EOF,
decodes the footer, builds `CompiledAuraPlan`, slices the in-memory body, and
visits fixed-width rows.

`next_batch` and `next_column_batch` also use in-memory byte slices through
`visit_i64_rows_file_range`.

## Existing Metadata To Use

No new layout metadata is required.

- Header: magic, version, profile, `header_len`; body starts at `header_len`.
- Trailer: `u32` footer length plus seal magic; footer offset is computed from
  EOF.
- Compiled footer (`AURP`): record count, schema, Aura1 block capacity, Aura1
  decode program.
- `CompiledAuraPlan::from_footer`: derives field count, Aura1 record width,
  Aura1 body size, field offsets, and conversion plan hash.
- `records::aura1_fixed_layout_info`: already exposes record width, body
  offset, body bytes, footer offset, record count, and conversion plan hash for
  in-memory files.

## API Shape

Keep existing in-memory APIs:

```rust
AuraReader::open(reader)
AuraReader::open_with_options(reader, options)
```

Add file-backed APIs:

```rust
AuraReader::open_path(path)
AuraReader::open_path_with_options(path, options)
AuraReader::open_file(file)
AuraReader::open_file_with_options(file, options)
```

The file APIs require `Read + Seek` internally. Path APIs open a `File`.
Mmap is not required for the first passing implementation; file-range reads are
enough to avoid the full-file `Vec<u8>` copy.

## Input Backend

Use an internal source enum:

```rust
enum AuraReaderSource {
    Memory(Vec<u8>),
    FileRange(FileBackedAuraInput),
}
```

`FileBackedAuraInput` stores:

- `File`
- `file_len`
- `body_offset`
- `footer_offset`
- `body_bytes`
- `record_width`
- `record_count`
- source counters

The source must expose range reads for Aura1 body row ranges:

```rust
read_body_range(row_start, row_count) -> Vec<u8>
```

This buffers only the requested batch/range, not the full file.

## File Metadata Open

`open_file_with_options` should:

1. seek to current file length,
2. read the fixed header prefix needed for `AuraHeader::encoded_len`,
3. read exactly `header_len` and decode the header,
4. seek to EOF trailer,
5. validate seal magic and read footer length,
6. read exactly footer bytes,
7. decode the footer,
8. validate header/schema agreement through existing helpers where possible,
9. build `CompiledAuraPlan`,
10. initialize `AuraReader` without reading the Aura1 body.

Aura0 and ingest can fall back to the current memory path unless later lanes
choose to add file-backed support. This sprint is about Aura1 fixed replay.

## Stats To Prove Behavior

Extend `AuraReaderStats` with:

- `source_kind`
- `replay_backend`
- `file_len`
- `bytes_read_at_open`
- `body_bytes_read_at_open`
- `footer_bytes_read_at_open`
- `bytes_read_during_replay`
- `bytes_read_total`
- `bytes_read_in_last_batch`
- `full_file_bytes_copied`
- `row_width_from_plan`
- `body_offset_from_header`
- `footer_offset_from_trailer`
- `record_count_from_footer`

Passing tests must prove:

- Aura1 file open reads header + footer/trailer only.
- Aura1 file open reads zero body bytes.
- Aura1 file open does not copy the full file.
- File-backed batch reads only the requested row range.
- File-backed replay scans the body range but does not allocate full row sets.

## Replay Implementation

For file-backed Aura1:

- `replay_i64` reads body ranges sequentially and calls the same fixed-row body
  visitor used by the in-memory path.
- Use `CompiledAuraPlan` already stored in the reader rather than reparsing the
  footer in every replay helper call.
- The first implementation may read chunks of rows into a scratch `Vec<u8>`.
  It must cap scratch size and count bytes read during replay.
- `next_batch` and `next_column_batch` read exactly the requested row byte
  range, then materialize only that batch.

## Benchmarks

Add source-mode-aware Aura1 benchmark operations to `aura_sdk_bench`:

- `aura1-replay-i64`
- `aura1-replay-file-range`
- `aura1-read-batches-row`
- `aura1-read-batches-row-file-range`
- `aura1-read-batches-columnar`
- `aura1-read-batches-columnar-file-range`
- `aura1-scan-raw`
- `aura1-scan-raw-file-range`
- `aura1-grouped-replay-primary`
- `aura1-grouped-replay-primary-file-range`
- `aura1-grouped-replay-symbol`
- `aura1-grouped-replay-symbol-file-range`
- `aura1-grouped-replay-pair`
- `aura1-grouped-replay-pair-file-range`

Every JSON should report source kind, replay backend, bytes read at open, total
bytes read, full-file bytes copied, row materialization counters, record width,
record count, median, p95, and records/sec.

## Commit Plan

1. Commit this plan.
2. Commit reader input audit/instrumentation if separate.
3. Commit file-backed Aura1 input backend and tests.
4. Commit file-backed benchmark matrix.
5. Commit docs/defaults.

## Acceptance Criteria

The sprint passes only if `AuraReader::open_path`/`open_file` for Aura1 avoid
the full-file byte copy, file-backed replay and batches match memory-backed
results, benchmarks compare both modes, all tests/builds/examples pass, and the
worktree is clean.
