# Aura1 Replay Perf Profile

External CPU profilers were not available in this environment.

Commands tried:

- `which perf; which cargo-flamegraph; which samply`
  - Exit 1; no profiler binaries were found on `PATH`.
- `perf --version`
  - Exit 127; `/bin/bash: line 1: perf: command not found`.
- `cargo flamegraph --version`
  - Exit 101; Cargo reported no `flamegraph` subcommand.

The current evidence therefore comes from internal benchmark timers emitted by
`aura_sdk_bench`:

- `stage_times_ms`
- `counters`
- `stage_sum_ms`
- `runtime_ms`
- `unexplained_ms`
- `unexplained_pct`

The fresh breakdown matrix is at:

- `/tmp/aura-benchmarks/aura1-replay-breakdown-20260624T-ms/sdk_full_matrix_summary.json`

Profiler follow-up:

- Install `perf` or `cargo-flamegraph`.
- Profile `aura1-replay-batch-touch-all`, `aura1-replay-per-row-touch-all`,
  `aura1-read-batches-row-file-range`, and
  `aura1-read-batches-columnar-file-range`.
- Compare top samples against the internal breakdown buckets:
  `all_field_loop_ms`, `field_load_ms`, `aura_value_materialize_ms`, and
  `field_major_decode_ms`.
