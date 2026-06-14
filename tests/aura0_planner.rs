use aura_codec::footer::{AuraFooter, CompressionDescriptor};
use aura_codec::plan::{Aura0Plan, FieldEncoding};
use aura_codec::schema::{
    generic_i64_parent_schema, ohlcv_schema, FieldRelation, FieldType, SchemaBuilder,
};
use aura_codec::stats::PhysicalWidth;
use aura_codec::{FieldRole, IngestStats};

#[test]
fn aura0_planner_uses_schema_relationships_and_fixed_step_time() {
    let schema = ohlcv_schema().unwrap();
    let open = schema.field("open").unwrap();
    let high = schema.field("high").unwrap();
    let low = schema.field("low").unwrap();
    let close = schema.field("close").unwrap();

    assert_eq!(FieldRole::PriceAnchor, open.role);
    assert_eq!(FieldRelation::DeltaFromField(open.index), high.relation);
    assert_eq!(FieldRelation::DeltaFromField(open.index), low.relation);
    assert_eq!(FieldRelation::DeltaFromField(open.index), close.relation);

    let minute = 60_000_000_000i64;
    let rows = [
        [
            1_700_000_000_000_000_000,
            10_000,
            10_012,
            9_995,
            10_006,
            1_000_000,
        ],
        [
            1_700_000_060_000_000_000,
            20_000,
            20_012,
            19_995,
            20_006,
            1_000_007,
        ],
        [
            1_700_000_120_000_000_000,
            10_000,
            10_012,
            9_995,
            10_006,
            999_998,
        ],
        [
            1_700_000_180_000_000_000,
            20_000,
            20_012,
            19_995,
            20_006,
            1_000_003,
        ],
    ];
    let mut stats = IngestStats::new_for_schema(&schema).unwrap();
    for row in rows {
        stats.observe_i64_record(&schema, &row).unwrap();
    }

    let plan = Aura0Plan::from_schema_stats(&schema, &stats).unwrap();
    let time_plan = plan.field("ts_open", &schema).unwrap();
    let high_plan = plan.field("high", &schema).unwrap();
    let volume_plan = plan.field("volume", &schema).unwrap();

    assert_eq!(FieldEncoding::ImplicitFixedStep, time_plan.encoding);
    assert_eq!(PhysicalWidth::Zero, time_plan.width);
    assert_eq!(1_700_000_000_000_000_000, time_plan.base_value);
    assert_eq!(minute, time_plan.step);

    assert_eq!(FieldEncoding::DerivedOffset, high_plan.encoding);
    assert_eq!(Some(open.index), high_plan.reference_field_index);
    assert_eq!(12, high_plan.base_value);
    assert_eq!(0, high_plan.estimated_bytes);

    assert_eq!(FieldEncoding::BitpackedDeltaBase, volume_plan.encoding);
    assert_eq!(999_998, volume_plan.base_value);
    assert_eq!(4, volume_plan.bit_width);
}

#[test]
fn aura_footer_preserves_aura0_encoding_parameters() {
    let schema = ohlcv_schema().unwrap();
    let mut stats = IngestStats::new_for_schema(&schema).unwrap();
    stats
        .observe_i64_record(
            &schema,
            &[
                1_700_000_000_000_000_000,
                10_000,
                10_012,
                9_995,
                10_006,
                1_000_000,
            ],
        )
        .unwrap();
    stats
        .observe_i64_record(
            &schema,
            &[
                1_700_000_060_000_000_000,
                10_003,
                10_011,
                9_997,
                10_004,
                1_000_007,
            ],
        )
        .unwrap();

    let plan = Aura0Plan::from_schema_stats(&schema, &stats).unwrap();
    let footer = AuraFooter::new(schema.clone(), stats)
        .with_compression(CompressionDescriptor::zstd(12))
        .with_aura0_plan(plan);
    let decoded = AuraFooter::decode(&footer.encode().unwrap()).unwrap();

    assert_eq!(footer.schema.fields, decoded.schema.fields);
    assert_eq!(footer.stats, decoded.stats);
    assert_eq!(footer.compression, decoded.compression);
    assert_eq!(footer.aura0_plan, decoded.aura0_plan);
    assert_eq!(footer.aura1_plan, decoded.aura1_plan);
    assert_eq!(footer.chunks, decoded.chunks);
    assert_eq!(
        FieldEncoding::ImplicitFixedStep,
        decoded
            .aura0_plan
            .as_ref()
            .unwrap()
            .field("ts_open", &schema)
            .unwrap()
            .encoding
    );
}

