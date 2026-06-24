# Aura1 Replay Millisecond Breakdown Plan

## Current Operation Map

Aura1 replay benchmarks now separate replay from raw scans, view construction,
parse kernels, and materialized reads. The next step is to explain elapsed time
with non-overlapping stage timers and counters for each operation family.

| operation | work included | work excluded | expected bottleneck | needed timers |
|---|---|---|---|---|
| aura1-replay-per-row-noop | Opens file-backed Aura1, builds row views in original order, calls one callback per record, touches no fields | Raw scan, batch callback, field decode, materialization | callback dispatch and row loop | setup, row slice setup, row view construction, callback, loop overhead |
| aura1-replay-per-row-touch-selected | One callback per record, selected fields chosen from CompiledAuraPlan/schema roles, checksum depends on selected values | Unselected fields, materialized rows/columns | callback count plus field loads/checksum | setup, selected recipe lookup, row slice setup, field load, checksum, callback/loop residual |
| aura1-replay-per-row-touch-all | One callback per record, every fixed-width field read and mixed into checksum | Materialization, batch/group callback | per-row callback plus all-field load/checksum | setup, row slice setup, field load, checksum, callback/loop residual |
| aura1-replay-batch-noop | One callback per fixed-width batch, creates borrowed batch views, touches no fields | Field decode, row materialization | batch range read and view construction | setup, range validation/read, view construction, batch callback, advance |
| aura1-replay-batch-touch-selected | One callback per batch, selected field loops read every row, checksum depends on selected values | Unselected fields, row/column materialization | selected field loop and checksum | setup, range validation/read, recipe setup, selected loop, checksum/loop residual, callback |
| aura1-replay-batch-touch-all | One callback per batch, all fields read by type-specialized checksum kernel | Materialized read, per-row callbacks | type-kernel all-field loop | setup, range validation/read, type-kernel dispatch, all-field loop, checksum/loop residual, callback |
| aura1-replay-grouped-touch-selected | Consecutive group detection, one callback per group, selected field rows touched | Global grouping, materialization | key comparison and high-cardinality callbacks | group recipe setup, key compare/boundary detection residual, grouped field touch, group callback |
| aura1-replay-grouped-touch-all | Consecutive group detection, one callback per group, all fields in each group touched | Global grouping, materialization | group detection plus all-field group scan | group recipe setup, key compare/boundary detection residual, grouped field touch, group callback |
| aura1-replay-grouped-aggregate | Consecutive group detection, aggregate selected fields per group | Full all-field decode unless selected by aggregate | group detection plus aggregate loop | group recipe setup, boundary detection, aggregate loop, group callback |
| aura1-scan-raw-file-range | File-backed body range read and minimal byte touch | Replay callbacks, field decode | file-range read and raw loop | raw validation, body read, raw loop/checksum |
| aura1-batch-view-only-file-range | Fixed batch view construction and callback only | Field access, parse | range read and callback count | range validation/read, view construction, callback, advance |
| aura1-batch-touch-all-fields-type-kernel-file-range | Parse kernel reads all fields and checksums inside batch callback shape | Consumer-facing replay semantics beyond callback wrapper, materialization | type-kernel parse loop | range read, kernel dispatch, all-field loop |
| aura1-read-batches-row-file-range | Materializes AuraRecordBatch rows and AuraValue values | Replay-only view APIs | AuraValue creation and row Vec allocation | row batch alloc, row Vec alloc, value materialization, field decode, row push |
| aura1-read-batches-columnar-file-range | Materializes AuraColumnBatch typed vectors | Row values and callbacks | typed Vec allocation/write and field-major decode | column alloc, typed Vec alloc, field-major decode, column write |

## Answers Required Before Instrumentation

1. True replay operations are the per-row, batch, and grouped `aura1-replay-*`
   operations. Raw scan, view-only, parse kernels, row batches, and column
   batches are controls and must not be reported as replay.
2. Parse kernels touch bytes and checksum fields but are only replay-shaped when
   wrapped in a consumer callback with explicit callback counts.
3. Materialized reads allocate `AuraRecordBatch` or `AuraColumnBatch`; their cost
   should be reported separately from replay.
4. View construction only creates borrowed fixed-width views and callback calls;
   it is not parse speed.
5. Per-row operations call the user callback once per record.
6. Batch operations call the user callback once per fixed-width batch.
7. Grouped operations call the user callback once per consecutive run.
8. Selected-field operations choose fields from `CompiledAuraPlan` and schema
   logical roles, then touch those fields for every row.
9. All-field operations read every field in schema order for every row.
10. Row batches allocate row vectors and `AuraValue` payloads; column batches
    allocate typed vectors. Replay paths should not allocate per row.
11. Row batch materialization creates `AuraValue`s. Replay and parse kernels
    should not.
12. File-backed replay reads header/footer at open and body ranges during replay.
13. Field offsets, widths, and physical load kinds come from `CompiledAuraPlan`
    and the Aura1 fixed layout, not field names.
14. The type-specialized kernel dispatches by precomputed physical width groups
    and is a parse kernel/control unless used by a replay callback operation.
15. Grouped replay uses schema-selected consecutive group fields and raw key
    comparisons for Aura1.

## Timer Strategy

The harness will record a representative iteration's stage tree and counters.
For summed timers, parent totals are not included in `stage_sum_ms`; residual
loop overhead is computed as `runtime_ms - measured_child_ms` where a callback
API hides nested work. Diagnostic timers will be labeled separately and excluded
from summed totals.

The JSON output must include:

- `stage_times_ms`
- `counters`
- `timer_tree_kind`
- `stage_sum_ms`
- `runtime_ms`
- `unexplained_ms`
- `unexplained_pct`

The first target is explanation, not optimization. Optional micro-optimizations
should only happen after the breakdown identifies a dominant cost.
