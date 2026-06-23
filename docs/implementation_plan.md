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
- `AuraRecordBatch` and `AuraValue`
- `AuraWriter`
- `AuraReader`
- `convert_aura`
- `WriterOptions`, `ReaderOptions`, and `ConvertOptions`
- examples for writing, reading, and conversion

Supported through the SDK:

- fixed-width scalar schemas
- timestamps in nanos or micros
- signed and unsigned integer physical types
- scaled i64 prices
- enum/flags integer fields
- Aura0 compact output
- Aura1 fixed-width output
- cross-format conversion through the existing generic i64 engine

Explicitly rejected for now:

- nullable fields
- floats
- binary and UTF-8 variable-width fields
- values outside declared integer ranges

## Implementation Boundary

The public SDK facade is schema-generic, but it still targets the existing generic i64 physical engine internally. This is deliberate for the first library milestone: unsupported physical types fail before encoding instead of silently corrupting values.

The current SDK does not make `aura-bench` the product API. Benchmark code remains a consumer of lower-level record helpers.

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

- Replace the SDK's row-oriented `Vec<Vec<AuraValue>>` batch with typed column arrays for large writes.
- Make `CompiledAuraPlan` the public SDK plan type rather than exposing only the lower-level compiled footer through the reader.
- Add generic benchmark matrix operations for generated schemas.
- Add property-based generators for broader dynamic schema coverage.
- Implement nullable/variable-width support only after the binary layout has explicit presence and offset encoding for those types.
