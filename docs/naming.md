# Naming

Aura exposes three public file levels:

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

Compressed chunks are an internal file-layout choice. Do not encode compression
or hot-layout variants into the extension.

```text
.aura0  may contain independently compressed chunks
.aura1  may be uncompressed or chunk-compressed based on the replay profile
```

Magic values identify the public level:

```text
AURA  ingest container
AUR0  Aura0 compact physical file
AUR1  Aura1 replay physical file
```

There is no `.aura2`. Additional replay layouts belong in `.aura1` header/footer
metadata, not in new public extensions.
