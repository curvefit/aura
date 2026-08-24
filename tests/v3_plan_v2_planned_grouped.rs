use std::io::Cursor;

use aura_codec::{
    canonical_v3_event_batch_sha256, compile_v3_planned_grouped,
    compile_v3_planned_grouped_attempt2, compile_v3_planned_grouped_attempt2_candidate,
    compile_v3_planned_grouped_attempt3, compile_v3_planned_grouped_attempt4,
    compile_v3_planned_grouped_attempt4_candidate, compile_v3_planned_grouped_attempt5,
    compile_v3_planned_grouped_attempt5_candidate, decode_any_compiled_footer,
    decode_v3_planned_grouped, decode_v3_planned_grouped_footer, encode_v3_planned_grouped_footer,
    AnyCompiledFooter, AuraHeader, AuraPlanV2, AuraV3Column, AuraV3ColumnValues as Values,
    AuraV3EventBatch, AuraV3VariableColumn, FieldRole, FieldScope, FieldType, PlanV2PhysicalCodec,
    PlanV2Selection, RelationshipPermissions, SchemaBuilder, V3EventLimits, V3GroupedAura0Reader,
    V3GroupedAura0Writer, V3GroupedLimits, V3GroupedWriterOptions,
    AURA_PLAN_V2_CROSS_DOMAIN_REGISTRY_VERSION, AURA_PLAN_V2_DOMAIN0_FROM_DOMAIN1_OP,
    AURA_PLAN_V2_DOMAIN1_FROM_DOMAIN0_OP, AURA_PLAN_V2_INTEGER_CODEC_REGISTRY_VERSION,
    AURA_PLAN_V2_PREVIOUS_WITHIN_DOMAIN_OP, AURA_PLAN_V2_REGISTRY_VERSION, AURA_PLAN_V2_VERSION,
    AURA_PLAN_V2_WITHIN_DOMAIN_REGISTRY_VERSION, MAX_AURA_PLAN_V2_BYTES,
    V3_PLANNED_GROUPED_BODY_ENCODING, V3_PLANNED_GROUPED_COMPACT_BLOCK_VERSION,
    V3_PLANNED_GROUPED_COMPACT_BODY_LAYOUT_VERSION, V3_PLANNED_GROUPED_FOOTER_LAYOUT_VERSION,
};
use sha2::{Digest, Sha256};

const PLAN_HASH_DOMAIN: &[u8] = b"aura-plan-v2-registry-v1\0";
const PLAN_HASH_DOMAIN_V2: &[u8] = b"aura-plan-v2-registry-v2\0";
const PLAN_HASH_DOMAIN_V3: &[u8] = b"aura-plan-v2-registry-v3\0";
const PLAN_HASH_DOMAIN_V4: &[u8] = b"aura-plan-v2-registry-v4\0";
const PLAN_HASH_DOMAIN_V5: &[u8] = b"aura-plan-v2-registry-v5\0";
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
        vec![batch.clone()]
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

#[test]
fn attempt3_selects_absolute_varints_by_complete_file_cost() {
    let schema = small_schema();
    let batches = vec![
        small_batch(schema.schema_id, &[1], &[-1], &[0, 0], &[], &[]),
        small_batch(
            schema.schema_id,
            &[2, 3],
            &[0, 1],
            &[0, 2, 4],
            &[0, 1, 0, 1],
            &[0, 1, -1, 2],
        ),
    ];
    let artifact =
        compile_v3_planned_grouped_attempt3(&schema, &batches, Default::default()).unwrap();
    assert_eq!(artifact.summary.file_bytes, artifact.bytes.len() as u64);
    assert_eq!(
        artifact.summary.file_bytes,
        artifact.summary.accounted_file_bytes
    );
    assert_eq!(artifact.inspection.candidates.len(), 4);
    assert_eq!(
        artifact
            .inspection
            .candidates
            .iter()
            .filter(|row| row.selected)
            .count(),
        1
    );
    assert_eq!(
        artifact.inspection.candidates[3].candidate_id,
        "registry3-compact-integer-codecs"
    );
    assert!(artifact.inspection.candidates[3].selected);
    assert_eq!(
        artifact.inspection.plan.registry_version,
        AURA_PLAN_V2_INTEGER_CODEC_REGISTRY_VERSION
    );
    assert!(!artifact.inspection.plan.relationships_attempted);
    assert!(artifact.inspection.schema_relationships_authorized_only);
    assert!(artifact.inspection.codecs.iter().any(|row| {
        row.selected == PlanV2PhysicalCodec::SignedZigZagUleb128
            && row
                .varint_bytes
                .is_some_and(|bytes| bytes < row.fixed_bytes)
    }));
    assert_eq!(
        decode_v3_planned_grouped(&artifact.bytes, Default::default())
            .unwrap()
            .batches,
        batches
    );
    let empty = compile_v3_planned_grouped_attempt3(&schema, &[], Default::default()).unwrap();
    assert!(empty.inspection.candidates[0].selected);
    assert_eq!(
        empty.inspection.plan.registry_version,
        AURA_PLAN_V2_REGISTRY_VERSION
    );
    assert!(empty.inspection.codecs.is_empty());
    eprintln!(
        "development attempt3 small integer complete bytes: r1={} r2={} r3_fixed={} r3_mixed={}",
        artifact.inspection.candidates[0].complete_bytes.unwrap(),
        artifact.inspection.candidates[1].complete_bytes.unwrap(),
        artifact.inspection.candidates[2].complete_bytes.unwrap(),
        artifact.inspection.candidates[3].complete_bytes.unwrap()
    );
}

