# Shadow Arrow protocol

`aura-logical-arrow-ipc-v1` is a provider-independent, offline boundary for
testing an external Aura compiler. It accepts one explicitly terminated Arrow
IPC **stream** and emits `standalone-aura-v3-value-block-v1`. The artifact is a
schema-bound reference block, not a complete `.aura`, `.aura0`, or `.aura1`
file. The reference-block artifacts remain unchanged; the strict flat and
grouped Arrow inputs can separately target the complete `flat-aura0-v3-v1`
and `grouped-aura0-v3-exact-v1` containers through `aura v3 aura0 seal`.

Discover the exact compiler capability without reading stdin or creating files:

```bash
aura shadow handshake --protocol aura-logical-arrow-ipc-v1 --json
```

The stable `aura-shadow-handshake-v1` result identifies the package version,
full local Git commit and dirty state when available, provenance evidence class, Cargo
lockfile SHA-256, supported
protocol/schema/artifact lists, both explicit Aura0 V3 complete targets,
the hash-domain contracts, and the Rust Arrow version and IPC protocol.

Encode a stream supplied on stdin:

```bash
aura shadow encode \
  --protocol aura-logical-arrow-ipc-v1 \
  --schema schema.json \
  --artifact-kind standalone-aura-v3-value-block-v1 \
  --output values.aurav3vb \
  --json < logical.arrow-stream
```

Seal the same strict logical stream as a complete flat V3 Aura0 file, then
verify it using only its embedded canonical schema:

```bash
aura v3 aura0 seal \
  --protocol aura-logical-arrow-ipc-v1 \
  --schema canonical-schema.json \
  --output events.aura0 \
  --json < logical.arrow-stream

aura v3 aura0 verify --input events.aura0 --json
```

The default and explicit `--mode exact` routes remain the existing exact flat
writer and result schema. A separate development-only request can score the
existing exact complete file against plan-bound fixed and absolute-varint
complete files:

```bash
aura v3 aura0 seal \
  --protocol aura-logical-arrow-ipc-v1 \
  --mode planned \
  --schema canonical-schema.json \
  --output planned-events.aura0 \
  --json < logical.arrow-stream
```

The `aura-v3-flat-aura0-planned-request-seal-result-v2` schema reports all
four candidates' applicability,
rejection/cost attribution, selected physical codec rows, dictionary counts
and byte attribution, complete bytes, the
selected candidate, the actual footer/body tuple, and a plan hash only when a
planned tuple wins. Exact fallback is intentional and is verified with the
unchanged exact verify schema. A selected planned tuple uses footer layout 3
and body encoding 4. Registry-1/layout-1 files retain container target
`flat-aura0-v3-planned-v1`, format label `planned_flat_codecs_v1`, and verify
schema `aura-v3-flat-aura0-planned-verify-result-v1`. Registry-2/layout-2
files use the corresponding `planned-v2`, `planned_flat_codecs_v2`, and
planned-verify-result-v2 identities. This preserves attempt-6 receipt meaning
rather than silently broadening a v1 result schema. Both planned
compilation and verification are conservatively bounded all-memory reference
operations: compilation retains the exact, fixed, mixed, and dictionary
complete candidates plus lane/dictionary scratch. Registry-2 dictionaries are
chunk-local, lexicographically canonical, exact-byte Utf8/DecimalText codecs
with present-only minimal bitpacked indices. They do not normalize DecimalText
or use RLE, Huffman, general compression, or field identity. This is not a
seekable or streaming readiness claim. Planned mode is not accepted for
grouped protocol v2.

Seal accepts canonical schema JSON only and publishes a new mode-0600 path via
the same create-once, held-handle verification and directory-fsync state
machine as the standalone shadow artifact. It never relabels an `AURAV3VB`
reference block as a complete file.

Default exact V3 seal and verify results identify container version 3, profile `aura0`, body
encoding `flat_exact_blocks_v1`, footer layout version 1, all publication-size
fields, schema and logical identities, and the exact artifact SHA-256. Both
carry build provenance. For backward compatibility, both complete V3 commands
currently use the legacy-named `aura-v3-flat-error-v1` error envelope; their
successful flat and grouped result schemas remain distinct. Committed
result-delivery failure distinguishes whether stale temporary cleanup is also
required.

