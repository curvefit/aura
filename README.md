# Aura

Aura is a binary event-file library and format laboratory. It provides a
generic, versioned container for normalized facts, compact cold storage, and
replay-oriented storage without tying the format to a venue or dataset name.

The current implementation has two deliberately separate surfaces:

* V2 is the default production container used by the existing SDK writer,
  reader, and conversion paths. It remains the compatibility baseline for
  `.aura`, `.aura0`, and `.aura1` files.
* V3 has one complete, explicitly bounded Aura0 subset: flat event-scoped
  fields encoded as exact-value blocks, with a self-contained schema/footer,
  checksums, hashes, a seekable writer/reader, and the developer CLI. V3 is
  usable for the supported subset, but it is not a claim that every Aura
  relationship or order-book layout is ready.

The public model is intentionally generic:

- `.aura` is the V2 ingest/preservation profile: normalized logical facts and
  seal-time statistics where available;
- a versioned schema header can declare positional fields, relationships,
  derived-expression references, repeated groups, timestamps, and opaque
  streams;
- `.aura0` is a compact cold-storage profile. The established V2 writer uses
  compiled instructions; the explicit V3 subset uses exact-value blocks;
- `.aura1` is the V2 replay-oriented fixed/block profile.

## Format Levels

| File | Current container | Purpose |
|---:|---|---|
| `.aura` | V2 ingest | Normalized facts plus seal-time optimization stats when known. |
| `.aura0` | V2 or explicit V3 | V2 compact instruction streams, or the supported V3 exact-value subset. |
| `.aura1` | V2 | Replay-oriented fixed/block encoding compiled from the V2 plan. |

The V2 levels trade disk for parsing speed. The generic V2 writer emits V2
containers even when its profile is `.aura0` or `.aura1`; V3 is selected only
through the V3 API or CLI. A V3 flat Aura0 file is self-contained and is not
converted by the V2 compiled-profile conversion path.

## Repository Scope

Aura contains generic binary codec mechanics and research prototypes:

- varint and zigzag delta encoding,
- fixed-width replay records,
- dynamic padded level blocks,
- chunk directories for independent compression frames,
- ingest-to-compiled conversion paths,
- synthetic benchmark inputs.

It does not include venue-specific adapters, private source semantics, capture
daemons, or production data. A caller or external schema author supplies the
logical schema; Aura validates it and, where a planner is supported, selects
physical instructions from the declared relationships and observed values.
Dataset names and venue labels are not part of the V3 flat seal decision.


## Docs

- [SDK](docs/SDK.md) explains the public schema, writer, reader, and converter API.
- [Schema API](docs/SCHEMA.md), [writer API](docs/WRITER.md), [reader API](docs/READER.md), [conversion API](docs/CONVERSION.md), and [errors](docs/ERRORS.md) document the current library surface.
- [Format levels](docs/tiers.md) explains ingest, Aura0, and Aura1.
- [Aura container](docs/container.md) explains the header/body/footer shape.
- [Format specification](docs/FORMAT.md) records the V2 and V3 wire layouts,
  compatibility boundary, and current limits.
- [Field programs](docs/field-programs.md) explains compact decode instructions.
- [Schemas](docs/schemas.md) explains logical schema construction.
- [Shadow Arrow protocol](docs/SHADOW_PROTOCOL.md) specifies the offline external-compiler boundary and safe reference-block publication.
- [Chunked storage](docs/chunking.md) explains independent compression chunks.
- [Compression policy](docs/compression.md) explains why chunks beat whole-file streams.
- [Aura1 block padding](docs/hot-padding.md) explains fixed-width replay blocks.
- [Conversion flow](docs/conversion.md) explains compiled materialization.
- [Naming](docs/naming.md) lists prototype file extensions and magic values.

## Quick checks

```bash
cargo test
cargo run --bin aura -- schema validate --input schema.json
cargo run --bin aura -- schema canonicalize --input schema.json --output canonical-schema.json
cargo run --bin aura -- shadow handshake --protocol aura-logical-arrow-ipc-v1 --json
cargo run --release --bin aura -- v3 aura0 seal \
  --protocol aura-logical-arrow-ipc-v1 \
  --schema <canonical-schema.json> \
  --output <new-file.aura0> --json < <arrow-ipc-stream.bin>
cargo run --release --bin aura -- v3 aura0 verify \
  --input <new-file.aura0> --json
cargo test --test v3_aura0_container
cargo run --bin aura-size -- 10000 1 8
cargo run --example roundtrip
```

The V3 seal command reads one Arrow IPC stream from standard input and requires
the schema file to already be Aura's canonical external schema JSON. It writes
an absent `.aura0` destination through a temporary file, syncs it, and
publishes it atomically. Verification reopens the complete file and checks the
header, footer, schema fingerprint, per-chunk stored and logical hashes, body
hash, statistics, global logical hash, and exact file length. A failed or
interrupted seal is not a valid artifact; an existing destination is never
silently replaced by this command.

The supported V3 flat subset is intentionally narrow: event-scoped fields,
exact fixed and variable values, optional validity bitmaps, no groups,
repeated fields, derived expressions, or byte-200 dual-domain execution, and
no V3 Aura1 conversion. The format ceilings are a 16 MiB front header, 64 MiB
footer, 1 TiB body, 65,536 chunks, 16,777,216 rows, 1 GiB per exact-value
block, and 16 MiB per variable value. Default API limits are lower (256 MiB
body, 4,194,304 rows, 4,096 chunks, and 256 MiB per block); the hard limits
are an explicit API choice and remain capped by the format ceilings. The
reader treats files as untrusted input and rejects malformed lengths, hashes,
types, offsets, UTF-8, decimal text, and trailing data before trusting the
decoded result. These limits and checks are safety ceilings, not performance
claims.

V3 schema headers can describe relationships and groups for future planning,
but the complete flat Aura0 writer rejects those declarations until a later
container/profile contract proves their exact event-to-child semantics. In
particular, Flag 200 is a V3 discriminator declaration, not evidence that
order-book body execution is supported today. Aura0 is therefore a usable
shadow/fallback artifact for its flat subset, not yet a blanket replacement
for Parquet in Grimoire.
