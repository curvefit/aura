use aura_codec::{
    canonical_v3_batch_sha256, canonical_v3_schema_fingerprint, decode_v3_value_block,
    encode_v3_value_block, records, validate_decimal_text_v1, validate_v3_batch, AuraError,
    AuraFooter, AuraHeader, AuraV3Batch, AuraV3Column, AuraV3ColumnValues as Values,
    AuraV3VariableColumn, DerivedExpression, DerivedExpressionOp, FieldRole, FieldTransform,
    FieldType, GroupDescriptor, RelationshipPermissions, SchemaBuilder, TransformCandidates,
    V3ValueLimits, MAX_V3_VALUE_ROWS,
};
use sha2::Digest;

fn variable(parts: &[&[u8]]) -> AuraV3VariableColumn {
    let mut offsets = vec![0];
    let mut data = Vec::new();
    for part in parts {
        data.extend_from_slice(part);
        offsets.push(data.len() as u32);
    }
    AuraV3VariableColumn { offsets, data }
}

fn all_types_schema() -> aura_codec::SchemaDescriptor {
    SchemaBuilder::new("v3_exact_values")
        .v3()
        .field("ts_ms", FieldType::TimestampMs, FieldRole::Timestamp)
        .field("i8", FieldType::I8, FieldRole::Value)
        .nullable_field("u8", FieldType::U8, FieldRole::Value)
        .field("i16", FieldType::I16, FieldRole::Value)
        .field("u16", FieldType::U16, FieldRole::Value)
        .field("i32", FieldType::I32, FieldRole::Value)
        .field("u32", FieldType::U32, FieldRole::Value)
        .field("i64", FieldType::I64, FieldRole::Value)
        .field("u64", FieldType::U64, FieldRole::Value)
        .field("ts_ns", FieldType::TimestampNs, FieldRole::Value)
        .field("i128", FieldType::I128, FieldRole::Value)
        .nullable_field("opaque", FieldType::Opaque16, FieldRole::Identifier)
        .nullable_field("utf8", FieldType::Utf8, FieldRole::Identifier)
        .nullable_field("decimal", FieldType::DecimalText, FieldRole::Price)
        .finish()
        .unwrap()
}

fn all_types_batch(schema_id: u32) -> AuraV3Batch {
    let valid = Some(vec![0b0000_0101]);
    AuraV3Batch {
        schema_id,
        row_count: 3,
        columns: vec![
            AuraV3Column {
                slot: 0,
                validity: None,
                values: Values::TimestampMs(vec![i64::MIN, 0, i64::MAX]),
            },
            AuraV3Column {
                slot: 1,
                validity: None,
                values: Values::I8(vec![i8::MIN, 0, i8::MAX]),
            },
            AuraV3Column {
                slot: 2,
                validity: valid.clone(),
                values: Values::U8(vec![0, 0, u8::MAX]),
            },
            AuraV3Column {
                slot: 3,
                validity: None,
                values: Values::I16(vec![i16::MIN, 0, i16::MAX]),
            },
            AuraV3Column {
                slot: 4,
                validity: None,
                values: Values::U16(vec![0, 1, u16::MAX]),
            },
            AuraV3Column {
                slot: 5,
                validity: None,
                values: Values::I32(vec![i32::MIN, 0, i32::MAX]),
            },
            AuraV3Column {
                slot: 6,
                validity: None,
                values: Values::U32(vec![0, 1, u32::MAX]),
            },
            AuraV3Column {
                slot: 7,
                validity: None,
                values: Values::I64(vec![i64::MIN, 0, i64::MAX]),
            },
            AuraV3Column {
                slot: 8,
                validity: None,
                values: Values::U64(vec![0, i64::MAX as u64 + 1, u64::MAX]),
            },
            AuraV3Column {
                slot: 9,
                validity: None,
                values: Values::TimestampNs(vec![i64::MIN, 0, i64::MAX]),
            },
            AuraV3Column {
                slot: 10,
                validity: None,
                values: Values::I128(vec![i128::MIN, 0, i128::MAX]),
            },
            AuraV3Column {
                slot: 11,
                validity: valid.clone(),
                values: Values::Opaque16(vec![[1; 16], [0; 16], [255; 16]]),
            },
            AuraV3Column {
                slot: 12,
                validity: valid.clone(),
                values: Values::Utf8(variable(&[b"", b"", "é\0".as_bytes()])),
            },
            AuraV3Column {
                slot: 13,
                validity: valid,
                values: Values::DecimalText(variable(&[
                    b"+01.20",
                    b"",
                    "\u{2003}-0\u{2003}".as_bytes(),
                ])),
            },
        ],
    }
}

