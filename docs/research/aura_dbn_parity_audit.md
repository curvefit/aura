# Aura DBN-Parity Audit

Date: 2026-06-25

Scope inspected:

- `src/lib.rs`
- `src/schema.rs`
- `src/types.rs`
- `src/options.rs`
- `src/writer.rs`
- `src/reader.rs`
- `src/convert.rs`
- `src/program.rs`
- `src/records.rs`
- `src/bin/aura_sdk_bench.rs`
- `docs/FORMAT.md`
- `docs/FOOTER.md`
- `docs/SDK.md`
- `docs/READER.md`
- `docs/WRITER.md`
- `docs/CONVERSION.md`

## Audit Summary

Aura already has most of the fixed-width replay substrate needed for a DBN-like
hot path:

- `.aura`, `.aura0`, and `.aura1` roles are documented as ingest, compact cold,
  and fixed-width replay profiles.
- `.aura1` rows are fixed-width, schema/index ordered, little-endian, and driven
  by `CompiledAuraPlan`.
- `AuraReader::open_path` / `open_file` avoid full-file copying for Aura1 and
  range-read body batches.
- Borrowed Aura1 replay exists through `Aura1FixedBatchView`, `Aura1RowView`,
  selected row views, and `OrderBookDeltaBatch`.
- The SDK benchmark includes both order-book delta extraction and
  extract+book-apply operations.

The DBN-parity gaps were concentrated in two places at audit time:

1. Aura does not yet expose a common historical/live event-source trait.
2. Aura metadata is schema/plan self-describing, but not market-data
   self-sufficient in the DBN sense: no dataset/source/venue fields, no
   symbol/instrument mapping table, and no time-aware symbology contract.

Phase 3 implemented the first gap for the Aura1 hot path:
`AuraEventSource`, `AuraEventBatch`, `AuraMemorySource`, `AuraFileSource`, and
`AuraLiveSource<R>` now let one event loop consume historical memory bytes,
historical files, and live fixed-record streams. The second gap remains explicit
and intentionally unimplemented.

## Area Table

