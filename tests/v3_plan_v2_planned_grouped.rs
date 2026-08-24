use std::io::Cursor;

use aura_codec::{
    canonical_v3_event_batch_sha256, compile_v3_planned_grouped,
    compile_v3_planned_grouped_attempt2, compile_v3_planned_grouped_attempt2_candidate,
    decode_any_compiled_footer, decode_v3_planned_grouped, decode_v3_planned_grouped_footer,
    encode_v3_planned_grouped_footer, AnyCompiledFooter, AuraHeader, AuraPlanV2, AuraV3Column,
    AuraV3ColumnValues as Values, AuraV3EventBatch, AuraV3VariableColumn, FieldRole, FieldScope,
    FieldType, PlanV2Selection, RelationshipPermissions, SchemaBuilder, V3EventLimits,
    V3GroupedAura0Reader, V3GroupedAura0Writer, V3GroupedLimits, V3GroupedWriterOptions,
    AURA_PLAN_V2_REGISTRY_VERSION, AURA_PLAN_V2_VERSION, MAX_AURA_PLAN_V2_BYTES,
    V3_PLANNED_GROUPED_BODY_ENCODING, V3_PLANNED_GROUPED_COMPACT_BLOCK_VERSION,
    V3_PLANNED_GROUPED_COMPACT_BODY_LAYOUT_VERSION, V3_PLANNED_GROUPED_FOOTER_LAYOUT_VERSION,
};
use sha2::{Digest, Sha256};

const PLAN_HASH_DOMAIN: &[u8] = b"aura-plan-v2-registry-v1\0";
const PLANNED_HEADER_HASH_DOMAIN: &[u8] = b"aura-v3-planned-grouped-header-v1\0";
const PLANNED_FOOTER_HASH_DOMAIN: &[u8] = b"aura-v3-planned-grouped-footer-v1\0";
const PLANNED_BODY_HASH_DOMAIN: &[u8] = b"aura-v3-planned-grouped-body-v1\0";

fn permissions() -> RelationshipPermissions {
    RelationshipPermissions::none()
        .with_split()
        .with_within_domain()
        .with_across_domain_same_field()
        .with_joint_same_field()
}

fn small_schema() -> aura_codec::SchemaDescriptor {
    let schema = SchemaBuilder::new("planned_direct_small")
        .field("ts", FieldType::TimestampMs, FieldRole::Timestamp)
        .field("signed_sequence", FieldType::I64, FieldRole::Sequence)
        .repeated_field("side", FieldType::U8, FieldRole::Side)
        .repeated_field("value", FieldType::I64, FieldRole::Value)
        .dual_domain_repeated_group(7, vec![2, 3], 2, permissions())
        .finish()
        .unwrap();
    let groups = schema.groups.clone();
    schema.with_v3_groups(groups).unwrap()
}

fn small_batch(
    schema_id: u32,
    timestamps: &[i64],
    sequences: &[i64],
    offsets: &[u32],
    sides: &[u8],
    values: &[i64],
) -> AuraV3EventBatch {
    AuraV3EventBatch {
        schema_id,
        event_count: timestamps.len() as u32,
        child_offsets: offsets.to_vec(),
        event_columns: vec![
            AuraV3Column {
                slot: 0,
                validity: None,
                values: Values::TimestampMs(timestamps.to_vec()),
            },
            AuraV3Column {
                slot: 1,
                validity: None,
                values: Values::I64(sequences.to_vec()),
            },
        ],
        repeated_columns: vec![
            AuraV3Column {
                slot: 2,
                validity: None,
                values: Values::U8(sides.to_vec()),
            },
            AuraV3Column {
                slot: 3,
                validity: None,
                values: Values::I64(values.to_vec()),
            },
        ],
    }
}

fn all_types_schema() -> aura_codec::SchemaDescriptor {
    let mut schema = SchemaBuilder::new("planned_direct_all_grouped_types")
        .field("ts", FieldType::TimestampMs, FieldRole::Timestamp)
        .field("signed_sequence", FieldType::I64, FieldRole::Sequence)
        .field("timestamp_ns", FieldType::TimestampNs, FieldRole::Timestamp)
        .field("timestamp_ms", FieldType::TimestampMs, FieldRole::Timestamp)
        .repeated_field("side", FieldType::U8, FieldRole::Side)
        .repeated_field("i8", FieldType::I8, FieldRole::Value)
        .repeated_field("i16", FieldType::I16, FieldRole::Value)
        .repeated_field("u16", FieldType::U16, FieldRole::Value)
        .repeated_field("i32", FieldType::I32, FieldRole::Value)
        .repeated_field("u32", FieldType::U32, FieldRole::Value)
        .repeated_field("i64", FieldType::I64, FieldRole::Value)
        .repeated_field("u64", FieldType::U64, FieldRole::Value)
        .repeated_field("i128", FieldType::I128, FieldRole::Value)
        .repeated_field("opaque16", FieldType::Opaque16, FieldRole::Value)
        .repeated_field("utf8", FieldType::Utf8, FieldRole::Value)
        .repeated_field("decimal", FieldType::DecimalText, FieldRole::Value)
        .dual_domain_repeated_group(9, (4..16).collect(), 4, permissions())
        .finish()
        .unwrap();
    for field in &mut schema.fields[5..] {
        field.nullable = true;
    }
    let groups = schema.groups.clone();
    schema.with_v3_groups(groups).unwrap()
}

