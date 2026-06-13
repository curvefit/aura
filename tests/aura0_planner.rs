use aura_codec::footer::{AuraFooter, CompressionDescriptor};
use aura_codec::plan::{Aura0Plan, FieldEncoding};
use aura_codec::schema::{ohlcv_schema, FieldRelation};
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

    assert_eq!(FieldEncoding::DeltaRelated, high_plan.encoding);
    assert_eq!(Some(open.index), high_plan.reference_field_index);
    assert_eq!(PhysicalWidth::I8, high_plan.width);

    assert_eq!(FieldEncoding::DeltaPrevious, volume_plan.encoding);
    assert_eq!(1_000_000, volume_plan.base_value);
    assert_eq!(PhysicalWidth::I8, volume_plan.width);
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

    assert_eq!(FieldEncoding::DeltaPrevious, high_plan.encoding);
    assert_eq!(100, high_plan.base_value);
    assert!(high_plan.estimated_bytes < 4);
}