#[test]
fn attempt3_extremes_nulls_and_fixed_only_types_round_trip() {
    let schema = all_types_schema();
    let batch = all_types_batch(schema.schema_id);
    let artifact = compile_v3_planned_grouped_attempt3(
        &schema,
        std::slice::from_ref(&batch),
        Default::default(),
    )
    .unwrap();
    assert_eq!(
        decode_v3_planned_grouped(&artifact.bytes, Default::default())
            .unwrap()
            .batches,
        vec![batch.clone()]
    );
    for field_type in [
        FieldType::I128,
        FieldType::Opaque16,
        FieldType::Utf8,
        FieldType::DecimalText,
    ] {
        let row = artifact
            .inspection
            .codecs
            .iter()
            .find(|row| row.field_type == field_type)
            .unwrap();
        assert!(!row.eligible);
        assert_eq!(row.selected, PlanV2PhysicalCodec::FixedWidth);
        assert!(row.varint_bytes.is_none());
    }
    assert_eq!(
        artifact
            .inspection
            .codecs
            .iter()
            .find(|row| row.field_type == FieldType::I64)
            .unwrap()
            .selected,
        PlanV2PhysicalCodec::SignedZigZagUleb128
    );
    assert_eq!(
        artifact
            .inspection
            .codecs
            .iter()
            .find(|row| row.field_type == FieldType::U64)
            .unwrap()
            .selected,
        PlanV2PhysicalCodec::UnsignedUleb128
    );
    let extreme_schema = SchemaBuilder::new("integer_extremes")
        .field("ts", FieldType::TimestampMs, FieldRole::Timestamp)
        .field("signed", FieldType::I64, FieldRole::Sequence)
        .repeated_field("side", FieldType::U8, FieldRole::Side)
        .repeated_field("unsigned", FieldType::U64, FieldRole::Value)
        .dual_domain_repeated_group(2, vec![2, 3], 2, permissions())
        .finish()
        .unwrap();
    let groups = extreme_schema.groups.clone();
    let extreme_schema = extreme_schema.with_v3_groups(groups).unwrap();
    let extreme_batch = AuraV3EventBatch {
        schema_id: extreme_schema.schema_id,
        event_count: 2,
        child_offsets: vec![0, 1, 3],
        event_columns: vec![
            AuraV3Column {
                slot: 0,
                validity: None,
                values: Values::TimestampMs(vec![i64::MIN, i64::MAX]),
            },
            AuraV3Column {
                slot: 1,
                validity: None,
                values: Values::I64(vec![i64::MIN, i64::MAX]),
            },
        ],
        repeated_columns: vec![
            AuraV3Column {
                slot: 2,
                validity: None,
                values: Values::U8(vec![0, 1, 0]),
            },
            AuraV3Column {
                slot: 3,
                validity: None,
                values: Values::U64(vec![u64::MAX, u64::MAX, u64::MAX]),
            },
        ],
    };
    let extremes = compile_v3_planned_grouped_attempt3(
        &extreme_schema,
        std::slice::from_ref(&extreme_batch),
        Default::default(),
    )
    .unwrap();
    assert_eq!(
        decode_v3_planned_grouped(&extremes.bytes, Default::default())
            .unwrap()
            .batches,
        vec![extreme_batch]
    );
    assert!(extremes
        .inspection
        .codecs
        .iter()
        .filter(|row| matches!(row.logical_slot, 0 | 1 | 3))
        .all(|row| row.selected == PlanV2PhysicalCodec::FixedWidth));
    eprintln!(
        "development attempt3 all-types complete bytes: r1={} r2={} r3_fixed={} r3_mixed={}",
        artifact.inspection.candidates[0].complete_bytes.unwrap(),
        artifact.inspection.candidates[1].complete_bytes.unwrap(),
        artifact.inspection.candidates[2].complete_bytes.unwrap(),
        artifact.inspection.candidates[3].complete_bytes.unwrap()
    );
}

#[test]
fn attempt3_codec_plan_is_canonical_and_rechunking_stable() {
    let schema = small_schema();
    let whole = small_batch(
        schema.schema_id,
        &[1, 2, 3],
        &[-1, 0, 1],
        &[0, 0, 2, 4],
        &[0, 1, 0, 1],
        &[0, 1, -1, 2],
    );
    let split = vec![
        small_batch(schema.schema_id, &[1], &[-1], &[0, 0], &[], &[]),
        small_batch(
            schema.schema_id,
            &[2, 3],
            &[0, 1],
            &[0, 2, 4],
            &[0, 1, 0, 1],
            &[0, 1, -1, 2],
        ),
    ];
    let one = compile_v3_planned_grouped_attempt3(
        &schema,
        std::slice::from_ref(&whole),
        Default::default(),
    )
    .unwrap();
    let many = compile_v3_planned_grouped_attempt3(&schema, &split, Default::default()).unwrap();
    assert_eq!(one.summary.plan_sha256, many.summary.plan_sha256);
    assert_eq!(
        one.summary.global_logical_sha256,
        many.summary.global_logical_sha256
    );
    assert_eq!(one.inspection.codecs, many.inspection.codecs);

    let footer =
        decode_v3_planned_grouped_footer(footer_bytes(&one.bytes), V3GroupedLimits::HARD).unwrap();
    let encoded = footer.plan.encode(&schema).unwrap();
    assert_eq!(AuraPlanV2::decode(&schema, &encoded).unwrap(), footer.plan);
    let codec_count = footer.plan.physical_stream_codecs.len();
    let codec_start = encoded.len() - 32 - footer.plan.decode_order.len() * 2 - codec_count;
    let codec_count_offset = codec_start - 2;
    assert_eq!(
        u16::from_le_bytes(encoded[codec_count_offset..codec_start].try_into().unwrap()) as usize,
        codec_count
    );
    assert_eq!(
        codec_start + codec_count + footer.plan.decode_order.len() * 2,
        encoded.len() - 32
    );
    let mut unknown = encoded.clone();
    unknown[codec_start] = 99;
    resign_plan_with_domain(&mut unknown, PLAN_HASH_DOMAIN_V3);
    assert!(AuraPlanV2::decode(&schema, &unknown).is_err());
    let mut bad_selector = encoded;
    let selector = footer.plan.source_order_selector_stream_id.unwrap() as usize;
    bad_selector[codec_start + selector] = PlanV2PhysicalCodec::UnsignedUleb128 as u8;
    resign_plan_with_domain(&mut bad_selector, PLAN_HASH_DOMAIN_V3);
    assert!(AuraPlanV2::decode(&schema, &bad_selector).is_err());
    let mut bad_count = footer.plan.encode(&schema).unwrap();
    bad_count[codec_count_offset..codec_start]
        .copy_from_slice(&(codec_count as u16 + 1).to_le_bytes());
    resign_plan_with_domain(&mut bad_count, PLAN_HASH_DOMAIN_V3);
    assert!(AuraPlanV2::decode(&schema, &bad_count).is_err());
}

