# Dynamic hot padding

Variable level counts prevent every event from having the same byte length. Aura
handles this by keeping headers and levels fixed-width while padding the level
sections per event.

For a hot profile with `block_size = 8`:

```text
EventHeader fixed
BidLevel[round_up(bid_count, 8)]
AskLevel[round_up(ask_count, 8)]
```

An event with 3 bid changes and 11 ask changes stores:

```text
bid slots: 8  = 3 real + 5 padding
ask slots: 16 = 11 real + 5 padding
```

Large outliers are naturally split across more fixed-size blocks. They do not
force every other event to pad to the same size.

Candidate block sizes:

```text
4, 8, 10, 16, 20, 32
```

A hot builder should choose the largest block size whose padding overhead is
acceptable for the file. Cold seal statistics can make this a one-pass decision.
