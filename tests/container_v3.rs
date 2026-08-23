use aura_codec::schema::{
    decode_group_descriptor_table, decode_schema_descriptor, decode_v3_schema_map,
    encode_group_descriptor_table, encode_schema_descriptor, generic_i64_parent_schema, FieldRole,
    FieldType, GroupDescriptor, RelationshipPermissions, SchemaBuilder, SchemaEncodingVersion,
    SchemaMapHint,
};
use aura_codec::{
    parse_schema_json, records, validate_schema_container_compatibility, AuraContainerVersion,
    AuraError, AuraFooter, AuraHeader, AuraI64Writer, DerivedExpression, DerivedExpressionOp,
    IngestStats, Profile, MAX_V3_HEADER_BYTES, V3_HEADER_PREFIX_SIZE,
};

fn permissions() -> RelationshipPermissions {
    RelationshipPermissions::none()
        .with_split()
        .with_within_domain()
        .with_across_domain_same_field()
        .with_joint_same_field()
}

fn v3_schema(groups_reversed: bool) -> aura_codec::Result<aura_codec::SchemaDescriptor> {
    let repeated = GroupDescriptor::repeated(
        10,
        vec![5, 6],
        RelationshipPermissions::none()
            .with_split()
            .with_within_domain(),
    );
    let dual = GroupDescriptor::dual_domain_repeated(20, vec![2, 3, 4], 2, permissions());
    let builder = SchemaBuilder::new("v3_grouped_book")
        .v3()
        .field("ts", FieldType::TimestampNs, FieldRole::Timestamp)
        .field("sequence", FieldType::U64, FieldRole::Sequence)
        .repeated_field("side", FieldType::U8, FieldRole::Side)
        .repeated_field("price", FieldType::I64, FieldRole::Price)
        .repeated_field("quantity", FieldType::I64, FieldRole::Quantity)
        .repeated_field("code", FieldType::U8, FieldRole::Enum)
        .repeated_field("value", FieldType::I64, FieldRole::Value);
    if groups_reversed {
        builder.group(dual).group(repeated).finish()
    } else {
        builder.group(repeated).group(dual).finish()
    }
}

#[test]
fn v3_multiple_timestamp_roles_keep_one_primary_front_marker() {
    let schema = SchemaBuilder::new("multi_timestamp")
        .v3()
        .field("source_ts_ms", FieldType::TimestampMs, FieldRole::Timestamp)
        .nullable_field(
            "source_time_aux_ms",
            FieldType::TimestampMs,
            FieldRole::Timestamp,
        )
        .field("sequence", FieldType::U64, FieldRole::Sequence)
        .finish()
        .unwrap();
    assert_eq!(
        schema.compact_schema_map.as_deref(),
        Some(&[100, 255, 0][..])
    );

    let descriptor = encode_schema_descriptor(&schema).unwrap();
    assert_eq!(descriptor[4], 4);
    assert_eq!(decode_schema_descriptor(&descriptor).unwrap(), schema);
    let canonical = schema.to_canonical_json().unwrap();
    assert_eq!(parse_schema_json(&canonical).unwrap(), schema);

    let header = AuraHeader::new(Profile::Ingest)
        .with_container_version(AuraContainerVersion::V3)
        .with_schema_mapping(schema.compact_schema_map.clone().unwrap())
        .unwrap();
    assert_eq!(
        AuraHeader::decode(&header.encode().unwrap()).unwrap(),
        header
    );

    let rebuilt = SchemaBuilder::new("multi_timestamp")
        .v3()
        .field("source_ts_ms", FieldType::TimestampMs, FieldRole::Timestamp)
        .nullable_field(
            "source_time_aux_ms",
            FieldType::TimestampMs,
            FieldRole::Timestamp,
        )
        .field("sequence", FieldType::U64, FieldRole::Sequence)
        .finish()
        .unwrap();
    assert_eq!(rebuilt.schema_id, schema.schema_id);
    let reordered = SchemaBuilder::new("multi_timestamp")
        .v3()
        .nullable_field(
            "source_time_aux_ms",
            FieldType::TimestampMs,
            FieldRole::Timestamp,
        )
        .field("source_ts_ms", FieldType::TimestampMs, FieldRole::Timestamp)
        .field("sequence", FieldType::U64, FieldRole::Sequence)
        .finish()
        .unwrap();
    assert_ne!(reordered.schema_id, schema.schema_id);
}