#[test]
fn aura0_planner_scores_related_fields_instead_of_forcing_them() {
    let schema = ohlcv_schema().unwrap();
    let mut stats = IngestStats::new_for_schema(&schema).unwrap();
    for row in [
        [0, 0, 100, 0, 0, 0],
        [1, 1_000_000, 100, 0, 0, 0],
        [2, 0, 100, 0, 0, 0],
        [3, 1_000_000, 100, 0, 0, 0],
    ] {
        stats.observe_i64_record(&schema, &row).unwrap();
    }

    let plan = Aura0Plan::from_schema_stats(&schema, &stats).unwrap();
    let high_plan = plan.field("high", &schema).unwrap();

    assert_eq!(FieldEncoding::BitpackedDeltaBase, high_plan.encoding);
    assert_eq!(100, high_plan.base_value);
    assert!(high_plan.estimated_bytes < 4);
}

#[test]
fn aura0_planner_uses_bitpacked_previous_deltas() {
    let mut stats = IngestStats::new(1).unwrap();
    for idx in 0..130 {
        stats.observe_i64(0, 10_000 + i64::from(idx)).unwrap();
    }

    let plan = Aura0Plan::from_stats(&stats);
    let field = &plan.fields[0];

    assert_eq!(FieldEncoding::BitpackedDeltaPreviousOffset, field.encoding);
    assert_eq!(10_000, field.base_value);
    assert_eq!(1, field.step);
    assert_eq!(0, field.bit_width);
    assert_eq!(0, field.estimated_bytes);
}

#[test]
fn aura0_planner_uses_bitpacked_related_deltas() {
    let schema = ohlcv_schema().unwrap();
    let mut stats = IngestStats::new_for_schema(&schema).unwrap();
    for idx in 0..130 {
        let open = if idx % 2 == 0 { 10_000 } else { 1_000_000 };
        let row = [
            i64::from(idx),
            open,
            open + i64::from(idx % 2),
            open - 1,
            open,
            1_000,
        ];
        stats.observe_i64_record(&schema, &row).unwrap();
    }

    let plan = Aura0Plan::from_schema_stats(&schema, &stats).unwrap();
    let high = plan.field("high", &schema).unwrap();

    assert_eq!(FieldEncoding::BitpackedDeltaRelatedOffset, high.encoding);
    assert_eq!(0, high.base_value);
    assert_eq!(1, high.bit_width);
    assert_eq!(17, high.estimated_bytes);
}

#[test]
fn aura0_planner_derives_constant_related_offsets() {
    let schema = generic_i64_parent_schema("derived_offset", &[255, 0, 2]).unwrap();
    let mut stats = IngestStats::new_for_schema(&schema).unwrap();
    for idx in 0..8 {
        let parent = 1_000_000 + i64::from(idx) * 10;
        stats
            .observe_i64_record(&schema, &[i64::from(idx), parent, parent + 59_999])
            .unwrap();
    }

    let plan = Aura0Plan::from_schema_stats(&schema, &stats).unwrap();
    let field = plan.field("v2", &schema).unwrap();

    assert_eq!(FieldEncoding::DerivedOffset, field.encoding);
    assert_eq!(Some(1), field.reference_field_index);
    assert_eq!(59_999, field.base_value);
    assert_eq!(0, field.estimated_bytes);
}

#[test]
fn aura0_planner_uses_biased_bitpacked_related_deltas() {
    let schema = generic_i64_parent_schema("biased_related", &[255, 0, 2]).unwrap();
    let mut stats = IngestStats::new_for_schema(&schema).unwrap();
    for idx in 0..130 {
        let parent = if idx % 2 == 0 { 1_000_000 } else { 2_000_000 };
        let delta = -130 + i64::from(idx);
        stats
            .observe_i64_record(&schema, &[i64::from(idx), parent, parent + delta])
            .unwrap();
    }

    let plan = Aura0Plan::from_schema_stats(&schema, &stats).unwrap();
    let field = plan.field("v2", &schema).unwrap();

    assert_eq!(FieldEncoding::BitpackedDeltaRelatedOffset, field.encoding);
    assert_eq!(Some(1), field.reference_field_index);
    assert_eq!(-130, field.base_value);
    assert_eq!(8, field.bit_width);
    assert_eq!(130, field.estimated_bytes);
}

