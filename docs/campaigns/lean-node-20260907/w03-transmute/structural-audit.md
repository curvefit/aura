# w03 structural and entropy audit

Status: bounded read-only audit against `cc04f75c9217e6c6a575145ab0ed98df232f622d`.
No production format or hot-path code was changed. The companion
`examples/structural_frontier.rs` is a bounded complete-V2-container pair
experiment; it changes no production files and can persist the two validated experimental artifacts when given an output directory.

The older `420052c` checkout was a divergent rewritten-history snapshot. It is
not the source of truth for this audit. `cc04f75` is the current deployed/main
line and includes the shared integer analysis plus `I64SearchEffort::Fast`.

## Flag 200 has two deliberately different meanings

The dialect must be recorded with every result. Reusing a V2 interpretation in
V3 silently changes field count and can make a plausible byte stream decode to
the wrong events.

| Dialect | Meaning of byte `200` | Where grouping/domain facts come from | Current consequence |
|---|---|---|---|
| V2 `decode_schema_map` | Zero-width control byte. It consumes no logical slot and must be immediately followed by `201..=239`; that next byte starts a repeated group of width `byte - 200`. The `SchemaMapEntry` for the group carries `DualDomainGroup { width }`. | The compact map's group-width byte; domain names/order are not encoded. | `generic_i64_parent_schema("...", &[100, 0, 200, 203, 0, 0])` has two event fields and three repeated fields. The generic planner sees scope/relations; it does not implement a V2 physical bid/ask split from byte `200` alone. |
| V3 `decode_v3_schema_map` | One-to-one field marker. It consumes and marks the field at that logical slot as `DualDomainDiscriminator`; `201..=239` is invalid. | The V3 group descriptor table supplies repeated child slots, exactly two domains, the discriminator slot, and relationship permission bits. | The complete grouped subset requires a repeated, non-null `U8` field with role `Side`, values only `0` or `1`, and the map byte `200` at that field. `200` authorizes no transform by itself. |

The complete V3 grouped writer (`V3GroupedAura0Writer`) preserves exact event
and child columns, authoritative `event_count + 1` offsets, selector order,
validity, and hashes. It performs no physical relationship planning,
compression, or Huffman coding. The planned-grouped development compiler is a
separate route. Its relationship permissions are schema facts, not codec
choices; the compiler records the selected operation and dependency in `AUP2`
and scores the entire sealed artifact.

Relevant current source points are `src/schema.rs:1419-1548` (V2 map),
`src/schema.rs:1550-1631` (V3 map), `src/v3_events.rs:120-167` (exact grouped
subset), and `src/v3_plan_v2.rs:1-20,330-590` (plan authorization).

## Current supported frontier

The V3 planned-grouped route currently has these registries:

- Registry 1: exact `AURAV3EB` direct lanes.
- Registry 2: plan-bound compact direct/split-domain lanes.
- Registry 3: direct lanes with per-field fixed width, unsigned canonical
  ULEB128, or signed ZigZag canonical ULEB128.
- Registry 4: registry-3 physical codecs plus checked previous-within-domain
  residuals. State resets for both domains at every event; the first value of
  each nonempty domain is absolute.
- Registry 5: registry-3 physical codecs plus checked cross-domain same-field
  residuals. Pairing is by ordinal occurrence inside each event; the source
  domain and every unmatched target tail remain absolute.
- Registry 6: checked same-child `I64` parent residuals for an explicit repeated
  `DeltaFromField(parent)` relation. This is a public development candidate,
  but the held planned-grouped writer still invokes
  `compile_v3_planned_grouped_attempt5`, so registry 6 is not the current writer
  default.

The grouped V3 physical codec table is deliberately small:
`FixedWidth`, unsigned ULEB128, and signed ZigZag ULEB128. Its
`VariableByteDictionaryBitpacked` entry is rejected by grouped Plan v2 and is
for flat Utf8/DecimalText; the temporal and prefix/suffix codecs are also flat
only. There is no Huffman physical codec in V3 Plan v2. The V3 source comments
explicitly exclude Huffman, Zstandard, null sparsity, and provider meaning from
registries 1-6.

