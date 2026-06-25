# Aura DBN-Parity Status

Date: 2026-06-25

Status: **partial with explicit gaps**.

Aura now has a coherent DBN-like hot replay story for `.aura1`: the same
fixed-width Aura1 body layout is consumed from file range reads, memory-backed
bytes, read-stream live input, and fixed-width live-frame chunks through one
`AuraEventSource` API. Aura intentionally does not copy DBN's single-format
design: `.aura` remains ingest/preservation, `.aura0` remains compact semantic
cold storage, and `.aura1` is the fixed-width replay/transport candidate.

## Claim Table

| DBN claim | Aura status | Implementation evidence | Benchmark evidence | Remaining gap | Next patch |
|---|---|---|---|---|---|
| end-to-end | Partial. Aura has an end-to-end Aura1 hot path, but not one binary format for all roles. | `AuraEventSource`, `AuraFileSource`, `AuraMemorySource`, `AuraLiveSource`, `AuraLiveFrameSource`; `CompiledAuraPlan` drives record width and field offsets. | Event-source matrix: file/memory/live/live-frame all replay same checksums with `rows_materialized = 0`. | `.aura` and `.aura0` are not direct live/event-source formats. | Add conversion/source adapters only if cold/ingest replay needs the same callback surface. |
| same code historical/live | Implemented for Aura1 fixed-width batches. | One generic trait yields `AuraEventBatch`; tests use one generic `consume_source<S>` for file, memory, live stream, and live frame sources. | `live_read` passes the file-relative target in the final event-source matrix; `live_frame` passes dense/larger but misses sparse. | Domain-specialized batches such as order-book deltas still live on reader APIs, not as source batch associated types; live-frame needs more stable sparse-case benchmarking before becoming the preferred live transport. | Add optional typed event-source adapters for `OrderBookDeltaBatch`; retest live-frame with real transport buffers. |
| zero-copy | Partial but tested. Aura1 hot replay avoids row vectors and `AuraValue`; memory/file/live-frame yield borrowed fixed-batch views. | `Aura1FixedBatchView`, `OrderBookDeltaBatch`, event-source stats, tests `event_source_*_zero_copy_stats`, `orderbook_delta_batch_no_row_materialization`. | JSON reports `rows_materialized = 0`; file/live-frame report `bytes_copied = 0`; memory source reports full sealed-byte ownership copy. | File source range-reads into bounded buffers, not mmap; memory source currently owns sealed bytes. | Add borrowed/mmap file source and borrowed sealed-byte memory source if API ownership requires it. |
| symbology metadata | Minimal v1 implemented. | `AuraMetadata`, `SymbolMap`, `WriterOptions::metadata`, `AuraReader::metadata`, header-comment encoding. | Metadata tests cover Aura1/Aura0 roundtrip, symbol map roundtrip, metadata-before-replay, and zero hot-loop symbol lookup. | Static symbol map only; no time-ranged symbology or dataset schema registry. | Add versioned/time-aware symbology blocks when market-data datasets require it. |
| fixed widths/offsets | Implemented for Aura1. | `CompiledAuraPlan` exposes record count, Aura1 record width, body size, and field offsets; Aura1 body is schema-order fixed-width little-endian rows. | Aura1 extraction replay reaches 62.94M to 129.76M records/sec on SDK fixtures. | Some compatibility APIs still materialize rows/values. | Keep materialized APIs, but steer hot paths to fixed views. |
| compression-friendly | Implemented as profile guidance, not DBN clone. | Aura1 raw can be zstd-compressed externally; Aura0 compact/hybrid profiles exist; Aura0 byte lanes support lz4/zstd. | Storage matrix: Aura1.zstd L3 gives 2.30x to 2.93x raw compression; Aura0 compact gives 4.10x to 7.67x on SDK fixtures. | No standalone Aura1.lz4 profile; grimoire huff/nohuff unavailable in closeout run. | Add first-class Aura1.lz4 only if transport users need lower-latency compressed fixed-width chunks. |
| modern CPU/cache | Partial. Layout is sequential and offset-driven; book apply remains cache-sensitive. | Fixed rows, precomputed offsets, caller batch size, source-level compiled-plan access. | Extraction-only path is much faster than extract+apply; book variants show structure/cache dominates larger books. | No formal cache-line packing policy; dense-ladder only helps bounded price ranges. | Add layout advisor and real book data-structure work if production replay requires it. |
| full order-book replay | Partial. Benchmarks include full extract+book-apply and variant apply structures. | `aura1-replay-orderbook-deltas-apply-batch`; book variant operations; state hashes match across variants. | `sdk-larger`: current variant 10.27 ms / 3.19M msg/s; dense ladder 6.31 ms / 5.19M msg/s; specialized extract+apply 2.49 ms / 13.18M msg/s. | Benchmark book is not a production MBO book; user target of 19.1M msg/s on sdk-larger not met by generic variant harness. | Assign next patch to production-grade book data structure and action semantics. |
| normalization format | Intentional difference. Aura separates preservation, replay, and cold semantic roles. | `.aura`, `.aura1`, `.aura0` roles documented in `docs/FORMAT.md`. | Storage matrix shows why: Aura1 is fastest for extraction; Aura0 compact is smaller. | Not DBN-style single format. | Keep the three-role model unless product requirements demand a single normal form. |

