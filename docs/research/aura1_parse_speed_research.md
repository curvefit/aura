# Aura1 Parse Speed Research

Date: 2026-06-23

Target:

```text
Aura1 fixed-width files should parse, scan, replay, and optionally grouped-replay
as fast as possible using dynamic generic schemas.
```

This sprint does not optimize Aura0 compression, Aura1-to-Aura0 encoding, byte
lanes, or compact Aura0-vs-zstd behavior.

## Baseline Observations

The SDK closeout matrix showed a large gap on generated dynamic schemas:

- Aura1 replay on `sdk-larger`: about 35M records/sec.
- Aura1 read batches on `sdk-larger`: about 7-8M records/sec.
- Batch reads report `streaming_reader_used=true`,
  `full_file_materialized=false`, and bounded `max_rows_materialized_at_once`.

The code-level reason is clear:

- `AuraReader::replay_i64` for Aura1 delegates to
  `records::visit_i64_rows_file`, which uses fixed-width body offsets and a
  reusable row buffer.
- `AuraReader::next_batch` calls `read_aura1_batch`, which allocates one
  `Vec<i64>` per row, then `AuraRecordBatch::from_i64_decoded` converts every
  scalar into an `AuraValue`.
- `AuraRecordBatch` remains the compatibility API and stores
  `Vec<Vec<AuraValue>>`.
- `AuraColumnBatch` exists, but reader-side Aura1 parsing does not yet build it
  directly.

## DBN / Fixed-Width Replay Lessons

The relevant DBN-like lesson is not a specific market schema. It is the replay
model:

- parse metadata/header/schema once;
- know record width before the hot loop;
- scan fixed-width records sequentially;
- avoid per-record allocation;
- avoid field-name lookup inside the row loop;
- expose borrowed or lightweight row views when materialized rows are not
  required.

Aura1 already has the important pieces: `CompiledAuraPlan` contains record
count, field count, record width, and Aura1 body size, and
`visit_i64_rows_file_range` can walk a row range without decoding the whole
file. The missing SDK layer is a faster materialization target than
`Vec<Vec<AuraValue>>`.

## Arrow-Style Repeated-Value Layout Lessons

Arrow's run-end encoding represents repeated values with run boundaries rather
than repeating the value for every logical row. That maps well to Aura1 grouped
eventing without changing row semantics:

- grouping should preserve physical row order;
- groups should be consecutive runs, not global aggregation;
- selected group fields should be compiled once by field name or field ID;
- high-cardinality data should degrade gracefully to one-row groups;
- grouped replay changes callback semantics, so it must not be presented as
  the same work as per-row replay.

The first implementation should compute runs on the fly over Aura1 fixed rows.
An optional footer/sidecar group index can be researched later if on-the-fly run
detection costs too much.

## Generic Schema Parse Optimization

Aura1 parse should stay dynamic-schema driven:

- field offsets and widths come from `CompiledAuraPlan`, not hard-coded record
  widths;
- timestamp, symbol, side, and event-type fields must be selected from schema
  metadata or explicit field names/IDs;
- row materialization is a compatibility path, not the only parse path;
- column batches can avoid per-row `Vec` allocation and per-cell `AuraValue`
  enum construction in read-heavy code;
- borrowed fixed-width batch views are a larger API because they must expose
  lifetimes and typed field access safely.

The low-risk optimization is reader-side `AuraColumnBatch` construction for
Aura1. It keeps public schema semantics, avoids row vectors, and uses existing
SDK column types.

## Grouped Eventing Design

The first grouped replay mode should be consecutive-run grouping:

```text
GroupBy::fields(["ts_event"])
GroupBy::fields(["ts_event", "symbol_id"])
```

Runtime behavior:

- compile field names to positional indexes once;
- reject unknown fields before replay;
- require selected fields to be fixed-width SDK-supported fields;
- read Aura1 rows in order with `replay_i64`;
- compare only selected key slots;
- emit a group when the key changes;
- expose `row_start`, `row_count`, shared key values, and source field indexes;
- preserve order exactly.

This gives downstream users a lower callback-count API on repeated timestamp or
symbol bursts while keeping normal replay and batch APIs unchanged.

## Precomputation Opportunities

Already available:

- record width from `CompiledAuraPlan`;
- field count from `CompiledAuraPlan`;
- body size and output size from `CompiledAuraPlan`;
- canonical field order as positional field order.

Should be compiled at parse/open time or at grouped replay start:

- field-name to index lookup;
- group key indexes;
- per-batch typed column constructors;
- expected batch row count;
- selected group field count and key buffer length.

Still per row:

- endian decode of every selected or materialized field;
- callback invocation for replay/group output;
- equality compare for group key fields.

Still per field for materialized batches:

- append decoded scalar to the target row or column representation;
- convert to SDK physical column type where required.

## Prior-Art Notes

- Stream VByte separates control bytes from data bytes, which is useful for
  future compact integer streams but not directly for Aura1 fixed rows.
- Zstd is fast at byte output because it emits already-formed bytes and avoids
  semantic row materialization. Aura1 replay should be judged separately from
  row or value materialization.
- Columnar systems often favor dictionary/run encodings and auxiliary
  structures when repeated values are common; for Aura1 the non-invasive analog
  is grouped replay or an optional group index.

## Ranked Experiments

1. Add direct Aura1 column-batch reads that avoid intermediate
   `Vec<Vec<i64>>` and `AuraValue` materialization.
2. Add `aura1-read-batches-columnar` benchmark operation and compare it to row
   batches and replay on generated schemas.
3. Add `aura1-scan-raw` benchmark operation that validates fixed byte scanning
   ceiling.
4. Add generic `GroupBy`, `AuraEventGroup`, and `AuraReader::grouped_replay`.
5. Generate repeated timestamp, repeated symbol, timestamp+symbol, high
   cardinality, and mixed-burst fixtures.
6. Add grouped replay benchmarks with callback-count reduction and group stats.
7. Add typed replay benchmark that performs a schema-aware callback without
   row/value materialization.
8. Benchmark batch sizes 128, 1024, 8192, and 65536 for Aura1 row and column
   batches.
9. Prototype borrowed `Aura1FixedBatchView<'a>` only if column batches do not
   close enough of the gap.
10. Research optional footer group indexes after on-the-fly grouped replay is
    measured.

## Decision Before Coding

Implement experiments 1-6 first. They are dynamic-schema safe, keep row
semantics intact, and directly target the measured replay-vs-batch gap.
