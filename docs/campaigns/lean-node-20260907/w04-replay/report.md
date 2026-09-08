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

## First measured profile

Before changing the decoder, the maintained explicit-event decoder and SDK open
were measured under the shared lock on two retained historical parent projections.
These are a screening corpus, not full production book schemas. All events,
children, schema and header facts matched independently decoded Aura0. One warmup
and three repetitions were run; individual results are in `baseline.json`.

| Sample | Events | Level updates | Decode median ms | Full consume median ms | SDK open median ms |
| --- | ---: | ---: | ---: | ---: | ---: |
| retained ETH parent | 4,307 | 113,461 | 8.896 | 10.770 | 10.428 |
| retained BTC parent | 3,046 | 227,239 | 15.918 | 19.913 | 21.704 |

The consume operation decodes and black-box consumes every returned value. The
open operation charges owned input copying and all required SDK validation.
Decoding dominates the measured useful consumption stage. No external CPU/heap
profiler was installed; CPU was measured with Linux schedstat. RSS samples include
the independent reference event vectors, and are not decoder-only peak memory.
Seven focused tests passed against the baseline before the optimization.

A corpus discovery child violated its no-heavy-job instruction and ran four native
receipt verifications without the shared lock around 00:20:48–00:21:31 UTC. No
exact subprocess timestamps were retained. This was reported to the integration
owner; overlapping timing must be excluded. The w04 baseline above ran afterward
under the lock.
