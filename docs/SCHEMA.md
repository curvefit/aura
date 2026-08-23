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

The current V2 writer supports fixed-width scalar values backed by the generic
i64 physical engine: booleans, signed/unsigned integer widths, timestamp
nanos/micros, scaled i64 values, enum u8, and flags u32.

Unsupported v1 types reject during schema build:

- nullable fields
- `Utf8`
- `Binary`
- `F32`
- `F64`

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

Relationship flags authorize a bounded planner search; they do not select a
codec or exact residual direction. The chosen complete inverse belongs in the
compiled footer. Full V3 file writing remains disabled while the footer/plan
contract is under construction; current V2 writers reject V3 schemas rather
than emitting cross-wired files.

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
platform. Schema CLI support does not imply that complete V3 Aura0 files are
implemented or stable.