The grouped v2 stream has a corresponding complete-file command. It accepts
the same canonical schema and strict nested Arrow contract as the standalone
v2 boundary, and verification discovers the grouped footer tuple and embedded
schema without a schema argument:

```bash
aura v3 aura0 seal \
  --protocol aura-logical-arrow-ipc-v2 \
  --schema canonical-grouped-schema.json \
  --output grouped-events.aura0 \
  --json < grouped.arrow-stream

aura v3 aura0 verify --input grouped-events.aura0 --json
```

An explicitly terminated stream with no record batches seals as a complete
zero-event, zero-child, zero-chunk file. Positive-event input seals as exact
`AURAV3EB` version-1 chunks. Grouped seal and verify use the distinct stable
result schemas `aura-v3-grouped-aura0-seal-result-v1` and
`aura-v3-grouped-aura0-verify-result-v1`. Their results report protocol v2,
the `grouped_exact_events_v1` body identity, body/event-block/footer layout
versions, `compression: "none"`, event/child/chunk counts, schema and logical
hashes, exact file bytes and artifact SHA-256, and pinned build evidence. They
do not claim a physical planner, compression, Aura1 support, or default
production status.

Complete-file verification holds one regular non-symlink file, boundedly reads
its footer routing tuple, dispatches to the exact-flat, planned-flat, or grouped
reader, performs
the complete reader verification, and hashes exactly the verified held file.
All seal routes use the same mode-0600 temporary-file, create-once hard-link,
held-identity, pre/post-link verification, directory-sync, rollback/ambiguity,
and committed-stdout-loss recovery state machine. Existing destinations,
symlinks, and group/world-writable parent directories are rejected.

Verify and recover the path-free result for an already committed block without
writing anything:

```bash
aura shadow verify \
  --protocol aura-logical-arrow-ipc-v1 \
  --schema schema.json \
  --input values.aurav3vb \
  --json
```

Verify boundedly reads a regular, non-symlink `.aurav3vb`, checks the embedded
strong schema fingerprint, decodes it, and recomputes row count plus schema,
logical, and artifact SHA-256 values.

The schema uses strict bounded `aura-schema-json-v1`. Arrow fields must match
the canonical Aura descriptor exactly by count, order, name, nullability, and
type. Schema and field metadata, big-endian schemas, dictionaries, compressed
record batches, timezones, Boolean, floats, `LargeUtf8`, and every implicit or
lossy conversion are rejected. A Boolean-role Aura `U8` must contain only 0 or
1. `I128` is `FixedSizeBinary(16)` containing little-endian two's-complement;
`Opaque16` is the same Arrow width without numeric interpretation. Timestamp
units are exact and timezone-free. Multiple record batches append in stream
batch/row order. Nullable presence is retained LSB-first; null, empty, and zero
remain distinct.

The stream is frozen to little-endian Arrow metadata V5 with the default
64-byte metadata/body/buffer alignment and a continuation marker on every
message and EOS. Alternate 8/16/32-byte V5 alignment is rejected. It must carry
an explicit IPC EOS marker.
Truncation, malformed frame
lengths, unexpected messages, and any byte after EOS fail closed. The library
hard ceiling is 1 GiB for IPC input, 1 GiB for the reference block, and
16,777,216 rows. Conservative defaults are 256 MiB input, 256 MiB block, and
4,194,304 rows, and 4,096 record batches (hard batch ceiling 65,536); callers may
configure lower limits. Variable values are capped
at 16 MiB and cumulative offsets at `u32`. Limits and predicted block size are
checked incrementally with fallible allocation. Conversion and CLI verification
remain intentionally all-memory operations: at peak they may hold the bounded
IPC input, Arrow batch storage, Aura columns, encoded block, and one bounded
verification read. This is a reference boundary, not a streaming production
compiler.

## Grouped exact-event protocol v2

`aura-logical-arrow-ipc-v2` is the grouped counterpart to the flat v1
boundary. Its Arrow stream contains event-scoped fields in global slot order,
followed by exactly one nonnullable reserved field named
`__aura_repeated_v1`. That field is a nonnullable `List` of nonnullable
`Struct` values whose children are every repeated field in global slot order,
with their declared scalar types and nullability. Empty event batches and
empty child lists are exact values; list offsets define event/child ownership.

