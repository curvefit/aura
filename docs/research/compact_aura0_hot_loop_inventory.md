# Compact Aura0 Hot Loop Inventory

Status: RESEARCH COMPLETE: REMAINING HOT WORK IS SEMANTIC RECONSTRUCTION

Scope: compact semantic Aura0 decode/write path only.

## Current Dataflow

Production compact decode enters through the fair byte-output operation and
the profiled Aura0-to-Aura1 path in `src/records.rs`. The current best
production path uses the materialized compact semantic decode, then a
specialized fixed-row Aura1 writer in `src/generic_planner.rs`.

High-level flow:

1. Parse Aura0 footer/schema/stream program once.
2. Decode compact semantic stream bodies.
3. Materialize 12 streams with 2,754,892 values.
4. Traverse grouped partition runs.
5. Reconstruct segmented deltas.
6. Reconstruct sparse/presence fields.
7. Pack eight logical fields into each fixed-width Aura1 row.

## Stream Decode Table

Measured final warm30 compact runs:

| Dataset | Stream Count | Materialized Values | Cursor Values | Decode ms | Writer ms |
|---|---:|---:|---:|---:|---:|
| grimoire-50mb-huff | 12 | 2,754,892 | 0 | 28.313 | 34.008 |
| grimoire-50mb-nohuff | 12 | 2,754,892 | 0 | 32.008 | 32.868 |

The cursor prototype removes full stream materialization, but its measured row
execution is slower than the materialized writer on these fixtures. It remains
behind `--decode-path cursor` and is not the default compact path.

## Row Write Table

The grimoire-compatible fixed Aura1 rows are 46 bytes and contain eight logical
fields. The kept writer optimization specializes the final stores for the
known partitioned sparse Aura1 layout:

- precheck `i32` and `i8` field conversions before the raw pointer write;
- write fixed row bytes with unaligned stores;
- use a dense `i8 -> base` table for partition base lookup where the fixed
  Aura1 partition field is actually i8;
- specialize the no-Huffman exact slot layout when all stream slots match the
  grimoire Aura1 layout and no output guard is active.

The generic guarded and fallback writers remain in place for safety and strict
verification.

## Avoidable Work Found

The following work was reduced:

- repeated per-row numeric narrowing in the fixed-row pointer writer;
- binary-search partition base lookup in the materialized writer;
- generic source dispatch for the no-Huffman exact streaming layout.

The following work remains:

- full compact stream materialization in the fastest default path;
- row-level sparse/presence branches;
- segmented delta accumulation;
- eight field stores per row;
- writer counters still report one row store per Aura1 record.

## Rejected Hot Loop Experiments

| Experiment | Result | Decision |
|---|---|---|
| Cursor-direct decode | Removed stream materialization but huff/nohuff cursor remained much slower. | Keep behind flag only. |
| Pointer-advance row loop | Slowed huff/nohuff versus indexed pointer writes. | Reverted. |
| Delta accumulator in codec loops | Improved local decode shape but worsened total/p95. | Reverted. |
| Sparse mask prevalidation | Added a second mask pass and slowed both datasets. | Reverted. |
| Exact writer for huff plan | Hurt huff, helped no-Huffman only when gated. | Kept only as no-Huffman specialization. |

## Next Useful Code Target

The remaining compact-only target is a semantic format or codec change, not
another small row-store edit:

- row-group instruction tape;
- plan-indexed streams without materialized `Vec<i64>` for every stream;
- control/data integer codecs such as Stream VByte or BP128-style blocks;
- direct field writer recipes over row groups.