The V2 generic engine is different. `GenericStreamOp` already supports
`PackedDictionary`, `HuffmanDictionary`, `PreviousValueDelta`,
`DeltaOfDelta`, RLE, bitpacking, and other operations. `I64SearchEffort::Fast`
does *not* mean no-Huffman: it omits bitplane-RLE and delta-of-delta candidate
trials but retains dictionary/Huffman analysis, previous-value deltas, the
schema-authorized relationships, and exact fallbacks. `Bounded` omits
BlockLocal trials and also retains Huffman. The generated V2 `huff` fixture is
currently blocked because the public writer did not select a
`HuffmanDictionary` under the 2x speed gate; `nohuff` has zero Huffman streams.
The huff workload therefore requires the external compatible
`grimoire-50mb-huff` artifact until a repo-native fixture is added.

## Two candidates only

These are the only two structural candidates worth screening in this window.
Each must be compared to its direct/absolute counterpart on the same decoded
events and with a fixed physical codec policy. Do not combine the two
relationship families into a new search framework. The complete-container
Huffman pair is available only on the V2 generic stream route; V3's grouped
Plan v2 route remains no-Huffman.

### A. Previous-within-domain (registry 4)

Use `compile_v3_planned_grouped_attempt4` for the complete scorer, or
`compile_v3_planned_grouped_attempt4_candidate` to force a selected repeated
slot and `PlanV2PhysicalCodec::FixedWidth` or
`PlanV2PhysicalCodec::SignedZigZagUleb128`. The scorer compares:

```text
registry1-direct
registry2-compact-direct
registry3-compact-fixed
registry3-compact-integer-codecs
registry4-absolute-integer-codecs
registry4-previous-within-domain
```

For each eligible non-null signed repeated field, the transformed lane is the
first value per domain followed by checked `value - previous_value` within that
domain. Both domain states reset at each event. Selection requires the best
transformed lane plus the two-byte `u16` discriminator dependency to be smaller
than the absolute lane; a complete-file tie stays absolute/direct.

This is the strongest first structural screen because it has simple state and
does not assume bid/ask correspondence. The likely decode cost is one scalar
decode per child plus one domain branch and checked accumulator update per
child, followed by an allocated logical column. The current inverse allocates a
`Vec<i64>` and loops over every event/child; it does not write a consumer row
directly.

The no-Huffman side of this candidate is fully supported by registry 4 with
fixed or signed ULEB physical lanes. A Huffman side is not a valid V3 artifact;
use the companion stream probe only to measure entropy headroom on the exact
same residual values. Do not call its probe bytes a V3 archive size.

### B. Cross-domain same-field (registry 5)

Use `compile_v3_planned_grouped_attempt5` for the complete seven-way scorer, or
`compile_v3_planned_grouped_attempt5_candidate` with an explicit `[(slot, op)]`
list and fixed/signed-ULEB physical codec. The scorer compares both directions:

```text
op 3: domain 0 value - same-ordinal domain 1 value
op 4: domain 1 value - same-ordinal domain 0 value
```

Pairing restarts at each event and uses ordinal occurrence in each domain. The
source domain stays absolute and unequal tails stay absolute. A candidate is
eligible only for a non-null repeated signed integer-like field and only when
the group has `with_across_domain_same_field()` permission. Selection again
requires the transformed lane plus a two-byte discriminator dependency to beat
the absolute lane, with complete-file strict comparison and earlier-direct tie
breaking.

The likely decode cost is higher than A: after physical lane decode, the current
inverse clones the stored lane, allocates `source_positions` per event, scans
the selector once to collect the source domain, scans it again to find target
ordinals, and performs checked additions for paired targets. This can be a
useful synthetic control when same-ordinal values are intentionally related,
but it is a poor default hypothesis for independently ordered book sides.

Again, V3 has no Huffman variant. If the experiment needs a structural plus
Huffman pair, apply the same residual values to the existing V2
`GenericStreamOp::PackedDictionary` and `GenericStreamOp::HuffmanDictionary`
body operations. The companion example does exactly this for one selected
high-volume structural stream and rebuilds a complete V2 Aura0 container. Do
not add a new V3 registry just to make the comparison look symmetric.

The V2 paired experiment preserves the selected outer structural operation and
changes only its nested residual operation. It considers
`PreviousValueDelta`, `DeltaOfDelta`, and `FixedStrideDelta`, chooses the
largest stream whose residual dictionary has at most 4,096 entries, and emits
one `PackedDictionary` (no entropy stage) and one `HuffmanDictionary` variant.
This is intentionally one stream, not a new global planner; an input without a
low-cardinality nested structural stream fails closed.

## Exact APIs and fixtures

Construct the V3 schema with the public builder:

```rust
let relationships = RelationshipPermissions::none()
    .with_split()
    .with_within_domain()
    .with_across_domain_same_field();
let schema = SchemaBuilder::new("dual-domain-levels")
    .v3()
    .field("ts", FieldType::TimestampMs, FieldRole::Timestamp)
    .repeated_field("side", FieldType::U8, FieldRole::Side)
    .repeated_field("value", FieldType::I64, FieldRole::Value)
    .dual_domain_repeated_group(1, vec![1, 2], 1, relationships)
    .finish()?;
```

`SchemaBuilder::dual_domain_repeated_group` sets the V3 group descriptor and
`SchemaDescriptor::with_v3_groups` can be used when starting from an existing
descriptor. Build batches as `AuraV3EventBatch` with `event_columns`,
`repeated_columns`, and monotonic `child_offsets` of length `event_count + 1`.
The side column must be `AuraV3ColumnValues::U8` with values in `0..=1`.

The checked-in test fixtures are better than inventing another corpus:

- `tests/v3_plan_v2_planned_grouped.rs::small_schema` and `small_batch` are the
  minimal V3 two-domain shape.
- `attempt4_previous_within_domain_roundtrips_and_wins_complete_cost` uses two
  events with 100 alternating children and a within-domain-friendly value
  pattern; it checks candidate selection, exact inverse, and rechunk stability.
- `attempt5_cross_candidate_wins_only_on_complete_cost_and_is_rechunking_stable`
  uses two events with 96 same-ordinal pairs per domain; it checks both
  orientations, complete-cost selection, inverse, and rechunk stability.
- `attempt5_overflow_nullable_and_unauthorized_relationships_fall_back` is the
  required negative fixture for overflow, nullable values, and missing
  authorization.
- `tests/fixtures/v3-grouped-container/three-event-two-chunk.aura0.hex` is the
  exact grouped no-plan control (3 events, 4 children). It is not a planned
  structural artifact.
- `tests/fixtures/v3-planned-v5/` is a flat Utf8 prefix/suffix fixture. It is
  not suitable for a dual-domain grouped measurement.
- For V2 Huffman/no-Huffman controls, run `aura-fixture-gen` for the generated
  matrix and use the external `grimoire-50mb-huff` only when its immutable
  source/plan identity is available. The generated `huff` row must remain
  explicitly blocked.

The complete in-memory calls are:

```rust
let within = compile_v3_planned_grouped_attempt4(&schema, &batches, V3GroupedLimits::default())?;
let cross = compile_v3_planned_grouped_attempt5(&schema, &batches, V3GroupedLimits::default())?;
let decoded = decode_v3_planned_grouped(&within.bytes, V3GroupedLimits::HARD)?;
assert_eq!(decoded.batches, batches);
```

The held writer is only a registry-1..5 route and deliberately rereads exact
chunks before invoking the finite planner:

```rust
let mut writer = V3PlannedGroupedIngestWriter::try_new(
    Cursor::new(Vec::new()), schema.clone(), V3PlannedGroupedWriterOptions::default(),
)?;
for batch in &batches { writer.write_batch(batch)?; }
let (_stream, artifact) = writer.finish()?;
```

Use the all-memory attempt APIs for A/B candidate isolation; use the held writer
only to prove that the selected attempt-5 bytes and hashes survive the staged
write path. The current CLI command
`aura v3 aura0 seal --protocol aura-logical-arrow-ipc-v2` is the exact grouped
writer, not the planned physical planner.

## Accounting contract

Every candidate result must report all of the following separately:

1. Logical source facts: schema fingerprint/ID, event count, child count,
   per-event offsets, selector counts, source-order logical SHA-256, source
   file/member hashes, and the exact scale/type/nullability contract.
2. Physical body: every block header, child-offset table, selector bytes,
   validity bitmap, fixed/varint lane bytes, transformed residual bytes, and
   any per-block framing.
3. Header/footer/trailer: V3 header, complete `AUP2` plan, schema descriptor,
   plan hash, header/body/global logical hashes, chunk descriptors, footer hash,
   footer-length word, and `sealed:)`.
4. Creation and decode costs: source-to-artifact time, peak RSS, allocations if
   available, and artifact decode/replay time. Separate candidate construction
   from expansion/replay; a complete-file byte win does not imply a decode win.

`V3PlannedGroupedSummary` already exposes
`header_bytes`, `body_bytes`, `footer_bytes`, `file_bytes`, and
`accounted_file_bytes`; require `file_bytes == accounted_file_bytes`. The
planned-grouped footer uses a 216-byte prefix, the encoded schema and plan, an
8-byte chunk-table prefix, 120 bytes per chunk, a 32-byte footer hash, and the
12-byte common trailer. Each selected relationship adds its dependency and
changes the plan/body version, so charging only residual lane bytes is invalid.