#[test]
fn exact_roundtrip_all_types_edges_nulls_unicode_and_deterministic_bytes() {
    let schema = all_types_schema();
    let batch = all_types_batch(schema.schema_id);
    validate_v3_batch(&schema, &batch, V3ValueLimits::default()).unwrap();
    let first = encode_v3_value_block(&schema, &batch, V3ValueLimits::default()).unwrap();
    let second = encode_v3_value_block(&schema, &batch, V3ValueLimits::default()).unwrap();
    assert_eq!(first, second);
    assert_eq!(
        batch,
        decode_v3_value_block(&schema, &first, V3ValueLimits::default()).unwrap()
    );
    assert_eq!(
        Some(aura_codec::AuraV3ValueRef::U64(u64::MAX)),
        batch.columns[8].value_ref(2).unwrap()
    );
    assert_eq!(None, batch.columns[12].value_ref(1).unwrap());
    assert_eq!(
        Some(aura_codec::AuraV3ValueRef::Utf8("")),
        batch.columns[12].value_ref(0).unwrap()
    );
}

#[test]
fn decimal_text_v1_preserves_but_strictly_validates_original_utf8() {
    for valid in [
        "+1.2",
        "-0",
        ".5",
        "5.",
        "000.0100",
        "\u{2003}+01.20\u{00a0}",
    ] {
        validate_decimal_text_v1(valid).unwrap();
    }
    for invalid in [
        "", "  ", "+", ".", "1.2.3", "1e3", "NaN", "Inf", "1 2", "１２",
    ] {
        assert_eq!(
            validate_decimal_text_v1(invalid),
            Err(AuraError::InvalidValue("decimal text v1"))
        );
    }

    let white_space = [
        '\u{0009}', '\u{000a}', '\u{000b}', '\u{000c}', '\u{000d}', '\u{0020}', '\u{0085}',
        '\u{00a0}', '\u{1680}', '\u{2000}', '\u{2001}', '\u{2002}', '\u{2003}', '\u{2004}',
        '\u{2005}', '\u{2006}', '\u{2007}', '\u{2008}', '\u{2009}', '\u{200a}', '\u{2028}',
        '\u{2029}', '\u{202f}', '\u{205f}', '\u{3000}',
    ];
    for character in white_space {
        validate_decimal_text_v1(&format!("{character}+01.20{character}")).unwrap();
    }
    for character in ['\u{180e}', '\u{200b}', '\u{feff}'] {
        assert!(validate_decimal_text_v1(&format!("{character}1{character}")).is_err());
    }
}

fn one_nullable_batch(schema_id: u32, validity: u8, values: Vec<i64>) -> AuraV3Batch {
    AuraV3Batch {
        schema_id,
        row_count: values.len() as u32,
        columns: vec![AuraV3Column {
            slot: 0,
            validity: Some(vec![validity]),
            values: Values::I64(values),
        }],
    }
}

#[test]
fn canonical_hash_is_deterministic_and_sensitive_to_presence_value_and_row_order() {
    let schema = SchemaBuilder::new("hash")
        .v3()
        .nullable_field("value", FieldType::I64, FieldRole::Value)
        .finish()
        .unwrap();
    let ordered = one_nullable_batch(schema.schema_id, 0b11, vec![0, 7]);
    let reversed = one_nullable_batch(schema.schema_id, 0b11, vec![7, 0]);
    let null_zero = one_nullable_batch(schema.schema_id, 0b10, vec![0, 7]);
    let hash = canonical_v3_batch_sha256(&schema, &ordered, V3ValueLimits::default()).unwrap();
    let hash_hex = hash
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    assert_eq!(
        "e0f10375bcc9fd4fd3ac035ea6dc0565ebcde9dc83ddea60dc0bfbdd92a6aaaf",
        hash_hex
    );
    assert_eq!(
        hash,
        canonical_v3_batch_sha256(&schema, &ordered, V3ValueLimits::default()).unwrap()
    );
    assert_ne!(
        hash,
        canonical_v3_batch_sha256(&schema, &reversed, V3ValueLimits::default()).unwrap()
    );
    assert_ne!(
        hash,
        canonical_v3_batch_sha256(&schema, &null_zero, V3ValueLimits::default()).unwrap()
    );
}

