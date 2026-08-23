# Shadow Arrow protocol

`aura-logical-arrow-ipc-v1` is a provider-independent, offline boundary for
testing an external Aura compiler. It accepts one explicitly terminated Arrow
IPC **stream** and emits `standalone-aura-v3-value-block-v1`. The artifact is a
schema-bound reference block, not a complete `.aura`, `.aura0`, or `.aura1`
file. The reference-block artifact remains unchanged; the same strict logical
Arrow input can separately target the complete `flat-aura0-v3-v1` container
through `aura v3 aura0 seal`.

Discover the exact compiler capability without reading stdin or creating files:

```bash
aura shadow handshake --protocol aura-logical-arrow-ipc-v1 --json
```

The stable `aura-shadow-handshake-v1` result identifies the package version,
full local Git commit and dirty state when available, provenance evidence class, Cargo
lockfile SHA-256, supported
protocol/schema/artifact lists, the single `flat-aura0-v3-v1` complete target,
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

Seal accepts canonical schema JSON only and publishes a new mode-0600 path via
the same create-once, held-handle verification and directory-fsync state
machine as the standalone shadow artifact. It never relabels an `AURAV3VB`
reference block as a complete file.

V3 seal and verify results identify container version 3, profile `aura0`, body
encoding `flat_exact_blocks_v1`, footer layout version 1, all publication-size
fields, schema and logical identities, and the exact artifact SHA-256. Both
carry build provenance. V3 failures use `aura-v3-flat-error-v1`; committed
result-delivery failure distinguishes whether stale temporary cleanup is also
required.

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

## Safe publication

Trusted shadow publication is currently Unix-only and fails closed elsewhere.
It requires a new `.aurav3vb` path and a real parent directory that is not group
or world writable. Existing paths, parent symlinks, non-regular collisions, and
extension mismatches are refused. This deliberately narrow scope avoids a
weaker portable path-race fallback.

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