| Area | Current Aura status | Evidence | Missing piece | Priority |
|---|---|---|---|---|
| A. End-to-end representation | Partial but coherent across tiers. `.aura` preserves ingest facts, `.aura0` stores compact/semantic cold data, `.aura1` stores fixed-width replay data. Phase 3 packages `.aura1` disk/memory/live consumption behind one source interface. | `docs/FORMAT.md:7` defines `.aura`; `docs/FORMAT.md:9` defines `.aura0`; `docs/FORMAT.md:11` defines `.aura1`; `src/source.rs` defines `AuraEventSource`; `tests/event_source.rs` covers memory/file/live consumption. | Remaining gap is market-data metadata, not the event-source shape. | Medium |
| A. Borrow/mmap Aura1 bytes | Borrowed memory-backed views exist; file-backed Aura1 avoids full-file copies but range-reads into bounded `Vec<u8>` chunks rather than mmap. | `src/reader.rs:1525` exposes `replay_fixed_batches`; `src/reader.rs:1541` borrows memory body slices; `src/reader.rs:1575` uses file-range chunks; `docs/READER.md:48` says mmap is intentionally absent in v1. | No mmap-backed API; file-backed batch views borrow chunk buffers, not the file mapping. | Medium |
| A. Stable transport layout | Aura1 has stable fixed-width rows for current SDK types, and Phase 3 names Aura1 fixed batches as the event-source unit. | `docs/FORMAT.md` documents the end-to-end source model; `src/source.rs` defines `AuraLiveSource<R>` over Aura1 body records; `src/program.rs:500` computes record width from the Aura1 plan. | Transport framing, metadata exchange, and symbology are still outside v1. | Medium |
| A. Borrowed views exposed | Yes for Aura1 replay; not for generic `next_batch`. | `docs/SDK.md:149` documents low-level fixed replay; `docs/SDK.md:160` distinguishes batch callback replay from `replay_i64`; `src/reader.rs:1595` exposes `replay_orderbook_deltas`. | `next_batch` still returns materialized `AuraRecordBatch`; common source API should default to borrowed fixed batches where possible. | High |
| B. Same code historical/live | Implemented for Aura1 fixed batches. One generic loop can consume memory, file, and live sources. | `src/source.rs` defines `AuraEventSource`; `tests/event_source.rs` uses one `consume_source<S>` for `AuraMemorySource`, `AuraFileSource`, and `AuraLiveSource<R>`. | `.aura`/`.aura0` do not directly implement the source trait; they must be converted/expanded to Aura1 first. | Low |
| B. Same event type | Implemented for fixed-width batches through `AuraEventBatch`; historical and live sources both yield `Aura1FixedBatchView` semantics. | `src/source.rs` implements `AuraEventBatch` for `Aura1FixedBatchView`; `tests/event_source.rs` compares historical and live checksums. | Order-book-specific borrowed batches are not yet an event-source batch specialization. | Medium |
| C. Zero-copy | Partial. Memory-backed Aura1 source borrows body slices without per-record allocation. File-backed source avoids whole-file copy but copies each requested range into a temporary buffer. Generic `next_batch` materializes values. | `src/reader.rs` exposes `next_fixed_batch`; `src/source.rs` uses it for memory/file sources; `src/reader.rs:1428` materializes `AuraRecordBatch`; `src/reader.rs:1366` records memory open as full-file copied. | Mmap-backed file batches and non-Aura1 zero-copy sources remain unimplemented. | Medium |
| C. `AuraValue` materialization | Present in row batches; avoidable with column batches and fixed views. | `src/reader.rs:1450` constructs `AuraRecordBatch::from_i64_decoded`; `docs/READER.md:91` says `next_column_batch` avoids intermediate row/value materialization. | Source API should not force `AuraValue`; retain materialized APIs as compatibility. | High |
| D. Metadata/schema | Strong for schema/plan replay; weak for market-data self-sufficiency. | `src/header.rs:141` stores profile, stream/dictionary IDs, base time, schema map, derived expressions, comment; `src/footer.rs:56` stores schema, stats, compression, plans, chunks; `src/program.rs:471` stores record width/body size/field order/hash in `CompiledAuraPlan`. | Add or explicitly defer dataset/source/venue metadata and symbology mapping tables. | Medium |
| D. Symbology | Missing as a first-class feature. `stream_id` and `dictionary_id` exist, but there is no instrument mapping table or time-aware symbol mapping. | `src/header.rs:143` and `src/options.rs:62` expose stream/dictionary IDs only; no symbology types or mapping structs are exported in `src/lib.rs`. | Document as explicit gap; do not fake DBN-style symbology until Aura has real dataset semantics. | Medium |
| E. Compression | Strong as profile options, but documentation frames it as benchmark/profile guidance rather than DBN parity. | `docs/FORMAT.md:82` documents compact/fast/hybrid; `docs/FORMAT.md:92` documents lz4/zstd/raw byte lanes; `src/options.rs:30` exposes `AuraProfile`; `src/options.rs:71` defaults byte-lane codec to lz4. | Keep profile docs honest: compact is semantic cold, fast/hybrid are byte expansion profiles, `.aura1.zst` remains a storage baseline. | Low |
| E. Aura0 compact vs Aura1.zst | Documented as not universally beating `.aura1.zst`. | `docs/FORMAT.md:169` documents compact semantic Aura0 losing to `.aura1.zst` on tested grimoire artifacts; `docs/FORMAT.md:183` documents fast/hybrid byte-lane speed. | No implementation gap for this task; maintain honest benchmark labels. | Low |
| F. CPU/cache | Good basic substrate. Record widths and field offsets are precomputed; hot loops are sequential; batch size is caller-controlled. | `src/program.rs:500` computes Aura1 record width; `src/program.rs:588` exposes field offsets; `src/reader.rs:1546` and `src/reader.rs:1622` scan sequential batch ranges. | No formal cache-line policy or batch-size heuristic; source API should expose plan/record width to callers. | Medium |
| G. Full order-book replay | Partial but materially better than raw scan. There are separate extraction and extract+book-apply benchmark paths and tests verify zero row materialization. | `src/bin/aura_sdk_bench.rs:1604` extracts/checksums order-book delta fields; `src/bin/aura_sdk_bench.rs:1664` decodes and applies to `BenchOrderBook`; `tests/sdk_api.rs:696` tests extraction without rows; `tests/sdk_api.rs:739` asserts no full-file copy. | Book implementation is a benchmark-level price-level hash table, not a production MBO book with priority queues, full action semantics, or snapshot/session validation. | Medium |