#[test]
fn validity_and_null_placeholders_are_canonical() {
    let schema = SchemaBuilder::new("nulls")
        .v3()
        .nullable_field("value", FieldType::I64, FieldRole::Value)
        .finish()
        .unwrap();
    let mut bad_padding = one_nullable_batch(schema.schema_id, 0b1000_0001, vec![1]);
    assert_eq!(
        validate_v3_batch(&schema, &bad_padding, V3ValueLimits::default()),
        Err(AuraError::InvalidValue("v3 value validity padding"))
    );
    bad_padding.columns[0].validity = Some(vec![0]);
    bad_padding.columns[0].values = Values::I64(vec![1]);
    assert_eq!(
        validate_v3_batch(&schema, &bad_padding, V3ValueLimits::default()),
        Err(AuraError::InvalidValue("v3 null placeholder"))
    );

    let nonnullable = SchemaBuilder::new("required")
        .v3()
        .field("value", FieldType::I64, FieldRole::Value)
        .finish()
        .unwrap();
    let batch = AuraV3Batch {
        schema_id: nonnullable.schema_id,
        row_count: 1,
        columns: vec![AuraV3Column {
            slot: 0,
            validity: Some(vec![1]),
            values: Values::I64(vec![1]),
        }],
    };
    assert_eq!(
        validate_v3_batch(&nonnullable, &batch, V3ValueLimits::default()),
        Err(AuraError::InvalidValue("v3 value validity"))
    );
}

#[test]
fn lower_limits_are_enforced_without_large_allocations_and_cannot_raise_hard_limits() {
    let schema = SchemaBuilder::new("limited")
        .v3()
        .field("text", FieldType::Utf8, FieldRole::Value)
        .finish()
        .unwrap();
    let batch = AuraV3Batch {
        schema_id: schema.schema_id,
        row_count: 1,
        columns: vec![AuraV3Column {
            slot: 0,
            validity: None,
            values: Values::Utf8(variable(&[b"12345"])),
        }],
    };
    let limits = V3ValueLimits {
        max_block_bytes: 128,
        max_variable_value_bytes: 4,
        max_rows: 1,
    };
    assert_eq!(
        encode_v3_value_block(&schema, &batch, limits),
        Err(AuraError::InvalidValue("v3 variable value length"))
    );
    let rows_limit = V3ValueLimits {
        max_rows: 0,
        ..V3ValueLimits::default()
    };
    assert_eq!(
        validate_v3_batch(&schema, &batch, rows_limit),
        Err(AuraError::InvalidValue("v3 value row count"))
    );
}

#[test]
fn decoder_rejects_header_type_flag_length_trailing_and_truncation_corruption() {
    let schema = SchemaBuilder::new("corruption")
        .v3()
        .field("value", FieldType::I64, FieldRole::Value)
        .finish()
        .unwrap();
    let batch = AuraV3Batch {
        schema_id: schema.schema_id,
        row_count: 1,
        columns: vec![AuraV3Column {
            slot: 0,
            validity: None,
            values: Values::I64(vec![9]),
        }],
    };
    let encoded = encode_v3_value_block(&schema, &batch, V3ValueLimits::default()).unwrap();

    let mut cases = Vec::new();
    let mut magic = encoded.clone();
    magic[0] = 0;
    cases.push(magic);
    let mut version = encoded.clone();
    version[8..10].copy_from_slice(&2u16.to_le_bytes());
    cases.push(version);
    let mut header_flags = encoded.clone();
    header_flags[10] = 1;
    cases.push(header_flags);
    let mut schema_id = encoded.clone();
    schema_id[12] ^= 1;
    cases.push(schema_id);
    let mut columns = encoded.clone();
    columns[52..56].copy_from_slice(&2u32.to_le_bytes());
    cases.push(columns);
    let mut slot = encoded.clone();
    slot[64..66].copy_from_slice(&1u16.to_le_bytes());
    cases.push(slot);
    let mut ty = encoded.clone();
    ty[66] = FieldType::U64 as u8;
    cases.push(ty);
    let mut unknown_ty = encoded.clone();
    unknown_ty[66] = 255;
    cases.push(unknown_ty);
    let mut flags = encoded.clone();
    flags[67] = 0x80;
    cases.push(flags);
    let mut fixed_len = encoded.clone();
    fixed_len[72..76].copy_from_slice(&7u32.to_le_bytes());
    cases.push(fixed_len);
    let mut declared = encoded.clone();
    declared[56..64].copy_from_slice(&1u64.to_le_bytes());
    cases.push(declared);
    let mut fingerprint = encoded.clone();
    fingerprint[16] ^= 1;
    cases.push(fingerprint);
    let mut trailing = encoded.clone();
    trailing.push(0);
    cases.push(trailing);
    cases.push(encoded[..encoded.len() - 1].to_vec());
    for corrupted in cases {
        assert!(decode_v3_value_block(&schema, &corrupted, V3ValueLimits::default()).is_err());
    }
    assert_eq!(
        decode_v3_value_block(&schema, &[], V3ValueLimits::default()),
        Err(AuraError::UnexpectedEof)
    );
}