## Event-Source Benchmark Evidence

Command:

```bash
target/release/aura_sdk_bench \
  --fixture-dir /tmp/aura-dbn-closeout-fixtures \
  --output-dir /tmp/aura-dbn-closeout-event-50 \
  --iterations 50 \
  --warmups 5 \
  --batch-size 8192 \
  --datasets sdk-dense,sdk-sparse,sdk-larger \
  --operations aura1-event-source-file-orderbook-apply,aura1-event-source-memory-orderbook-apply,aura1-event-source-live-orderbook-apply,aura1-event-source-live-frame-orderbook-apply,aura1-replay-orderbook-deltas-apply-batch
```

| Dataset | Source | Median ms | Records/sec | Relative to file | Checksum | Rows materialized | Bytes copied |
|---|---|---:|---:|---:|---:|---:|---:|
| sdk-dense | file | 0.527 | 7.77M | 1.000x | 2618018316009584531 | 0 | 0 |
| sdk-dense | memory | 0.461 | 8.88M | 0.875x | 2618018316009584531 | 0 | 82,398 |
| sdk-dense | live_read | 0.589 | 6.95M | 1.118x | 2618018316009584531 | 0 | 81,920 |
| sdk-dense | live_frame | 0.472 | 8.67M | 0.896x | 2618018316009584531 | 0 | 0 |
| sdk-dense | specialized | 0.245 | 16.74M | 0.465x | 2618018316009584531 | 0 | 0 |
| sdk-sparse | file | 0.148 | 13.86M | 1.000x | 12663966905939215160 | 0 | 0 |
| sdk-sparse | memory | 0.129 | 15.83M | 0.878x | 12663966905939215160 | 0 | 33,226 |
| sdk-sparse | live_read | 0.151 | 13.54M | 1.024x | 12663966905939215160 | 0 | 32,768 |
| sdk-sparse | live_frame | 0.243 | 8.42M | 1.646x | 12663966905939215160 | 0 | 0 |
| sdk-sparse | specialized | 0.126 | 16.25M | 0.851x | 12663966905939215160 | 0 | 0 |
| sdk-larger | file | 3.844 | 8.52M | 1.000x | 5381816171423939829 | 0 | 0 |
| sdk-larger | memory | 2.740 | 11.96M | 0.713x | 5381816171423939829 | 0 | 655,820 |
| sdk-larger | live_read | 3.757 | 8.72M | 0.978x | 5381816171423939829 | 0 | 655,360 |
| sdk-larger | live_frame | 3.720 | 8.81M | 0.968x | 5381816171423939829 | 0 | 0 |
| sdk-larger | specialized | 2.447 | 13.39M | 0.637x | 5381816171423939829 | 0 | 0 |

## Book-Apply Benchmark Evidence

Command:

```bash
target/release/aura_sdk_bench \
  --fixture-dir /tmp/aura-dbn-closeout-fixtures \
  --output-dir /tmp/aura-dbn-closeout-book \
  --iterations 10 \
  --warmups 2 \
  --batch-size 8192 \
  --datasets sdk-dense,sdk-sparse,sdk-larger \
  --operations aura1-book-apply-current,aura1-book-apply-packed-key,aura1-book-apply-per-instrument,aura1-book-apply-side-split,aura1-book-apply-dense-ladder,aura1-book-apply-btree
```

| Dataset | Variant | Book levels | Extract ms | Apply ms | Total ms | Records/sec | State hash |
|---|---|---:|---:|---:|---:|---:|---:|
| sdk-dense | current | 1,028 | 0.224 | 0.462 | 0.755 | 5.43M | 6405392817282514879 |
| sdk-dense | packed-key | 1,028 | 0.136 | 0.446 | 0.611 | 6.70M | 6405392817282514879 |
| sdk-dense | per-instrument | 1,028 | 0.121 | 0.548 | 0.696 | 5.88M | 6405392817282514879 |
| sdk-dense | side-split | 1,028 | 0.130 | 0.546 | 0.708 | 5.79M | 6405392817282514879 |
| sdk-dense | dense-ladder | 1,028 | 0.137 | 0.486 | 0.653 | 6.27M | 6405392817282514879 |
| sdk-dense | btree | 1,028 | 0.122 | 0.583 | 0.737 | 5.56M | 6405392817282514879 |
| sdk-larger | current | 16,448 | 2.361 | 7.488 | 10.267 | 3.19M | 16876803521481714042 |
| sdk-larger | packed-key | 16,448 | 1.896 | 8.481 | 10.787 | 3.04M | 16876803521481714042 |
| sdk-larger | per-instrument | 16,448 | 1.187 | 6.796 | 8.050 | 4.07M | 16876803521481714042 |
| sdk-larger | side-split | 16,448 | 1.059 | 6.528 | 7.651 | 4.28M | 16876803521481714042 |
| sdk-larger | dense-ladder | 16,448 | 1.082 | 5.160 | 6.314 | 5.19M | 16876803521481714042 |
| sdk-larger | btree | 16,448 | 1.086 | 7.471 | 8.624 | 3.80M | 16876803521481714042 |

All variants produced matching state hashes per dataset. The alternatives did
not beat 19.1M msg/s on `sdk-larger`; the limiting cost is the benchmark book
mutation structure/cache behavior, not Aura1 extraction. The faster existing
specialized extract+apply path reached 13.18M records/sec on `sdk-larger` in
the storage matrix, still below the target.

## Zero-Copy Proof

- `event_source_file_zero_copy_stats`: file range source reports no full-file
  materialization, no row materialization, zero source bytes copied, and ranged
  bytes read.
- `event_source_memory_zero_copy_stats`: memory source reports no row
  materialization but does report sealed-byte ownership copy.
- `event_source_live_bounded_buffer_stats`: live-frame source reports bounded
  buffer behavior, zero rows materialized, zero full-file bytes copied, and zero
  source bytes copied.
- `orderbook_delta_batch_no_row_materialization`: order-book delta replay
  touches payload fields without row or `AuraValue` materialization.
- `borrowed_batch_rejects_truncated_body`: live-frame source rejects
  non-record-aligned bodies.

## Metadata And Symbology Proof

AuraMetadata v1 stores optional `dataset`, `source`, `venue`,
`writer_version`, a static `SymbolMap`, and safe custom key/value strings. The
writer serializes it into the header comment with an `AURAMETA1|` tag. The
reader exposes it immediately after open through `AuraReader::metadata()`.

Tests cover:

- `metadata_roundtrips_aura1`
- `metadata_roundtrips_aura0`
- `symbol_map_roundtrips`
- `reader_metadata_available_before_replay`
- `orderbook_replay_does_not_string_lookup_hot_loop`
- `unknown_metadata_policy`

The order-book hot loop uses numeric symbol IDs. Symbol strings are resolved
from `SymbolMap` only outside replay.

## Storage Profile Evidence

See `docs/research/dbn_storage_profile_comparison.md`.

Key result: Aura1 is the hot fixed-width replay profile; Aura1.zstd L3 is the
compressed fixed-width baseline; Aura0 compact is the cold semantic profile.
Standalone Aura1.lz4 is not implemented.
