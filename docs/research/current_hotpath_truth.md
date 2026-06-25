# Current Orderbook Hot-Path Truth

This note locks the accepted baseline for the orderbook fusion work.

1. Older DBN-parity reports showing 5.19M records/sec are stale.
2. The current accepted baseline is commit `4debdd3 Add prepared orderbook replay session`.
3. The current `sdk-larger` production prepared result is 20.30M records/sec.
4. The current bottleneck is extraction/handoff: `extract_ms = 1.224`.
5. The update/remove loop is already fast: `update_ms = 0.383`.
6. State hashing is verify-only.
7. Production mode must not include `state_hash_ms`.
8. The next target is fused Aura1 extraction plus orderbook apply.