#[test]
fn decoder_rejects_variable_offset_utf8_decimal_and_null_corruption() {
    let schema = SchemaBuilder::new("variable_corruption")
        .v3()
        .nullable_field("decimal", FieldType::DecimalText, FieldRole::Price)
        .finish()
        .unwrap();
    let batch = AuraV3Batch {
        schema_id: schema.schema_id,
        row_count: 2,
        columns: vec![AuraV3Column {
            slot: 0,
            validity: Some(vec![0b01]),
            values: Values::DecimalText(variable(&[b"1.0", b""])),
        }],
    };
    let encoded = encode_v3_value_block(&schema, &batch, V3ValueLimits::default()).unwrap();
    // Planes begin after 64-byte block header + 20-byte column header.
    let validity_at = 84;
    let offsets_at = 85;
    let data_at = offsets_at + 12;
    let mut padding = encoded.clone();
    padding[validity_at] |= 0x80;
    let mut first_offset = encoded.clone();
    first_offset[offsets_at] = 1;
    let mut decreasing = encoded.clone();
    decreasing[offsets_at + 4..offsets_at + 8].copy_from_slice(&4u32.to_le_bytes());
    let mut final_offset = encoded.clone();
    final_offset[offsets_at + 8..offsets_at + 12].copy_from_slice(&2u32.to_le_bytes());
    let mut null_nonempty = encoded.clone();
    null_nonempty[offsets_at + 4..offsets_at + 8].copy_from_slice(&2u32.to_le_bytes());
    let mut invalid_utf8 = encoded.clone();
    invalid_utf8[data_at] = 0xff;
    let mut invalid_decimal = encoded.clone();
    invalid_decimal[data_at] = b'e';
    for corrupted in [
        padding,
        first_offset,
        decreasing,
        final_offset,
        null_nonempty,
        invalid_utf8,
        invalid_decimal,
    ] {
        assert!(decode_v3_value_block(&schema, &corrupted, V3ValueLimits::default()).is_err());
    }
}

#[test]
fn exact_field_constraints_and_derived_exclusion_are_enforced() {
    let bad_timestamp_role = SchemaBuilder::new("bad_ts_role")
        .v3()
        .field("ts", FieldType::TimestampMs, FieldRole::Value)
        .finish();
    assert_eq!(
        bad_timestamp_role,
        Err(AuraError::InvalidValue("timestamp_ms field"))
    );

    let bad_text_candidates = SchemaBuilder::new("bad_text_candidates")
        .v3()
        .field_with_candidates(
            "text",
            FieldType::Utf8,
            FieldRole::Identifier,
            TransformCandidates::empty()
                .with(FieldTransform::Absolute)
                .with(FieldTransform::Bitpack),
        )
        .finish();
    assert_eq!(
        bad_text_candidates,
        Err(AuraError::InvalidValue("exact text field"))
    );

    let text = SchemaBuilder::new("derived_text")
        .v3()
        .field("text", FieldType::DecimalText, FieldRole::Value)
        .field("number", FieldType::I64, FieldRole::Value)
        .finish()
        .unwrap();
    let derived = text.with_derived_expressions(vec![DerivedExpression::new(
        1,
        1,
        DerivedExpressionOp::Add,
        vec![0],
    )
    .unwrap()]);
    assert_eq!(
        derived,
        Err(AuraError::InvalidValue("exact value derived expression"))
    );

    let bad_opaque_candidates = SchemaBuilder::new("bad_opaque_candidates")
        .v3()
        .field_with_candidates(
            "opaque",
            FieldType::Opaque16,
            FieldRole::Identifier,
            TransformCandidates::empty()
                .with(FieldTransform::Absolute)
                .with(FieldTransform::Bitpack),
        )
        .finish();
    assert_eq!(
        bad_opaque_candidates,
        Err(AuraError::InvalidValue("opaque16 field"))
    );

    let bad_opaque_scale = SchemaBuilder::new("bad_opaque_scale")
        .v3()
        .field("opaque", FieldType::Opaque16, FieldRole::Identifier)
        .finish()
        .unwrap()
        .with_field_scales(vec![1])
        .unwrap();
    assert_eq!(
        bad_opaque_scale.validate(),
        Err(AuraError::InvalidValue("opaque16 field"))
    );

    let opaque_input = SchemaBuilder::new("opaque_derived")
        .v3()
        .field("opaque", FieldType::Opaque16, FieldRole::Identifier)
        .field("number", FieldType::I64, FieldRole::Value)
        .finish()
        .unwrap()
        .with_derived_expressions(vec![DerivedExpression::new(
            1,
            1,
            DerivedExpressionOp::Add,
            vec![0],
        )
        .unwrap()]);
    assert_eq!(
        opaque_input,
        Err(AuraError::InvalidValue("exact value derived expression"))
    );
}

