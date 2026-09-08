# Aura1 explicit-event replay campaign

Status: discovery; no performance claim or production activation.

The original Aura checkout was clean at
`420052ceb50ea7a720dbf6bf46ebd6e4d3d61634`, but its `main` is obsolete divergent
history. This isolated campaign branch starts at maintained/deployed SDK revision
`cc04f75c9217e6c6a575145ab0ed98df232f622d` (actual remote `curvefit/aura`).
Existing worktrees and production services were preserved.

Current call-graph finding: generic fixed-row replay already has borrowed views,
compiled load recipes, reusable book engines and a fused mutation path. The fused
path deliberately excludes timestamps, flags, sequences and order IDs and has no
explicit-event boundary callback. It is therefore not an equivalent full-fact
consumer for retained Grimoire explicit events.

`records::decode_i64_events_file` on Aura1 first materializes every fixed row,
then reconstructs events and copies their child fields. `AuraReader::open_memory`
also calls this complete decoder for explicit-event validation and discards the
result. This is a concrete allocation/copy hypothesis, pending fresh profiling.

All heavy builds, tests and measurements acquire
`/tmp/lean-node-20260907-1000/bench.lock`; w01 schedules derived-worker quiescence.
Live collectors and receiver remain running. No storage-format change is planned.
