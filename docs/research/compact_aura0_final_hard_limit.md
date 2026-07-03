# Compact Aura0 Final Hard Limit

Status: FAILED_WITH_HARD_LIMIT FOR CURRENT COMPACT LAYOUT

Scope: compact semantic `.aura0 -> .aura1` byte expansion only. Byte lanes and
compressed Aura1 payloads are excluded as solutions.

## Final Fresh Warm30 Result

Post-commit result directory for commit `ba3cc23`:
`/tmp/aura-benchmarks/compact-research-final-ba3cc23/`

| Dataset | Path | Bytes In | Aura1 Bytes | Median ms | P95 ms | Records/s | Output MB/s | JSON |
|---|---|---:|---:|---:|---:|---:|---:|---|
| huff | compact Aura0 kept | 1,879,040 | 36,410,980 | 64.569 | 73.300 | 12.26M | 537.8 | `huff_compact_warm30.json` |
| huff | zstd L3 Aura1 | 5,542,945 | 36,410,980 | 62.425 | 66.238 | 12.68M | 556.3 | `huff_zstd_l3_warm30.json` |
| nohuff | compact Aura0 kept | 2,246,910 | 36,403,133 | 69.679 | 95.991 | 11.36M | 498.2 | `nohuff_compact_warm30.json` |
| nohuff | zstd L3 Aura1 | 5,538,275 | 36,403,133 | 63.164 | 65.224 | 12.53M | 549.6 | `nohuff_zstd_l3_warm30.json` |

Gap to zstd L3:

- huff: compact is 2.144 ms slower.
- nohuff: compact is 6.515 ms slower.

Zstd L9 was also measured and was faster than L3 on these fixtures:

- huff L9: 56.140 ms, 3,268,730 bytes.
- nohuff L9: 58.357 ms, 3,264,085 bytes.

## Final Stage Split

| Dataset | Decode ms | Writer ms | Notes |
|---|---:|---:|---|
| huff | 29.082 | 33.330 | Huffman decode remains visible; writer is still half the total. |
| nohuff | 33.410 | 33.127 | No-Huffman exact writer helped, but semantic reconstruction still dominates. |

The kept compact patch removed some avoidable writer work, but not the core
semantic reconstruction:

- 12 materialized streams;
- 2,754,892 materialized values;
- grouped/partitioned traversal;
- segmented delta reconstruction;
- sparse and presence checks;
- eight logical field stores per Aura1 row.

## Correctness Evidence

Verify-mode outputs matched the source Aura1 bytes:

- huff: `huff_compact_verify.json`,
  `output_bytes_equal=true`, output hash `12194870092346231300`.
- nohuff: `nohuff_compact_verify.json`,
  `output_bytes_equal=true`, output hash `10372430540135078667`.

Production timing did not include verification:

- `guard_mode=no_guard`
- `canonical_hash_mode=none`
- `output_verification_runtime_ns=0`

## Experiments Completed

At least ten concrete compact experiments were run or retained as benchmark
evidence:

1. zstd L1/L3/L9 opponent truth table;
2. cursor path activation in fair byte-output benchmark;
3. cursor fixed-row writer/direct-output prototype;
4. zero-fill elimination probe;
5. materialized streaming-config writer;
6. raw append no-guard writer;
7. pointer-offset/slice construction removal probe;
8. prechecked fixed-row stores;
9. dense i8 partition-base map;
10. cursor-direct recheck after writer improvements;
11. pointer-advance row loop;
12. no-Huffman exact streaming writer;
13. delta accumulator loop rewrite;
14. sparse mask prevalidation.

## What Was Eliminated

- Some per-row narrowing before fixed-row stores.
- Binary-search base lookup in the materialized fixed partition path.
- Generic source dispatch for the exact no-Huffman grimoire layout.
- Redundant output zero-fill in earlier compact work.

## What Still Happens Per Row Or Field

- Presence mask read and branch.
- Sparse stream conditional read.
- Segmented delta accumulation.
- Eight field writes into Aura1 row layout.
- Row-level semantic assembly.

## Hard Limit Judgment

The current compact layout is close to zstd L3 on huff but does not beat it
stably, and it remains materially slower on no-Huffman. The remaining work is
not just accidental allocation or one obvious generic branch. It is the cost of
semantic stream reconstruction and row packing that zstd avoids by emitting
already-formed bytes.

This is a format/layout limitation for the current compact semantic design.
A compact v2 may still pass, but it needs a real semantic stream redesign, not
another row-store micro-optimization.

Smallest plausible compact v2 patch:

- row-group instruction tape in the footer;
- plan-indexed stream slots;
- control/data integer streams for deltas and dictionary codes;
- RLE/constant stream modes promoted to row-group recipes;
- direct field writer recipes over row groups;
- no full-file `Vec<i64>` stream materialization.
