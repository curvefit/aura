# Aura transmutation path audit

Status: `IMPLEMENTED_LOCALLY` for the duplicate explicit-event stream decode;
the change is uncommitted so the parent can review it with the concurrent
transmutation work. Audit base: `cc04f75c9217e6c6a575145ab0ed98df232f622d`.
No benchmark or build was run in this pass because the campaign lock was held
by the scheduled benchmark slot. `cargo fmt --all -- --check` and
`git diff --check` pass.

The prior compact hard-limit evidence is in history commit `2566419`:
`docs/research/compact_aura0_final_hard_limit.md`,
`compact_aura0_hot_loop_inventory.md`, and `zstd_opponent_analysis.md`. Those
reports measured the old grimoire huff/no-huff fixtures, where compact semantic
Aura0 remained slower than Aura1.zstd after cursor, writer, zero-fill, pointer,
delta, sparse-mask, and no-Huffman micro-experiments. They are negative leads,
not measurements of `cc04f75` or the current v12 corpus. The remaining cost is
semantic stream reconstruction and row materialization; another scalar row
store rewrite is a dominated probe.

## Current call graph

The maintained Aura0 to Aura1 route is:

```text
convert_aura (src/convert.rs)
  -> records::compile_aura0_to_aura1_bytes_with_lane (Aura0 source)
     -> compile_i64_file_inner(..., Aura1)
        -> try_compile_explicit_i64_events (explicit plan, first)
        -> try_compile_i64_fast / try_compile_aura0_to_aura1...

aura-bench fair Aura0
  -> try_compile_i64_file_profiled_with_options
     -> profiled_compile_parts
     -> try_compile_aura0_to_aura1_with_execution_options
        -> CompiledFooter::decode + metadata checks
        -> direct Aura1 body writer or materialized columns fallback

generic explicit decode
  -> decode_generic_i64_events_body
     -> decode generic stream frames
     -> materialize logical rows
     -> split rows back into event values and child rows
```

For a generic non-explicit Aura0 plan, the profiled direct path decodes stream
values once and writes Aura1 rows from them. For an explicit plan,
`try_compile_explicit_i64_events` is intentionally selected before that direct
path because Aura1 must retain event boundaries, including events with zero
children. The Aura1 event sidecar stores child counts and the values for
zero-child events; nonempty event values are checked against the first fixed
row.

## Implemented candidate: one framed decode for explicit events

Before this patch, `generic_planner::decode_generic_i64_events_body` did the
following:

1. `decode_generic_i64_stream_values` decoded every framed stream into a
   `BTreeMap<u16, Vec<i64>>`.
2. `decode_generic_i64_rows_body(plan.clone(), bytes, ...)` parsed every frame
   again, copied each encoded stream body into a new `Vec<u8>`, and
   `decode_generic_i64_rows` decoded every stream a second time into another
   value map before materializing rows.
3. The event decoder then read event values from the first map and child values
   from the second pass's rows.

The new path in `src/generic_planner.rs` uses
`decode_generic_i64_stream_values_checked` followed by
`materialize_generic_i64_rows_from_stream_values`. It scans the framed
headers/body slices once, checks all declared counts before invoking a codec,
decodes each stream once, then reuses those values for the existing row
materializer. It retains the body copies only as borrowed frame slices in a
small frame-header vector; stream payload bytes are not copied.

The validation contract is preserved:

