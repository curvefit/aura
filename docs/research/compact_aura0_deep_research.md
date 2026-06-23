# Compact Aura0 Deep Decode Research

Status: RESEARCH COMPLETE; COMPACT EXPERIMENTS EXECUTED

Scope: compact semantic `.aura0 -> .aura1` byte expansion only. Aura1 byte
lanes, LZ4 byte lanes, zstd-compressed Aura1 payloads inside Aura0, fast
profiles, and hybrid profiles are controls, not pass conditions.

## Sources Inspected

External:

- Databento Binary Encoding: fixed metadata followed by records, all little
  endian, and common record header structure.
  <https://databento.com/docs/standards-and-conventions/databento-binary-encoding>
- Zstandard compression format: frame/block model, raw/RLE/compressed blocks,
  bounded block sizes, literals plus sequences, and reusable FSE modes.
  <https://github.com/facebook/zstd/blob/dev/doc/zstd_compression_format.md>
- Stream VByte: separates control bytes from data bytes and decodes deltas in
  SIMD-friendly groups.
  <https://arxiv.org/abs/1709.08990>
- SIMD-BP128 / SIMD-FastPFOR: vectorized bitpacking and exception-oriented
  integer decode.
  <https://arxiv.org/abs/1209.2137>

Local:

- `src/body.rs`: compact stream cursor and materialized stream codecs.
- `src/generic_planner.rs`: generic Aura0 plan decode, stream materialization,
  partitioned sparse Aura1 writer, and cursor experiments.
- `src/records.rs`: profiled Aura0-to-Aura1 transcode entry points and fair
  benchmark path wiring.
- `src/program.rs`: compiled footer and `CompiledAuraPlan` metadata.
- `tests/aura_bench_cli.rs` and `tests/writer_reader_api.rs`: benchmark and
  correctness coverage for compact transcode.

## Why Zstd Wins On Aura1 Bytes

Zstd decompresses already-formed Aura1 bytes. Its block header determines raw,
RLE, or compressed block behavior; compressed blocks combine a literals section
and a sequences section. Once the decompressor has the frame/block metadata and
tables, it emits bytes directly into the output buffer with match copies and
literals. It does not know or reconstruct market-data fields.

Compact Aura0 performs more semantic work:

- decode 12 compact streams into about 2.75 million logical values;
- reconstruct partitioned event runs and grouped fields;
- reconstruct segmented deltas;
- reconstruct sparse/presence-controlled fields;
- combine streams into eight logical row fields;
- pack each logical row into a 46-byte Aura1 record.

Zstd avoids all of that field-level work. It also benefits from block-level
metadata, repeated table modes, raw/RLE block escapes, and a mature C decoder.
Compact Aura0 can copy some of those properties without becoming a byte lane:

- per-block codec choice for semantic streams;
- raw/RLE/constant stream modes when semantic data allows;
- prepared decode tables;
- block-level output bounds;
- table-driven direct field writers;
- control/data separation for integer streams.

## DBN And Fixed-Width Replay Lessons

DBN puts metadata before records and uses record headers/layouts that let the
reader scan records sequentially without rebuilding schema decisions per row.
Aura1 already follows the replay side of this: fixed row width, numeric symbols,
little-endian fields, and no row allocation in fixed scan benchmarks.

The implication for compact Aura0 is strict: Aura0 decode must compile footer
metadata into an execution plan before the hot loop. Every schema lookup,
stream ID lookup, field offset lookup, row offset calculation, and dynamic
writer decision left in the per-row loop moves Aura0 away from DBN-like replay
and toward slow semantic interpretation.

## Integer Codec Lessons

Stream VByte teaches that scalar varint-style decode is not the speed ceiling.
The key format property is separate control and data streams, so the decoder can
load control bytes predictably and decode fixed groups of integers with little
branching. This maps to Aura0 streams such as timestamp deltas, price deltas,
sizes, sparse indexes, and dictionary codes.

