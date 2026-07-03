# Aura1 Grouped Eventing

Date: 2026-06-23

## Status

Implemented as an SDK opt-in consecutive-run replay API:

```rust
reader.grouped_replay(&GroupBy::fields(["ts_event", "symbol_id"]), |group| {
    // one callback per consecutive run
    Ok(())
})?;
```

It works with both memory-backed readers and file-backed Aura1 readers opened
with `AuraReader::open_path`/`open_file`. File-backed grouped replay uses the
same fixed-width range-read path as `replay_i64`; it does not copy the whole
Aura1 file into memory before detecting groups.

The current Aura1 implementation compares the selected fixed-width key bytes
directly in the hot loop. It does not allocate a typed key vector per row.
Typed `AuraValue` keys are materialized only when a group is emitted.

## Semantics

- Grouping preserves physical row order.
- Grouping is consecutive-run only; it is not global aggregation.
- Group fields are selected by field name or field ID.
- Field lookups are compiled once before replay.
- Unknown fields reject before replay.
- Unsupported variable-width fields reject through the v1 schema policy.
- High-cardinality data degrades to one-row groups.
- Grouped replay does not change Aura1 file bytes or row semantics.

## Public Types

- `GroupBy`
- `AuraGroupKey`
- `AuraEventGroup`
- `AuraGroupStats`

`AuraEventGroup` exposes:

- `row_start`
- `row_count`
- shared key field names
- shared key values

## Benchmark Interpretation

Grouped replay changes callback semantics. A grouped replay result can be faster
than per-row replay when callback count falls substantially, but it is not the
same amount of callback work.

The benchmark JSON reports:

- `group_by_fields`
- `group_count`
- `groups_per_sec`
- `rows_per_group_avg`
- `rows_per_group_p95`
- `callback_count_reduction`
- `source_kind`
- `replay_backend`
- `bytes_read_at_open`
- `body_bytes_read_at_open`
- `full_file_bytes_copied`

## Targeted Evidence

Fresh targeted matrix:

`/tmp/aura-benchmarks/aura1-parse-speed-final-4bada14/sdk_full_matrix_summary.json`

Selected rows:

| Dataset | Operation | Median ms | P95 ms | Groups | Callback Reduction | P95 Rows/Group |
|---|---|---:|---:|---:|---:|---:|
| `repeated-timestamp` | grouped primary file-range | 1.506 | 1.608 | 3,125 | 32.00x | 32 |
| `repeated-symbol` | grouped symbol file-range | 1.499 | 1.703 | 3,125 | 32.00x | 32 |
| `repeated-timestamp-symbol` | grouped pair file-range | 1.826 | 1.987 | 2,084 | 47.98x | 48 |
| `mixed-burst` | grouped primary file-range | 3.864 | 4.616 | 28,247 | 3.54x | 16 |
| `high-cardinality` | grouped primary file-range | 8.850 | 9.460 | 100,000 | 1.00x | 1 |

Decision: keep grouped replay as an opt-in API. It helps repeated-run datasets
and degrades predictably on high-cardinality data.

## Footer Index Decision

No footer group index is implemented in this sprint. On-the-fly grouped replay
is sufficient to prove the API and semantics. A footer or sidecar run index
would add format surface area and should only be considered if grouped callback
cost remains too high for repeated-run workloads.