#[test]
fn aura0_planner_uses_biased_bitpacked_previous_deltas() {
    let mut stats = IngestStats::new(1).unwrap();
    let mut value = 10_000;
    stats.observe_i64(0, value).unwrap();
    for idx in 0..130 {
        value += 1_000 + i64::from(idx);
        stats.observe_i64(0, value).unwrap();
    }

    let plan = Aura0Plan::from_stats(&stats);
    let field = &plan.fields[0];

    assert_eq!(FieldEncoding::BitpackedDeltaPreviousOffset, field.encoding);
    assert_eq!(10_000, field.base_value);
    assert_eq!(1_000, field.step);
    assert_eq!(8, field.bit_width);
    assert_eq!(130, field.estimated_bytes);
}

#[test]
fn aura0_planner_delta_packs_numeric_identifier_roles() {
    let schema = SchemaBuilder::new("bybit_spot_tick_v1")
        .field("ts_event", FieldType::TimestampNs, FieldRole::Timestamp)
        .field("exec_id", FieldType::I64, FieldRole::Identifier)
        .field("seq", FieldType::I64, FieldRole::Sequence)
        .field("price", FieldType::I64, FieldRole::Price)
        .field("size", FieldType::I64, FieldRole::Quantity)
        .field("side", FieldType::I8, FieldRole::Side)
        .finish()
        .unwrap();
    let mut stats = IngestStats::new_for_schema(&schema).unwrap();
    for idx in 0..64 {
        stats
            .observe_i64_record(
                &schema,
                &[
                    1_700_000_000_000_000_000 + i64::from(idx) * 1_000_000,
                    2_290_000_001_158_834_000 + i64::from(idx),
                    109_538_440_000 + i64::from(idx / 4),
                    637_000 + i64::from(idx % 3),
                    1 + i64::from(idx % 31),
                    if idx % 2 == 0 { 1 } else { -1 },
                ],
            )
            .unwrap();
    }

    let plan = Aura0Plan::from_schema_rows_stats(&schema, &stats, &[]).unwrap();
    let exec_id = plan.field("exec_id", &schema).unwrap();

    assert_eq!(
        FieldEncoding::BitpackedDeltaPreviousOffset,
        exec_id.encoding
    );
    assert_eq!(2_290_000_001_158_834_000, exec_id.base_value);
    assert_eq!(1, exec_id.step);
    assert_eq!(0, exec_id.bit_width);
    assert_eq!(0, exec_id.estimated_bytes);
}

#[test]
fn aura0_planner_keeps_side_and_flags_out_of_residual_search() {
    let schema = SchemaBuilder::new("bybit_tick_flags_v1")
        .field("ts_event", FieldType::TimestampNs, FieldRole::Timestamp)
        .field("seq", FieldType::I64, FieldRole::Sequence)
        .field("price", FieldType::I64, FieldRole::Price)
        .field("size", FieldType::I64, FieldRole::Quantity)
        .field("side", FieldType::I8, FieldRole::Side)
        .field("flag_a", FieldType::I8, FieldRole::Flag)
        .field("flag_b", FieldType::I8, FieldRole::Flag)
        .finish()
        .unwrap();
    let mut stats = IngestStats::new_for_schema(&schema).unwrap();
    let mut rows = Vec::new();
    for idx in 0..128 {
        let row = vec![
            1_700_000_000_000_000_000 + i64::from(idx % 17) * 1_000_000,
            599_456_860_000 + i64::from(idx / 3),
            637_000 + i64::from(idx % 5),
            1 + i64::from(idx % 19),
            if idx % 2 == 0 { 1 } else { -1 },
            0,
            if idx % 13 == 0 { 1 } else { 0 },
        ];
        stats.observe_i64_record(&schema, &row).unwrap();
        rows.push(row);
    }

    let plan = Aura0Plan::from_schema_rows_stats(&schema, &stats, &rows).unwrap();
    let side = plan.field("side", &schema).unwrap();
    let flag_a = plan.field("flag_a", &schema).unwrap();
    let flag_b = plan.field("flag_b", &schema).unwrap();

    assert_eq!(FieldEncoding::BitpackedDeltaBase, side.encoding);
    assert_eq!(FieldEncoding::BitpackedDeltaBase, flag_a.encoding);
    assert_eq!(FieldEncoding::BitpackedDeltaBase, flag_b.encoding);
    assert_eq!(2, side.bit_width);
    assert_eq!(0, flag_a.bit_width);
    assert_eq!(1, flag_b.bit_width);
}
