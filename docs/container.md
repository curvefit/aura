# Aura container

Aura files use one container shape across all public levels:

```text
Header
Body
Footer
```

## Header

The header is fixed-width and appears at byte zero.

```text
magic           AURA | AUR0 | AUR1
version         format version
profile         ingest | Aura0 | Aura1
header_len      fixed header size
schema_id       logical schema hash
flags           sealed/open state
footer_offset   zero while open, patched when sealed
footer_len      zero while open, patched when sealed
reserved
```

Open writers use zero footer pointers. When the file is sealed, the writer
appends the footer and patches the header pointer.

## Body

The body is profile-specific:

```text
.aura   normalized generous ingest records
.aura0  compact compiled records
.aura1  replay compiled blocks
```

The body should not need per-record schema mutation. Schema and layout decisions
are file-level facts recorded in the footer.

## Footer

The footer stores the facts readers and converters need before replaying the
body:

```text
schema descriptor
ingest stats
compression descriptor
Aura0 physical plan
Aura1 physical plan
chunk table
```

The footer is what makes conversion deterministic. A converter can read the
header, jump to the footer, select the compiled physical plan, and then process
chunks without re-reading source payloads to discover ranges or group shapes.
