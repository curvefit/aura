# Compact Aura0 to Aura1 Dataflow

Status: CODEFLOW ARCHAEOLOGY COMPLETE

## Current Dataflow

Compact semantic Aura0 byte expansion enters the fast/profiled path through
`records::try_compile_i64_file_profiled(... Profile::Aura1 ...)`, which calls
`try_compile_aura0_to_aura1_fast_profiled` for Aura0 input.

The compact semantic path excludes the Aura1 byte lane. It decodes the compiled
footer, validates the schema, builds `CompiledAuraPlan`, then uses the generic
Aura0 instruction plan to decode streams and write an Aura1 body.

## Hot Stages

| Stage | File/function | Current behavior | Avoidable work | Proposed experiment |
| --- | --- | --- | --- | --- |
| Footer/plan parse | `src/records.rs::try_compile_aura0_to_aura1_fast_profiled` | builds `CompiledAuraPlan` once | not the current bottleneck | keep |
| Stream materialization | `src/generic_planner.rs::decode_generic_i64_stream_values_profiled` | decodes all streams into `BTreeMap<u16, Vec<i64>>` | 12 stream vectors, 2,754,892 values | cursor-direct or indexed stream slots |
| Cursor path | `src/generic_planner.rs::try_encode_generic_i64_aura1_body_streaming` | cursor support exists but was not wired into fair benchmark; it produced temp body | temp body copy and generic per-field writes | direct final output and fixed-row writer |
| Writer | `src/generic_planner.rs::try_write_partitioned_sparse_i64_aura1_body_profiled` | writes 791,360 fixed rows with per-row slice/offset | zero-fill, per-row offset, per-field packing | fixed-row precomputed offsets, row template |
| Generic writer | `src/generic_planner.rs::try_write_generic_i64_aura1_body_from_streams_profiled` | used by no-Huffman artifact; still fixed-row capable | allocation/zero-fill and source dispatch | no-Huffman/source specialization |
| BTreeMap use | `src/generic_planner.rs` stream/value maps | maps stream IDs to vectors/cursors | map construction/lookups outside or near hot loops | plan-indexed slots |
| Stream codecs | `src/body.rs::try_generic_i64_stream_cursor`, `decode_generic_stream_body` | supports bitpack, delta, dictionary, packed dictionary, Huffman cursor | some codecs still materialize or use scalar dispatch | stream microbenchmarks |

## Shortest Cursor Path

The shortest cursor path is not a new format. It is:

1. Parse footer and build plan once.
2. Build stream cursors from existing stream bodies.
3. Use `StreamingAura1Config` to reconstruct row fields.
4. Write directly into Aura1 body bytes.

Fresh measurement after wiring `--decode-path cursor` into the fair benchmark:

```text
huff cursor before fixed-row writer: 176.782 ms
huff cursor after fixed-row writer: 132.718 ms
huff cursor after direct final output: 128.901 ms
nohuff cursor after fixed-row writer: 125.376 ms
nohuff cursor after direct final output: 115.964 ms
```

This proves stream-vector materialization can be removed, but the current cursor
row reconstruction/write loop is still slower than the materialized writer.

## Current Best Direction

The materialized writer is still the fastest compact path on the grimoire
fixtures. Next compact-only work should focus on:

1. no-Huffman source/row writer specialization;
2. plan-indexed stream slots instead of `BTreeMap` stream maps;
3. stream codec microbenchmarks to see if integer decode can close the gap.
