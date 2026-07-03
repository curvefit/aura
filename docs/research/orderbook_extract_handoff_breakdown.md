# Orderbook Extract/Handoff Breakdown

Scope: prepared orderbook replay after commit `4debdd3`, using `sdk-larger` as the primary target.

## Baseline

Current accepted baseline:

| Path | extract_ms | update_ms | reset_ms | production_total_ms | production rec/s |
|---|---:|---:|---:|---:|---:|
| prepared extract + apply | 1.224 | 0.383 | 0.008 | 1.615 | 20.30M |

The update/remove loop is not the bottleneck. Extraction and handoff dominate.

## Breakdown

The old prepared path was:

```text
Aura1 body
  -> replay_orderbook_deltas
  -> OrderBookDeltaBatch
  -> Vec<OrderBookDelta>
  -> PreparedOrderBookEngine::replay(&[OrderBookDelta])
```

The fused path is:

```text
Aura1 body
  -> replay_orderbook_deltas_fused
  -> PreparedOrderBookEngine::apply_values(...)
```

## Questions

1. Is extract_ms mostly field loads?

Partly. The old path decoded timestamp, instrument, side, price, size, flags/action/sequence/order_id as configured, then copied decoded values into `OrderBookDelta` records. Fused production decodes only instrument, side, price, size, and optional action because those are the only fields needed for book mutation.

2. Is it batch construction?

Batch construction is small but nonzero. Fused replay creates zero `OrderBookDeltaBatch` objects in the hot path and reports `delta_batch_structs_created = 0`.

3. Is it callback/handoff?

Yes. The old path crosses a batch callback, fills a delta vector, then calls the engine. Fused replay has no user callback and reports `callback_count = 0` for the fused operation.

4. Is it selected-field kernel overhead?

Yes for the old path. Fused replay bypasses the selected-field checksum/kernel path and reports `selected_field_kernel_dispatch_count = 0`.

5. Is it optional-field handling?

The fused loop specializes once per batch for `action` present vs absent. It does not branch on optional action per row, and it does not load flags/sequence/order_id.

6. Is it checksum/state accounting?

No for production. Production mode does not compute a final state hash. Verify mode computes the hash separately and reports `state_hash_ms`.

7. What can be eliminated by fusing extraction and apply?

Fusing eliminates `Vec<OrderBookDelta>` materialization in measured replay, `OrderBookDeltaBatch` construction, selected-field kernel dispatch, batch callback handoff, unused timestamp/flags/sequence/order-id loads, and the separate apply pass over a decoded delta slice.