#[test]
fn v3_timestamp_roles_reject_forged_scope_type_and_scale() {
    assert_eq!(
        SchemaBuilder::new("forged_second_primary")
            .v3()
            .field("ts", FieldType::TimestampMs, FieldRole::Timestamp)
            .field("aux", FieldType::TimestampMs, FieldRole::Timestamp)
            .v3_schema_mapping(vec![100, 100])
            .finish(),
        Err(AuraError::InvalidValue("time slot"))
    );
    assert_eq!(
        SchemaBuilder::new("repeated_timestamp")
            .v3()
            .field("ts", FieldType::TimestampNs, FieldRole::Timestamp)
            .repeated_field("aux", FieldType::TimestampMs, FieldRole::Timestamp)
            .repeated_group(1, vec![1], RelationshipPermissions::none())
            .finish(),
        Err(AuraError::InvalidValue("timestamp scope"))
    );
    for field_type in [
        FieldType::Utf8,
        FieldType::DecimalText,
        FieldType::Opaque16,
        FieldType::U8,
    ] {
        assert!(matches!(
            SchemaBuilder::new("invalid_timestamp_type")
                .v3()
                .field("ts", field_type, FieldRole::Timestamp)
                .finish(),
            Err(AuraError::InvalidValue("v3 timestamp field"))
        ));
    }

    let mut bad_ns_scale = SchemaBuilder::new("bad_ns_scale")
        .v3()
        .field("ts", FieldType::TimestampNs, FieldRole::Timestamp)
        .finish()
        .unwrap();
    bad_ns_scale.fields[0].scale = -9;
    assert_eq!(
        bad_ns_scale.validate(),
        Err(AuraError::InvalidValue("v3 timestamp field"))
    );
    let mut bad_i64_scale = SchemaBuilder::new("bad_i64_scale")
        .v3()
        .field("ts", FieldType::I64, FieldRole::Timestamp)
        .finish()
        .unwrap();
    bad_i64_scale.fields[0].scale = -3;
    assert_eq!(
        bad_i64_scale.validate(),
        Err(AuraError::InvalidValue("v3 timestamp field"))
    );
    let mut bad_ms_scale = SchemaBuilder::new("bad_ms_scale")
        .v3()
        .field("ts", FieldType::TimestampMs, FieldRole::Timestamp)
        .finish()
        .unwrap();
    bad_ms_scale.fields[0].scale = -3;
    assert_eq!(
        bad_ms_scale.validate(),
        Err(AuraError::InvalidValue("timestamp_ms field"))
    );

    let mut micros = SchemaBuilder::new("valid_i64_micros")
        .v3()
        .field("ts", FieldType::I64, FieldRole::Timestamp)
        .finish()
        .unwrap();
    micros.fields[0].scale = -6;
    assert!(micros.validate().is_ok());
}

#[test]
fn v3_header_round_trips_authoritative_sections() {
    let schema = v3_schema(false).unwrap();
    let header = AuraHeader::new(Profile::Ingest)
        .with_container_version(AuraContainerVersion::V3)
        .with_stream(7, 11, 13)
        .with_schema_mapping(schema.compact_schema_map.clone().unwrap())
        .unwrap()
        .with_groups(schema.groups.clone())
        .unwrap()
        .with_comment("grouped book")
        .unwrap();

    let encoded = header.encode().unwrap();
    assert_eq!(encoded.len(), AuraHeader::encoded_len(&encoded).unwrap());
    assert_eq!(
        encoded.len() as u32,
        u32::from_le_bytes(encoded[7..11].try_into().unwrap())
    );
    assert_eq!(7, u32::from_le_bytes(encoded[23..27].try_into().unwrap()));
    assert!(encoded.len() > V3_HEADER_PREFIX_SIZE + 7);
    assert_eq!(header, AuraHeader::decode(&encoded).unwrap());
}

#[test]
fn v3_slot_200_consumes_the_actual_discriminator_field() {
    let schema = v3_schema(false).unwrap();
    let mapping = schema.compact_schema_map.as_deref().unwrap();
    assert_eq!(&[100, 0, 200, 0, 0, 242, 0], mapping);

    let entries = decode_v3_schema_map(mapping, &schema.groups).unwrap();
    assert_eq!(mapping.len(), entries.len());
    assert_eq!(2, entries[2].field_index);
    assert_eq!(SchemaMapHint::DualDomainDiscriminator, entries[2].hint);
    assert_eq!(3, entries[3].field_index);
}