#[test]
fn stale_schema_identity_is_rejected_for_every_identity_bearing_section() {
    let schema = SchemaBuilder::new("identity")
        .field("base", FieldType::I64, FieldRole::Value)
        .field("derived", FieldType::I64, FieldRole::Value)
        .repeated_field("child", FieldType::I64, FieldRole::Value)
        .finish()
        .unwrap()
        .with_derived_expressions(vec![DerivedExpression::with_literals(
            1,
            1,
            DerivedExpressionOp::Add,
            vec![0],
            vec![1],
            0,
        )
        .unwrap()])
        .unwrap()
        .with_v3_groups(vec![GroupDescriptor::repeated(
            7,
            vec![2],
            RelationshipPermissions::none(),
        )])
        .unwrap();
    let batch = AuraV3Batch {
        schema_id: schema.schema_id,
        row_count: 1,
        columns: (0..3)
            .map(|slot| AuraV3Column {
                slot,
                validity: None,
                values: Values::I64(vec![i64::from(slot)]),
            })
            .collect(),
    };

    let mut mutations = Vec::new();
    let mut field = schema.clone();
    field.fields[0].field_type = FieldType::U64;
    mutations.push(field);
    let mut mapping = schema.clone();
    mapping.compact_schema_map.as_mut().unwrap()[0] = 255;
    mutations.push(mapping);
    let mut expression = schema.clone();
    expression.derived_expressions[0].literals[0] = 2;
    mutations.push(expression);
    let mut group = schema.clone();
    group.groups[0].relationships = RelationshipPermissions::none().with_split();
    mutations.push(group);

    for stale in mutations {
        assert_eq!(
            validate_v3_batch(&stale, &batch, V3ValueLimits::default()),
            Err(AuraError::InvalidValue("schema id"))
        );
    }
}

#[test]
fn reference_block_v1_rejects_grouped_or_repeated_schemas_instead_of_flattening() {
    let schema = SchemaBuilder::new("grouped")
        .v3()
        .field("event", FieldType::I64, FieldRole::Value)
        .repeated_field("child", FieldType::I64, FieldRole::Value)
        .repeated_group(1, vec![1], RelationshipPermissions::none())
        .finish()
        .unwrap();
    let batch = AuraV3Batch {
        schema_id: schema.schema_id,
        row_count: 1,
        columns: vec![
            AuraV3Column {
                slot: 0,
                validity: None,
                values: Values::I64(vec![1]),
            },
            AuraV3Column {
                slot: 1,
                validity: None,
                values: Values::I64(vec![2]),
            },
        ],
    };
    assert_eq!(
        validate_v3_batch(&schema, &batch, V3ValueLimits::default()),
        Err(AuraError::InvalidValue("v3 value repeated schema"))
    );
}

#[test]
fn zero_rows_round_trip_fixed_nullable_and_variable_columns() {
    let schema = SchemaBuilder::new("zero_rows")
        .v3()
        .field("required", FieldType::I64, FieldRole::Value)
        .nullable_field("optional", FieldType::U8, FieldRole::Value)
        .nullable_field("text", FieldType::Utf8, FieldRole::Value)
        .finish()
        .unwrap();
    let batch = AuraV3Batch {
        schema_id: schema.schema_id,
        row_count: 0,
        columns: vec![
            AuraV3Column {
                slot: 0,
                validity: None,
                values: Values::I64(vec![]),
            },
            AuraV3Column {
                slot: 1,
                validity: Some(vec![]),
                values: Values::U8(vec![]),
            },
            AuraV3Column {
                slot: 2,
                validity: Some(vec![]),
                values: Values::Utf8(AuraV3VariableColumn {
                    offsets: vec![0],
                    data: vec![],
                }),
            },
        ],
    };
    let bytes = encode_v3_value_block(&schema, &batch, V3ValueLimits::default()).unwrap();
    assert_eq!(
        batch,
        decode_v3_value_block(&schema, &bytes, V3ValueLimits::default()).unwrap()
    );
    assert_eq!(
        batch.columns[0].value_ref(0),
        Err(AuraError::InvalidValue("v3 value row index"))
    );
}

