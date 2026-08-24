# Aura Schema API

`AuraSchema` is the SDK schema object. Build one at runtime:

```rust
let schema = aura_codec::AuraSchema::builder()
    .field("ts_event", aura_codec::AuraType::TimestampNanos)
    .field("symbol_id", aura_codec::AuraType::U32)
    .field("price", aura_codec::AuraType::PriceI64Scaled { scale: 9 })
    .build()?;
# Ok::<(), aura_codec::AuraError>(())
```

The current V2 SDK writer supports fixed-width scalar values backed by the
generic i64 physical engine: booleans, signed/unsigned integer widths,
timestamp nanos/micros, scaled i64 values, enum u8, and flags u32. This is the
default production schema path and emits V2 containers.

Unsupported V2 SDK types reject during schema build or the typed writer
boundary:

- nullable fields
- `Utf8`
- `Binary`
- `F32`
- `F64`

These restrictions remain frozen for current V2 file writers. The explicit V3
exact-value API additionally supports `I128`, `Opaque16`, `TimestampMillis`,
`Utf8`, and `DecimalText` without projecting them to a different logical type.
V3 exact-value files may use nullable fields; null is represented by a validity
bitmap and is never replaced with a sentinel or placeholder. V3 nullability is
not retrofitted into the V2 SDK writer.

Schema field order is the canonical storage order. Schema name, schema hash, field names, logical roles, physical types, scale, and nullability flags are preserved through SDK Aura0/Aura1 writes and conversions.

## Aura V3 group declarations

Aura V3 adds an explicit schema dialect and versioned group descriptors. The
public `SchemaBuilder` can construct a V3 schema without a dataset-specific
Rust type:

```rust
use aura_codec::{
    FieldRole, FieldType, RelationshipPermissions, SchemaBuilder,
};

let relationships = RelationshipPermissions::none()
    .with_split()
    .with_within_domain()
    .with_across_domain_same_field()
    .with_joint_same_field();

let schema = SchemaBuilder::new("dual-domain-levels")
    .v3()
    .field("ts", FieldType::TimestampNs, FieldRole::Timestamp)
    .repeated_field("side", FieldType::U8, FieldRole::Side)
    .repeated_field("price", FieldType::I64, FieldRole::Price)
    .repeated_field("quantity", FieldType::I64, FieldRole::Quantity)
    .dual_domain_repeated_group(1, vec![1, 2, 3], 1, relationships)
    .finish()?;
# Ok::<(), aura_codec::AuraError>(())
```

Every V3 group is a disjoint column subset of the one repeated child row for an
event. Groups share the event's authoritative child count; they do not create
independent repeated arrays. Child slots are strictly increasing in global
field order.

In the V3 one-byte-per-field relationship map, byte `200` consumes and marks
the discriminator field itself. Group membership comes from the descriptor
table. V2's zero-width `200` followed by `201..239` remains a separate legacy
dialect and is never reinterpreted as V3.

Byte `100` remains the unique primary timestamp relationship marker and is
valid only for event-scoped slot 0. V3 may declare additional event-scoped
timestamp-role fields at later slots; their front-map byte is `255`
(`do-not-attempt`). The tag-4 full schema and canonical schema JSON remain
authoritative for every field's logical type, role, scale, name, and
nullability, so the auxiliary timestamps are not erased or coerced by the
compact relationship map.

V3 timestamp roles accept `timestamp_ns` at scale 0, `timestamp_ms` at scale 0,
and `i64` in the existing scale-0 generic form or scale -6
`TimestampMicros` form. Units remain encoded by `FieldType` and scale rather
than the relationship byte. Every timestamp-role field must be event-scoped;
text, opaque, boolean, and other non-timestamp physical types reject. This does
not alter the V2 relationship-map dialect or bytes.

Relationship flags are validated permissions; they do not select a codec or
exact residual direction in either complete V3 profile. The V2 planner may use
them when compiling a V2 footer, but V3 does not run a physical relationship
planner, choose compression, or emit Plan v2. The V3 flat Aura0 writer accepts
only flat event schemas with no groups, repeated fields, derived expressions,
or byte-200 discriminator. The V3 grouped writer accepts exactly one repeated
dual-domain group whose child slots cover all repeated fields, whose
discriminator is the non-null U8 `side` field marked by byte `200`, and whose
event/child values are encoded exactly. Both complete V3 writers reject
non-empty derived-expression tables. V2 writers reject V3 schemas rather than
emitting cross-wired files.

### V3 exact field types

Field type codes 1 through 11 are frozen. V3 appends, without renumbering:

| Code | JSON name | Contract |
| ---: | --- | --- |
| 12 | `timestamp_ms` | exact signed i64 milliseconds; role `timestamp`; scale zero; numeric timestamp candidates allowed |
| 13 | `utf8` | exact UTF-8 bytes; scale zero; no relation or derived-expression participation; `absolute` only |
| 14 | `decimal_text` | exact UTF-8 decimal spelling; the same structural restrictions as `utf8` |