For a V2 generic stream probe or complete pair, include the `AURI` plan bytes, each stream frame
(`u16 stream id + u64 value count + u32 body length`), and each body. A
`HuffmanDictionary` instruction includes base, unit, entry count, entry width,
and packed code lengths in the plan; its body includes sorted packed entries
and Huffman bits. A PackedDictionary body has the same sorted entries and
fixed-width dictionary codes. The companion example reports complete
`output_bytes`, `header_bytes`, `body_bytes`, `footer_bytes`, `trailer_bytes`,
`plan_bytes`, and selected-stream body bytes before/after. Its `output_bytes`
include the V2 header, all stream frames, full compiled footer, footer length,
and `sealed:)`; the input receipt and external source archive remain outside
Aura0 `file_bytes`.

## Runnable bounded probe and verification

The complete pair experiment accepts one of the staged V2 generation files and
is safe to run outside the heavy benchmark slot:

```bash
cargo run --release --offline --example structural_frontier -- \
  /home/anton/Downloads/lean-node-20260907/corpus-w03/current/bybit_delta.aura0 \
  > /tmp/structural-frontier-bybit-delta-cc04f75.json
```

It selects one existing nested structural stream, rebuilds complete no-Huffman
and Huffman containers while preserving all other stream bytes and footer
metadata, decodes both with `records::decode_i64_events_file`, checks event
parity and footer/header preservation, and prints machine-readable timing and
byte components. It rejects byte-lane/chunked files and inputs without a
low-cardinality nested structural stream so an unsupported pair is not
silently mislabeled.

Run the existing complete-candidate checks in the scheduled Rust test slot:

```bash
cargo test --offline --test v3_plan_v2_planned_grouped \
  attempt4_previous_within_domain_roundtrips_and_wins_complete_cost \
  -- --nocapture
cargo test --offline --test v3_plan_v2_planned_grouped \
  attempt5_cross_candidate_wins_only_on_complete_cost_and_is_rechunking_stable \
  -- --nocapture
cargo test --offline --test v3_planned_grouped_writer \
  incremental_writer_matches_attempt5_reference_complete_bytes -- --nocapture
```

For current V2 fair byte-output work, use the maintained release benchmark and
the campaign lock. Keep production timing at `--guard-mode no_guard
--canonical-hash-mode none`; use verify variants separately. Compare
`aura0-to-aura1-bytes` with `zstd-aura1-to-aura1-bytes` only when both produce
the same Aura1 byte sink and the manifest records the same logical workload.

## Prior evidence and stop conditions

The retained Operation-200 Python reports are useful negative evidence, not
current Aura format results. They normalize Bybit data, intentionally drop
`ts`, and produce experimental `OP200` files. The source sample has 863,933
rows, two snapshots, 30,233,396 bid levels, and 29,152,313 ask levels. On that
sample:

- v1 independent same-side chains: 114,040,472 bytes; encode 111.02 s; decode
  87.56 s; exact round trip.
- v2 added 11 Huffman streams and delta-of-delta/base history: 111,893,736
  bytes; encode 310.23 s; decode 161.87 s; exact round trip.
- v3 added same-side price gaps and replay-book quantity residuals:
  89,539,139 bytes (19.98% below v2); encode 268.80 s; decode 154.46 s.
- v4 reached 84,788,583 bytes (5.31% below v3) but decode rose to 168.65 s;
  measured base/gap delta-of-delta, row-RLE, and block absolute/residual
  searches were rejected. The all-stream Huffman-only probe was 88,929,778
  bytes, 4.88% *larger* than the v4 baseline because it omitted the winning
  per-stream Zstandard stage.
- The cross-side candidate screen selected `same_side`; cross-side base/level
  variants added roughly 0.04% to 29.9% to the price body. The same-side
  delta-stream parent probe added 5.49% (bid) or 6.24% (ask) complete bytes,
  despite exact round trips.

These results rule out reopening a broad cross-side or all-stream Huffman
search. Stop a new candidate when its complete artifact does not beat the
earlier direct artifact after plan/header/footer overhead, or when its decode
adds the selector/domain map work without a measured end-to-end win. Validate a
winner on a disjoint second sample with independent source decode and replay
checkpoints. A stream-level Huffman saving is headroom only until it has a
versioned footer, complete archive accounting, and the same consumer boundary.