#[test]
fn attempt3_fixed_width_wins_exact_codec_ties() {
    let schema = SchemaBuilder::new("integer_codec_tie")
        .field("ts", FieldType::TimestampMs, FieldRole::Timestamp)
        .field("tiny", FieldType::U8, FieldRole::Value)
        .repeated_field("side", FieldType::U8, FieldRole::Side)
        .repeated_field("value", FieldType::I64, FieldRole::Value)
        .dual_domain_repeated_group(1, vec![2, 3], 2, permissions())
        .finish()
        .unwrap();
    let groups = schema.groups.clone();
    let schema = schema.with_v3_groups(groups).unwrap();
    let batch = AuraV3EventBatch {
        schema_id: schema.schema_id,
        event_count: 2,
        child_offsets: vec![0, 0, 1],
        event_columns: vec![
            AuraV3Column {
                slot: 0,
                validity: None,
                values: Values::TimestampMs(vec![0, 1]),
            },
            AuraV3Column {
                slot: 1,
                validity: None,
                values: Values::U8(vec![1, 127]),
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
    let artifact =
        compile_v3_planned_grouped_attempt3(&schema, &[batch], Default::default()).unwrap();
    let tiny = artifact
        .inspection
        .codecs
        .iter()
        .find(|row| row.logical_slot == 1)
        .unwrap();
    assert_eq!(tiny.fixed_bytes, tiny.varint_bytes.unwrap());
    assert_eq!(tiny.selected, PlanV2PhysicalCodec::FixedWidth);
}

#[test]
fn attempt3_candidate_limit_failure_falls_back_to_smaller_valid_artifact() {
    let schema = small_schema();
    let batch = small_batch(
        schema.schema_id,
        &[1, 2, 3],
        &[-1, 0, 1],
        &[0, 0, 2, 4],
        &[0, 1, 0, 1],
        &[0, 1, -1, 2],
    );
    let compact = compile_v3_planned_grouped_attempt2_candidate(
        &schema,
        std::slice::from_ref(&batch),
        V3GroupedLimits::default(),
        PlanV2Selection::Direct,
    )
    .unwrap();
    let registry1 = compile_v3_planned_grouped(
        &schema,
        std::slice::from_ref(&batch),
        V3GroupedLimits::default(),
    )
    .unwrap();
    assert!(registry1.summary.body_bytes > compact.summary.body_bytes);
    let limits = V3GroupedLimits {
        max_body_bytes: compact.summary.body_bytes,
        ..Default::default()
    };
    let selected = compile_v3_planned_grouped_attempt3(&schema, &[batch], limits).unwrap();
    assert!(!selected.inspection.candidates[0].applicable);
    assert!(selected.inspection.candidates[0].complete_bytes.is_none());
    assert!(selected.inspection.candidates[0]
        .rejection
        .as_deref()
        .is_some_and(|reason| reason.contains("body length")));
    assert!(selected.inspection.candidates[1..]
        .iter()
        .any(|candidate| candidate.selected));
    assert_eq!(
        selected.inspection.codecs.is_empty(),
        selected.inspection.plan.registry_version != AURA_PLAN_V2_INTEGER_CODEC_REGISTRY_VERSION
    );
    assert_eq!(selected.summary.file_bytes, selected.bytes.len() as u64);
}

#[test]
fn attempt3_candidate_block_limit_uses_actual_physical_block_size() {
    let schema = small_schema();
    let batch = small_batch(
        schema.schema_id,
        &[1, 2, 3],
        &[-1, 0, 1],
        &[0, 0, 2, 4],
        &[0, 1, 0, 1],
        &[0, 1, -1, 2],
    );
    let registry1 =
        compile_v3_planned_grouped(&schema, std::slice::from_ref(&batch), Default::default())
            .unwrap();
    let compact = compile_v3_planned_grouped_attempt2_candidate(
        &schema,
        std::slice::from_ref(&batch),
        Default::default(),
        PlanV2Selection::Direct,
    )
    .unwrap();
    assert!(registry1.summary.body_bytes > compact.summary.body_bytes);
    let defaults = V3GroupedLimits::default();
    let limits = V3GroupedLimits {
        event_limits: V3EventLimits {
            max_block_bytes: compact.summary.body_bytes as usize,
            ..defaults.event_limits
        },
        ..defaults
    };
    let selected = compile_v3_planned_grouped_attempt3(&schema, &[batch], limits).unwrap();
    assert!(!selected.inspection.candidates[0].applicable);
    assert!(selected.inspection.candidates[0]
        .rejection
        .as_deref()
        .is_some_and(|reason| reason.contains("event block length")));
    assert!(selected.inspection.candidates[1..]
        .iter()
        .any(|candidate| candidate.selected));
    assert!(selected.summary.body_bytes <= compact.summary.body_bytes);
}

#[test]
fn attempt3_footer_limit_keeps_registry1_when_larger_plans_are_inapplicable() {
    let schema = all_types_schema();
    let batch = all_types_batch(schema.schema_id);
    let registry1 =
        compile_v3_planned_grouped(&schema, std::slice::from_ref(&batch), Default::default())
            .unwrap();
    let compact = compile_v3_planned_grouped_attempt2_candidate(
        &schema,
        std::slice::from_ref(&batch),
        Default::default(),
        PlanV2Selection::Direct,
    )
    .unwrap();
    assert!(compact.summary.footer_bytes > registry1.summary.footer_bytes);
    let limits = V3GroupedLimits {
        max_footer_bytes: registry1.summary.footer_bytes as usize,
        ..Default::default()
    };
    let selected = compile_v3_planned_grouped_attempt3(&schema, &[batch], limits).unwrap();
    assert!(selected.inspection.candidates[0].selected);
    assert_eq!(selected.summary.file_bytes, registry1.summary.file_bytes);
    for candidate in &selected.inspection.candidates[1..] {
        assert!(!candidate.applicable);
        assert!(candidate.complete_bytes.is_none());
        assert!(candidate
            .rejection
            .as_deref()
            .is_some_and(|reason| reason.contains("footer length")));
    }
    assert!(selected.inspection.codecs.is_empty());
}

#[test]
fn attempt3_excludes_structurally_dominated_absolute_split_candidate() {
    let small = small_schema();
    let small_batch = small_batch(
        small.schema_id,
        &[1, 2],
        &[-1, 0],
        &[0, 3, 4],
        &[0, 0, 0, 1],
        &[0, 1, 2, 3],
    );
    let all = all_types_schema();
    let all_batch = all_types_batch(all.schema_id);
    for (schema, batches) in [
        (&small, std::slice::from_ref(&small_batch)),
        (&all, std::slice::from_ref(&all_batch)),
    ] {
        let direct = compile_v3_planned_grouped_attempt2_candidate(
            schema,
            batches,
            Default::default(),
            PlanV2Selection::Direct,
        )
        .unwrap();
        let split = compile_v3_planned_grouped_attempt2_candidate(
            schema,
            batches,
            Default::default(),
            PlanV2Selection::SplitDomainDirect,
        )
        .unwrap();
        // Absolute splitting conserves selector/value bytes, cannot reduce
        // summed validity, adds variable-lane offsets, and stamps more plan
        // metadata. It is therefore not an attempt-3 codec candidate.
        assert!(split.summary.file_bytes >= direct.summary.file_bytes);
        let attempt3 =
            compile_v3_planned_grouped_attempt3(schema, batches, Default::default()).unwrap();
        assert!(attempt3
            .inspection
            .candidates
            .iter()
            .all(|candidate| candidate.selection == PlanV2Selection::Direct));
        assert!(attempt3
            .inspection
            .candidates
            .iter()
            .all(|candidate| !candidate.candidate_id.contains("split")));
    }
}

#[test]
fn attempt3_rejects_rehashed_noncanonical_and_overflow_varints() {
    let schema = small_schema();
    let batch = small_batch(schema.schema_id, &[0], &[0], &[0, 1], &[0], &[0]);
    let artifact = compile_v3_planned_grouped_attempt3(
        &schema,
        std::slice::from_ref(&batch),
        Default::default(),
    )
    .unwrap();
    assert_eq!(
        artifact.inspection.plan.registry_version,
        AURA_PLAN_V2_INTEGER_CODEC_REGISTRY_VERSION
    );
    let header_len = AuraHeader::encoded_len(&artifact.bytes).unwrap();
    let body = &artifact.bytes[header_len..footer_start(&artifact.bytes)];
    let first_event_value = 72 + (batch.event_count as usize + 1) * 4;
    assert_eq!(body[first_event_value], 0);
    let overlong = rebuild_with_replaced_body_range(
        &artifact,
        first_event_value..first_event_value + 1,
        &[0x80, 0x00],
    );
    assert!(decode_v3_planned_grouped(&overlong, V3GroupedLimits::HARD).is_err());
    let overflow = rebuild_with_replaced_body_range(
        &artifact,
        first_event_value..first_event_value + 1,
        &[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x02],
    );
    assert!(decode_v3_planned_grouped(&overflow, V3GroupedLimits::HARD).is_err());
    let truncated = rebuild_with_replaced_body_range(
        &artifact,
        first_event_value..first_event_value + 1,
        &[0x80],
    );
    assert!(decode_v3_planned_grouped(&truncated, V3GroupedLimits::HARD).is_err());
}

#[test]
fn attempt3_every_prefix_and_single_byte_mutation_fails_closed() {
    let schema = small_schema();
    let batch = small_batch(
        schema.schema_id,
        &[1, 2],
        &[0, 1],
        &[0, 1, 2],
        &[0, 1],
        &[1, 2],
    );
    let artifact =
        compile_v3_planned_grouped_attempt3(&schema, &[batch], Default::default()).unwrap();
    assert_eq!(
        artifact.inspection.plan.registry_version,
        AURA_PLAN_V2_INTEGER_CODEC_REGISTRY_VERSION
    );
    for length in 0..artifact.bytes.len() {
        let result = std::panic::catch_unwind(|| {
            decode_v3_planned_grouped(&artifact.bytes[..length], V3GroupedLimits::HARD)
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
            decode_v3_planned_grouped(&corrupted, V3GroupedLimits::HARD)
        });
        assert!(result.is_ok_and(|decoded| decoded.is_err()), "byte {index}");
    }
}

#[test]
fn registry1_registry2_plan_and_container_hashes_are_golden() {
    let schema = all_types_schema();
    let batch = all_types_batch(schema.schema_id);
    let registry1 =
        compile_v3_planned_grouped(&schema, std::slice::from_ref(&batch), Default::default())
            .unwrap();
    let registry2 = compile_v3_planned_grouped_attempt2_candidate(
        &schema,
        std::slice::from_ref(&batch),
        Default::default(),
        PlanV2Selection::Direct,
    )
    .unwrap();
    let footer1 =
        decode_v3_planned_grouped_footer(footer_bytes(&registry1.bytes), V3GroupedLimits::HARD)
            .unwrap();
    let footer2 =
        decode_v3_planned_grouped_footer(footer_bytes(&registry2.bytes), V3GroupedLimits::HARD)
            .unwrap();
    assert_eq!(
        hex_bytes(&footer1.plan_sha256),
        "ffc6f6b08a8a020d209d9731d6cdf4bac6d9503fccbe3d0f3f41f0fa8b67fc43"
    );
    assert_eq!(
        hex_bytes(&<[u8; 32]>::from(Sha256::digest(&registry1.bytes))),
        "1d245dfd95c7fedc710c34a9e2cdd6a23a093f65e1061dcf7246fbb1f2f353f0"
    );
    assert_eq!(
        hex_bytes(&footer2.plan_sha256),
        "d02ba9e0a52fb5ed937ad5666d68fc0fe6c098a1110cb8bba1d70c5eefef2646"
    );
    assert_eq!(
        hex_bytes(&<[u8; 32]>::from(Sha256::digest(&registry2.bytes))),
        "7abdc2934d74556a4469b41718ae436a693a85b19547bd8978379f0d3fa04b7d"
    );
    let mut registry2_code2 = footer2.plan.encode(&schema).unwrap();
    registry2_code2[57] = PlanV2Selection::PreviousWithinDomainMixed as u8;
    resign_plan_with_domain(&mut registry2_code2, PLAN_HASH_DOMAIN_V2);
    assert!(AuraPlanV2::decode(&schema, &registry2_code2).is_err());
}

#[test]
fn attempt4_previous_within_domain_roundtrips_and_wins_complete_cost() {
    let schema = small_schema();
    let make_batch = |start_event: usize, events: usize| {
        let mut timestamps = Vec::new();
        let mut sequences = Vec::new();
        let mut offsets = vec![0u32];
        let mut sides = Vec::new();
        let mut values = Vec::new();
        for event in 0..events {
            timestamps.push((start_event + event) as i64 + 1);
            sequences.push((start_event + event) as i64);
            for child in 0..100usize {
                let side = (child % 2) as u8;
                sides.push(side);
                values.push(if side == 0 { 1_000_000 } else { -1_000_000 } + (child / 2) as i64);
            }
            offsets.push(sides.len() as u32);
        }
        small_batch(
            schema.schema_id,
            &timestamps,
            &sequences,
            &offsets,
            &sides,
            &values,
        )
    };
    let whole = make_batch(0, 2);
    let split = vec![make_batch(0, 1), make_batch(1, 1)];
    let artifact = compile_v3_planned_grouped_attempt4(
        &schema,
        std::slice::from_ref(&whole),
        Default::default(),
    )
    .unwrap();
    assert_eq!(artifact.inspection.candidates.len(), 6);
    assert!(artifact.inspection.candidates[5].selected);
    assert_eq!(
        artifact.inspection.plan.registry_version,
        AURA_PLAN_V2_WITHIN_DOMAIN_REGISTRY_VERSION
    );
    assert!(artifact.inspection.plan.relationships_attempted);
    assert!(artifact.inspection.codecs.is_empty());
    let relationship = artifact
        .inspection
        .within_domain
        .iter()
        .find(|row| row.logical_slot == 3)
        .unwrap();
    assert!(relationship.authorized && relationship.eligible && relationship.applicable);
    assert!(relationship.selected);
    assert!(relationship.candidate_selected);
    assert_eq!(
        relationship.selected_op,
        AURA_PLAN_V2_PREVIOUS_WITHIN_DOMAIN_OP
    );
    assert_eq!(relationship.candidate_plan_bytes, 2);
    assert_eq!(relationship.selected_plan_bytes, 2);
    assert_eq!(
        decode_v3_planned_grouped(&artifact.bytes, Default::default())
            .unwrap()
            .batches,
        vec![whole]
    );
    let rechunked =
        compile_v3_planned_grouped_attempt4(&schema, &split, Default::default()).unwrap();
    assert_eq!(artifact.summary.plan_sha256, rechunked.summary.plan_sha256);
    assert_eq!(
        artifact.summary.global_logical_sha256,
        rechunked.summary.global_logical_sha256
    );
    eprintln!(
        "development attempt4 generic complete bytes: r1={} r2={} r3f={} r3m={} r4a={} r4w={}",
        artifact.inspection.candidates[0].complete_bytes.unwrap(),
        artifact.inspection.candidates[1].complete_bytes.unwrap(),
        artifact.inspection.candidates[2].complete_bytes.unwrap(),
        artifact.inspection.candidates[3].complete_bytes.unwrap(),
        artifact.inspection.candidates[4].complete_bytes.unwrap(),
        artifact.inspection.candidates[5].complete_bytes.unwrap(),
    );
}

#[test]
fn attempt4_okx_like_shape_nullable_and_overflow_streams_fall_back_exactly() {
    let mut schema = SchemaBuilder::new("generic_count_extension")
        .field("ts", FieldType::TimestampMs, FieldRole::Timestamp)
        .repeated_field("side", FieldType::U8, FieldRole::Side)
        .repeated_field("price", FieldType::I64, FieldRole::Price)
        .repeated_field("quantity", FieldType::I64, FieldRole::Quantity)
        .repeated_field("order_count", FieldType::U32, FieldRole::Count)
        .dual_domain_repeated_group(6, vec![1, 2, 3, 4], 1, permissions())
        .finish()
        .unwrap();
    schema.fields[3].nullable = true;
    schema.fields[4].nullable = true;
    let groups = schema.groups.clone();
    let schema = schema.with_v3_groups(groups).unwrap();
    let children = 80usize;
    let sides = (0..children)
        .map(|index| (index % 2) as u8)
        .collect::<Vec<_>>();
    let batch = AuraV3EventBatch {
        schema_id: schema.schema_id,
        event_count: 2,
        child_offsets: vec![0, 0, children as u32],
        event_columns: vec![AuraV3Column {
            slot: 0,
            validity: None,
            values: Values::TimestampMs(vec![1, 2]),
        }],
        repeated_columns: vec![
            AuraV3Column {
                slot: 1,
                validity: None,
                values: Values::U8(sides.clone()),
            },
            AuraV3Column {
                slot: 2,
                validity: None,
                values: Values::I64(
                    (0..children)
                        .map(|index| 5_000_000 + (index / 2) as i64)
                        .collect(),
                ),
            },
            AuraV3Column {
                slot: 3,
                validity: Some(vec![0xff; children.div_ceil(8)]),
                values: Values::I64(vec![0; children]),
            },
            AuraV3Column {
                slot: 4,
                validity: Some(vec![0xff; children.div_ceil(8)]),
                values: Values::U32(vec![1; children]),
            },
        ],
    };
    let artifact = compile_v3_planned_grouped_attempt4(
        &schema,
        std::slice::from_ref(&batch),
        Default::default(),
    )
    .unwrap();
    assert_eq!(
        decode_v3_planned_grouped(&artifact.bytes, Default::default())
            .unwrap()
            .batches,
        vec![batch]
    );
    eprintln!(
        "development attempt4 generic-count complete bytes: r1={} r2={} r3f={} r3m={} r4a={} r4w={}",
        artifact.inspection.candidates[0].complete_bytes.unwrap(),
        artifact.inspection.candidates[1].complete_bytes.unwrap(),
        artifact.inspection.candidates[2].complete_bytes.unwrap(),
        artifact.inspection.candidates[3].complete_bytes.unwrap(),
        artifact.inspection.candidates[4].complete_bytes.unwrap(),
        artifact.inspection.candidates[5].complete_bytes.unwrap(),
    );
    assert!(
        artifact
            .inspection
            .within_domain
            .iter()
            .find(|row| row.logical_slot == 2)
            .unwrap()
            .selected
    );
    for slot in [3u16, 4] {
        let row = artifact
            .inspection
            .within_domain
            .iter()
            .find(|row| row.logical_slot == slot)
            .unwrap();
        assert!(!row.eligible);
        assert!(!row.selected);
        assert_eq!(row.selected_plan_bytes, 0);
    }

    let overflow_schema = small_schema();
    let overflow = small_batch(
        overflow_schema.schema_id,
        &[1],
        &[1],
        &[0, 2],
        &[0, 0],
        &[i64::MIN, i64::MAX],
    );
    let overflow_artifact =
        compile_v3_planned_grouped_attempt4(&overflow_schema, &[overflow], Default::default())
            .unwrap();
    let row = overflow_artifact
        .inspection
        .within_domain
        .iter()
        .find(|row| row.logical_slot == 3)
        .unwrap();
    assert!(!row.applicable);
    assert!(row
        .rejection
        .as_deref()
        .is_some_and(|reason| reason.contains("overflow")));
    assert!(!row.selected);
    assert!(!overflow_artifact.inspection.candidates[5].selected);
    assert!(overflow_artifact
        .inspection
        .within_domain
        .iter()
        .all(|row| !row.selected && row.selected_plan_bytes == 0));
    assert_eq!(row.selected_plan_bytes, 0);
}

#[test]
fn attempt4_forbidden_permission_and_corruption_fail_closed() {
    let schema = SchemaBuilder::new("within_forbidden")
        .field("ts", FieldType::TimestampMs, FieldRole::Timestamp)
        .repeated_field("side", FieldType::U8, FieldRole::Side)
        .repeated_field("value", FieldType::I64, FieldRole::Value)
        .dual_domain_repeated_group(
            1,
            vec![1, 2],
            1,
            RelationshipPermissions::none().with_split(),
        )
        .finish()
        .unwrap();
    let groups = schema.groups.clone();
    let schema = schema.with_v3_groups(groups).unwrap();
    let batch = AuraV3EventBatch {
        schema_id: schema.schema_id,
        event_count: 1,
        child_offsets: vec![0, 4],
        event_columns: vec![AuraV3Column {
            slot: 0,
            validity: None,
            values: Values::TimestampMs(vec![1]),
        }],
        repeated_columns: vec![
            AuraV3Column {
                slot: 1,
                validity: None,
                values: Values::U8(vec![0, 1, 0, 1]),
            },
            AuraV3Column {
                slot: 2,
                validity: None,
                values: Values::I64(vec![1_000, -1_000, 1_001, -999]),
            },
        ],
    };
    let forbidden = compile_v3_planned_grouped_attempt4(
        &schema,
        std::slice::from_ref(&batch),
        Default::default(),
    )
    .unwrap();
    assert!(forbidden
        .inspection
        .within_domain
        .iter()
        .all(|row| !row.authorized && !row.selected));
    assert!(!forbidden.inspection.candidates[5].authorized);
    assert!(!forbidden.inspection.candidates[5].applicable);
    assert!(forbidden.inspection.candidates[5].complete_bytes.is_none());
    assert_eq!(
        forbidden.inspection.candidates[5].rejection.as_deref(),
        Some("schema does not authorize within-domain")
    );

    let allowed = small_schema();
    let children = 64usize;
    let allowed_batch = small_batch(
        allowed.schema_id,
        &[1],
        &[1],
        &[0, children as u32],
        &(0..children)
            .map(|index| (index % 2) as u8)
            .collect::<Vec<_>>(),
        &(0..children)
            .map(|index| 1_000_000 + (index / 2) as i64)
            .collect::<Vec<_>>(),
    );
    let artifact =
        compile_v3_planned_grouped_attempt4(&allowed, &[allowed_batch], Default::default())
            .unwrap();
    assert!(artifact.inspection.candidates[5].selected);
    for length in 0..artifact.bytes.len() {
        assert!(
            decode_v3_planned_grouped(&artifact.bytes[..length], V3GroupedLimits::HARD).is_err()
        );
    }
    for index in 0..artifact.bytes.len() {
        let mut corrupted = artifact.bytes.clone();
        corrupted[index] ^= 1;
        assert!(decode_v3_planned_grouped(&corrupted, V3GroupedLimits::HARD).is_err());
    }
    let footer =
        decode_v3_planned_grouped_footer(footer_bytes(&artifact.bytes), V3GroupedLimits::HARD)
            .unwrap();
    let mut bad_plan = footer.plan.clone();
    let transformed = bad_plan
        .streams
        .iter_mut()
        .find(|stream| stream.op == AURA_PLAN_V2_PREVIOUS_WITHIN_DOMAIN_OP)
        .unwrap();
    transformed.dependencies.clear();
    assert!(bad_plan.encode(&allowed).is_err());
    let mut wrong_selection = footer.plan.clone();
    wrong_selection.selection = PlanV2Selection::SplitDomainDirect;
    assert!(wrong_selection.encode(&allowed).is_err());
    let mut wrong_op = footer.plan;
    let transformed = wrong_op
        .streams
        .iter_mut()
        .find(|stream| stream.op == AURA_PLAN_V2_PREVIOUS_WITHIN_DOMAIN_OP)
        .unwrap();
    transformed.op = aura_codec::AURA_PLAN_V2_SPLIT_DOMAIN_DIRECT_OP;
    assert!(wrong_op.encode(&allowed).is_err());
}

#[test]
fn attempt4_bounded_property_inverse_is_exact() {
    let schema = small_schema();
    for seed in 0..24u64 {
        let events = (seed as usize % 4) + 1;
        let mut offsets = vec![0u32];
        let mut sides = Vec::new();
        let mut values = Vec::new();
        let mut state = seed.wrapping_add(1);
        for event in 0..events {
            let children = (seed as usize + event * 3) % 17;
            let mut previous = [10_000i64 + event as i64, -10_000i64 - event as i64];
            for _ in 0..children {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                let side = ((state >> 63) & 1) as u8;
                let delta = ((state >> 32) % 7) as i64 - 3;
                previous[side as usize] += delta;
                sides.push(side);
                values.push(previous[side as usize]);
            }
            offsets.push(sides.len() as u32);
        }
        let timestamps = (0..events).map(|value| value as i64).collect::<Vec<_>>();
        let sequences = (0..events).map(|value| -(value as i64)).collect::<Vec<_>>();
        let batch = small_batch(
            schema.schema_id,
            &timestamps,
            &sequences,
            &offsets,
            &sides,
            &values,
        );
        let artifact = compile_v3_planned_grouped_attempt4(
            &schema,
            std::slice::from_ref(&batch),
            Default::default(),
        )
        .unwrap();
        assert_eq!(
            decode_v3_planned_grouped(&artifact.bytes, Default::default())
                .unwrap()
                .batches,
            vec![batch],
            "seed {seed}"
        );
    }
}

#[test]
fn attempt4_op2_physical_i64_roundtrips_every_reachable_signed_type() {
    let schema = SchemaBuilder::new("within_signed_types")
        .field("ts", FieldType::TimestampMs, FieldRole::Timestamp)
        .repeated_field("side", FieldType::U8, FieldRole::Side)
        .repeated_field("i8", FieldType::I8, FieldRole::Value)
        .repeated_field("i16", FieldType::I16, FieldRole::Value)
        .repeated_field("i32", FieldType::I32, FieldRole::Value)
        .repeated_field_related_to("i64", FieldType::I64, FieldRole::Value, "i32")
        .repeated_field("timestamp_ns", FieldType::TimestampNs, FieldRole::Value)
        .dual_domain_repeated_group(9, vec![1, 2, 3, 4, 5, 6], 1, permissions())
        .finish()
        .unwrap();
    let groups = schema.groups.clone();
    let schema = schema.with_v3_groups(groups).unwrap();
    assert!(!matches!(
        schema.fields[5].relation,
        aura_codec::FieldRelation::None
    ));
    let batch = AuraV3EventBatch {
        schema_id: schema.schema_id,
        event_count: 2,
        child_offsets: vec![0, 0, 4],
        event_columns: vec![AuraV3Column {
            slot: 0,
            validity: None,
            values: Values::TimestampMs(vec![1, 2]),
        }],
        repeated_columns: vec![
            AuraV3Column {
                slot: 1,
                validity: None,
                values: Values::U8(vec![0, 1, 0, 1]),
            },
            AuraV3Column {
                slot: 2,
                validity: None,
                values: Values::I8(vec![100, -100, 101, -99]),
            },
            AuraV3Column {
                slot: 3,
                validity: None,
                values: Values::I16(vec![1_000, -1_000, 1_001, -999]),
            },
            AuraV3Column {
                slot: 4,
                validity: None,
                values: Values::I32(vec![100_000, -100_000, 100_001, -99_999]),
            },
            AuraV3Column {
                slot: 5,
                validity: None,
                values: Values::I64(vec![1_000_000, -1_000_000, 1_000_001, -999_999]),
            },
            AuraV3Column {
                slot: 6,
                validity: None,
                values: Values::TimestampNs(vec![9_000_000, -9_000_000, 9_000_001, -8_999_999]),
            },
        ],
    };
    let slots = [2u16, 3, 4, 5, 6];
    let artifact = compile_v3_planned_grouped_attempt4_candidate(
        &schema,
        std::slice::from_ref(&batch),
        Default::default(),
        &slots,
        PlanV2PhysicalCodec::SignedZigZagUleb128,
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
    for slot in slots {
        assert_eq!(
            footer.plan.streams[slot as usize].op,
            AURA_PLAN_V2_PREVIOUS_WITHIN_DOMAIN_OP
        );
        let physical = footer.plan.streams[slot as usize].physical_stream_ids[0] as usize;
        assert_eq!(
            footer.plan.physical_stream_codecs[physical],
            PlanV2PhysicalCodec::SignedZigZagUleb128
        );
    }
    let fixed = compile_v3_planned_grouped_attempt4_candidate(
        &schema,
        std::slice::from_ref(&batch),
        Default::default(),
        &slots,
        PlanV2PhysicalCodec::FixedWidth,
    )
    .unwrap();
    assert_eq!(
        decode_v3_planned_grouped(&fixed.bytes, Default::default())
            .unwrap()
            .batches,
        vec![batch]
    );
    let first_repeated_physical = 72 + 3 * 4 + 2 * 8 + 4;
    let corrupted = rebuild_with_replaced_body_range(
        &fixed,
        first_repeated_physical..first_repeated_physical + 8,
        &i64::MAX.to_le_bytes(),
    );
    assert!(decode_v3_planned_grouped(&corrupted, V3GroupedLimits::HARD).is_err());

    assert!(SchemaBuilder::new("repeated_ms_rejected")
        .field("ts", FieldType::TimestampMs, FieldRole::Timestamp)
        .repeated_field("side", FieldType::U8, FieldRole::Side)
        .repeated_field("timestamp_ms", FieldType::TimestampMs, FieldRole::Timestamp)
        .dual_domain_repeated_group(1, vec![1, 2], 1, permissions())
        .finish()
        .is_err());
}

#[test]
fn attempt4_rehashed_registry4_plan_mutations_fail_closed() {
    let schema = small_schema();
    let batch = small_batch(
        schema.schema_id,
        &[1],
        &[1],
        &[0, 4],
        &[0, 1, 0, 1],
        &[1_000_000, -1_000_000, 1_000_001, -999_999],
    );
    let artifact = compile_v3_planned_grouped_attempt4_candidate(
        &schema,
        &[batch],
        Default::default(),
        &[3],
        PlanV2PhysicalCodec::SignedZigZagUleb128,
    )
    .unwrap();
    let footer =
        decode_v3_planned_grouped_footer(footer_bytes(&artifact.bytes), V3GroupedLimits::HARD)
            .unwrap();
    let encoded = footer.plan.encode(&schema).unwrap();
    let mut starts = Vec::new();
    let mut offset = 64usize;
    for _ in &footer.plan.streams {
        starts.push(offset);
        let dependencies =
            u16::from_le_bytes(encoded[offset + 6..offset + 8].try_into().unwrap()) as usize;
        let physical =
            u16::from_le_bytes(encoded[offset + 12..offset + 14].try_into().unwrap()) as usize;
        offset += 14 + dependencies * 2 + physical * 2;
    }
    let codec_count = footer.plan.physical_stream_codecs.len();
    let codec_start = encoded.len() - 32 - footer.plan.decode_order.len() * 2 - codec_count;
    assert_eq!(offset + 2, codec_start);
    let transformed_start = starts[3];
    let dependency_start = transformed_start + 14;
    let physical_start = dependency_start + 2;
    let transformed_physical = footer.plan.streams[3].physical_stream_ids[0] as usize;

    let mut mutations = Vec::new();
    let mut wrong_selection = encoded.clone();
    wrong_selection[57] = PlanV2Selection::SplitDomainDirect as u8;
    mutations.push(wrong_selection);
    let mut wrong_op = encoded.clone();
    wrong_op[transformed_start + 5] = aura_codec::AURA_PLAN_V2_DIRECT_OP;
    mutations.push(wrong_op);
    let mut wrong_dependency = encoded.clone();
    wrong_dependency[dependency_start..dependency_start + 2].copy_from_slice(&3u16.to_le_bytes());
    mutations.push(wrong_dependency);
    let mut wrong_codec = encoded.clone();
    wrong_codec[codec_start + transformed_physical] = PlanV2PhysicalCodec::UnsignedUleb128 as u8;
    mutations.push(wrong_codec);
    let mut wrong_type = encoded.clone();
    wrong_type[transformed_start + 3] = FieldType::U64 as u8;
    mutations.push(wrong_type);
    let mut wrong_physical = encoded;
    wrong_physical[physical_start..physical_start + 2].copy_from_slice(&u16::MAX.to_le_bytes());
    mutations.push(wrong_physical);

    for mut mutation in mutations {
        resign_plan_with_domain(&mut mutation, PLAN_HASH_DOMAIN_V4);
        assert!(AuraPlanV2::decode(&schema, &mutation).is_err());
    }
}

#[test]
fn attempt5_both_cross_orientations_preserve_transitions_empty_events_and_unequal_tails() {
    let schema = small_schema();
    let batch = small_batch(
        schema.schema_id,
        &[1, 2, 3],
        &[10, 20, 30],
        &[0, 0, 7, 10],
        &[1, 0, 1, 0, 0, 1, 0, 1, 1, 1],
        &[1_001, 1_000, 2_001, 2_000, 3_000, 3_001, -77, 9, 8, 7],
    );
    for op in [
        AURA_PLAN_V2_DOMAIN0_FROM_DOMAIN1_OP,
        AURA_PLAN_V2_DOMAIN1_FROM_DOMAIN0_OP,
    ] {
        for codec in [
            PlanV2PhysicalCodec::FixedWidth,
            PlanV2PhysicalCodec::SignedZigZagUleb128,
        ] {
            let artifact = compile_v3_planned_grouped_attempt5_candidate(
                &schema,
                std::slice::from_ref(&batch),
                Default::default(),
                &[(3, op)],
                codec,
            )
            .unwrap();
            let decoded = decode_v3_planned_grouped(&artifact.bytes, Default::default()).unwrap();
            assert_eq!(decoded.batches, vec![batch.clone()]);
            assert_eq!(
                decoded.footer.plan.registry_version,
                AURA_PLAN_V2_CROSS_DOMAIN_REGISTRY_VERSION
            );
            assert_eq!(decoded.footer.plan.streams[3].op, op);
        }
    }
}

#[test]
fn attempt5_cross_candidate_wins_only_on_complete_cost_and_is_rechunking_stable() {
    let schema = small_schema();
    let make_batch = |first_event: usize, events: usize| {
        let mut offsets = vec![0u32];
        let mut sides = Vec::new();
        let mut values = Vec::new();
        for event in first_event..first_event + events {
            for ordinal in 0..96i64 {
                let base = if ordinal % 2 == 0 {
                    4_000_000_000 + ordinal * 1_000_003 + event as i64
                } else {
                    -4_000_000_000 - ordinal * 999_983 - event as i64
                };
                sides.extend_from_slice(&[0, 1]);
                values.extend_from_slice(&[base, base + 1]);
            }
            offsets.push(sides.len() as u32);
        }
        small_batch(
            schema.schema_id,
            &(first_event..first_event + events)
                .map(|value| value as i64)
                .collect::<Vec<_>>(),
            &(first_event..first_event + events)
                .map(|value| -(value as i64))
                .collect::<Vec<_>>(),
            &offsets,
            &sides,
            &values,
        )
    };
    let whole = make_batch(0, 2);
    let artifact = compile_v3_planned_grouped_attempt5(
        &schema,
        std::slice::from_ref(&whole),
        Default::default(),
    )
    .unwrap();
    assert_eq!(artifact.inspection.candidates.len(), 7);
    assert!(artifact.inspection.candidates[6].selected);
    assert_eq!(
        artifact.inspection.plan.registry_version,
        AURA_PLAN_V2_CROSS_DOMAIN_REGISTRY_VERSION
    );
    let row = artifact
        .inspection
        .cross_domain
        .iter()
        .find(|row| row.logical_slot == 3)
        .unwrap();
    assert!(row.authorized && row.eligible && row.applicable);
    assert!(row.candidate_selected && row.selected);
    assert!(matches!(
        row.selected_op,
        AURA_PLAN_V2_DOMAIN0_FROM_DOMAIN1_OP | AURA_PLAN_V2_DOMAIN1_FROM_DOMAIN0_OP
    ));
    assert_eq!(
        decode_v3_planned_grouped(&artifact.bytes, Default::default())
            .unwrap()
            .batches,
        vec![whole]
    );
    let rechunked = compile_v3_planned_grouped_attempt5(
        &schema,
        &[make_batch(0, 1), make_batch(1, 1)],
        Default::default(),
    )
    .unwrap();
    assert_eq!(artifact.summary.plan_sha256, rechunked.summary.plan_sha256);
    assert_eq!(
        artifact.summary.global_logical_sha256,
        rechunked.summary.global_logical_sha256
    );
}

#[test]
fn attempt5_overflow_nullable_and_unauthorized_relationships_fall_back() {
    let schema = small_schema();
    let overflow = small_batch(
        schema.schema_id,
        &[1],
        &[1],
        &[0, 2],
        &[0, 1],
        &[i64::MIN, i64::MAX],
    );
    let artifact =
        compile_v3_planned_grouped_attempt5(&schema, &[overflow], Default::default()).unwrap();
    let row = artifact
        .inspection
        .cross_domain
        .iter()
        .find(|row| row.logical_slot == 3)
        .unwrap();
    assert!(!row.applicable && !row.selected);
    assert!(row
        .rejection
        .as_deref()
        .is_some_and(|reason| reason.contains("overflow")));
    assert!(!artifact.inspection.candidates[6].selected);

    let mut nullable = small_schema();
    nullable.fields[3].nullable = true;
    let groups = nullable.groups.clone();
    let nullable = nullable.with_v3_groups(groups).unwrap();
    let nullable_batch = AuraV3EventBatch {
        schema_id: nullable.schema_id,
        event_count: 1,
        child_offsets: vec![0, 2],
        event_columns: vec![
            AuraV3Column {
                slot: 0,
                validity: None,
                values: Values::TimestampMs(vec![1]),
            },
            AuraV3Column {
                slot: 1,
                validity: None,
                values: Values::I64(vec![1]),
            },
        ],
        repeated_columns: vec![
            AuraV3Column {
                slot: 2,
                validity: None,
                values: Values::U8(vec![0, 1]),
            },
            AuraV3Column {
                slot: 3,
                validity: Some(vec![0]),
                values: Values::I64(vec![0, 0]),
            },
        ],
    };
    let nullable_artifact =
        compile_v3_planned_grouped_attempt5(&nullable, &[nullable_batch], Default::default())
            .unwrap();
    let row = nullable_artifact
        .inspection
        .cross_domain
        .iter()
        .find(|row| row.logical_slot == 3)
        .unwrap();
    assert!(!row.eligible && !row.selected);

    let forbidden = SchemaBuilder::new("cross_forbidden")
        .field("ts", FieldType::TimestampMs, FieldRole::Timestamp)
        .repeated_field("side", FieldType::U8, FieldRole::Side)
        .repeated_field("value", FieldType::I64, FieldRole::Value)
        .dual_domain_repeated_group(
            1,
            vec![1, 2],
            1,
            RelationshipPermissions::none().with_within_domain(),
        )
        .finish()
        .unwrap();
    let groups = forbidden.groups.clone();
    let forbidden = forbidden.with_v3_groups(groups).unwrap();
    let forbidden_batch = AuraV3EventBatch {
        schema_id: forbidden.schema_id,
        event_count: 1,
        child_offsets: vec![0, 2],
        event_columns: vec![AuraV3Column {
            slot: 0,
            validity: None,
            values: Values::TimestampMs(vec![1]),
        }],
        repeated_columns: vec![
            AuraV3Column {
                slot: 1,
                validity: None,
                values: Values::U8(vec![0, 1]),
            },
            AuraV3Column {
                slot: 2,
                validity: None,
                values: Values::I64(vec![4, 5]),
            },
        ],
    };
    let forbidden_artifact =
        compile_v3_planned_grouped_attempt5(&forbidden, &[forbidden_batch], Default::default())
            .unwrap();
    assert!(!forbidden_artifact.inspection.candidates[6].authorized);
    assert!(forbidden_artifact.inspection.candidates[6]
        .complete_bytes
        .is_none());
}

#[test]
fn attempt5_registry_plan_mutations_and_anonymous_label_bias_fail_closed() {
    let schema = small_schema();
    let batch = small_batch(
        schema.schema_id,
        &[1],
        &[1],
        &[0, 4],
        &[0, 1, 0, 1],
        &[9_000_000, 9_000_001, -8_000_000, -7_999_999],
    );
    let artifact = compile_v3_planned_grouped_attempt5_candidate(
        &schema,
        &[batch],
        Default::default(),
        &[(3, AURA_PLAN_V2_DOMAIN1_FROM_DOMAIN0_OP)],
        PlanV2PhysicalCodec::SignedZigZagUleb128,
    )
    .unwrap();
    for length in 0..artifact.bytes.len() {
        assert!(
            decode_v3_planned_grouped(&artifact.bytes[..length], V3GroupedLimits::HARD).is_err()
        );
    }
    for index in 0..artifact.bytes.len() {
        let mut corrupted = artifact.bytes.clone();
        corrupted[index] ^= 1;
        assert!(decode_v3_planned_grouped(&corrupted, V3GroupedLimits::HARD).is_err());
    }
    let footer =
        decode_v3_planned_grouped_footer(footer_bytes(&artifact.bytes), V3GroupedLimits::HARD)
            .unwrap();
    let mut no_dependency = footer.plan.clone();
    no_dependency.streams[3].dependencies.clear();
    assert!(no_dependency.encode(&schema).is_err());
    let mut unknown_op = footer.plan.clone();
    unknown_op.streams[3].op = 99;
    assert!(unknown_op.encode(&schema).is_err());
    let mut encoded = footer.plan.encode(&schema).unwrap();
    encoded[57] = PlanV2Selection::PreviousWithinDomainMixed as u8;
    resign_plan_with_domain(&mut encoded, PLAN_HASH_DOMAIN_V5);
    assert!(AuraPlanV2::decode(&schema, &encoded).is_err());
    assert!(compile_v3_planned_grouped_attempt5_candidate(
        &schema,
        &[],
        Default::default(),
        &[(u16::MAX, AURA_PLAN_V2_DOMAIN0_FROM_DOMAIN1_OP)],
        PlanV2PhysicalCodec::FixedWidth,
    )
    .is_err());

    let renamed = SchemaBuilder::new("completely_anonymous")
        .field("a", FieldType::TimestampMs, FieldRole::Timestamp)
        .field("b", FieldType::I64, FieldRole::Sequence)
        .repeated_field("c", FieldType::U8, FieldRole::Side)
        .repeated_field("d", FieldType::I64, FieldRole::Value)
        .dual_domain_repeated_group(7, vec![2, 3], 2, permissions())
        .finish()
        .unwrap();
    let groups = renamed.groups.clone();
    let renamed = renamed.with_v3_groups(groups).unwrap();
    let make_bias_batch = |schema_id| {
        let mut sides = Vec::new();
        let mut values = Vec::new();
        for ordinal in 0..96i64 {
            let base = if ordinal % 2 == 0 {
                5_000_000_000 + ordinal * 1_000_003
            } else {
                -5_000_000_000 - ordinal * 999_983
            };
            sides.extend_from_slice(&[0, 1]);
            values.extend_from_slice(&[base, base + 1]);
        }
        small_batch(schema_id, &[1], &[1], &[0, 192], &sides, &values)
    };
    let original_artifact = compile_v3_planned_grouped_attempt5(
        &schema,
        &[make_bias_batch(schema.schema_id)],
        Default::default(),
    )
    .unwrap();
    let renamed_artifact = compile_v3_planned_grouped_attempt5(
        &renamed,
        &[make_bias_batch(renamed.schema_id)],
        Default::default(),
    )
    .unwrap();
    assert_eq!(
        original_artifact.inspection.plan.selected,
        renamed_artifact.inspection.plan.selected
    );
    let original_footer = decode_v3_planned_grouped_footer(
        footer_bytes(&original_artifact.bytes),
        V3GroupedLimits::HARD,
    )
    .unwrap();
    let renamed_footer = decode_v3_planned_grouped_footer(
        footer_bytes(&renamed_artifact.bytes),
        V3GroupedLimits::HARD,
    )
    .unwrap();
    assert_eq!(
        original_footer.plan.streams[3].op,
        renamed_footer.plan.streams[3].op
    );
    assert_eq!(
        original_footer.plan.physical_stream_codecs,
        renamed_footer.plan.physical_stream_codecs
    );
}

fn plan_hash(bytes: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(PLAN_HASH_DOMAIN);
    hasher.update((bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
    hasher.finalize().into()
}

fn hex_bytes(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn resign_plan(bytes: &mut [u8]) {
    let hash_start = bytes.len() - 32;
    let hash = plan_hash(&bytes[..hash_start]);
    bytes[hash_start..].copy_from_slice(&hash);
}

fn resign_plan_with_domain(bytes: &mut [u8], domain: &[u8]) {
    let hash_start = bytes.len() - 32;
    let mut hasher = Sha256::new();
    hasher.update(domain);
    hasher.update((hash_start as u64).to_le_bytes());
    hasher.update(&bytes[..hash_start]);
    bytes[hash_start..].copy_from_slice(&<[u8; 32]>::from(hasher.finalize()));
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

fn rebuild_with_replaced_body_range(
    artifact: &aura_codec::V3PlannedGroupedArtifact,
    range: std::ops::Range<usize>,
    replacement: &[u8],
) -> Vec<u8> {
    let header_len = AuraHeader::encoded_len(&artifact.bytes).unwrap();
    let mut body = artifact.bytes[header_len..footer_start(&artifact.bytes)].to_vec();
    body.splice(range, replacement.iter().copied());
    let body_len = body.len() as u64;
    body[64..72].copy_from_slice(&body_len.to_le_bytes());
    let mut footer =
        decode_v3_planned_grouped_footer(footer_bytes(&artifact.bytes), V3GroupedLimits::HARD)
            .unwrap();
    footer.body_len = body_len;
    footer.body_sha256 = domain_hash(PLANNED_BODY_HASH_DOMAIN, &body);
    footer.chunks[0].stored_len = body_len;
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