## Detailed Findings

### A. End-to-end representation

Can the same Aura1 bytes be used on disk and in memory?

Yes for the hot replay layer. The file format writes fixed-width Aura1 rows and
the memory-backed reader borrows the Aura1 body directly when producing fixed
batch views. The file-backed reader uses the same layout but range-reads body
chunks from disk.

Can a reader borrow/mmap Aura1 records without converting them?

Memory-backed Aura1 replay borrows record bytes. File-backed Aura1 replay avoids
copying the full file, but it allocates a temporary range buffer per chunk.
Mmap is explicitly deferred in `docs/READER.md`.

Is Aura1 record layout stable enough for transport?

The layout is stable enough for the current SDK fixed-width scalar set:
schema-index order, little-endian values, widths from the Aura1 decode program,
and `CompiledAuraPlan` validation. The missing piece is a transport/source API
that names this contract.

Does the SDK expose borrowed views over records/batches?

Yes: `Aura1FixedBatchView`, `Aura1RowView`, `Aura1SelectedRowView`, and
`OrderBookDeltaBatch`. Generic row/column batch APIs remain materializing.

### B. Same code historical/live

Aura now has a common source trait for the Aura1 hot path. Historical
file/memory sources and live fixed-record streams implement `AuraEventSource`,
and tests prove the same generic event loop can consume all three. This does
not make `.aura` or `.aura0` live sources; they remain ingest/cold formats that
must be converted or expanded to Aura1 for shared replay.

### C. Zero-copy

Truly borrowed paths:

- Memory-backed Aura1 fixed batch replay.
- Memory-backed Aura1 row/selected row views.
- Memory-backed order-book delta batches.

Bounded-copy paths:

- File-backed Aura1 range reads allocate a chunk buffer per requested range.

Materializing paths:

- `open(Read)` copies the full input into memory.
- `next_batch` materializes `Vec<Vec<i64>>` and `AuraValue` rows.
- `convert_aura` decodes full i64 rows before writing output in several paths.

### D. Metadata/symbology

Aura is self-describing for generic replay: header plus footer recover profile,
schema, statistics, compression, chunks, and compiled plans. Aura is not yet
self-sufficient for market-data symbology: no dataset/source/venue metadata, no
instrument table, and no time-aware symbol mapping. This should remain explicit
rather than approximated with comments.

### E. Compression

Aura supports compact semantic Aura0 and fast/hybrid byte-lane profiles with
raw/lz4/zstd codecs. The current docs correctly state that compact Aura0 is a
cold semantic format and that `.aura1.zst` can beat compact Aura0 for
decode-to-Aura1 bytes on tested artifacts. This is an acceptable intentional
difference from DBN.

### F. CPU/cache

Aura1 is sequential and predictable. `CompiledAuraPlan` exposes record width,
body size, field count, and field offsets. Source consumers can now inspect the
plan through `AuraEventSource::compiled_plan`. The remaining ergonomic gap is
higher-level domain batches: order-book-specific batches are still reached
through reader replay APIs rather than the source trait.

### G. Full order-book replay

Aura has a real extract+apply benchmark path, not just a raw scan. The
benchmark reads required fields from `OrderBookDeltaBatch` and applies them to
a mutable book-like hash table. This is still not a production full MBO book:
priority, order-id life cycle, snapshot clearing, and action semantics are only
represented enough to separate reader cost from book-apply cost.
