use std::collections::BTreeMap;

use aura_codec::{
    BookStateHash, OrderBookApplyPlan, OrderBookDelta, OrderBookEngine, OrderBookEngineKind,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct RefKey {
    instrument: i64,
    side: i64,
    price: i64,
}

fn dense_deltas(count: usize, symbols: i64) -> Vec<OrderBookDelta> {
    (0..count)
        .map(|index| {
            let index = i64::try_from(index).unwrap();
            let symbol = index % symbols.max(1);
            OrderBookDelta::try_new(
                1_700_000_000_000_000_000 + index * 1_000_000,
                symbol,
                index % 2 + 1,
                101_000_000_000 + symbol * 10_000_000 + index % 257,
                1 + index % 100,
                index % 8,
                0,
                index,
                index + 10_000,
            )
            .unwrap()
        })
        .collect()
}

fn sparse_deltas(count: usize, symbols: i64) -> Vec<OrderBookDelta> {
    (0..count)
        .map(|index| {
            let index = i64::try_from(index).unwrap();
            let symbol = (index * 37) % symbols.max(1);
            OrderBookDelta::try_new(
                1_700_100_000_000_000_000 + (index / 3) * 1_000_000,
                symbol,
                index % 5,
                2_000_000 + symbol * 3 + index % 13,
                if index % 7 == 0 { 0 } else { 1 + index % 20 },
                0,
                0,
                index,
                0,
            )
            .unwrap()
        })
        .collect()
}

fn high_cardinality_deltas(count: usize) -> Vec<OrderBookDelta> {
    (0..count)
        .map(|index| {
            let index = i64::try_from(index).unwrap();
            let symbol = index % 4096;
            OrderBookDelta::try_new(
                1_700_200_000_000_000_000 + index * 1_000_000,
                symbol,
                index % 5,
                10_000_000 + symbol * 100 + index % 97,
                1 + index % 1_000,
                0,
                0,
                index,
                0,
            )
            .unwrap()
        })
        .collect()
}

fn reference_hash(deltas: &[OrderBookDelta]) -> BookStateHash {
    hash_ref_levels(reference_levels(deltas))
}

fn reference_levels(deltas: &[OrderBookDelta]) -> BTreeMap<RefKey, i64> {
    let mut levels = BTreeMap::<RefKey, i64>::new();
    for delta in deltas {
        let key = RefKey {
            instrument: delta.instrument,
            side: delta.side.raw_i64(),
            price: delta.price,
        };
        if delta.is_remove() {
            levels.remove(&key);
        } else {
            levels.insert(key, delta.size);
        }
    }
    levels
}

fn hash_ref_levels(levels: BTreeMap<RefKey, i64>) -> BookStateHash {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for (key, size) in levels {
        hash = mix_checksum(hash, key.instrument);
        hash = mix_checksum(hash, key.side);
        hash = mix_checksum(hash, key.price);
        hash = mix_checksum(hash, size);
    }
    BookStateHash(hash.wrapping_add(0x9e37_79b9_7f4a_7c15))
}

fn mix_checksum(checksum: u64, value: i64) -> u64 {
    checksum.wrapping_mul(0x9E37_79B1_85EB_CA87).rotate_left(7)
        ^ (value as u64).wrapping_add(0xC2B2_AE3D_27D4_EB4F)
}

fn apply(
    deltas: &[OrderBookDelta],
    kind: OrderBookEngineKind,
) -> aura_codec::Result<BookStateHash> {
    let plan = OrderBookApplyPlan::compile(deltas, kind)?;
    let mut engine = OrderBookEngine::with_plan(plan)?;
    let stats = engine.apply_all(deltas)?;
    Ok(stats.state_hash)
}

#[test]
fn orderbook_engine_matches_current_apply() -> aura_codec::Result<()> {
    let deltas = dense_deltas(4096, 4);
    let expected = reference_hash(&deltas);

    assert_eq!(expected, apply(&deltas, OrderBookEngineKind::Current)?);
    assert_eq!(expected, apply(&deltas, OrderBookEngineKind::Optimized)?);
    Ok(())
}

#[test]
fn orderbook_engine_dense_dataset_state_hash() -> aura_codec::Result<()> {
    let deltas = dense_deltas(4096, 4);
    assert_eq!(
        reference_hash(&deltas),
        apply(&deltas, OrderBookEngineKind::DenseLadder)?
    );
    Ok(())
}

#[test]
fn orderbook_engine_sparse_dataset_state_hash() -> aura_codec::Result<()> {
    let deltas = sparse_deltas(2048, 512);
    let plan =
        OrderBookApplyPlan::compile(&deltas, OrderBookEngineKind::PagedLadder { page_size: 64 })?;
    let mut engine = OrderBookEngine::with_plan(plan)?;
    let stats = engine.apply_all(&deltas)?;
    let actual = engine
        .active_levels()
        .into_iter()
        .map(|level| {
            (
                RefKey {
                    instrument: level.instrument,
                    side: level.side.raw_i64(),
                    price: level.price,
                },
                level.size,
            )
        })
        .collect::<BTreeMap<_, _>>();
    let expected = reference_levels(&deltas);
    let missing = expected
        .iter()
        .filter(|(key, value)| actual.get(key) != Some(value))
        .take(4)
        .collect::<Vec<_>>();
    let extra = actual
        .iter()
        .filter(|(key, value)| expected.get(key) != Some(value))
        .take(4)
        .collect::<Vec<_>>();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "missing={missing:?} extra={extra:?}"
    );
    assert_eq!(reference_hash(&deltas), stats.state_hash);
    Ok(())
}

