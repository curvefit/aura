# Compact Aura0 Integer Codec Hooks

Status: RESEARCH COMPLETE: CURRENT CODE HAS MANY SCALAR CODECS, BUT THE FASTEST
PATH IS LIMITED BY STREAM-TO-ROW RECONSTRUCTION

Scope: compact semantic stream codecs in `src/body.rs` and their connection to
the Aura1 writer.

## Existing Local Codecs

The compact stream layer already includes several integer-oriented codecs:

- scalar varint and zigzag helpers in `src/varint.rs`;
- base bitpack;
- previous-delta bitpack;
- previous-value varint;
- patched bitpack;
- RLE;
- bitplane RLE;
- dictionary and packed dictionary modes;
- Huffman dictionary mode;
- block-local variants.

The dispatch and decode hooks live primarily in `src/body.rs`, with cursor and
materialized consumption paths connected through `src/generic_planner.rs`.

## Prior Art Implication

Stream VByte and BP128/FastPFOR suggest better compact v2 stream shapes:

- split control and data streams;
- decode fixed groups of integers;
- store one bit width per group;
- handle exceptions separately;
- keep prefix-sum/delta reconstruction close to the writer.

These ideas are semantic compact formats, not Aura1 byte lanes.

## Experiment Outcome This Sprint

Two local scalar-code experiments were attempted and rejected:

1. Delta accumulator in the existing bitpack/varint decode loops.
   - Result: `final_huff_delta_accumulator_warm30.json` was 65.551 ms and
     `final_nohuff_delta_accumulator_warm30.json` was 66.129 ms, both worse
     than the best kept compact candidate for at least one dataset/p95.
   - Decision: reverted.

2. Sparse mask prevalidation before row writing.
   - Result: `final_huff_sparse_prevalidated_warm30.json` was 67.576 ms and
     `final_nohuff_sparse_prevalidated_warm30.json` was 69.963 ms.
   - Decision: reverted because the extra pass outweighed unchecked sparse
     indexing.

## Hard Lesson

Replacing one scalar loop inside the existing stream layout is not enough.
The compact path still has to connect semantic streams to Aura1 row fields.
The next integer-codec work should be a compact v2 prototype with:

- row-group boundaries;
- control/data stream separation;
- direct writer recipes;
- per-stream group metadata in the footer or instruction tape;
- decode APIs that can write into row fields or row-group scratch directly.
