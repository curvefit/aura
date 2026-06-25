# DBN-Parity Research for Aura

Date: 2026-06-25

This document uses DBN as a design benchmark, not as a format to copy. The
primary sources are official Databento documentation:

- Databento Binary Encoding:
  https://databento.com/docs/standards-and-conventions/databento-binary-encoding
- Zstandard:
  https://databento.com/docs/standards-and-conventions/working-with-zstandard
- `DBNStore.replay`:
  https://databento.com/docs/api-reference-historical/helpers/dbn-store-replay
- `Live.add_callback` and `Live.add_stream`:
  https://databento.com/docs/api-reference-live/client/add-callback
- MBO snapshots:
  https://databento.com/docs/standards-and-conventions/mbo-snapshot

## Required Questions

### 1. What does DBN mean by end-to-end?

DBN is one normalized binary representation for three trading-system uses:
file storage, real-time wire/messaging, and in-memory records. Its own docs
describe DBN as simultaneously a file format, real-time messaging format, and
in-memory representation. The practical claim is that a market-data payload can
move through capture, persistence, replay, research, and live consumers without
changing logical representation at every boundary.

### 2. What does DBN mean by zero-copy?

DBN uses fixed record structs and a common binary layout so clients can treat
payload bytes as records instead of decoding through an intermediate object
model. The docs contrast DBN with systems that need multiple serialize /
deserialize layers and call out zero-copy as a critical difference. The claim is
not that no client ever copies bytes; it is that the canonical representation
does not force a format conversion between disk, wire, and application memory.

### 3. What metadata does DBN store?

DBN metadata starts every stream/file and includes request reconstruction
parameters: version, metadata length, dataset, schema, start/end, limit,
input/output symbology types, `ts_out`, symbol string length, optional schema
definition bytes, requested symbols, partially resolved symbols, unresolved
symbols, and symbol mappings. Symbol mappings contain raw symbols and
time-bounded mapping intervals with start date, end date, and output symbol.
Each record also starts with a common header containing record length, record
type, publisher ID, instrument ID, and event timestamp.

### 4. How does DBN use fixed-width records?

DBN uses a fixed set of market-data message schemas. Records begin with a
16-byte `RecordHeader`, and record length is expressed in 32-bit words. That
supports sequential scans, predictable offsets, fast dispatch by record type,
and compression over uniform record streams. DBN intentionally does not expose a
general-purpose schema language for arbitrary user records.

### 5. How do DBN clients use the same code for live and historical?

Historical `DBNStore.replay` passes DBN records sequentially to a callback with a
record argument. Live `add_callback` also dispatches a single `DBNRecord` to a
callback, and `add_stream` writes binary DBN records to a byte stream/file. That
means user code can be structured around event callbacks over DBN records,
whether the source is historical storage or live transport.

### 6. What does DBN compress with zstd/lz4?

Databento recommends Zstandard for historical streaming and batch downloads to
reduce transmitted and stored bytes. The docs explicitly discuss `.zst`
decompression tooling for Databento data. LZ4 is not presented in the official
DBN docs above as the primary public download compression path; for Aura parity
the relevant design property is that fixed-width binary records should compress
well with block compression such as zstd or lz4.

### 7. What does "in-memory representation" mean in DBN?

It means the record bytes have a stable struct layout that can be consumed
directly as records in process memory. DBN's FAQ acknowledges that DBN is
"pretty much" raw structs plus tooling and conventions. The important property
for Aura is not copying DBN structs, but providing stable borrowed views over
fixed-width Aura1 rows and batches.

### 8. What does full order-book replay benchmark actually include?

A real full order-book replay benchmark must include more than raw byte scan or
view construction. For MBO, the stream contains order actions such as clear,
add, cancel, modify, and trade-style events; snapshots are streamed as clear
plus outstanding add records and preserve priority order. A full replay
benchmark should therefore separate:

1. record extraction from the binary stream,
2. decoding required order-book fields,
3. applying actions to an order-book data structure,
4. optional snapshot/reset handling and sequence/flag handling,
5. final checksum or observable book state.

DBN documentation demonstrates callback replay and order-book snapshot
semantics, but the public docs reviewed here do not define one canonical
benchmark harness. Aura should report extraction-only and extract+apply numbers
separately.

