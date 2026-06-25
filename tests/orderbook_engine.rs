use std::collections::BTreeMap;
use std::io::Cursor;

use aura_codec::{
    AuraReader, AuraRecordBatch, AuraSchema, AuraType, AuraValue, AuraWriter, BookApplyStats,
    BookStateHash, OrderBookApplyMode, OrderBookApplyPlan, OrderBookDelta, OrderBookDeltaSpec,
    OrderBookEngine, OrderBookEngineKind, OrderBookLifecycleMode, OrderBookReplaySession,
    PreparedOrderBookApplyPlan, PreparedOrderBookEngine, WriterOptions,
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

fn write_aura1(schema: AuraSchema, rows: Vec<Vec<AuraValue>>) -> Vec<u8> {
    let batch = AuraRecordBatch::new(schema.clone(), rows).unwrap();
    let mut bytes = Vec::new();
    let mut writer = AuraWriter::try_new(&mut bytes, schema, WriterOptions::aura1()).unwrap();
    writer.write_batch(batch).unwrap();
    writer.finish().unwrap();
    bytes
}

fn default_fused_schema() -> AuraSchema {
    AuraSchema::builder()
        .field("ts_event", AuraType::TimestampNanos)
        .field("symbol_id", AuraType::U32)
        .field("side", AuraType::EnumU8)
        .field("price", AuraType::PriceI64Scaled { scale: 4 })
        .field("size", AuraType::I64)
        .field("action", AuraType::U8)
        .build()
        .unwrap()
}

fn default_fused_spec(schema: &AuraSchema) -> OrderBookDeltaSpec {
    OrderBookDeltaSpec::builder()
        .timestamp("ts_event")
        .instrument("symbol_id")
        .side("side")
        .price("price")
        .size("size")
        .action_optional("action")
        .build(schema)
        .unwrap()
}

fn deltas_from_aura1(bytes: &[u8], spec: &OrderBookDeltaSpec) -> Vec<OrderBookDelta> {
    let reader = AuraReader::open(Cursor::new(bytes)).unwrap();
    let mut deltas = Vec::new();
    reader
        .replay_orderbook_deltas(2, spec, |batch| {
            for row in 0..batch.row_count() {
                deltas.push(OrderBookDelta::try_new(
                    batch.timestamp(row)?,
                    batch.instrument(row)?,
                    batch.side(row)?,
                    batch.price(row)?,
                    batch.size(row)?,
                    batch.flags(row)?.unwrap_or(0),
                    batch.action(row)?.unwrap_or(0),
                    batch.sequence(row)?.unwrap_or(0),
                    batch.order_id(row)?.unwrap_or(0),
                )?);
            }
            Ok(())
        })
        .unwrap();
    deltas
}

fn run_fused(
    bytes: &[u8],
    spec: &OrderBookDeltaSpec,
    mode: OrderBookApplyMode,
    expected_hash: Option<BookStateHash>,
) -> aura_codec::Result<BookApplyStats> {
    let deltas = deltas_from_aura1(bytes, spec);
    let plan = PreparedOrderBookApplyPlan::build(&deltas, OrderBookEngineKind::Optimized)?;
    let mut engine = PreparedOrderBookEngine::with_capacity(&plan)?;
    engine.reset_for_replay();
    let reader = AuraReader::open(Cursor::new(bytes))?;
    let replay = reader.replay_orderbook_deltas_fused(2, spec, &mut engine)?;
    assert_eq!(deltas.len(), replay.records);
    assert_eq!(0, replay.rows_materialized);
    assert_eq!(0, replay.values_materialized);
    assert_eq!(0, replay.delta_batch_structs_created);
    engine.finish_profiled(mode, expected_hash, replay.apply_update_ns, 0)
}

fn fused_rows() -> Vec<Vec<AuraValue>> {
    vec![
        vec![
            1_700_000_000_000_000_000_i64.into(),
            7_u64.into(),
            1_u64.into(),
            1000_i64.into(),
            10_i64.into(),
            0_u64.into(),
        ],
        vec![
            1_700_000_000_000_000_001_i64.into(),
            7_u64.into(),
            1_u64.into(),
            1000_i64.into(),
            15_i64.into(),
            0_u64.into(),
        ],
        vec![
            1_700_000_000_000_000_002_i64.into(),
            7_u64.into(),
            1_u64.into(),
            1000_i64.into(),
            0_i64.into(),
            0_u64.into(),
        ],
        vec![
            1_700_000_000_000_000_003_i64.into(),
            8_u64.into(),
            2_u64.into(),
            1002_i64.into(),
            3_i64.into(),
            0_u64.into(),
        ],
    ]
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

#[test]
fn prepared_orderbook_session_reuses_buffers_without_hash_in_production() -> aura_codec::Result<()>
{
    let deltas = dense_deltas(32_768, 64);
    let mut session = OrderBookReplaySession::prepare(&deltas, OrderBookEngineKind::Optimized)?;

    let first = session.replay(&deltas, OrderBookApplyMode::Production, None)?;
    let second = session.replay(&deltas, OrderBookApplyMode::Production, None)?;

    assert_eq!(OrderBookApplyMode::Production, first.apply_mode);
    assert_eq!(OrderBookLifecycleMode::Prepared, first.lifecycle_mode);
    assert_eq!(0, first.state_hash.0);
    assert!(!first.state_hash_checked);
    assert!(second.engine_reused);
    assert!(second.buffers_reused);
    assert_eq!(first.book_levels, second.book_levels);
    assert_eq!(first.adds, second.adds);
    assert_eq!(first.modifies, second.modifies);
    assert_eq!(first.deletes, second.deletes);
    assert_eq!(first.allocation_count_proxy, second.allocation_count_proxy);
    Ok(())
}

#[test]
fn prepared_orderbook_session_verify_hashes_without_rebuilding_engine() -> aura_codec::Result<()> {
    let deltas = dense_deltas(4096, 4);
    let expected = reference_hash(&deltas);
    let mut session = OrderBookReplaySession::prepare(&deltas, OrderBookEngineKind::Optimized)?;

    let stats = session.replay(&deltas, OrderBookApplyMode::Verify, Some(expected))?;

    assert_eq!(OrderBookApplyMode::Verify, stats.apply_mode);
    assert_eq!(OrderBookLifecycleMode::Prepared, stats.lifecycle_mode);
    assert_eq!(expected, stats.state_hash);
    assert!(stats.state_hash_checked);
    assert!(stats.engine_reused);
    assert!(stats.buffers_reused);
    Ok(())
}

#[test]
fn fused_orderbook_matches_prepared_verify_state_hash() -> aura_codec::Result<()> {
    let schema = default_fused_schema();
    let spec = default_fused_spec(&schema);
    let bytes = write_aura1(schema, fused_rows());
    let deltas = deltas_from_aura1(&bytes, &spec);
    let expected = reference_hash(&deltas);

    let fused = run_fused(&bytes, &spec, OrderBookApplyMode::Verify, Some(expected))?;

    assert_eq!(expected, fused.state_hash);
    assert!(fused.state_hash_checked);
    Ok(())
}

#[test]
fn fused_orderbook_matches_existing_prepared_engine() -> aura_codec::Result<()> {
    let schema = default_fused_schema();
    let spec = default_fused_spec(&schema);
    let bytes = write_aura1(schema, fused_rows());
    let deltas = deltas_from_aura1(&bytes, &spec);
    let mut session = OrderBookReplaySession::prepare(&deltas, OrderBookEngineKind::Optimized)?;
    let prepared = session.replay(&deltas, OrderBookApplyMode::Verify, None)?;

    let fused = run_fused(
        &bytes,
        &spec,
        OrderBookApplyMode::Verify,
        Some(prepared.state_hash),
    )?;

    assert_eq!(prepared.state_hash, fused.state_hash);
    assert_eq!(prepared.book_levels, fused.book_levels);
    assert_eq!(prepared.adds, fused.adds);
    assert_eq!(prepared.modifies, fused.modifies);
    assert_eq!(prepared.deletes, fused.deletes);
    Ok(())
}

#[test]
fn fused_orderbook_reordered_schema() -> aura_codec::Result<()> {
    let schema = AuraSchema::builder()
        .field("qty", AuraType::I64)
        .field("px", AuraType::PriceI64Scaled { scale: 4 })
        .field("venue_side", AuraType::EnumU8)
        .field("event_time", AuraType::TimestampNanos)
        .field("instrument", AuraType::U32)
        .field("act", AuraType::U8)
        .build()
        .unwrap();
    let rows = vec![
        vec![
            5_i64.into(),
            200_i64.into(),
            1_u64.into(),
            1_i64.into(),
            12_u64.into(),
            0_u64.into(),
        ],
        vec![
            0_i64.into(),
            200_i64.into(),
            1_u64.into(),
            2_i64.into(),
            12_u64.into(),
            0_u64.into(),
        ],
    ];
    let bytes = write_aura1(schema.clone(), rows);
    let spec = OrderBookDeltaSpec::builder()
        .timestamp("event_time")
        .instrument("instrument")
        .side("venue_side")
        .price("px")
        .size("qty")
        .action_optional("act")
        .build(&schema)?;

    let fused = run_fused(&bytes, &spec, OrderBookApplyMode::Verify, None)?;

    assert_eq!(0, fused.book_levels);
    assert_eq!(1, fused.zero_size_removes);
    Ok(())
}

#[test]
fn fused_orderbook_non_grimoire_names() -> aura_codec::Result<()> {
    let schema = AuraSchema::builder()
        .field("when", AuraType::TimestampNanos)
        .field("venue_instrument", AuraType::U32)
        .field("direction_code", AuraType::EnumU8)
        .field("limit_px", AuraType::PriceI64Scaled { scale: 2 })
        .field("open_qty", AuraType::I64)
        .build()
        .unwrap();
    let rows = vec![vec![
        10_i64.into(),
        44_u64.into(),
        2_u64.into(),
        9900_i64.into(),
        2_i64.into(),
    ]];
    let bytes = write_aura1(schema.clone(), rows);
    let spec = OrderBookDeltaSpec::builder()
        .timestamp("when")
        .instrument("venue_instrument")
        .side("direction_code")
        .price("limit_px")
        .size("open_qty")
        .build(&schema)?;

    let fused = run_fused(&bytes, &spec, OrderBookApplyMode::Verify, None)?;

    assert_eq!(1, fused.book_levels);
    assert_eq!(1, fused.adds);
    Ok(())
}

#[test]
fn fused_orderbook_extra_irrelevant_fields_ignored() -> aura_codec::Result<()> {
    let schema = AuraSchema::builder()
        .field("ts_event", AuraType::TimestampNanos)
        .field("symbol_id", AuraType::U32)
        .field("side", AuraType::EnumU8)
        .field("price", AuraType::PriceI64Scaled { scale: 4 })
        .field("size", AuraType::I64)
        .field("unused_sequence", AuraType::U64)
        .field("unused_flags", AuraType::FlagsU32)
        .build()
        .unwrap();
    let rows = vec![vec![
        1_i64.into(),
        1_u64.into(),
        1_u64.into(),
        100_i64.into(),
        9_i64.into(),
        123_u64.into(),
        7_u64.into(),
    ]];
    let bytes = write_aura1(schema.clone(), rows);
    let spec = OrderBookDeltaSpec::builder()
        .timestamp("ts_event")
        .instrument("symbol_id")
        .side("side")
        .price("price")
        .size("size")
        .build(&schema)?;

    let deltas = deltas_from_aura1(&bytes, &spec);
    let plan = PreparedOrderBookApplyPlan::build(&deltas, OrderBookEngineKind::Optimized)?;
    let mut engine = PreparedOrderBookEngine::with_capacity(&plan)?;
    let reader = AuraReader::open(Cursor::new(&bytes))?;
    let replay = reader.replay_orderbook_deltas_fused(8, &spec, &mut engine)?;

    assert_eq!(4, replay.values_decoded);
    assert_eq!(1, replay.engine_apply_calls);
    Ok(())
}

#[test]
fn fused_orderbook_zero_size_remove() -> aura_codec::Result<()> {
    let schema = default_fused_schema();
    let spec = default_fused_spec(&schema);
    let bytes = write_aura1(schema, fused_rows());

    let fused = run_fused(&bytes, &spec, OrderBookApplyMode::Verify, None)?;

    assert_eq!(1, fused.book_levels);
    assert_eq!(1, fused.zero_size_removes);
    assert_eq!(1, fused.deletes);
    Ok(())
}

#[test]
fn fused_orderbook_invalid_side_rejects() {
    let schema = AuraSchema::builder()
        .field("ts_event", AuraType::TimestampNanos)
        .field("symbol_id", AuraType::U32)
        .field("side", AuraType::I16)
        .field("price", AuraType::PriceI64Scaled { scale: 4 })
        .field("size", AuraType::I64)
        .build()
        .unwrap();
    let rows = vec![vec![
        1_i64.into(),
        1_u64.into(),
        (-1_i64).into(),
        100_i64.into(),
        9_i64.into(),
    ]];
    let bytes = write_aura1(schema.clone(), rows);
    let spec = OrderBookDeltaSpec::builder()
        .timestamp("ts_event")
        .instrument("symbol_id")
        .side("side")
        .price("price")
        .size("size")
        .build(&schema)
        .unwrap();
    let deltas = vec![OrderBookDelta::try_new(1, 1, 1, 100, 9, 0, 0, 0, 0).unwrap()];
    let plan = PreparedOrderBookApplyPlan::build(&deltas, OrderBookEngineKind::Optimized).unwrap();
    let mut engine = PreparedOrderBookEngine::with_capacity(&plan).unwrap();
    let reader = AuraReader::open(Cursor::new(&bytes)).unwrap();

    assert!(reader
        .replay_orderbook_deltas_fused(8, &spec, &mut engine)
        .is_err());
}

#[test]
fn fused_orderbook_preserves_order() -> aura_codec::Result<()> {
    let schema = default_fused_schema();
    let spec = default_fused_spec(&schema);
    let ordered = write_aura1(
        schema.clone(),
        vec![
            vec![
                1_i64.into(),
                3_u64.into(),
                1_u64.into(),
                50_i64.into(),
                10_i64.into(),
                0_u64.into(),
            ],
            vec![
                2_i64.into(),
                3_u64.into(),
                1_u64.into(),
                50_i64.into(),
                0_i64.into(),
                0_u64.into(),
            ],
            vec![
                3_i64.into(),
                3_u64.into(),
                1_u64.into(),
                50_i64.into(),
                20_i64.into(),
                0_u64.into(),
            ],
        ],
    );
    let reordered = write_aura1(
        schema,
        vec![
            vec![
                2_i64.into(),
                3_u64.into(),
                1_u64.into(),
                50_i64.into(),
                0_i64.into(),
                0_u64.into(),
            ],
            vec![
                1_i64.into(),
                3_u64.into(),
                1_u64.into(),
                50_i64.into(),
                10_i64.into(),
                0_u64.into(),
            ],
            vec![
                3_i64.into(),
                3_u64.into(),
                1_u64.into(),
                50_i64.into(),
                20_i64.into(),
                0_u64.into(),
            ],
        ],
    );

    let ordered_stats = run_fused(&ordered, &spec, OrderBookApplyMode::Verify, None)?;
    let reordered_stats = run_fused(&reordered, &spec, OrderBookApplyMode::Verify, None)?;

    assert_eq!(2, ordered_stats.adds);
    assert_eq!(0, ordered_stats.modifies);
    assert_eq!(1, ordered_stats.deletes);
    assert_eq!(1, reordered_stats.adds);
    assert_eq!(1, reordered_stats.modifies);
    assert_eq!(1, reordered_stats.deletes);
    Ok(())
}

#[test]
fn fused_orderbook_production_verify_consistency() -> aura_codec::Result<()> {
    let schema = default_fused_schema();
    let spec = default_fused_spec(&schema);
    let bytes = write_aura1(schema, fused_rows());
    let deltas = deltas_from_aura1(&bytes, &spec);
    let expected = reference_hash(&deltas);

    let production = run_fused(&bytes, &spec, OrderBookApplyMode::Production, None)?;
    let verify = run_fused(&bytes, &spec, OrderBookApplyMode::Verify, Some(expected))?;

    assert_eq!(0, production.state_hash.0);
    assert_eq!(expected, verify.state_hash);
    assert_eq!(production.book_levels, verify.book_levels);
    assert_eq!(production.adds, verify.adds);
    assert_eq!(production.modifies, verify.modifies);
    assert_eq!(production.deletes, verify.deletes);
    Ok(())
}
