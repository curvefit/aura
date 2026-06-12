# Aura1 block padding

Variable event shapes prevent every logical event from having the same byte
length. `.aura1` handles this by compiling records into fixed-width replay
blocks chosen from ingest statistics.

For a replay layout with `block_capacity = 4`:

```text
BlockHeader fixed
Slot[4]
```

Seven same-timestamp updates become two fixed-width blocks:

```text
block 0: timestamp T, count 4, slots 0..3 real
block 1: timestamp T, count 3, slots 0..2 real, slot 3 padding
```

Logical order is block order, then slot order. Repeated timestamps are valid when
a timestamp run exceeds the chosen capacity.

Candidate capacities:

```text
1, 2, 4, 8, 16, 32
```

An `.aura1` builder should choose the largest capacity whose padding overhead is
acceptable for the sealed file. The decision comes from `.aura` ingest stats, so
the builder does not need to scan the raw source payloads again.
