# SDK Blocker Resolution Plan

## 1. Current SDK Architecture Map

The SDK facade currently sits on top of the existing generic i64 record engine:

- `src/schema.rs`
  - `AuraSchema`, `AuraSchemaBuilder`, `AuraField`, `AuraType`
  - wraps `SchemaDescriptor`
- `src/types.rs`
  - `AuraValue`
  - row-oriented `AuraRecordBatch`
- `src/writer.rs`
  - `AuraWriter<W>`
  - buffers SDK rows, emits ingest, then compiles to requested profile
- `src/reader.rs`
  - `AuraReader`
  - decodes the full file into rows and exposes schema plus one batch
- `src/convert.rs`
  - `convert_aura`
  - decodes enough metadata to convert between supported profiles
- `src/program.rs`
  - `CompiledAuraPlan`
  - already represents the compiled footer conversion plan, but is not exported as the SDK's central public plan

Benchmark-only logic still lives primarily in `src/bin/aura_bench.rs`. SDK work must not make that binary the product API.

## 2. Public API Invariants

- Public SDK calls return `AuraError`/`Result`.
- Unsupported schema features reject before bytes are written.
- Schema field order is the writer's canonical order.
- Field names, physical types, logical roles, scales, and nullability flags must roundtrip.
- `Aura0` default remains compact.
- Fast/hybrid profiles remain explicit opt-in.
- Public APIs must not require grimoire fixtures, grimoire field names, eight fields, twelve streams, or a 46-byte row.

## 3. Dynamic Schema Invariants

For v1 SDK support:

- Supported fields are fixed-width scalar values representable by the existing i64 engine.
- Field IDs are unique and stable inside `AuraSchema`.
- Field names are unique.
- Nullable, floats, binary, and UTF-8 reject explicitly.
- Reordered fields are valid.
- The legacy compact timestamp shortcut only applies to slot 0; reordered timestamps are stored as normal fixed-width timestamp values.

## 4. Reader/Writer/Converter Dataflow

Writer:

1. Validate `AuraSchema`.
2. Accept row or column batches.
3. Convert SDK values to positional i64 rows in schema order.
4. Encode sealed ingest through `I64FileInput`.
5. Compile to target profile if requested.

Reader:

1. Read sealed Aura bytes.
2. Decode profile and footer.
3. Build schema facade and compiled plan.
4. Expose schema before batches.
5. Expose batch iteration over decoded rows for v1.

Converter:

1. Decode source footer/schema once.
2. Convert through existing record helpers.
3. Preserve schema and rows.
4. Verify row equality when requested.

## 5. CompiledAuraPlan Ownership Model

`program::CompiledAuraPlan` is the canonical compiled conversion plan. This sprint should:

- re-export it from the crate root
- add `CompiledAuraPlan::from_schema(&AuraSchema)` for SDK planning without file bytes
- expose `AuraWriter::compiled_plan()`
- expose `AuraReader::compiled_plan()`
- use plan data for SDK-visible width/offset/hash proof

The existing `CompiledFooter` remains the serialized footer representation. `CompiledAuraPlan` is the runtime execution contract.

## 6. Batch API Decision

Support both:

- Keep `AuraRecordBatch` for simple row-oriented examples and tests.
- Add `AuraColumnBatch`, `AuraColumn`, and `AuraColumnBatchBuilder` for Parquet/Arrow-like usage.

Column batches should validate:

- all schema fields present exactly once
- lengths match
- column type matches schema field type
- extra/missing columns reject
- input column order can differ from schema order

The writer should accept both row batches and column batches. Internally v1 may still lower to positional i64 rows.

## 7. Streaming Reader Design

Minimal v1:

- Add `AuraReader::next_batch(batch_size)` or equivalent cursor API.
- Add `AuraReader::batches(batch_size)` iterator.
- Add Aura1 replay visitor over decoded rows now, with a documented limitation that the current reader still decodes the file through existing row materialization.

True non-materializing readers require deeper decode APIs. That is an implementation gap, not a format blocker.

## 8. Generic Benchmark Matrix Design

Add SDK-level benchmark support independent of grimoire:

- generated tiny
- generated narrow
- generated wide
- generated reordered
- generated dense
- generated sparse
- generated edge-case

Operations:

- write Aura1
- write Aura0 compact
- read Aura1
- Aura0 to Aura1
- Aura1 to Aura0
- zstd comparison where applicable

The smoke matrix can be a test-friendly command; full JSON artifacts should go under `/tmp/aura-benchmarks/sdk-generic-<timestamp>/`.

## 9. Metadata Preservation Design

Current gap:

- field metadata roundtrips
- rows roundtrip
- schema display name/schema ID are rewritten by compiled profile paths

Minimal resolution:

- preserve schema name through the SDK by ensuring compiled profile paths carry the footer schema descriptor name
- assert field metadata roundtrips through both profiles and conversions
- document which metadata participates in schema hash

If schema ID cannot be preserved without changing existing hash semantics, defer with exact blocker.

## 10. Unsupported v1 Features

Remain unsupported for v1 and must reject clearly:

- nullable fields
- variable-width `Utf8`
- variable-width `Binary`
- floats until the physical encoding is specified
- unsigned `U64` values above `i64::MAX` because the current engine stores the physical lane as i64

## 11. Subagent Dependency Graph

Read-only lanes first:

1. API/test discovery: public API gaps and assertions.
2. Plan/metadata discovery: where schema name/id is lost and how `CompiledAuraPlan` is built.
3. Benchmark discovery: smallest reusable generated-schema benchmark path.

Implementation lanes:

1. Public plan exposure and metadata preservation.
2. Columnar batch API.
3. Batch iteration/replay API.
4. Generic fixture/property tests.
5. Generic benchmark smoke operation.
6. Docs/examples/defaults.

## 12. Commit Plan

1. `Docs SDK blocker resolution plan`
2. `Expose SDK compiled plan and metadata`
3. `Add typed columnar Aura batch API`
4. `Add streaming AuraReader batch API`
5. `Add generic SDK fixtures and benchmarks`
6. `Add Aura SDK examples and docs`
7. `Document Aura SDK defaults and v1 limitations`

Each commit requires:

- `git diff --check`
- `cargo check`
- `cargo check --examples`
- targeted tests
- `cargo test`
- `cargo build --release --bin aura-bench`

## Minimal SDK Definition

The minimal SDK that feels like a Parquet writer module is:

- dynamic schema builder
- row and column batch types
- writer with `write_batch`/`finish`
- reader with schema-first access and batch iteration
- converter with explicit target format
- public compiled plan for introspection
- clear unsupported feature errors
- examples that compile as crate consumers

## Benchmark-Only Code Movement

Do not move grimoire-specific benchmarking into SDK APIs. Reuse only generic helpers and keep grimoire paths as benchmark fixtures.

## Remaining Grimoire Risks

The core generic i64 engine is already schema-driven, but some compact optimizations prefer timestamp-at-field-0. SDK tests must keep reordered and non-grimoire schemas to prevent those assumptions from leaking into the public API.