fn all_types_batch(schema_id: u32) -> AuraV3EventBatch {
    let validity = || Some(vec![0b0000_0101]);
    AuraV3EventBatch {
        schema_id,
        event_count: 3,
        child_offsets: vec![0, 0, 2, 3],
        event_columns: vec![
            AuraV3Column {
                slot: 0,
                validity: None,
                values: Values::TimestampMs(vec![10, 20, 30]),
            },
            AuraV3Column {
                slot: 1,
                validity: None,
                values: Values::I64(vec![-9, 0, -10]),
            },
            AuraV3Column {
                slot: 2,
                validity: None,
                values: Values::TimestampNs(vec![-4, 0, 4]),
            },
            AuraV3Column {
                slot: 3,
                validity: None,
                values: Values::TimestampMs(vec![-5, 0, 5]),
            },
        ],
        repeated_columns: vec![
            AuraV3Column {
                slot: 4,
                validity: None,
                values: Values::U8(vec![0, 1, 0]),
            },
            AuraV3Column {
                slot: 5,
                validity: validity(),
                values: Values::I8(vec![-1, 0, 0]),
            },
            AuraV3Column {
                slot: 6,
                validity: validity(),
                values: Values::I16(vec![-2, 0, 0]),
            },
            AuraV3Column {
                slot: 7,
                validity: validity(),
                values: Values::U16(vec![2, 0, 0]),
            },
            AuraV3Column {
                slot: 8,
                validity: validity(),
                values: Values::I32(vec![-3, 0, 0]),
            },
            AuraV3Column {
                slot: 9,
                validity: validity(),
                values: Values::U32(vec![3, 0, 0]),
            },
            AuraV3Column {
                slot: 10,
                validity: validity(),
                values: Values::I64(vec![i64::MIN, 0, 0]),
            },
            AuraV3Column {
                slot: 11,
                validity: validity(),
                values: Values::U64(vec![u64::MAX, 0, 0]),
            },
            AuraV3Column {
                slot: 12,
                validity: validity(),
                values: Values::I128(vec![i128::MIN, 0, 0]),
            },
            AuraV3Column {
                slot: 13,
                validity: validity(),
                values: Values::Opaque16(vec![[7; 16], [0; 16], [0; 16]]),
            },
            AuraV3Column {
                slot: 14,
                validity: validity(),
                values: Values::Utf8(AuraV3VariableColumn {
                    offsets: vec![0, 0, 0, 1],
                    data: b"x".to_vec(),
                }),
            },
            AuraV3Column {
                slot: 15,
                validity: validity(),
                values: Values::DecimalText(AuraV3VariableColumn {
                    offsets: vec![0, 1, 1, 2],
                    data: b"00".to_vec(),
                }),
            },
        ],
    }
}

#[test]
fn canonical_aup2_direct_plan_is_deterministic_self_contained_and_inspectable() {
    let schema = small_schema();
    let plan = AuraPlanV2::direct_for_schema(&schema).unwrap();
    let first = plan.encode(&schema).unwrap();
    let second = plan.encode(&schema).unwrap();
    assert_eq!(first, second);
    assert_eq!(&first[..4], b"AUP2");
    assert_eq!(AuraPlanV2::decode(&schema, &first).unwrap(), plan);
    assert_eq!(plan.plan_version, AURA_PLAN_V2_VERSION);
    assert_eq!(plan.registry_version, AURA_PLAN_V2_REGISTRY_VERSION);
    assert_eq!(plan.schema_id, schema.schema_id);
    assert_eq!(plan.hash(&schema).unwrap(), plan.hash(&schema).unwrap());
    let inspection = plan.inspection();
    assert_eq!(inspection.selected, PlanV2Selection::Direct);
    assert!(inspection.authoritative_source_order);
    assert_eq!(
        inspection.authorized_relationship_bits,
        permissions().bits()
    );
    assert!(!inspection.relationships_attempted);
    assert!(plan
        .streams
        .iter()
        .all(|stream| stream.op == 0 && stream.dependencies.is_empty()));
}

#[test]
fn aup2_rejects_duplicate_cycle_unauthorized_unknown_noncanonical_trailing_and_oversize() {
    let schema = small_schema();
    let plan = AuraPlanV2::direct_for_schema(&schema).unwrap();

    let mut duplicate = plan.clone();
    duplicate.streams[1].slot = duplicate.streams[0].slot;
    assert!(duplicate.validate(&schema).is_err());

    let mut cycle = plan.clone();
    cycle.streams[0].dependencies = vec![1];
    cycle.streams[1].dependencies = vec![0];
    assert!(cycle.validate(&schema).is_err());

    let mut unauthorized = plan.clone();
    unauthorized.authorized_relationship_bits = 0;
    assert!(unauthorized.validate(&schema).is_err());

    let encoded = plan.encode(&schema).unwrap();
    let mut unknown = encoded.clone();
    unknown[69] = 99;
    resign_plan(&mut unknown);
    assert!(AuraPlanV2::decode(&schema, &unknown).is_err());

    let mut registry1_split = encoded.clone();
    registry1_split[57] = PlanV2Selection::SplitDomainDirect as u8;
    resign_plan(&mut registry1_split);
    assert!(AuraPlanV2::decode(&schema, &registry1_split).is_err());

    let mut noncanonical = plan.clone();
    noncanonical.streams.swap(0, 1);
    assert!(noncanonical.validate(&schema).is_err());

    let mut trailing = encoded[..encoded.len() - 32].to_vec();
    trailing.push(0);
    let total = trailing.len() + 32;
    trailing[12..16].copy_from_slice(&(total as u32).to_le_bytes());
    let hash = plan_hash(&trailing);
    trailing.extend_from_slice(&hash);
    assert!(AuraPlanV2::decode(&schema, &trailing).is_err());

    assert!(AuraPlanV2::decode(&schema, &vec![0; MAX_AURA_PLAN_V2_BYTES + 1]).is_err());
}