#[test]
fn validity_is_lsb_first_at_byte_boundaries() {
    let schema = SchemaBuilder::new("bitmap")
        .v3()
        .nullable_field("value", FieldType::U8, FieldRole::Value)
        .finish()
        .unwrap();
    for rows in [7usize, 8, 15, 16] {
        let mut bitmap = vec![0; rows.div_ceil(8)];
        bitmap[(rows - 1) / 8] |= 1 << ((rows - 1) % 8);
        let mut values = vec![0; rows];
        values[rows - 1] = 1;
        let batch = AuraV3Batch {
            schema_id: schema.schema_id,
            row_count: rows as u32,
            columns: vec![AuraV3Column {
                slot: 0,
                validity: Some(bitmap.clone()),
                values: Values::U8(values),
            }],
        };
        let bytes = encode_v3_value_block(&schema, &batch, V3ValueLimits::default()).unwrap();
        assert_eq!(
            batch,
            decode_v3_value_block(&schema, &bytes, V3ValueLimits::default()).unwrap()
        );
        assert_eq!(
            Some(aura_codec::AuraV3ValueRef::U8(1)),
            batch.columns[0].value_ref(rows - 1).unwrap()
        );
        assert_eq!(
            batch.columns[0].value_ref(rows),
            Err(AuraError::InvalidValue("v3 value row index"))
        );
        if rows % 8 != 0 {
            let mut bad_padding = batch.clone();
            *bad_padding.columns[0]
                .validity
                .as_mut()
                .unwrap()
                .last_mut()
                .unwrap() |= 0x80;
            assert_eq!(
                validate_v3_batch(&schema, &bad_padding, V3ValueLimits::default()),
                Err(AuraError::InvalidValue("v3 value validity padding"))
            );
        }
    }
}

fn one_text_batch(schema_id: u32, present: bool, value: &[u8]) -> AuraV3Batch {
    AuraV3Batch {
        schema_id,
        row_count: 1,
        columns: vec![AuraV3Column {
            slot: 0,
            validity: Some(vec![u8::from(present)]),
            values: Values::DecimalText(variable(&[value])),
        }],
    }
}

fn one_utf8_batch(schema_id: u32, present: bool, value: &[u8]) -> AuraV3Batch {
    AuraV3Batch {
        schema_id,
        row_count: 1,
        columns: vec![AuraV3Column {
            slot: 0,
            validity: Some(vec![u8::from(present)]),
            values: Values::Utf8(variable(&[value])),
        }],
    }
}

#[test]
fn variable_hash_distinguishes_null_empty_and_exact_decimal_spellings() {
    let utf8_schema = SchemaBuilder::new("utf8_hash")
        .v3()
        .nullable_field("value", FieldType::Utf8, FieldRole::Value)
        .finish()
        .unwrap();
    let utf8_null = one_utf8_batch(utf8_schema.schema_id, false, b"");
    let utf8_empty = one_utf8_batch(utf8_schema.schema_id, true, b"");
    assert_ne!(
        canonical_v3_batch_sha256(&utf8_schema, &utf8_null, V3ValueLimits::default()).unwrap(),
        canonical_v3_batch_sha256(&utf8_schema, &utf8_empty, V3ValueLimits::default()).unwrap()
    );

    let schema = SchemaBuilder::new("decimal_hash")
        .v3()
        .nullable_field("value", FieldType::DecimalText, FieldRole::Value)
        .finish()
        .unwrap();
    let null = one_text_batch(schema.schema_id, false, b"");
    let null_hash = canonical_v3_batch_sha256(&schema, &null, V3ValueLimits::default()).unwrap();
    let spellings = [b"0".as_slice(), b"00", b"+0", b"-0", b" 0 "];
    let hashes = spellings
        .iter()
        .map(|value| {
            canonical_v3_batch_sha256(
                &schema,
                &one_text_batch(schema.schema_id, true, value),
                V3ValueLimits::default(),
            )
            .unwrap()
        })
        .collect::<Vec<_>>();
    assert!(hashes.iter().all(|hash| *hash != null_hash));
    for left in 0..hashes.len() {
        for right in left + 1..hashes.len() {
            assert_ne!(hashes[left], hashes[right]);
        }
    }

    let all_null = AuraV3Batch {
        schema_id: schema.schema_id,
        row_count: 4,
        columns: vec![AuraV3Column {
            slot: 0,
            validity: Some(vec![0]),
            values: Values::DecimalText(AuraV3VariableColumn {
                offsets: vec![0; 5],
                data: vec![],
            }),
        }],
    };
    assert!(canonical_v3_batch_sha256(&schema, &all_null, V3ValueLimits::default()).is_ok());
}