#[test]
fn group_table_and_v3_hash_are_deterministic_but_preserve_child_order() {
    let first = v3_schema(false).unwrap();
    let reversed = v3_schema(true).unwrap();
    assert_eq!(SchemaEncodingVersion::V3, first.encoding_version);
    assert_eq!(first.schema_id, reversed.schema_id);
    assert_eq!(
        encode_schema_descriptor(&first).unwrap(),
        encode_schema_descriptor(&reversed).unwrap()
    );

    let encoded = encode_group_descriptor_table(&reversed.groups).unwrap();
    let decoded = decode_group_descriptor_table(&encoded).unwrap();
    assert_eq!(
        vec![10, 20],
        decoded
            .iter()
            .map(|group| group.group_id)
            .collect::<Vec<_>>()
    );
    assert_eq!(vec![2, 3, 4], decoded[1].child_slots);

    let decoded_schema =
        decode_schema_descriptor(&encode_schema_descriptor(&first).unwrap()).unwrap();
    assert_eq!(first.schema_id, decoded_schema.schema_id);
    assert_eq!(SchemaEncodingVersion::V3, decoded_schema.encoding_version);
    assert_eq!(first.compact_schema_map, decoded_schema.compact_schema_map);
}

#[test]
fn v3_flat_identity_is_explicit_and_v2_hash_stays_frozen() {
    let v2 = generic_i64_parent_schema("compat-v2-plain", &[100, 0, 2, 0]).unwrap();
    assert_eq!(SchemaEncodingVersion::V2, v2.encoding_version);
    assert_eq!(590_236_859, v2.schema_id);
    assert_eq!(&[100, 0, 2, 0], v2.compact_schema_map.as_deref().unwrap());

    let v3 = SchemaBuilder::new("flat")
        .v3()
        .field("ts", FieldType::TimestampNs, FieldRole::Timestamp)
        .field("value", FieldType::I64, FieldRole::Value)
        .finish()
        .unwrap();
    assert!(v3.groups.is_empty());
    assert_eq!(SchemaEncodingVersion::V3, v3.encoding_version);
    assert_eq!(&[100, 0], v3.compact_schema_map.as_deref().unwrap());
    assert_ne!(
        encode_schema_descriptor(&v2).unwrap()[4],
        encode_schema_descriptor(&v3).unwrap()[4],
        "the byte after the outer u32 length is the explicit schema encoding tag"
    );
}

#[test]
fn field_type_codes_are_append_only_and_v2_rejects_every_v3_only_type() {
    let old = [
        FieldType::I8,
        FieldType::U8,
        FieldType::I16,
        FieldType::U16,
        FieldType::I32,
        FieldType::U32,
        FieldType::I64,
        FieldType::U64,
        FieldType::TimestampNs,
        FieldType::I128,
        FieldType::Opaque16,
    ];
    for (code, field_type) in (1u8..=11).zip(old) {
        assert_eq!(field_type, FieldType::from_code(code).unwrap());
    }
    for (field_type, role) in [
        (FieldType::TimestampMs, FieldRole::Timestamp),
        (FieldType::Utf8, FieldRole::Identifier),
        (FieldType::DecimalText, FieldRole::Price),
    ] {
        assert_eq!(
            SchemaBuilder::new("v2_reject")
                .field("value", field_type, role)
                .finish(),
            Err(AuraError::InvalidValue("v3-only field type"))
        );
    }

    let mut forged_v2 = SchemaBuilder::new("forged")
        .v3()
        .field("text", FieldType::Utf8, FieldRole::Value)
        .finish()
        .unwrap();
    forged_v2.encoding_version = SchemaEncodingVersion::V2;
    assert_eq!(
        validate_schema_container_compatibility(&forged_v2, AuraContainerVersion::V2),
        Err(AuraError::InvalidValue("v3-only field type"))
    );
    assert_eq!(
        encode_schema_descriptor(&forged_v2),
        Err(AuraError::InvalidValue("v3-only field type"))
    );
}