#[test]
fn planned_direct_roundtrips_every_grouped_exact_type_and_complete_accounting() {
    let schema = all_types_schema();
    let empty = compile_v3_planned_grouped(&schema, &[], V3GroupedLimits::default()).unwrap();
    let decoded_empty =
        decode_v3_planned_grouped(&empty.bytes, V3GroupedLimits::default()).unwrap();
    assert_eq!(decoded_empty.summary.event_count, 0);
    assert_eq!(decoded_empty.summary.child_count, 0);
    assert_eq!(decoded_empty.summary.chunk_count, 0);
    assert!(decoded_empty.batches.is_empty());

    let batch = all_types_batch(schema.schema_id);
    let artifact = compile_v3_planned_grouped(
        &schema,
        std::slice::from_ref(&batch),
        V3GroupedLimits::default(),
    )
    .unwrap();
    assert_eq!(artifact.summary.file_bytes, artifact.bytes.len() as u64);
    assert_eq!(artifact.summary.file_bytes, 1964);
    assert_eq!(
        artifact.summary.file_bytes,
        artifact.summary.accounted_file_bytes
    );
    assert_eq!(artifact.inspection.body_encoding, 3);
    assert_eq!(artifact.inspection.compression, "none");
    assert_eq!(artifact.inspection.plan.selected, PlanV2Selection::Direct);
    assert!(artifact.inspection.schema_relationships_authorized_only);
    assert!(!artifact.inspection.plan.relationships_attempted);
    let decoded = decode_v3_planned_grouped(&artifact.bytes, V3GroupedLimits::default()).unwrap();
    assert_eq!(decoded.batches, vec![batch.clone()]);
    let split = compile_v3_planned_grouped_attempt2_candidate(
        &schema,
        std::slice::from_ref(&batch),
        Default::default(),
        PlanV2Selection::SplitDomainDirect,
    )
    .unwrap();
    let split_repeat = compile_v3_planned_grouped_attempt2_candidate(
        &schema,
        std::slice::from_ref(&batch),
        V3GroupedLimits::default(),
        PlanV2Selection::SplitDomainDirect,
    )
    .unwrap();
    assert_eq!(split.bytes, split_repeat.bytes);
    assert_eq!(split.summary, split_repeat.summary);
    assert_eq!(
        decode_v3_planned_grouped(&split.bytes, Default::default())
            .unwrap()
            .batches,
        vec![batch]
    );
    assert_eq!(decoded.summary, artifact.summary);
    assert_eq!(decoded.footer.plan_sha256, artifact.summary.plan_sha256);
    eprintln!(
        "development synthetic planned direct complete bytes: {}",
        artifact.summary.file_bytes
    );
}

#[test]
fn planned_direct_and_exact_body2_share_canonical_hash_across_legal_rechunking() {
    let schema = small_schema();
    let whole = small_batch(
        schema.schema_id,
        &[10, 20, 30],
        &[-3, 0, -4],
        &[0, 0, 2, 3],
        &[0, 1, 0],
        &[100, 0, -100],
    );
    let split = vec![
        small_batch(schema.schema_id, &[10], &[-3], &[0, 0], &[], &[]),
        small_batch(
            schema.schema_id,
            &[20, 30],
            &[0, -4],
            &[0, 2, 3],
            &[0, 1, 0],
            &[100, 0, -100],
        ),
    ];
    let planned_whole = compile_v3_planned_grouped(
        &schema,
        std::slice::from_ref(&whole),
        V3GroupedLimits::default(),
    )
    .unwrap();
    let planned_split =
        compile_v3_planned_grouped(&schema, &split, V3GroupedLimits::default()).unwrap();
    assert_eq!(
        planned_whole.summary.global_logical_sha256,
        planned_split.summary.global_logical_sha256
    );
    assert_eq!(
        planned_whole.summary.plan_sha256,
        planned_split.summary.plan_sha256
    );
    assert_eq!(planned_whole.summary.file_bytes, 997);
    assert_eq!(planned_split.summary.file_bytes, 1277);
    assert_ne!(planned_whole.bytes, planned_split.bytes);

    let mut exact_writer = V3GroupedAura0Writer::try_new(
        Cursor::new(Vec::new()),
        schema.clone(),
        V3GroupedWriterOptions::default(),
    )
    .unwrap();
    for batch in &split {
        exact_writer.write_batch(batch).unwrap();
    }
    let (exact_cursor, exact_write_summary) = exact_writer.finish().unwrap();
    let exact_bytes = exact_cursor.into_inner();
    assert_eq!(exact_write_summary.file_bytes, exact_bytes.len() as u64);
    eprintln!(
        "development synthetic complete bytes: planned_whole={} planned_split={} exact_body2_split={}",
        planned_whole.summary.file_bytes,
        planned_split.summary.file_bytes,
        exact_write_summary.file_bytes
    );
    let mut exact_reader = V3GroupedAura0Reader::open(Cursor::new(exact_bytes)).unwrap();
    let exact_summary = exact_reader.verify_all().unwrap();
    assert_eq!(
        exact_summary.global_logical_sha256,
        planned_whole.summary.global_logical_sha256
    );
    assert_eq!(
        canonical_v3_event_batch_sha256(&schema, &whole, Default::default()).unwrap(),
        planned_whole.summary.global_logical_sha256
    );
}

#[test]
fn planned_footer_dispatch_is_additive_and_wrong_tuple_fails_closed() {
    let schema = small_schema();
    let batch = small_batch(schema.schema_id, &[1], &[-1], &[0, 0], &[], &[]);
    let artifact =
        compile_v3_planned_grouped(&schema, &[batch], V3GroupedLimits::default()).unwrap();
    let footer = footer_bytes(&artifact.bytes);
    match decode_any_compiled_footer(footer).unwrap() {
        AnyCompiledFooter::V3PlannedGrouped(value) => {
            assert_eq!(value.plan_sha256, artifact.summary.plan_sha256)
        }
        other => panic!("wrong route: {other:?}"),
    }
    assert_eq!(
        u16::from_le_bytes(footer[6..8].try_into().unwrap()),
        V3_PLANNED_GROUPED_FOOTER_LAYOUT_VERSION
    );
    assert_eq!(footer[8], V3_PLANNED_GROUPED_BODY_ENCODING);
    let mut wrong = footer.to_vec();
    wrong[8] = 2;
    assert!(decode_any_compiled_footer(&wrong).is_err());
}