#[test]
fn exact_limits_and_tiny_overflow_mutations_fail_before_allocation() {
    let schema = SchemaBuilder::new("limits")
        .v3()
        .field("text", FieldType::Utf8, FieldRole::Value)
        .finish()
        .unwrap();
    let batch = AuraV3Batch {
        schema_id: schema.schema_id,
        row_count: 1,
        columns: vec![AuraV3Column {
            slot: 0,
            validity: None,
            values: Values::Utf8(variable(&[b"z"])),
        }],
    };
    let bytes = encode_v3_value_block(&schema, &batch, V3ValueLimits::default()).unwrap();
    let exact = V3ValueLimits {
        max_block_bytes: bytes.len(),
        ..V3ValueLimits::default()
    };
    assert_eq!(
        bytes,
        encode_v3_value_block(&schema, &batch, exact).unwrap()
    );
    assert!(decode_v3_value_block(&schema, &bytes, exact).is_ok());
    let one_short = V3ValueLimits {
        max_block_bytes: bytes.len() - 1,
        ..V3ValueLimits::default()
    };
    assert_eq!(
        decode_v3_value_block(&schema, &bytes, one_short),
        Err(AuraError::InvalidValue("v3 value block length"))
    );
    let mut stale_schema = schema.clone();
    stale_schema.name.push_str("_stale");
    assert_eq!(
        decode_v3_value_block(&stale_schema, &bytes, one_short),
        Err(AuraError::InvalidValue("v3 value block length"))
    );

    let too_many_rows = AuraV3Batch {
        schema_id: schema.schema_id,
        row_count: (MAX_V3_VALUE_ROWS + 1) as u32,
        columns: batch.columns.clone(),
    };
    assert_eq!(
        validate_v3_batch(&schema, &too_many_rows, V3ValueLimits::default()),
        Err(AuraError::InvalidValue("v3 value row count"))
    );
    for range in [72..76, 76..80, 80..84] {
        let mut corrupted = bytes.clone();
        corrupted[range].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(decode_v3_value_block(&schema, &corrupted, V3ValueLimits::default()).is_err());
    }
    let mut total_overflow = bytes.clone();
    total_overflow[56..64].copy_from_slice(&u64::MAX.to_le_bytes());
    assert_eq!(
        decode_v3_value_block(&schema, &total_overflow, V3ValueLimits::default()),
        Err(AuraError::InvalidValue("v3 value block length"))
    );
}

#[test]
fn variable_source_order_and_schema_fingerprint_are_exact() {
    let schema = SchemaBuilder::new("source_order")
        .v3()
        .field("text", FieldType::Utf8, FieldRole::Value)
        .finish()
        .unwrap();
    let batch = AuraV3Batch {
        schema_id: schema.schema_id,
        row_count: 4,
        columns: vec![AuraV3Column {
            slot: 0,
            validity: None,
            values: Values::Utf8(variable(&[b"z", b"a", b"z", b""])),
        }],
    };
    let fingerprint = canonical_v3_schema_fingerprint(&schema).unwrap();
    let bytes = encode_v3_value_block(&schema, &batch, V3ValueLimits::default()).unwrap();
    assert_eq!(&fingerprint, &bytes[16..48]);
    assert_eq!(
        batch,
        decode_v3_value_block(&schema, &bytes, V3ValueLimits::default()).unwrap()
    );
}

#[test]
fn reference_block_is_rejected_by_container_and_v2_decoders() {
    let schema = SchemaBuilder::new("not_a_container")
        .v3()
        .field("value", FieldType::I64, FieldRole::Value)
        .finish()
        .unwrap();
    let batch = AuraV3Batch {
        schema_id: schema.schema_id,
        row_count: 1,
        columns: vec![AuraV3Column {
            slot: 0,
            validity: None,
            values: Values::I64(vec![1]),
        }],
    };
    let bytes = encode_v3_value_block(&schema, &batch, V3ValueLimits::default()).unwrap();
    assert!(AuraHeader::decode(&bytes).is_err());
    assert!(AuraFooter::decode(&bytes).is_err());
    assert!(records::decode_i64_file(&bytes).is_err());
    assert!(records::decode_typed_file(&bytes).is_err());
}

const V3_FIXTURE_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/v3");

fn golden_v3_fixture() -> (aura_codec::SchemaDescriptor, AuraV3Batch) {
    let schema = SchemaBuilder::new("source_order")
        .v3()
        .field("text", FieldType::Utf8, FieldRole::Value)
        .finish()
        .unwrap();
    let batch = AuraV3Batch {
        schema_id: schema.schema_id,
        row_count: 4,
        columns: vec![AuraV3Column {
            slot: 0,
            validity: None,
            values: Values::Utf8(variable(&[b"z", b"a", b"z", b""])),
        }],
    };
    (schema, batch)
}