SIMD-BP128/FastPFOR teaches that block-of-128 integer layouts are a better
shape for predictable hot loops: one bit width per group, optional exceptions,
and decode into registers or a scratch lane before applying prefix sums.

Current Aura0 already has bitpacked modes, dictionary modes, and Huffman modes,
but the measured path still materializes streams and then writes rows. The next
question is not only whether the codec is fast, but whether decoded values can
be consumed directly by a field writer without building full `Vec<i64>` streams.

## Footer And Parse-Time Precomputation

The compact footer should store or enable the following without hot-loop
discovery:

- stream ID to dense slot mapping;
- stream codec ID and value count;
- stream body offset and length;
- row group ranges;
- Aura1 body offset, record width, and exact output size;
- field output byte offset and width;
- field to stream slot mapping;
- dictionary table width and entry count;
- delta mode and base/reset policy;
- presence/null stream mapping and bit layout;
- constant field values and RLE run descriptions;
- direct writer recipe ID;
- validation input and output ranges;
- decode function ID or enum recipe.

Build at parse time:

- `CompiledAuraDecodePlan` or equivalent derived from `CompiledAuraPlan` plus
  generic Aura0 plan;
- dense `Vec` stream slots instead of `BTreeMap<u16, _>` in hot loops;
- row writer recipes ordered by Aura1 field offset;
- block-level output ranges and one range check per row group;
- scratch buffer allocation plan.

Still unavoidable per row for current compact layout:

- some delta accumulation;
- sparse/presence branch for zero-heavy fields;
- final Aura1 field stores;
- row pointer advancement.

## What Currently Happens Too Late

The current compact profiled path still discovers or resolves several things
during decode/write that should be compiled:

- stream bodies are decoded into materialized maps/vectors before the writer;
- writer selects between partitioned sparse variants after stream decode;
- some stream lookup is keyed by `u16` IDs rather than a dense recipe index;
- the streaming writer has row-specific sparse/presence logic instead of a
  generated row group program;
- integer codec experiments are not benchmarked per stream, so codec vs writer
  responsibility remains mixed.

## Ranked Compact Experiments

1. Zstd opponent truth table: L1/L3/L9, warm20, same memory sink, production
   mode, verify mode separately.
2. Hot-loop inventory and precise counters: dense stream count, BTreeMap lookup
   count, stream materialization values, row stores, writer bytes.
3. Instruction tape design: compile stream slots and row writer recipes once.
4. Plan-indexed stream slots: replace `BTreeMap<u16, Vec<i64>>` lookup in
   decode/write paths with dense vectors.
5. Cursor-direct decode: drive stream cursors into final Aura1 rows without
   full stream vectors.
6. No-Huffman raw fast path: bypass generic dictionary/Huffman dispatch where
   stream op is raw/base-bitpack/constant.
7. Dictionary fast path: decode packed dictionary codes directly to output or
   scratch with precomputed entry table.
8. Delta fast path: accumulate segmented deltas directly into output fields.
9. Constant/RLE stream fast path: fill or patch fields without per-row cursor
   calls.
10. Row-template writer: copy a template row and patch changing fields when
    constant/default fields dominate.
11. Fixed-row unrolled writer: compare current pointer writer with an explicitly
    unrolled store/memcpy strategy.
12. Scratch buffer reuse: preallocate and reuse codec scratch by plan.
13. Stream VByte prototype: scalar control/data stream over extracted deltas.
14. BP128-style prototype: block-of-128 bitpacked deltas over extracted streams.
15. Group varint prototype: four-value control byte groups for small deltas.
16. Compact v2 row-group prototype: semantic row-group streams with direct
    writer recipes and per-block codec choice.

Pass/fail implication:

- Existing layout passes only if compact Aura0 beats zstd L3 on huff and
  nohuff with production mode and the same sink.
- Compact v2 passes only if a semantic-stream prototype, not a byte lane, beats
  zstd L3 on both datasets.
- Failure requires evidence from at least 10 concrete experiments and a hard
  limit report that separates codec, reconstruction, and writer costs.