#[test]
fn planned_artifact_every_prefix_and_single_byte_mutation_fails_without_panicking() {
    let schema = small_schema();
    let batch = small_batch(schema.schema_id, &[1], &[-1], &[0, 0], &[], &[]);
    let artifact =
        compile_v3_planned_grouped(&schema, &[batch], V3GroupedLimits::default()).unwrap();
    for length in 0..artifact.bytes.len() {
        let result = std::panic::catch_unwind(|| {
            decode_v3_planned_grouped(&artifact.bytes[..length], V3GroupedLimits::default())
        });
        assert!(
            result.is_ok_and(|decoded| decoded.is_err()),
            "prefix {length}"
        );
    }
    for index in 0..artifact.bytes.len() {
        let mut corrupted = artifact.bytes.clone();
        corrupted[index] ^= 1;
        let result = std::panic::catch_unwind(|| {
            decode_v3_planned_grouped(&corrupted, V3GroupedLimits::default())
        });
        assert!(result.is_ok_and(|decoded| decoded.is_err()), "byte {index}");
    }
}

#[test]
fn planned_direct_preserves_multiple_signed_sequence_fields() {
    let schema = SchemaBuilder::new("planned_multiple_signed_sequences")
        .field("ts", FieldType::TimestampMs, FieldRole::Timestamp)
        .field("sequence_a", FieldType::I64, FieldRole::Sequence)
        .field("sequence_b", FieldType::I64, FieldRole::Sequence)
        .repeated_field("side", FieldType::U8, FieldRole::Side)
        .repeated_field("value", FieldType::I64, FieldRole::Value)
        .dual_domain_repeated_group(4, vec![3, 4], 3, permissions())
        .finish()
        .unwrap();
    let groups = schema.groups.clone();
    let schema = schema.with_v3_groups(groups).unwrap();
    let batch = AuraV3EventBatch {
        schema_id: schema.schema_id,
        event_count: 3,
        child_offsets: vec![0, 0, 1, 2],
        event_columns: vec![
            AuraV3Column {
                slot: 0,
                validity: None,
                values: Values::TimestampMs(vec![1, 2, 3]),
            },
            AuraV3Column {
                slot: 1,
                validity: None,
                values: Values::I64(vec![-1, 0, i64::MIN]),
            },
            AuraV3Column {
                slot: 2,
                validity: None,
                values: Values::I64(vec![i64::MAX, -2, 0]),
            },
        ],
        repeated_columns: vec![
            AuraV3Column {
                slot: 3,
                validity: None,
                values: Values::U8(vec![0, 1]),
            },
            AuraV3Column {
                slot: 4,
                validity: None,
                values: Values::I64(vec![0, -9]),
            },
        ],
    };
    let artifact = compile_v3_planned_grouped(
        &schema,
        std::slice::from_ref(&batch),
        V3GroupedLimits::default(),
    )
    .unwrap();
    let decoded = decode_v3_planned_grouped(&artifact.bytes, V3GroupedLimits::default()).unwrap();
    assert_eq!(decoded.batches, vec![batch]);
}

#[test]
fn planned_header_rejects_resigned_valid_noncanonical_comment() {
    let schema = small_schema();
    let batch = small_batch(schema.schema_id, &[1], &[-1], &[0, 0], &[], &[]);
    let artifact =
        compile_v3_planned_grouped(&schema, &[batch], V3GroupedLimits::default()).unwrap();
    let original_header_len = AuraHeader::encoded_len(&artifact.bytes).unwrap();
    let original_footer_start = footer_start(&artifact.bytes);
    let body = &artifact.bytes[original_header_len..original_footer_start];
    let mut header = AuraHeader::decode(&artifact.bytes[..original_header_len]).unwrap();
    header.comment = "valid but noncanonical".to_owned();
    let header_bytes = header.encode().unwrap();
    let mut footer =
        decode_v3_planned_grouped_footer(footer_bytes(&artifact.bytes), V3GroupedLimits::HARD)
            .unwrap();
    footer.header_sha256 = domain_hash(PLANNED_HEADER_HASH_DOMAIN, &header_bytes);
    let footer_bytes = encode_v3_planned_grouped_footer(&footer, V3GroupedLimits::HARD).unwrap();
    let mut resigned = Vec::new();
    resigned.extend_from_slice(&header_bytes);
    resigned.extend_from_slice(body);
    resigned.extend_from_slice(&footer_bytes);
    resigned.extend_from_slice(&(footer_bytes.len() as u32).to_le_bytes());
    resigned.extend_from_slice(b"sealed:)");
    assert!(decode_v3_planned_grouped(&resigned, V3GroupedLimits::HARD).is_err());
}

