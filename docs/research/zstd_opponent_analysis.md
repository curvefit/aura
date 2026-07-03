# Zstd Opponent Analysis

Status: RESEARCH COMPLETE: ZSTD L3 REMAINS THE PRODUCT TARGET

Scope: compact semantic `.aura0 -> .aura1` byte expansion versus
`.aura1.zst -> .aura1` byte expansion. Byte lanes are not part of this target.

## Local Benchmark Behavior

The fair zstd operation is implemented in `src/bin/aura_bench.rs` under
`zstd-aura1-to-aura1-bytes`. The harness requires a reference Aura1 file,
compresses that Aura1 payload with the requested zstd level, and times
decompression back into a memory `Vec<u8>`.

Important implementation points:

- zstd work is byte expansion only; it does not parse records.
- production mode uses `guard_mode=no_guard` and
  `canonical_hash_mode=none`.
- output sink is `memory_vec`, the same sink used by the compact Aura0 fair
  byte-output operation.
- verify variants are separate from production timing.

The field name `aura1_zstd_compressed_bytes` records the measured compressed
zstd payload size. The top-level `input_bytes` for zstd rows is the reference
Aura1 input file size because the benchmark operation takes Aura1 as source and
builds the zstd payload internally.

## Fresh Warm30 Target

Result directory:
`/tmp/aura-benchmarks/compact-research-20260623T193509Z/`

| Dataset | Zstd Level | Zstd Bytes | Aura1 Bytes | Median ms | P95 ms | JSON |
|---|---:|---:|---:|---:|---:|---|
| grimoire-50mb-huff | 1 | 5,771,930 | 36,410,980 | 64.380 | 68.906 | `final_huff_zstd_l1_warm30.json` |
| grimoire-50mb-huff | 3 | 5,542,945 | 36,410,980 | 62.988 | 70.100 | `final2_huff_zstd_l3_warm30.json` |
| grimoire-50mb-huff | 9 | 3,268,730 | 36,410,980 | 56.140 | 57.200 | `final_huff_zstd_l9_warm30.json` |
| grimoire-50mb-nohuff | 1 | 5,767,215 | 36,403,133 | 62.773 | 65.797 | `final_nohuff_zstd_l1_warm30.json` |
| grimoire-50mb-nohuff | 3 | 5,538,275 | 36,403,133 | 62.366 | 69.237 | `final2_nohuff_zstd_l3_warm30.json` |
| grimoire-50mb-nohuff | 9 | 3,264,085 | 36,403,133 | 58.357 | 65.902 | `final_nohuff_zstd_l9_warm30.json` |

## Interpretation

Zstd wins because it inflates already-formed Aura1 bytes. It performs block
decompression, literal copies, and match copies; it does not reconstruct market
data semantics.

Compact Aura0 must still:

- decode 12 semantic streams;
- reconstruct 2,754,892 stream values;
- rebuild grouped, partitioned, segmented-delta, sparse, and presence-derived
  fields;
- pack eight logical fields into every 46-byte Aura1 row.

The relevant product target remains zstd L3. Zstd L9 is not the target stated
by the sprint, but it is a stronger opponent here: it is both smaller and
faster than L3 on these fixtures.

## Decision

Zstd benchmark behavior is fair enough for the compact target:

- same memory output sink;
- no guard or canonical hash in production;
- no row materialization;
- verify runs separate from production.

The compact path must beat 62.988 ms on huff and 62.366 ms on nohuff to pass
the stated L3 target.

Post-commit rerun for `ba3cc23` is in
`/tmp/aura-benchmarks/compact-research-final-ba3cc23/`:

- huff zstd L3: 62.425 ms median, 66.238 ms p95.
- nohuff zstd L3: 63.164 ms median, 65.224 ms p95.

Those post-commit values are the final scoreboard values for the compact
semantic sprint.
