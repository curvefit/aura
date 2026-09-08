# Naming

The default V2 SDK exposes three public file levels:

```text
.aura   canonical normalized ingest file
.aura0  compact cold file compiled from ingest stats
.aura1  replay-optimized file compiled from ingest stats
```

Live writers should use a temporary suffix until the footer is sealed:

```text
market-data-2026-06-12T19.aura.tmp
market-data-2026-06-12T19.aura
```

The V2 `AuraWriter` buffers its rows and returns its output only after a
successful finish/seal; it does not expose a path-based streaming writer. The
separate V3 writers use explicit seekable-output APIs and are documented in
`docs/WRITER.md`. Any path-based publisher must write to a temporary path and
promote only after the footer length and `sealed:)` trailer are validated.

Compressed chunks are an internal file-layout choice. Do not encode compression
or hot-layout variants into the extension.

```text
.aura0  may contain independently compressed chunks
.aura1  may be uncompressed or chunk-compressed based on the replay profile
```

For V2, the container header begins with the four-byte `AURA` magic, followed by
a little-endian u16 container version and the profile byte at offset 6. The
profile selects ingest, Aura0, or Aura1. V3 uses the same `.aura0` extension for
its explicitly dispatched Aura0 containers; the extension alone does not select
the container version.

```text
AURA + version 2 + profile 0  ingest container
AURA + version 2 + profile 1  Aura0 compact physical file
AURA + version 2 + profile 2  Aura1 replay physical file
```

Complete files end with a four-byte little-endian footer length followed by the
eight-byte seal magic `sealed:)`. The seal is a file trailer, not a header field.

There is no `.aura2`; additional replay layouts belong in `.aura1` metadata,
not in new public extensions. Explicit V3 Aura0 layouts remain `.aura0` files
with their own versioned header/footer contract.