#[test]
fn planned_footer_descriptor_limits_are_symmetric_and_exact() {
    let schema = small_schema();
    let batch = small_batch(
        schema.schema_id,
        &[1, 2],
        &[-1, -2],
        &[0, 1, 2],
        &[0, 1],
        &[3, 4],
    );
    let artifact =
        compile_v3_planned_grouped(&schema, &[batch], V3GroupedLimits::default()).unwrap();
    let footer =
        decode_v3_planned_grouped_footer(footer_bytes(&artifact.bytes), V3GroupedLimits::HARD)
            .unwrap();
    let stored_len = footer.chunks[0].stored_len as usize;
    let event_fields = footer
        .schema
        .fields
        .iter()
        .filter(|field| field.scope == FieldScope::Event)
        .count();
    let repeated_fields = footer.schema.fields.len() - event_fields;
    let logical_values = footer.chunks[0].event_count as usize * event_fields
        + footer.chunks[0].child_count as usize * repeated_fields;
    let hard_bytes = encode_v3_planned_grouped_footer(&footer, V3GroupedLimits::HARD).unwrap();
    let exact = V3GroupedLimits {
        max_footer_bytes: hard_bytes.len(),
        max_body_bytes: footer.body_len,
        max_chunks: footer.chunks.len(),
        max_events: footer.event_count,
        max_children: footer.child_count,
        event_limits: V3EventLimits {
            max_block_bytes: stored_len,
            max_variable_value_bytes: V3EventLimits::default().max_variable_value_bytes,
            max_events: footer.chunks[0].event_count as usize,
            max_children: footer.chunks[0].child_count as usize,
            max_values: logical_values,
        },
    };
    assert_eq!(
        encode_v3_planned_grouped_footer(&footer, exact).unwrap(),
        hard_bytes
    );
    assert!(decode_v3_planned_grouped_footer(&hard_bytes, exact).is_ok());

    let mut lower = exact;
    lower.max_footer_bytes -= 1;
    assert!(encode_v3_planned_grouped_footer(&footer, lower).is_err());
    assert!(decode_v3_planned_grouped_footer(&hard_bytes, lower).is_err());
    lower = exact;
    lower.event_limits.max_block_bytes -= 1;
    assert!(encode_v3_planned_grouped_footer(&footer, lower).is_err());
    lower = exact;
    lower.event_limits.max_events -= 1;
    assert!(encode_v3_planned_grouped_footer(&footer, lower).is_err());
    lower = exact;
    lower.event_limits.max_children -= 1;
    assert!(encode_v3_planned_grouped_footer(&footer, lower).is_err());
    lower = exact;
    lower.event_limits.max_values -= 1;
    assert!(encode_v3_planned_grouped_footer(&footer, lower).is_err());

    let mut zero_stored = footer.clone();
    zero_stored.chunks[0].stored_len = 0;
    zero_stored.body_len = 0;
    assert!(encode_v3_planned_grouped_footer(&zero_stored, V3GroupedLimits::HARD).is_err());
}

#[test]
fn resigned_footer_tuple_reserved_count_and_range_corruption_fail_closed() {
    let schema = small_schema();
    let batch = small_batch(schema.schema_id, &[1], &[-1], &[0, 0], &[], &[]);
    let artifact =
        compile_v3_planned_grouped(&schema, &[batch], V3GroupedLimits::default()).unwrap();
    let footer = footer_bytes(&artifact.bytes);
    let schema_len = u32::from_le_bytes(footer[48..52].try_into().unwrap()) as usize;
    let plan_len = u32::from_le_bytes(footer[52..56].try_into().unwrap()) as usize;
    let descriptor = 216 + schema_len + plan_len + 8;
    for (name, offset) in [
        ("tuple", 8usize),
        ("reserved", 9),
        ("count", 12),
        ("range", descriptor + 40),
        ("descriptor_reserved", descriptor + 4),
    ] {
        let mut corrupted = footer.to_vec();
        corrupted[offset] ^= 1;
        resign_footer(&mut corrupted);
        assert!(
            decode_v3_planned_grouped_footer(&corrupted, V3GroupedLimits::HARD).is_err(),
            "{name}"
        );
    }
}

#[test]
fn existing_v2_flat_and_exact_grouped_dispatch_remain_distinct() {
    let v2 = include_bytes!("fixtures/v2/plain-compact.aura0");
    assert!(matches!(
        decode_any_compiled_footer(footer_bytes(v2)).unwrap(),
        AnyCompiledFooter::V2(_)
    ));
    let flat = include_bytes!("fixtures/v3-container/three-row-two-chunk.aura0");
    assert!(matches!(
        decode_any_compiled_footer(footer_bytes(flat)).unwrap(),
        AnyCompiledFooter::V3Flat(_)
    ));
    let schema = small_schema();
    let batch = small_batch(schema.schema_id, &[1], &[-1], &[0, 0], &[], &[]);
    let mut writer = V3GroupedAura0Writer::try_new(
        Cursor::new(Vec::new()),
        schema,
        V3GroupedWriterOptions::default(),
    )
    .unwrap();
    writer.write_batch(&batch).unwrap();
    let exact = writer.finish().unwrap().0.into_inner();
    assert!(matches!(
        decode_any_compiled_footer(footer_bytes(&exact)).unwrap(),
        AnyCompiledFooter::V3Grouped(_)
    ));
}

#[test]
fn planned_direct_preserves_nullable_event_scope_null_and_zero() {
    let mut schema = SchemaBuilder::new("planned_nullable_event")
        .field("ts", FieldType::TimestampMs, FieldRole::Timestamp)
        .nullable_field("event_value", FieldType::I64, FieldRole::Value)
        .repeated_field("side", FieldType::U8, FieldRole::Side)
        .repeated_field("value", FieldType::I64, FieldRole::Value)
        .dual_domain_repeated_group(3, vec![2, 3], 2, permissions())
        .finish()
        .unwrap();
    let groups = schema.groups.clone();
    schema = schema.with_v3_groups(groups).unwrap();
    let batch = AuraV3EventBatch {
        schema_id: schema.schema_id,
        event_count: 2,
        child_offsets: vec![0, 0, 1],
        event_columns: vec![
            AuraV3Column {
                slot: 0,
                validity: None,
                values: Values::TimestampMs(vec![1, 2]),
            },
            AuraV3Column {
                slot: 1,
                validity: Some(vec![0b10]),
                values: Values::I64(vec![0, 0]),
            },
        ],
        repeated_columns: vec![
            AuraV3Column {
                slot: 2,
                validity: None,
                values: Values::U8(vec![0]),
            },
            AuraV3Column {
                slot: 3,
                validity: None,
                values: Values::I64(vec![0]),
            },
        ],
    };
    let artifact = compile_v3_planned_grouped(
        &schema,
        std::slice::from_ref(&batch),
        V3GroupedLimits::default(),
    )
    .unwrap();
    assert_eq!(
        decode_v3_planned_grouped(&artifact.bytes, V3GroupedLimits::default())
            .unwrap()
            .batches,
        vec![batch]
    );
}