### 9. Which DBN properties should Aura copy?

Aura should copy the design principles: one canonical metadata contract, fixed
record widths for hot replay, borrowed record/batch views, common event-source
interfaces for historical and live feeds, sequential predictable access,
compression-friendly binary rows, and explicit metadata sufficient for
self-describing replay.

### 10. Which DBN properties should Aura intentionally not copy?

Aura should not copy DBN's fixed market-data schema catalog or record headers.
Aura's intended value is generic schema-driven layout: `.aura` as ingest /
preservation, `.aura1` as fixed-width replay, and `.aura0` as compact semantic
cold storage. Aura should keep `AuraSchema`, `AuraHeader`, `AuraFooter`, and
`CompiledAuraPlan` as its format drivers instead of adopting DBN record types or
symbology conventions wholesale.

## Claim-by-Claim Table

| DBN claim | What DBN does | Aura current state | Gap | Implementation target |
|---|---|---|---|---|
| End-to-end representation | One DBN stream/file layout is usable for storage, transport, and in-memory record handling. | Aura has three explicit tiers: `.aura` ingest, `.aura0` compact, `.aura1` fixed-width replay. `.aura1` is the candidate for disk/in-memory hot path. | Aura's public docs do not yet describe a coherent DBN-like end-to-end story across tiers and sources. | Document Aura's intended end-to-end path and expose a common source API over fixed-width Aura1 batches. |
| Same code historical/live | Historical replay and live callbacks both dispatch `DBNRecord`-like events. | Aura has `AuraReader` for files/memory and callback-style replay methods, but no first-class live/historical source trait. | User code cannot target one `AuraEventSource` interface. | Add `AuraEventSource`, `AuraFileSource`, `AuraMemorySource`, and `AuraLiveSource<R>`. |
| Zero-copy | Fixed structs allow clients to borrow/interpret records without conversion layers. | Aura1 has `Aura1FixedBatchView`, `Aura1RowView`, selected row views, file-range stats, and order-book delta batches. | Generic `next_batch` materializes `AuraRecordBatch`; `Read` sources copy full bytes; file-backed batches currently allocate per chunk rather than mmap. | Make borrowed Aura1 batch views part of a common event-source API; document which paths allocate. |
| Self-sufficient metadata | Metadata includes request parameters, dataset, schema, symbology request and mappings; records carry publisher/instrument/timestamp. | Aura footer stores schema, stats, compression descriptor, chunks, and compiled plans; header stores profile, stream/dictionary IDs, base time, compact schema map, derived expressions, comment. | No first-class dataset/source/venue fields, no symbol/instrument mapping table, no time-aware symbology contract. | Document this as an explicit gap; avoid inventing DBN symbology until Aura has real dataset semantics. |
| Fixed lengths and offsets | DBN records use a common header and fixed schema structs with predictable offsets. | Aura1 rows are fixed-width, schema-index ordered, little-endian, and driven by `CompiledAuraPlan`. | Aura1 is fixed-width, but not a stable transport profile in docs/API yet. | Treat Aura1 fixed batches as the hot replay/transport candidate and expose record width from source plan. |
| Compression-friendly | Fixed binary streams compress well; Databento recommends zstd for historical download/storage. | Aura produces `.aura1.zst`; `.aura0` has compact/fast/hybrid byte-lane profiles with lz4/zstd codecs. | Compression profiles are spread across benchmark docs and not framed as DBN-parity design. | Document `.aura1.zst` versus `.aura0` roles and profile tradeoffs in the parity audit. |
| Optimized for modern CPUs | Sequential layouts, predictable record lengths, and small fixed structs favor prefetch/cache behavior. | Aura1 scan/replay loops are sequential; field offsets are precomputed; batch sizes exist in SDK bench. | Cache-line guidance is partial; no explicit source API guarantee for stable batch sizing. | Put record width and batch sizing in the source docs/tests. |
| Extremely fast full order-book replay | DBN's value proposition depends on replaying MBO event streams, not just scanning bytes. | Aura has `OrderBookDeltaBatch` extraction and SDK bench operations for batch extraction and extract+book-apply. | Results must keep book-apply costs separated and avoid labeling extraction-only as full replay. | Document extraction-only versus extract+apply; source API should support borrowed delta batches later. |