`Opaque16` likewise has scale zero, no relation, `absolute` as its only
candidate, and no derived-expression input/output participation. It is
preserved exactly by the V3 value block but is never exposed as a numeric
transform candidate. The V2 SDK typed writer has the narrower V2 body
limitation described above.

`Utf8` has no provider-specific role restriction: any otherwise appropriate
logical role is allowed. `DecimalTextV1` validates semantics after Unicode
outer trimming but retains every original byte. “Outer whitespace” is frozen
to the Unicode 15.1 `White_Space` set: U+0009..U+000D, U+0020, U+0085, U+00A0,
U+1680, U+2000..U+200A, U+2028, U+2029, U+202F, U+205F, and U+3000. U+180E,
U+200B, and U+FEFF are not whitespace in this grammar. It accepts an optional
sign, at most one decimal point, and requires at least one ASCII digit. Thus
`+1.2`, `-0`, `.5`, `5.`, leading/trailing zeros, and Unicode outer whitespace
are valid and preserved. Exponents, internal whitespace, Unicode digits,
`NaN`/`Inf`, empty strings, and sign- or dot-only strings reject.

The standalone exact-value block v1 is intentionally flat and event-scoped.
Repeated fields and non-empty group declarations reject because its single row
count cannot represent event-to-child boundaries; child rows are never
flattened under the event row count. This block is the body block used by the
complete V3 flat Aura0 container, which adds the V3 header, footer, chunk table,
second-pass statistics, hashes, and final seal around it (the flat CLI adds
atomic publication). The grouped
Aura0 profile instead uses `AURAV3EB` event blocks with authoritative
`event_count + 1` child offsets, scoped event/repeated columns, validity
bitmaps, and source-order logical hashing.

## Canonical external schema JSON v1

`SchemaDescriptor::from_json` parses the versioned, provider-independent V3
schema declaration. `SchemaDescriptor::to_canonical_json` emits Aura's
deterministic pretty-JSON form. This is an Aura schema format, not RFC 8785/JCS.

```json
{
  "schema_format": "aura-schema",
  "schema_version": 1,
  "schema_encoding": "v3",
  "name": "golden",
  "schema_id": 106027077,
  "fields": [
    {
      "id": 0,
      "name": "value",
      "type": "i64",
      "role": "value",
      "scale": 0,
      "scope": "event",
      "nullable": false,
      "relation": { "kind": "none" },
      "transform_candidates": ["absolute"]
    }
  ],
  "groups": [],
  "derived_expressions": []
}
```

`schema_id` may be omitted on input and is always emitted. When supplied,
it must match the ID computed from the validated binary V3 descriptor.

The parser is strict:

- unknown and duplicate object keys reject at every object level;
- enum spellings are exact and case-sensitive;
- fields canonicalize by stable field ID, groups by group ID, and derived
  expressions by expression ID;
- group child-slot order remains semantic and must already follow global field
  order;
- transform and relationship arrays reject duplicates and emit in registry
  order;
- `dual_domain` is required for each group and is either an object or explicit
  `null`;
- a byte-200 discriminator must agree with exactly one dual-domain group; and
- malformed relations, cycles, overlaps, stale schema IDs, BOMs, trailing data,
  and excessive nesting reject.

Input and canonical output, including the single trailing newline, are limited
to 16 MiB. Parsing and emission use checked, fallible allocation and reject
documents outside the schema-JSON resource envelope. Canonicalization preserves
exact UTF-8 strings; it does not Unicode-normalize names.

Use the unified developer CLI without editing Rust source:

```bash
aura schema validate --input schema.json
aura schema validate --input schema.json --json
aura schema canonicalize --input schema.json
aura schema canonicalize --input schema.json --output canonical-schema.json
```

File output validates and serializes before promotion, rejects symlink and
non-regular destinations, writes a destination-local exclusive temporary file,
syncs its contents, and atomically renames it on the current Unix development
platform. The complete flat V3 Aura0 CLI uses the same canonical JSON contract:

```bash
cargo run --release --bin aura -- v3 aura0 seal \
  --protocol aura-logical-arrow-ipc-v1 \
  --schema <canonical-schema.json> \
  --output <new-file.aura0> --json < <arrow-ipc-stream.bin>
cargo run --release --bin aura -- v3 aura0 verify \
  --input <new-file.aura0> --json
```

Flat protocol v1 also accepts explicit development mode `--mode planned`.
This does not change the schema contract or default exact command; it asks the
bounded all-memory planner to score complete exact/fixed/mixed files and may
still select the byte-identical exact fallback.

The schema author supplies the logical field declarations and relationship
permissions. Aura's pinned implementation validates them; the decoder does not
infer a dataset, venue, or symbol from the input. For the complete flat V3
profile, the schema must have only event-scoped fields, no groups or derived
expressions, and no byte-200 discriminator. For the complete grouped profile,
the schema must satisfy the exact one-group/two-domain/byte-200 subset above.
Both resulting files embed the validated schema and complete decode metadata,
so verification needs no schema sidecar. Grouped Flag200 execution is exact
logical event/child behavior, not physical relationship planning or compression.