#[test]
fn attempt2_scores_real_direct_and_split_files_and_preserves_exact_inverse() {
    let schema = all_types_schema();
    let batch = all_types_batch(schema.schema_id);
    let direct = compile_v3_planned_grouped_attempt2_candidate(
        &schema,
        std::slice::from_ref(&batch),
        V3GroupedLimits::default(),
        PlanV2Selection::Direct,
    )
    .unwrap();
    let split = compile_v3_planned_grouped_attempt2_candidate(
        &schema,
        std::slice::from_ref(&batch),
        V3GroupedLimits::default(),
        PlanV2Selection::SplitDomainDirect,
    )
    .unwrap();
    for candidate in [&direct, &split] {
        assert_eq!(candidate.summary.file_bytes, candidate.bytes.len() as u64);
        assert_eq!(
            candidate.summary.file_bytes,
            candidate.summary.accounted_file_bytes
        );
        assert_eq!(candidate.inspection.body_layout_version, 2);
        assert_eq!(candidate.inspection.direct_block_version, 2);
        assert_eq!(
            decode_v3_planned_grouped(&candidate.bytes, V3GroupedLimits::default())
                .unwrap()
                .batches,
            vec![batch.clone()]
        );
        assert_eq!(
            candidate.summary.global_logical_sha256,
            canonical_v3_event_batch_sha256(&schema, &batch, Default::default()).unwrap()
        );
    }
    assert_eq!(
        split.inspection.plan.selected,
        PlanV2Selection::SplitDomainDirect
    );
    assert!(split.inspection.plan.relationships_attempted);
    assert!(!split.inspection.schema_relationships_authorized_only);
    assert!(direct.inspection.schema_relationships_authorized_only);
    assert!(
        split.inspection.plan.authorized_relationship_bits & RelationshipPermissions::SPLIT != 0
    );
    let split_footer =
        decode_v3_planned_grouped_footer(footer_bytes(&split.bytes), V3GroupedLimits::HARD)
            .unwrap();
    assert_eq!(
        split_footer.plan.decode_order,
        (0..schema.fields.len() as u16).collect::<Vec<_>>()
    );
    assert_eq!(split_footer.plan.event_child_offsets_stream_id, Some(0));
    let discriminator = &split_footer.plan.streams[split_footer.plan.discriminator_slot as usize];
    assert_eq!(
        split_footer.plan.source_order_selector_stream_id,
        discriminator.physical_stream_ids.first().copied()
    );
    assert!(split_footer.plan.streams.iter().all(|stream| {
        !stream.physical_stream_ids.is_empty()
            && (stream.scope == FieldScope::Event
                || stream.slot == split_footer.plan.discriminator_slot
                || stream.physical_stream_ids.len() == 2)
    }));
    assert!(split.bytes.windows(3).all(|window| window != b"bid"));
    assert!(split.bytes.windows(3).all(|window| window != b"ask"));

    let selected = compile_v3_planned_grouped_attempt2(
        &schema,
        std::slice::from_ref(&batch),
        V3GroupedLimits::default(),
    )
    .unwrap();
    assert_eq!(selected.inspection.candidates.len(), 3);
    assert!(selected.inspection.candidates[2].authorized);
    assert!(selected.inspection.candidates[2].applicable);
    assert_eq!(
        selected.inspection.candidates[1].complete_bytes,
        Some(direct.summary.file_bytes)
    );
    assert_eq!(
        selected.inspection.candidates[2].complete_bytes,
        Some(split.summary.file_bytes)
    );
    let registry1_bytes = selected.inspection.candidates[0].complete_bytes.unwrap();
    assert_eq!(
        selected.summary.file_bytes,
        registry1_bytes
            .min(direct.summary.file_bytes)
            .min(split.summary.file_bytes)
    );
    assert_eq!(
        selected
            .inspection
            .candidates
            .iter()
            .filter(|row| row.selected)
            .count(),
        1
    );
    assert_eq!(selected.inspection.plan.selected, PlanV2Selection::Direct);
    assert!(selected.inspection.candidates[1].selected);
    eprintln!(
        "development attempt2 all-types complete bytes: registry1={} compact_direct={} compact_split={} selected={}",
        registry1_bytes,
        direct.summary.file_bytes,
        split.summary.file_bytes,
        selected.summary.file_bytes
    );
}

