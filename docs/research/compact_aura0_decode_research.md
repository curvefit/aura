# Compact Aura0 Decode Research

Status: RESEARCH COMPLETE; COMPACT OPTIMIZATION REQUIRED

## Scope

The product target is compact semantic `.aura0 -> .aura1` uncompressed bytes.
Aura1 byte lanes, LZ4 byte caches, and compressed Aura1 payloads are excluded
from the pass condition and are only valid as controls.

## External Prior Art

### DBN / Fixed-Width Replay

Databento Binary Encoding documents the replay properties Aura1 should copy:
metadata is self-describing and parsed once; records use fixed layouts; DBN data
is structured the same in memory, on wire, and on disk; and fixed lengths and
offsets enable sequential access, prefetching, and few copies.

Source:
`https://databento.com/docs/standards-and-conventions/databento-binary-encoding`

Implication for Aura:

- Aura1 is the right replay target.
- Compact Aura0 can only beat zstd if semantic decode produces Aura1 bytes with
  very little per-record interpretation.
- Footer/program parsing must compile into row-store recipes before the hot loop.

### Zstd Decode

The zstd format is frame/block based. A frame has header metadata, blocks, and
optional checksums; blocks are raw, RLE, or compressed. The decompressor mostly
inflates already-formed bytes into a contiguous output buffer. It avoids
semantic stream joins, dictionary-to-field reconstruction, and row packing.

Source:
`https://github.com/facebook/zstd/blob/dev/doc/zstd_compression_format.md`

Implication for Aura:

- A fair compact Aura0 comparison is harder than zstd because compact Aura0 does
  more logical work.
- Prepared contexts may reduce frame/setup overhead, but the measured grimoire
  zstd path is dominated by fast byte inflation, not schema interpretation.
- Compact Aura0 must remove materialization, map lookups, and per-row dispatch
  before a code-only win is plausible.

### SIMD Integer Compression

Stream VByte separates control bytes from data bytes and is designed for SIMD
decode; the paper reports multi-billion-integer/s decode rates on differential
integer streams.

Source:
`https://arxiv.org/abs/1709.08990`

SIMD-BP128 and SIMD-FastPFOR operate on integer blocks, using vectorized
bitpacking and exceptions. The BP128/FastPFOR literature reports materially
faster integer decode than older variable-byte/PFOR families while preserving
compression density.

Sources:
`https://arxiv.org/abs/1209.2137`
`https://github.com/fast-pack/FastPFOR`

Implication for Aura:

- Current varint/bitpack paths should be measured per stream. If integer decode
  dominates, an Aura0 compact format revision may need control/data stream
  separation or BP128-style block packing.
- A production change should start as a microbenchmark over extracted Aura0
  streams, then graduate only if it beats the current stream decoder and
  preserves footer semantics.

## Market-Data Decode Patterns

Likely fast paths:

- timestamp streams: fixed step or monotonic delta, direct accumulator into
  `ts` output offset;
- price streams: small signed deltas or related deltas, zigzag/bitpack decode
  directly into output;
- size streams: small unsigned deltas or RLE/constant;
- symbols: dictionary/packed dictionary with repeated IDs; avoid string lookup;
- side/flags/type: constant, RLE, bitset, or tiny dictionary;
- sparse many-symbol rows: presence masks should drive conditional writes, not
  full zero-fill plus overwrite unless measured faster.

## Precomputation Opportunities

Precompute in the footer:

- stream IDs, codec IDs, value counts, block boundaries;
- row width and field byte offsets;
- field-to-stream mappings;
- dictionary layouts and widths;
- delta base/reset policy;
- presence/null bitmap layout;
- optional stream codec hints such as constant, fixed-step, raw, no-Huffman.

Precompute when parsing Aura0:

- body stream slices by stream index;
- block output byte ranges;
- exact output size;
- one bounds/range check per block;
- cursor decode recipes.

Compile into `CompiledAuraPlan` or a new `CompiledAuraDecodePlan`:

- fixed Aura1 record width and body offset;
- field output offsets and widths;
- stream cursor recipes in row-store order;
- stream index slots that avoid `BTreeMap` lookup in row loops;
- fast path flags for no nulls/no dict/no Huffman/raw/constant/fixed-step;
- writer recipes for direct stores or row-template patching.

Compute once per block instead of per record:

- output byte start/end;
- footer/schema compatibility checks;
- stream cursor initialization;
- strict guard setup;
- row template for constant fields;
- partition run ranges.

Skip in production mode:

- canonical hash;
- output-byte guard scan;
- decoded-row verification;
- string symbol materialization;
- full row vectors.

## Ranked Compact Aura0 Decode Experiments

1. Baseline truth rerun: compact semantic materialized, existing cursor, zstd L1/L3/L9 on huff/nohuff.
2. Precise stage/counter split: decode streams, cursor construction, materialization allocation/fill, field reconstruction, row stores, copies.
3. Plan-indexed stream slots: replace decode/write `BTreeMap<u16, ...>` lookups with `Vec` slots keyed by precompiled stream order.
4. Cursor-direct row writer: drive `GenericI64StreamCursor` directly into final Aura1 output without full stream vectors.
5. Raw/no-Huffman fast path: specialize no-Huffman fixtures, bypass generic dispatch where streams are direct/bitpack/raw.
6. Constant/RLE/fixed-step fast path: fill or patch output fields without per-row cursor calls when stream semantics permit.
7. Row-template writer: prefill/copy a row template for constant/default fields and patch changing fields.
8. Zero-fill elimination: allocate output without redundant fill only where every byte is written before read, or prove `Vec::resize` cost is negligible.
9. Scratch-buffer reuse: reuse codec scratch buffers and avoid per-stream `Vec` reallocations.
10. Integer codec microbenchmarks: Stream VByte, BP128-style bitpack, RLE/constant over extracted streams.

Failure is not valid until at least seven concrete compact-decode experiments
are measured and either kept or rejected.