| Existing check | New location | Contract |
| --- | --- | --- |
| `record_count * field_count` and body byte ceilings | `decode_generic_i64_stream_values_checked` | Same `validate_i64_decode_dimensions_usize` limits and errors. |
| Maximum stream count and minimum frame-header bytes | checked framed scan | Same `field_count * 16` cap and `stream_count * 14 <= remaining` guard. |
| Per-stream and aggregate declared value counts | checked framed scan, before codec calls | Same `MAX_V2_I64_DECODE_VALUES` per-stream/aggregate cap; aggregate overflow is rejected before a large output allocation. |
| Exact stream body framing/trailing bytes | `reader.finish()` before codec calls | Same rejection of truncated or trailing framed input. |
| Stream ID to instruction lookup and full codec body consumption | one codec pass | Same unknown-ID, body-type, value-count, and codec-specific errors. |
| Explicit child-count sum | `validate_explicit_event_counts` in event decoder | Still checks nonnegative counts and exact sum to `record_count`. |
| Quotient/remainder plan authorization | `validate_quotient_remainder_decode_plan(&plan, field_count)` | Same output/divisor/producer/stream-target checks; helper now accepts the plan directly. |
| Row reconstruction, presence/sparse fields, partitions, derived values and checked arithmetic | `materialize_generic_i64_rows_from_stream_values` | Existing materialization logic is shared unchanged after stream decode. |

The two-phase framed scan is deliberate. A one-phase loop would check the
aggregate only after decoding an earlier frame, weakening the old
`decode_generic_i64_rows_body` behavior for a hostile aggregate declaration.
The remaining `BTreeMap` lookup is outside the row loop and is a bounded,
low-risk first step. A plan-indexed slot table can be a separate experiment
only if a current profile shows map setup/lookup is material.

The owned patch is in `src/generic_planner.rs`; the focused regression file is
`tests/generic_event_decode.rs`. It checks zero-child and adjacent-identical
event boundaries, exact event/child values, the stream-count cap, and aggregate
declared-value rejection before codec allocation. No source format or event
semantics were changed.

## Explicit Aura0 to Aura1 replan cost

The second concrete cost is in `src/records.rs::try_compile_explicit_i64_events`
after the `events` match. The old code cloned every event value/child vector,
called `encode_generic_i64_events` (which replanned and re-encoded a compact
body), then called `encode_generic_i64_rows_body`; for a target `Profile::Aura1`
that `compact_body` was immediately discarded. It also flattened events into
Aura1 rows afterward.

For Aura1, the source ingest/compiled footer already carries a validated
explicit plan. The target needs the fixed Aura1 rows plus the event sidecar;
it does not need a newly selected compact body. A safe target-specific change
therefore preserves the source `generic_aura0_plan`, retains schema/event
validation, and performs the required flatten once. This may change Aura1
footer bytes for an imported artifact whose source plan was noncanonical, so
the acceptance contract is semantic event/order/row parity. Generated writer
inputs should additionally assert byte equality with the existing two-step
reference. The parent owns this `records.rs` change; this audit does not claim
or duplicate it.

The required Aura1 checks remain:

* `decode_i64_file_metadata` validates the sealed container, header/footer
  agreement, compiled plan authorization, dimensions, schema width, and event
  plan before event decoding.
* `decode_generic_i64_events_body` validates stream framing, child-count sum,
  derived/presence/relationship reconstruction, and exact event values.
* `flatten_i64_events` validates every fixed logical row before Aura1 packing.
* `append_explicit_event_sidecar` preserves all event counts and values for
  zero-child events, while nonempty event values remain checked by the Aura1
  reader.

## SDK conversion boundary

`src/convert.rs::convert_aura` always calls `records::decode_i64_file` before
dispatching on the target. For compiled input and a compiled target, this
materializes all logical rows solely to obtain `source_format`,
`record_count`, and `schema_hash`; the subsequent compiler reparses metadata
and either decodes semantic streams or scans fixed Aura1 rows to columns. With
`verify=true`, the source is decoded again for row equality. This is a separate
SDK-level materialization/copy cost from the fair benchmark path.

A future low-risk split is:

* for target `.aura0`/`.aura1`, use `decode_i64_file_metadata` for summary
  fields and let the selected compiler perform its normal source checks;
* retain full `decode_i64_file` for target ingest `.aura` and for the
  verify-only source comparison;
* keep `read_to_end` because the generic `Read` API requires buffering.

Regression coverage should exercise compiled Aura0 to Aura1, Aura1 to Aura0,
ingest to each compiled profile, verification on/off, malformed sealed input,
and summary field parity. Do not remove required source decoding from verify
mode.

## Fair Aura versus Zstd accounting