#[test]
fn attempt2_generic_and_okx_like_asymmetry_zero_child_and_cost_fallback() {
    let generic = small_schema();
    let generic_batches = vec![
        small_batch(generic.schema_id, &[1], &[-1], &[0, 0], &[], &[]),
        small_batch(
            generic.schema_id,
            &[2, 3],
            &[-2, -3],
            &[0, 3, 4],
            &[0, 0, 0, 1],
            &[0, 1, 2, 3],
        ),
    ];
    let generic_direct = compile_v3_planned_grouped_attempt2_candidate(
        &generic,
        &generic_batches,
        V3GroupedLimits::default(),
        PlanV2Selection::Direct,
    )
    .unwrap();
    let generic_split = compile_v3_planned_grouped_attempt2_candidate(
        &generic,
        &generic_batches,
        V3GroupedLimits::default(),
        PlanV2Selection::SplitDomainDirect,
    )
    .unwrap();
    let selected =
        compile_v3_planned_grouped_attempt2(&generic, &generic_batches, V3GroupedLimits::default())
            .unwrap();
    let empty_selected =
        compile_v3_planned_grouped_attempt2(&generic, &[], V3GroupedLimits::default()).unwrap();
    assert_eq!(
        empty_selected.inspection.plan.selected,
        PlanV2Selection::Direct
    );
    assert_eq!(
        empty_selected.inspection.candidates[2].rejection.as_deref(),
        Some("complete cost did not beat direct")
    );
    assert_eq!(
        decode_v3_planned_grouped(&generic_split.bytes, Default::default())
            .unwrap()
            .batches,
        generic_batches
    );
    let mut exact_writer = V3GroupedAura0Writer::try_new(
        Cursor::new(Vec::new()),
        generic.clone(),
        V3GroupedWriterOptions::default(),
    )
    .unwrap();
    for batch in &generic_batches {
        exact_writer.write_batch(batch).unwrap();
    }
    let (_, exact_summary) = exact_writer.finish().unwrap();
    assert_eq!(
        exact_summary.global_logical_sha256,
        generic_split.summary.global_logical_sha256
    );

    let mut okx = SchemaBuilder::new("generic_order_count_extension")
        .field("ts", FieldType::TimestampMs, FieldRole::Timestamp)
        .field("sequence", FieldType::I64, FieldRole::Sequence)
        .repeated_field("side", FieldType::U8, FieldRole::Side)
        .repeated_field("price", FieldType::I64, FieldRole::Price)
        .repeated_field("quantity", FieldType::I64, FieldRole::Quantity)
        .repeated_field("order_count", FieldType::U32, FieldRole::Count)
        .dual_domain_repeated_group(8, vec![2, 3, 4, 5], 2, permissions())
        .finish()
        .unwrap();
    okx.fields[5].nullable = true;
    let groups = okx.groups.clone();
    okx = okx.with_v3_groups(groups).unwrap();
    let okx_batch = AuraV3EventBatch {
        schema_id: okx.schema_id,
        event_count: 2,
        child_offsets: vec![0, 0, 3],
        event_columns: vec![
            AuraV3Column {
                slot: 0,
                validity: None,
                values: Values::TimestampMs(vec![1, 2]),
            },
            AuraV3Column {
                slot: 1,
                validity: None,
                values: Values::I64(vec![-1, -2]),
            },
        ],
        repeated_columns: vec![
            AuraV3Column {
                slot: 2,
                validity: None,
                values: Values::U8(vec![1, 1, 0]),
            },
            AuraV3Column {
                slot: 3,
                validity: None,
                values: Values::I64(vec![10, 11, 9]),
            },
            AuraV3Column {
                slot: 4,
                validity: None,
                values: Values::I64(vec![0, 2, 3]),
            },
            AuraV3Column {
                slot: 5,
                validity: Some(vec![0b101]),
                values: Values::U32(vec![0, 0, 7]),
            },
        ],
    };
    let okx_split = compile_v3_planned_grouped_attempt2_candidate(
        &okx,
        std::slice::from_ref(&okx_batch),
        Default::default(),
        PlanV2Selection::SplitDomainDirect,
    )
    .unwrap();
    assert_eq!(
        decode_v3_planned_grouped(&okx_split.bytes, Default::default())
            .unwrap()
            .batches,
        vec![okx_batch]
    );
    eprintln!(
        "development attempt2 generic complete bytes: registry1={} compact_direct={} compact_split={} selected={}; okx_split={}",
        selected.inspection.candidates[0].complete_bytes.unwrap(),
        generic_direct.summary.file_bytes,
        generic_split.summary.file_bytes,
        selected.summary.file_bytes,
        okx_split.summary.file_bytes
    );
}

#[test]
fn attempt2_forbidden_split_invalid_selector_corruption_and_bounds_fail_closed() {
    let schema = small_schema();
    let batch = small_batch(schema.schema_id, &[1], &[-1], &[0, 1], &[0], &[1]);
    let split = compile_v3_planned_grouped_attempt2_candidate(
        &schema,
        std::slice::from_ref(&batch),
        Default::default(),
        PlanV2Selection::SplitDomainDirect,
    )
    .unwrap();
    for index in 0..split.bytes.len() {
        let mut corrupted = split.bytes.clone();
        corrupted[index] ^= 1;
        assert!(decode_v3_planned_grouped(&corrupted, Default::default()).is_err());
    }
    let mut bad_mapping =
        decode_v3_planned_grouped_footer(footer_bytes(&split.bytes), V3GroupedLimits::HARD)
            .unwrap();
    bad_mapping.plan.streams[0].physical_stream_ids[0] = u16::MAX;
    assert!(encode_v3_planned_grouped_footer(&bad_mapping, V3GroupedLimits::HARD).is_err());
    for block_offset in [60usize, 64] {
        let resigned = resign_attempt2_body_mutation(&split, block_offset);
        assert!(decode_v3_planned_grouped(&resigned, V3GroupedLimits::HARD).is_err());
    }
    let mut lower = V3GroupedLimits::default();
    lower.event_limits.max_block_bytes = split.summary.body_bytes as usize - 1;
    assert!(compile_v3_planned_grouped_attempt2_candidate(
        &schema,
        std::slice::from_ref(&batch),
        lower,
        PlanV2Selection::SplitDomainDirect
    )
    .is_err());

    let forbidden = SchemaBuilder::new("split_forbidden")
        .field("ts", FieldType::TimestampMs, FieldRole::Timestamp)
        .field("sequence", FieldType::I64, FieldRole::Sequence)
        .repeated_field("side", FieldType::U8, FieldRole::Side)
        .repeated_field("value", FieldType::I64, FieldRole::Value)
        .dual_domain_repeated_group(
            1,
            vec![2, 3],
            2,
            RelationshipPermissions::none().with_within_domain(),
        )
        .finish()
        .unwrap();
    let groups = forbidden.groups.clone();
    let forbidden = forbidden.with_v3_groups(groups).unwrap();
    let forbidden_batch = small_batch(forbidden.schema_id, &[1], &[-1], &[0, 1], &[0], &[1]);
    assert!(compile_v3_planned_grouped_attempt2_candidate(
        &forbidden,
        std::slice::from_ref(&forbidden_batch),
        Default::default(),
        PlanV2Selection::SplitDomainDirect
    )
    .is_err());
    let selected =
        compile_v3_planned_grouped_attempt2(&forbidden, &[forbidden_batch], Default::default())
            .unwrap();
    assert_eq!(selected.inspection.plan.selected, PlanV2Selection::Direct);
    assert!(!selected.inspection.candidates[2].authorized);

    let mut invalid_side = batch;
    invalid_side.repeated_columns[0].values = Values::U8(vec![2]);
    assert!(
        compile_v3_planned_grouped_attempt2(&schema, &[invalid_side], Default::default()).is_err()
    );
    assert_eq!(
        split.inspection.body_layout_version,
        V3_PLANNED_GROUPED_COMPACT_BODY_LAYOUT_VERSION
    );
    assert_eq!(
        split.inspection.direct_block_version,
        V3_PLANNED_GROUPED_COMPACT_BLOCK_VERSION
    );
}