#[test]
fn v3_header_rejects_malformed_u32_lengths_before_section_allocation() {
    let schema = v3_schema(false).unwrap();
    let mut encoded = AuraHeader::new(Profile::Aura0)
        .with_container_version(AuraContainerVersion::V3)
        .with_schema_mapping(schema.compact_schema_map.clone().unwrap())
        .unwrap()
        .with_groups(schema.groups)
        .unwrap()
        .encode()
        .unwrap();

    encoded[23..27].copy_from_slice(&u32::MAX.to_le_bytes());
    assert_eq!(
        AuraHeader::decode(&encoded),
        Err(AuraError::InvalidValue("header length"))
    );

    let mut short_prefix = encoded[..10].to_vec();
    short_prefix[4..6].copy_from_slice(&3u16.to_le_bytes());
    assert_eq!(
        AuraHeader::encoded_len(&short_prefix),
        Err(AuraError::UnexpectedEof)
    );
}

#[test]
fn v3_groups_reject_duplicate_overlap_and_bad_slots() {
    let duplicate_id = SchemaBuilder::new("duplicate")
        .v3()
        .field("event", FieldType::I64, FieldRole::Value)
        .repeated_field("a", FieldType::I64, FieldRole::Value)
        .repeated_field("b", FieldType::I64, FieldRole::Value)
        .repeated_group(1, vec![1], RelationshipPermissions::none())
        .repeated_group(1, vec![2], RelationshipPermissions::none())
        .finish();
    assert_eq!(
        duplicate_id,
        Err(AuraError::InvalidValue("duplicate group id"))
    );

    let overlap = SchemaBuilder::new("overlap")
        .v3()
        .field("event", FieldType::I64, FieldRole::Value)
        .repeated_field("a", FieldType::I64, FieldRole::Value)
        .repeated_field("b", FieldType::I64, FieldRole::Value)
        .repeated_group(1, vec![1, 2], RelationshipPermissions::none())
        .repeated_group(2, vec![2], RelationshipPermissions::none())
        .finish();
    assert_eq!(
        overlap,
        Err(AuraError::InvalidValue("overlapping group child slot"))
    );

    let bad_slot = SchemaBuilder::new("bad_slot")
        .v3()
        .field("event", FieldType::I64, FieldRole::Value)
        .repeated_group(1, vec![9], RelationshipPermissions::none())
        .finish();
    assert_eq!(bad_slot, Err(AuraError::InvalidValue("group child slot")));
}

#[test]
fn v3_dual_groups_require_an_exact_slot_marker_and_two_domains() {
    let missing = SchemaBuilder::new("missing_marker")
        .v3_schema_mapping(vec![100, 0])
        .field("ts", FieldType::TimestampNs, FieldRole::Timestamp)
        .repeated_field("side", FieldType::U8, FieldRole::Side)
        .dual_domain_repeated_group(1, vec![1], 1, permissions())
        .finish();
    assert_eq!(
        missing,
        Err(AuraError::InvalidValue("dual-domain schema marker"))
    );

    let stray = SchemaBuilder::new("stray_marker")
        .v3_schema_mapping(vec![100, 200])
        .field("ts", FieldType::TimestampNs, FieldRole::Timestamp)
        .field("side", FieldType::U8, FieldRole::Side)
        .finish();
    assert_eq!(
        stray,
        Err(AuraError::InvalidValue("dual-domain schema marker"))
    );

    let mut bad_domains =
        GroupDescriptor::dual_domain_repeated(1, vec![1], 1, RelationshipPermissions::none());
    bad_domains.dual_domain.as_mut().unwrap().domain_count = 3;
    let result = SchemaBuilder::new("too_many_domains")
        .v3()
        .field("event", FieldType::I64, FieldRole::Value)
        .repeated_field("side", FieldType::U8, FieldRole::Side)
        .group(bad_domains)
        .finish();
    assert_eq!(result, Err(AuraError::InvalidValue("dual-domain count")));

    assert!(decode_v3_schema_map(
        &[100, 201],
        &[GroupDescriptor::repeated(
            1,
            vec![1],
            RelationshipPermissions::none()
        )]
    )
    .is_err());
}

#[test]
fn v3_group_table_rejects_unknown_kind_and_flags() {
    let group = GroupDescriptor::dual_domain_repeated(1, vec![1], 1, permissions());
    let encoded = encode_group_descriptor_table(&[group]).unwrap();

    let mut unknown_kind = encoded.clone();
    unknown_kind[5] = 99;
    assert_eq!(
        decode_group_descriptor_table(&unknown_kind),
        Err(AuraError::InvalidValue("group kind"))
    );

    let mut unknown_relationship = encoded.clone();
    unknown_relationship[6] |= 0x80;
    assert_eq!(
        decode_group_descriptor_table(&unknown_relationship),
        Err(AuraError::InvalidValue("group relationship flags"))
    );

    let mut unknown_descriptor_flag = encoded;
    unknown_descriptor_flag[7] |= 0x80;
    assert_eq!(
        decode_group_descriptor_table(&unknown_descriptor_flag),
        Err(AuraError::InvalidValue("group descriptor flags"))
    );
}

