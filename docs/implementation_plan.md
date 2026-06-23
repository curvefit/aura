# Aura SDK Implementation Plan

## Goal

Expose Aura as a reusable Rust library for dynamic fixed-width market-data schemas:

- define a schema at runtime
- write `.aura`, `.aura0`, or `.aura1`
- read files and recover schema plus rows
- convert between supported formats
- keep benchmark-specific grimoire code out of the public API

## Current SDK Scope

Implemented:

- `AuraSchema`, `AuraSchemaBuilder`, `AuraField`, and `AuraType`
- `AuraRecordBatch`, `AuraColumnBatch`, `AuraColumn`, and `AuraValue`
- `AuraWriter`
- `AuraReader`
- `convert_aura`
- `WriterOptions`, `ReaderOptions`, and `ConvertOptions`
- public `CompiledAuraPlan`
- examples for writing, columnar writing, reading, batch iteration, replay, and conversion
- generic SDK fixture generation and smoke benchmark metadata

Supported through the SDK:

- fixed-width scalar schemas
- timestamps in nanos or micros
- signed and unsigned integer physical types
- scaled i64 prices
- enum/flags integer fields
- Aura0 compact output
- Aura1 fixed-width output
- cross-format conversion through the existing generic i64 engine
- schema name, schema ID, field metadata, schema hash, and plan hash preservation for SDK writes/conversions

Explicitly rejected for now:

- nullable fields
- floats
- binary and UTF-8 variable-width fields
- values outside declared integer ranges

## Implementation Boundary

The public SDK facade is schema-generic, but it still targets the existing generic i64 physical engine internally. This is deliberate for the first library milestone: unsupported physical types fail before encoding instead of silently corrupting values.

The current SDK does not make `aura-bench` the product API. Benchmark code remains a consumer of lower-level record helpers.

The reader now exposes true batch iteration and replay APIs. Aura1 rows are visited from the fixed-width body without full-file row materialization. Aura0 compact opens from metadata and lazily builds bounded row batches from compact stream columns.

## Verification Plan

Required local checks:

- `cargo check`
- `cargo check --examples`
- `cargo test`
- `cargo test --test aura_bench_cli -- --nocapture`
- `cargo test --test footer_preservation -- --nocapture` when present
- `cargo test --test writer_reader_api -- --nocapture` when present
- `cargo build --release --bin aura-bench`
- run SDK examples

## Remaining Work

- Add true Aura0 row-group streaming if compact v2 adds row-group stream offsets.
- Add property-based random generators beyond the deterministic fixture families.
- Implement nullable/variable-width support only after the binary layout has explicit presence and offset encoding for those types.