#[test]
fn attempt2_physical_ids_follow_direct_and_split_order_when_side_is_not_first() {
    let schema = SchemaBuilder::new("discriminator_not_first")
        .field("ts", FieldType::TimestampMs, FieldRole::Timestamp)
        .repeated_field("value_before_side", FieldType::I64, FieldRole::Value)
        .repeated_field("side", FieldType::U8, FieldRole::Side)
        .repeated_field("value_after_side", FieldType::I64, FieldRole::Value)
        .dual_domain_repeated_group(5, vec![1, 2, 3], 2, permissions())
        .finish()
        .unwrap();
    let groups = schema.groups.clone();
    let schema = schema.with_v3_groups(groups).unwrap();
    let batch = AuraV3EventBatch {
        schema_id: schema.schema_id,
        event_count: 1,
        child_offsets: vec![0, 3],
        event_columns: vec![AuraV3Column {
            slot: 0,
            validity: None,
            values: Values::TimestampMs(vec![1]),
        }],
        repeated_columns: vec![
            AuraV3Column {
                slot: 1,
                validity: None,
                values: Values::I64(vec![10, 20, 30]),
            },
            AuraV3Column {
                slot: 2,
                validity: None,
                values: Values::U8(vec![1, 0, 1]),
            },
            AuraV3Column {
                slot: 3,
                validity: None,
                values: Values::I64(vec![-1, -2, -3]),
            },
        ],
    };
    for selection in [PlanV2Selection::Direct, PlanV2Selection::SplitDomainDirect] {
        let artifact = compile_v3_planned_grouped_attempt2_candidate(
            &schema,
            std::slice::from_ref(&batch),
            Default::default(),
            selection,
        )
        .unwrap();
        assert_eq!(
            decode_v3_planned_grouped(&artifact.bytes, Default::default())
                .unwrap()
                .batches,
            vec![batch.clone()]
        );
        let footer =
            decode_v3_planned_grouped_footer(footer_bytes(&artifact.bytes), V3GroupedLimits::HARD)
                .unwrap();
        let physical_slots = footer
            .plan
            .streams
            .iter()
            .filter(|stream| !stream.physical_stream_ids.is_empty())
            .flat_map(|stream| {
                stream
                    .physical_stream_ids
                    .iter()
                    .map(move |id| (*id, stream.slot))
            })
            .collect::<Vec<_>>();
        if selection == PlanV2Selection::Direct {
            assert!(physical_slots.contains(&(2, 2)));
            assert!(physical_slots.contains(&(3, 1)));
        } else {
            assert!(physical_slots.contains(&(2, 2)));
            assert!(physical_slots
                .iter()
                .any(|(id, slot)| *id > 2 && *slot == 1));
        }
    }
}

fn plan_hash(bytes: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(PLAN_HASH_DOMAIN);
    hasher.update((bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
    hasher.finalize().into()
}

fn resign_plan(bytes: &mut [u8]) {
    let hash_start = bytes.len() - 32;
    let hash = plan_hash(&bytes[..hash_start]);
    bytes[hash_start..].copy_from_slice(&hash);
}

fn domain_hash(domain: &[u8], bytes: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(domain);
    hasher.update((bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
    hasher.finalize().into()
}

fn resign_footer(bytes: &mut [u8]) {
    let hash_start = bytes.len() - 32;
    let hash = domain_hash(PLANNED_FOOTER_HASH_DOMAIN, &bytes[..hash_start]);
    bytes[hash_start..].copy_from_slice(&hash);
}

fn footer_start(file: &[u8]) -> usize {
    let footer_len_offset = file.len() - 12;
    let footer_len = u32::from_le_bytes(
        file[footer_len_offset..footer_len_offset + 4]
            .try_into()
            .unwrap(),
    ) as usize;
    footer_len_offset - footer_len
}

fn resign_attempt2_body_mutation(
    artifact: &aura_codec::V3PlannedGroupedArtifact,
    block_offset: usize,
) -> Vec<u8> {
    let header_len = AuraHeader::encoded_len(&artifact.bytes).unwrap();
    let mut body = artifact.bytes[header_len..footer_start(&artifact.bytes)].to_vec();
    body[block_offset] ^= 1;
    let mut footer =
        decode_v3_planned_grouped_footer(footer_bytes(&artifact.bytes), V3GroupedLimits::HARD)
            .unwrap();
    footer.body_sha256 = domain_hash(PLANNED_BODY_HASH_DOMAIN, &body);
    footer.chunks[0].stored_sha256 = Sha256::digest(&body).into();
    let footer_bytes = encode_v3_planned_grouped_footer(&footer, V3GroupedLimits::HARD).unwrap();
    let mut rebuilt = Vec::new();
    rebuilt.extend_from_slice(&artifact.bytes[..header_len]);
    rebuilt.extend_from_slice(&body);
    rebuilt.extend_from_slice(&footer_bytes);
    rebuilt.extend_from_slice(&(footer_bytes.len() as u32).to_le_bytes());
    rebuilt.extend_from_slice(b"sealed:)");
    rebuilt
}

fn footer_bytes(file: &[u8]) -> &[u8] {
    &file[footer_start(file)..file.len() - 12]
}