#[test]
fn v3_encoding_is_independent_of_group_and_header_builder_order() {
    let first = v3_schema(false).unwrap();
    let second = v3_schema(true).unwrap();
    let first_header = AuraHeader::new(Profile::Ingest)
        .with_container_version(AuraContainerVersion::V3)
        .with_groups(first.groups)
        .unwrap()
        .with_schema_mapping(first.compact_schema_map.unwrap())
        .unwrap()
        .encode()
        .unwrap();
    let second_header = AuraHeader::new(Profile::Ingest)
        .with_container_version(AuraContainerVersion::V3)
        .with_schema_mapping(second.compact_schema_map.unwrap())
        .unwrap()
        .with_groups(second.groups)
        .unwrap()
        .encode()
        .unwrap();
    assert_eq!(first_header, second_header);
}

#[test]
fn v3_stable_field_ids_are_validated() {
    let mut schema = v3_schema(false).unwrap();
    schema.fields[3].index = 9;
    assert_eq!(
        schema.validate(),
        Err(AuraError::InvalidValue("stable field id"))
    );
}

#[test]
fn v3_group_child_columns_must_follow_global_field_order() {
    let result = SchemaBuilder::new("permuted_children")
        .v3()
        .field("event", FieldType::I64, FieldRole::Value)
        .repeated_field("a", FieldType::I64, FieldRole::Value)
        .repeated_field("b", FieldType::I64, FieldRole::Value)
        .repeated_group(1, vec![2, 1], RelationshipPermissions::none())
        .finish();
    assert_eq!(
        result,
        Err(AuraError::InvalidValue("group child slot order"))
    );
}

#[test]
fn v3_header_rejects_expression_dependency_cycles() {
    let expressions = vec![
        DerivedExpression::new(1, 1, DerivedExpressionOp::AddResidual, vec![2]).unwrap(),
        DerivedExpression::new(2, 2, DerivedExpressionOp::AddResidual, vec![1]).unwrap(),
    ];
    let result = AuraHeader::new(Profile::Ingest)
        .with_container_version(AuraContainerVersion::V3)
        .with_schema_mapping(vec![100, 101, 102])
        .unwrap()
        .with_derived_expressions(expressions)
        .unwrap()
        .encode();
    assert_eq!(
        result,
        Err(AuraError::InvalidValue("derived expression cycle"))
    );
}

#[test]
fn v3_header_size_ceiling_precedes_file_backed_allocation() {
    assert_eq!(16 * 1024 * 1024, MAX_V3_HEADER_BYTES);
    assert_eq!(aura_codec::header::MAX_V3_HEADER_BYTES, MAX_V3_HEADER_BYTES);
    let oversized_len = MAX_V3_HEADER_BYTES + 1;
    let mut oversized = vec![0; oversized_len];
    oversized[..4].copy_from_slice(b"AURA");
    oversized[4..6].copy_from_slice(&3u16.to_le_bytes());
    oversized[6] = Profile::Ingest as u8;
    oversized[7..11].copy_from_slice(&(oversized_len as u32).to_le_bytes());
    assert_eq!(
        AuraHeader::encoded_len(&oversized[..11]),
        Err(AuraError::InvalidValue("header length"))
    );
    assert_eq!(
        AuraHeader::decode(&oversized),
        Err(AuraError::InvalidValue("header length"))
    );

    let too_large_comment = "x".repeat(MAX_V3_HEADER_BYTES);
    assert_eq!(
        AuraHeader::new(Profile::Ingest)
            .with_container_version(AuraContainerVersion::V3)
            .with_schema_mapping(vec![100])
            .unwrap()
            .with_comment(too_large_comment),
        Err(AuraError::InvalidValue("header length"))
    );
}

#[test]
fn v3_header_rejects_inconsistent_section_sum() {
    let mut encoded = AuraHeader::new(Profile::Ingest)
        .with_container_version(AuraContainerVersion::V3)
        .with_schema_mapping(vec![100, 0])
        .unwrap()
        .encode()
        .unwrap();
    encoded[23..27].copy_from_slice(&3u32.to_le_bytes());
    assert_eq!(
        AuraHeader::decode(&encoded),
        Err(AuraError::InvalidValue("header length"))
    );
}