The stream contract remains Arrow 54 IPC stream metadata V5, little-endian,
64-byte aligned, explicitly terminated, metadata-free, dictionary-free and
uncompressed. Aura performs a bounded raw framing/node/buffer/offset preflight
before Arrow decoding and then emits only
`standalone-aura-v3-event-block-v1` (`AURAV3EB`):

```bash
aura shadow encode \
  --protocol aura-logical-arrow-ipc-v2 \
  --schema grouped-schema.json \
  --artifact-kind standalone-aura-v3-event-block-v1 \
  --output events.aurav3eb \
  --json < grouped.arrow-stream

aura shadow verify \
  --protocol aura-logical-arrow-ipc-v2 \
  --schema grouped-schema.json \
  --input events.aurav3eb \
  --json
```

This is a standalone grouped reference artifact, not a complete Aura0 file.
The existing v1 flat protocol and artifact remain unchanged.

Grouped v2 is also deliberately all-memory. Its conservative defaults cap the
IPC input and `AURAV3EB` block at 256 MiB, record batches at 4,096, events at
1,048,576, repeated children at 4,194,304, total logical scalar values at
16,777,216, and each logical variable-width value at 16 MiB. Absolute hard
ceilings are 1 GiB input/block, 65,536 batches, 4,194,304 events, 16,777,216
children, 67,108,864 values, and 16 MiB per logical variable-width value.
Callers may choose lower ceilings or explicitly opt toward the hard envelope;
values above the hard ceilings are clamped. Raw IPC backing outside a sliced
logical child range remains structurally validated and counts toward the
input-byte cap, but it is not an Aura logical value and therefore does not
count toward event-block size or logical value limits. Peak memory may include
the bounded IPC bytes, Arrow views, accumulated Aura columns, the encoded event
block, and one bounded verification read.

## Safe publication

Trusted shadow publication is currently Unix-only and fails closed elsewhere.
It requires a new `.aurav3vb` or `.aurav3eb` path and a real parent directory
that is not group or world writable. Existing paths, parent symlinks,
non-regular collisions, and extension mismatches are refused. This deliberately
narrow scope avoids a weaker portable path-race fallback.

Aura opens and validates the directory before creating anything. It creates an
exclusive destination-local mode-0600 temporary file for read/write and keeps
that handle open through write, flush, file fsync, seek/read SHA-256, block
decode, and logical-hash verification. Device, inode, regular-file status, and
size must match the held handle before and after the create-once hard link. A
collision never replaces the winner.

The first directory fsync after the final hard link is the commit point. A
pre-link failure removes only names proven to reference this invocation's inode
and syncs cleanup. `publication_cleanup_required` means no final was committed,
but an owned mode-0600 temp may remain; the coordinator may identify and remove
only that proven temp, then sync the directory. It must not blindly delete or
overwrite another path.

After linking, a failed commit fsync attempts owned-name rollback.
`publication_ambiguous` means rollback could not prove the final name absent;
the coordinator must inspect and verify identity/content before adopting or
cleaning anything, never blindly overwrite or delete it.

After commit, temp unlink or its second directory fsync cannot invalidate the
final artifact; JSON success sets `stale_temp_cleanup_required` when bounded
cleanup/reconciliation may be needed. If JSON result delivery itself fails,
`publication_committed_result_unavailable` means the final is durable: the
coordinator should run read-only `aura shadow verify` and adopt the verified
final, without rewriting it. JSON contains no input values or path.

## Build provenance

Normal local builds obtain informational commit and dirty evidence through
short-timeout, bounded, read-only Git commands using a fixed system executable,
with system/global configuration and caches disabled and no network access.
Source archives without Git report `unavailable`; no commit is fabricated.
Reproducible builders may set both `AURA_BUILD_GIT_COMMIT_OVERRIDE` (exactly 40
lowercase hexadecimal characters) and `AURA_BUILD_GIT_DIRTY_OVERRIDE`
(`true` or `false`). Supplying only one or an invalid value fails the build.
Local Git evidence is labeled `git-informational`; it is not cryptographic
attestation. Overrides are labeled `override-untrusted`. Production acceptance
requires an independently pinned executable SHA-256 and approved build
provenance, and is expected to reject dirty, unavailable, or override-untrusted
evidence.
