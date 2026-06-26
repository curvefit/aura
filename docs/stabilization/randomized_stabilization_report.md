# Randomized Stabilization Report

Date: 2026-06-25

## Scope

This pass verifies the rough copy against deterministic randomized schemas and batches. It does not start an optimization sprint, DBN-parity sprint, Aura0-vs-zstd sprint, or speculative format-design change.

## Randomized Matrix

The integration test `tests/randomized_stabilization.rs` covers:

| Test | Coverage |
| --- | --- |
| `randomized_aura1_roundtrip` | Aura1 write/read on 100 generated schemas |
| `randomized_aura0_roundtrip` | Aura0 compact write/read on 100 generated schemas |
| `randomized_cross_format_roundtrip` | Aura0 -> Aura1 and Aura1 -> Aura0 conversions |
| `randomized_streaming_batches` | Aura0/Aura1 batch sizes 1, 2, 7, 127, 128, 129, 8192 |
| `randomized_replay_hash` | Aura1 row replay and fixed-batch replay hashes |
| `randomized_orderbook_replay_if_schema_compatible` | Random compatible orderbook schemas with reordered non-market names |
| `randomized_grouped_replay_if_available` | Grouped replay on generated schemas |
| `randomized_unsupported_features_reject` | Nullable, Utf8, Binary, F32, and F64 rejection |
| `randomized_corrupt_aura1_rejects` | Random Aura1 files with invalid magic/header/body/footer/seal/version |
| `randomized_corrupt_aura0_rejects` | Random Aura0 files with invalid magic/header/body/footer/seal/version |
| `randomized_corrupt_footer_rejects` | Footer corruption on both compact and replay formats |
| `randomized_invalid_orderbook_schema_rejects` | Missing required roles and invalid side values |

The ignored stress test `randomized_stress_1000_seeds` is available for longer local runs.

The user-facing verifier also passed:

```bash
cargo run --bin aura-verify-random -- --cases 1000 --max-records 10000 --formats aura1,aura0 --check-cross-format --check-streaming --check-replay --check-orderbook --seed 176609594574376 --output /tmp/aura-random-verify/report.json
```

Result: 1000/1000 generated cases passed, 200 compatible orderbook cases passed, and the `failures` array was empty.

## Generated Schema Coverage

- Field counts: 1 through 32.
- Record counts: 0, 1, 2, 3, 16, 127, 128, 129, 1024, plus random counts up to the configured maximum.
- Field order: natural, shuffled, and orderbook-compatible shuffled layouts.
- Field names: non-market names by default; orderbook tests use explicit non-grimoire role names.
- Values: repeated values, constants, monotonic and duplicate timestamps, large timestamp gaps, min/max integer boundaries, dense few-symbol cases, sparse many-symbol cases, flags, enums, and zero-size orderbook removes.

## Fixes Made

- `src/stats.rs`: rough-step residual and gap accounting now avoids debug overflow on extreme legal i64 deltas.
- `src/schema.rs`: timestamp fields outside slot 0 now fall back to the generic header marker instead of rejecting the schema.
- `src/writer.rs`: SDK writer omits the default field-name header comment when it would exceed the encoded header limit.
- `src/records.rs`: ingest stamping treats the generic compact plan as optional when a legal schema/value set cannot be represented by that planner.
- `src/random_verify.rs`: added deterministic reusable randomized verification helpers.
- `src/bin/aura_verify_random.rs`: added a user-facing randomized verification command that emits JSON reports and exits nonzero on failures.
- `tests/randomized_stabilization.rs`: added the randomized roundtrip, replay, orderbook, unsupported-feature, and corruption tests.

## Accepted V1 Limitations

- Nullable fields are rejected.
- Utf8 and Binary fields are rejected.
- F32 and F64 fields are rejected.
- Orderbook APIs require explicit field mapping by name through `OrderBookDeltaSpec`.
- Some compact generic-plan paths remain optional; files can fall back to stamped legacy compact/replay plans when the generic compact plan cannot represent an edge column.

## Remaining Bugs

No known randomized correctness bug remains after this pass.

## Next Heavy Research Loops

- Compact Aura0 semantic decode speed.
- Orderbook apply engine.
- Aura1 grouped replay/indexing.
- Full DBN parity, if desired.
- Variable-width/null v2.
- Metadata/symbology v2.