`src/bin/aura_bench.rs::load_fair_bytes_context` requires the input SHA-256 to
match the declared Aura0 or Aura1 reference. It compresses the complete
reference Aura1 once before timing and records the compressed hash/size. The
fair operations then use a preloaded input and the same conceptual
`memory_vec` output contract:

* `aura0-to-aura1-bytes` expands the Aura0 file to a complete sealed Aura1
  `Vec<u8>` through the maintained Aura conversion path.
* `zstd-aura1-to-aura1-bytes` decompresses the prebuilt zstd frame to a
  complete sealed Aura1 `Vec<u8>`.
* `*-verify` compares the resulting bytes with the same reference Aura1 after
  the timed operation; equality/hash work is outside production runtime.

The output sink and output bytes are therefore aligned. Required validation is
not symmetric in the current fair byte operation: the Aura route validates
the Aura0 container/footer/schema/limits and generic semantic reconstruction;
the Zstd branch at `fair_aura1_bytes_operation` calls only
`zstd::stream::decode_all`. It does not call `aura1_fixed_layout_info`, parse
the Aura1 footer, or replay rows. `load_fair_bytes_context` does validate and
count the reference Aura1, but that setup is outside every timed iteration.
The verify variant checks byte equality and an output guard after timing; it
does not make Zstd pay Aura1 validation during the measured operation.

Consequently, the fair result is accurately described as “Aura0 semantic
expansion to sealed Aura1 bytes versus Zstd frame expansion to the same bytes.”
It must not be described as equal full replay validation. For a usable-replay
comparison, add a paired operation that charges the same Aura1 layout check
and selected consumer/replay work on both outputs, or report the separate
`zstd-decompress-plus-replay` and Aura1 replay measurements. Keep production
fair byte runs at `--guard-mode no_guard --canonical-hash-mode none`; those
flags are enforced for fair byte operations. The JSON `benchmark_input_bytes`
and `compressed_input_bytes` fields must be used for stored-size denominators;
Zstd setup compression is not part of `median_runtime_ns`.

## Current fixtures and regression applicability

The generated V2 fixture matrix at `/tmp/aura-fixture-gen-test-2851614` has
rows-only cases (`tiny`, `sdk-*`, `larger`, repeated timestamp/symbol, and
`sparse-many-symbol`). `sparse-many-symbol` has generic groups and one Huffman
stream but no explicit event group, so it does not exercise the duplicate
event decoder. The generated `huff` entry remains explicitly blocked because
the public planner did not select a Huffman stream under its generation gate.

The small committed `tests/fixtures/v2/explicit-events.aura0` is a valid
explicit-event smoke fixture. The staged current corpus is
`/home/anton/Downloads/lean-node-20260907/corpus-w03/retained-manifest.json`:
it contains two reduced historic schema-10 relation projections with direct
and related Aura0 files, related Aura1 references, and independent logical
event/replay reports. The ETH sample has 4,307 events and 113,461 levels; the
BTC sample has 3,046 events and 227,239 levels. The related plans select the
schema-authorized slot-5 residual from slot 4; the direct plans do not. The
manifest explicitly omits source event kind, reset boundaries, segment
identity, and instrument partitioning, so these are useful transmutation
fixtures after the current Aura mapping is confirmed, not proof of full
production restoration semantics.

For the duplicate explicit-event decode, measure at least the committed smoke
fixture and one related current corpus file once the scheduled slot is free.
Compare event-order hash, child counts, final replay checkpoint, and output
Aura1 bytes where the source plan is canonical. Record stream count, declared
value count, and materialized row/value counts. Do not use the old grimoire
hard-limit timings as current baselines.

## Next validation command

After the parent releases the benchmark lock, run a focused offline test/build
on the parent-selected branch, then the bounded current-corpus fair commands.
The first focused check is:

```text
cargo test --offline --test generic_event_decode
```

The test must pass alongside the existing explicit-event suite. A measured
win is valid only if the one-pass result preserves explicit event boundaries,
derived relation values, zero-child events, exact Aura1 output (where byte
identity is part of the input contract), and the framing/aggregate limits
listed above.