fn hex_bytes(value: &str) -> Vec<u8> {
    let value = value.trim_end();
    assert!(value.len().is_multiple_of(2));
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

fn sha256_hex(bytes: &[u8]) -> String {
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn hex_string(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[test]
fn versioned_v3_value_fixture_is_exact_and_deterministic() {
    let (schema, batch) = golden_v3_fixture();
    let schema_json =
        std::fs::read_to_string(format!("{V3_FIXTURE_DIR}/flat-source-order.schema.json")).unwrap();
    assert_eq!(schema.to_canonical_json().unwrap(), schema_json);
    assert_eq!(schema, aura_codec::parse_schema_json(&schema_json).unwrap());

    let block_hex_text =
        std::fs::read_to_string(format!("{V3_FIXTURE_DIR}/flat-source-order.aurav3vb.hex"))
            .unwrap();
    let fixture_bytes = hex_bytes(&block_hex_text);
    let encoded = encode_v3_value_block(&schema, &batch, V3ValueLimits::default()).unwrap();
    assert_eq!(encoded, fixture_bytes);
    assert_eq!(
        batch,
        decode_v3_value_block(&schema, &fixture_bytes, V3ValueLimits::default()).unwrap()
    );

    let manifest: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(format!("{V3_FIXTURE_DIR}/manifest.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(Some(false), manifest["complete_aura_file"].as_bool());
    assert_eq!(
        Some(sha256_hex(schema_json.as_bytes()).as_str()),
        manifest["schema_json_sha256"].as_str()
    );
    assert_eq!(
        Some(sha256_hex(block_hex_text.as_bytes()).as_str()),
        manifest["block_hex_file_sha256"].as_str()
    );
    assert_eq!(
        Some(u64::from(batch.row_count)),
        manifest["row_count"].as_u64()
    );
    assert_eq!(
        Some(fixture_bytes.len() as u64),
        manifest["block_bytes"].as_u64()
    );
    assert_eq!(
        Some(sha256_hex(&fixture_bytes).as_str()),
        manifest["block_sha256"].as_str()
    );
    let logical = canonical_v3_batch_sha256(&schema, &batch, V3ValueLimits::default()).unwrap();
    assert_eq!(
        Some(hex_string(&logical).as_str()),
        manifest["logical_sha256"].as_str()
    );
    let fingerprint = canonical_v3_schema_fingerprint(&schema).unwrap();
    assert_eq!(
        Some(hex_string(&fingerprint).as_str()),
        manifest["schema_fingerprint_sha256"].as_str()
    );
}

#[test]
#[ignore = "fixture generation is an explicit V3 reference-block maintenance action"]
fn generate_versioned_v3_value_fixture() {
    let (schema, batch) = golden_v3_fixture();
    let bytes = encode_v3_value_block(&schema, &batch, V3ValueLimits::default()).unwrap();
    let logical = canonical_v3_batch_sha256(&schema, &batch, V3ValueLimits::default()).unwrap();
    let fingerprint = canonical_v3_schema_fingerprint(&schema).unwrap();
    let schema_json = schema.to_canonical_json().unwrap();
    let block_hex = format!("{}\n", hex_string(&bytes));
    std::fs::create_dir_all(V3_FIXTURE_DIR).unwrap();
    std::fs::write(
        format!("{V3_FIXTURE_DIR}/flat-source-order.schema.json"),
        &schema_json,
    )
    .unwrap();
    std::fs::write(
        format!("{V3_FIXTURE_DIR}/flat-source-order.aurav3vb.hex"),
        &block_hex,
    )
    .unwrap();
    let manifest = serde_json::json!({
        "fixture_version": 1,
        "artifact_kind": "standalone-aura-v3-value-block-v1",
        "complete_aura_file": false,
        "schema": "flat-source-order.schema.json",
        "block_hex": "flat-source-order.aurav3vb.hex",
        "schema_json_sha256": sha256_hex(schema_json.as_bytes()),
        "block_hex_file_sha256": sha256_hex(block_hex.as_bytes()),
        "schema_fingerprint_sha256": hex_string(&fingerprint),
        "block_sha256": sha256_hex(&bytes),
        "logical_sha256": hex_string(&logical),
        "row_count": batch.row_count,
        "block_bytes": bytes.len(),
    });
    std::fs::write(
        format!("{V3_FIXTURE_DIR}/manifest.json"),
        format!("{}\n", serde_json::to_string_pretty(&manifest).unwrap()),
    )
    .unwrap();
}