#[test]
fn v2_ingest_and_compiled_emitters_reject_v3_schemas() {
    let v3 = SchemaBuilder::new("v3_rejected_by_v2")
        .v3()
        .field("ts", FieldType::TimestampNs, FieldRole::Timestamp)
        .field("value", FieldType::I64, FieldRole::Value)
        .finish()
        .unwrap();
    let stats = IngestStats::new(v3.fields.len()).unwrap();
    assert_eq!(
        AuraFooter::new(v3.clone(), stats).encode(),
        Err(AuraError::InvalidValue("schema container version"))
    );

    let mut writer = AuraI64Writer::new(v3.clone());
    writer.push_row(vec![1, 2]).unwrap();
    assert_eq!(
        writer.finish(),
        Err(AuraError::InvalidValue("schema container version"))
    );

    let v2 = generic_i64_parent_schema("compiled_gate_v2", &[100, 0]).unwrap();
    let ingest = records::encode_ingest_i64_file(records::I64FileInput {
        schema: v2,
        rows: vec![vec![1, 2]],
        stream_id: 0,
        dictionary_id: 0,
        header_comment: None,
    })
    .unwrap();
    let compiled = records::compile_i64_file(&ingest, Profile::Aura0).unwrap();
    let mut compiled_footer = records::decode_i64_file(&compiled)
        .unwrap()
        .compiled_footer
        .unwrap();
    compiled_footer.schema = v3;
    assert_eq!(
        compiled_footer.encode(),
        Err(AuraError::InvalidValue("schema container version"))
    );
}

#[test]
fn v3_derived_expression_order_is_canonical_across_all_public_encodings() {
    let expression_1 =
        DerivedExpression::new(1, 1, DerivedExpressionOp::AddResidual, vec![0]).unwrap();
    let expression_2 =
        DerivedExpression::new(2, 2, DerivedExpressionOp::AddResidual, vec![0]).unwrap();
    let base_schema = || {
        SchemaBuilder::new("expression_order")
            .field("ts", FieldType::TimestampNs, FieldRole::Timestamp)
            .field("a", FieldType::I64, FieldRole::Value)
            .field("b", FieldType::I64, FieldRole::Value)
            .finish()
            .unwrap()
    };
    let forward = base_schema()
        .with_derived_expressions(vec![expression_1.clone(), expression_2.clone()])
        .unwrap()
        .into_v3()
        .unwrap();
    let reverse = base_schema()
        .with_derived_expressions(vec![expression_2.clone(), expression_1.clone()])
        .unwrap()
        .into_v3()
        .unwrap();
    assert_eq!(forward.schema_id, reverse.schema_id);
    assert_eq!(forward.derived_expressions, reverse.derived_expressions);
    assert_eq!(
        encode_schema_descriptor(&forward).unwrap(),
        encode_schema_descriptor(&reverse).unwrap()
    );
    let canonical_json = forward.to_canonical_json().unwrap();
    assert_eq!(canonical_json, reverse.to_canonical_json().unwrap());
    assert_eq!(
        forward,
        parse_schema_json(&canonical_json).unwrap(),
        "canonical JSON reparsing retains the same binary schema identity"
    );

    let mapping = forward.compact_schema_map.clone().unwrap();
    let forward_header = AuraHeader::new(Profile::Ingest)
        .with_container_version(AuraContainerVersion::V3)
        .with_schema_mapping(mapping.clone())
        .unwrap()
        .with_derived_expressions(vec![expression_1.clone(), expression_2.clone()])
        .unwrap()
        .encode()
        .unwrap();
    let reverse_header = AuraHeader::new(Profile::Ingest)
        .with_schema_mapping(mapping)
        .unwrap()
        .with_derived_expressions(vec![expression_2, expression_1])
        .unwrap()
        .with_container_version(AuraContainerVersion::V3)
        .encode()
        .unwrap();
    assert_eq!(forward_header, reverse_header);
    let decoded_header = AuraHeader::decode(&reverse_header).unwrap();
    assert_eq!(
        vec![1, 2],
        decoded_header
            .derived_expressions
            .iter()
            .map(|expression| expression.expression_id)
            .collect::<Vec<_>>()
    );
}