#[test]
fn orderbook_engine_larger_dataset_state_hash() -> aura_codec::Result<()> {
    let deltas = dense_deltas(32_768, 64);
    assert_eq!(
        reference_hash(&deltas),
        apply(&deltas, OrderBookEngineKind::DirectIndex)?
    );
    Ok(())
}

#[test]
fn orderbook_engine_high_cardinality_state_hash() -> aura_codec::Result<()> {
    let deltas = high_cardinality_deltas(4096);
    assert_eq!(
        reference_hash(&deltas),
        apply(&deltas, OrderBookEngineKind::RunLocality)?
    );
    Ok(())
}

#[test]
fn orderbook_engine_zero_size_remove() {
    let deltas = vec![
        OrderBookDelta::try_new(1, 7, 1, 100, 10, 0, 0, 1, 0).unwrap(),
        OrderBookDelta::try_new(2, 7, 1, 100, 0, 0, 0, 2, 0).unwrap(),
    ];

    let plan = OrderBookApplyPlan::compile(&deltas, OrderBookEngineKind::Optimized).unwrap();
    let mut engine = OrderBookEngine::with_plan(plan).unwrap();
    let stats = engine.apply_all(&deltas).unwrap();

    assert_eq!(0, stats.book_levels);
    assert_eq!(1, stats.zero_size_removes);
    assert_eq!(1, stats.deletes);
}

#[test]
fn orderbook_engine_preserves_order() {
    let ordered = vec![
        OrderBookDelta::try_new(1, 3, 1, 50, 10, 0, 0, 1, 0).unwrap(),
        OrderBookDelta::try_new(2, 3, 1, 50, 0, 0, 0, 2, 0).unwrap(),
        OrderBookDelta::try_new(3, 3, 1, 50, 20, 0, 0, 3, 0).unwrap(),
    ];
    let reordered = vec![ordered[1], ordered[0], ordered[2]];

    let plan = OrderBookApplyPlan::compile(&ordered, OrderBookEngineKind::Optimized).unwrap();
    let mut engine = OrderBookEngine::with_plan(plan).unwrap();
    let ordered_stats = engine.apply_all(&ordered).unwrap();

    let plan = OrderBookApplyPlan::compile(&reordered, OrderBookEngineKind::Optimized).unwrap();
    let mut engine = OrderBookEngine::with_plan(plan).unwrap();
    let reordered_stats = engine.apply_all(&reordered).unwrap();

    assert_eq!(2, ordered_stats.adds);
    assert_eq!(0, ordered_stats.modifies);
    assert_eq!(1, ordered_stats.deletes);
    assert_eq!(1, reordered_stats.adds);
    assert_eq!(1, reordered_stats.modifies);
    assert_eq!(1, reordered_stats.deletes);
}

#[test]
fn orderbook_engine_rejects_invalid_side() {
    assert!(OrderBookDelta::try_new(1, 1, -1, 100, 10, 0, 0, 1, 0).is_err());
    assert!(OrderBookDelta::try_new(1, 1, 256, 100, 10, 0, 0, 1, 0).is_err());
}
