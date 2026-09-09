use aura_codec::experimental::{
    canonical_v3_batch_sha256, canonical_v3_schema_fingerprint, decode_v3_flat_aura0,
    decode_v3_flat_aura0_with_limits, decode_v3_flat_footer, encode_v3_flat_footer,
    encode_v3_value_block, v3_flat_body_sha256, v3_flat_header_sha256, AnyCompiledFooter,
    AuraV3Batch, AuraV3Column, AuraV3ColumnValues as Values, AuraV3VariableColumn,
    CanonicalV3RowHasher, V3FlatChunkDescriptor, V3FlatColumnStats, V3FlatFooter, V3ValueLimits,
    V3_FLAT_FOOTER_PREFIX_BYTES,
};
use aura_codec::{
    AuraContainerVersion, AuraError, AuraHeader, FieldRole, FieldType, Profile, SchemaBuilder,
    V3FlatLimits,
};
use sha2::{Digest, Sha256};

const FOOTER_DOMAIN: &[u8] = b"aura-v3-flat-aura0-footer-v1\0";

fn variable(parts: &[&[u8]]) -> AuraV3VariableColumn {
    let mut offsets = vec![0];
    let mut data = Vec::new();
    for part in parts {
        data.extend_from_slice(part);
        offsets.push(data.len() as u32);
    }
    AuraV3VariableColumn { offsets, data }
}

fn schema() -> aura_codec::SchemaDescriptor {
    SchemaBuilder::new("anonymous_flat_events")
        .v3()
        .field("ts", FieldType::TimestampMs, FieldRole::Timestamp)
        .nullable_field("seq", FieldType::U64, FieldRole::Sequence)
        .nullable_field("is_sell", FieldType::U8, FieldRole::Boolean)
        .field("symbol", FieldType::Utf8, FieldRole::Identifier)
        .nullable_field("price", FieldType::DecimalText, FieldRole::Price)
        .finish()
        .unwrap()
}

fn batches(schema_id: u32) -> Vec<AuraV3Batch> {
    vec![
        AuraV3Batch {
            schema_id,
            row_count: 2,
            columns: vec![
                AuraV3Column {
                    slot: 0,
                    validity: None,
                    values: Values::TimestampMs(vec![0, i64::MAX]),
                },
                AuraV3Column {
                    slot: 1,
                    validity: Some(vec![0b10]),
                    values: Values::U64(vec![0, u64::MAX]),
                },
                AuraV3Column {
                    slot: 2,
                    validity: Some(vec![0b10]),
                    values: Values::U8(vec![0, 1]),
                },
                AuraV3Column {
                    slot: 3,
                    validity: None,
                    values: Values::Utf8(variable(&[b"", "BTC-α\0".as_bytes()])),
                },
                AuraV3Column {
                    slot: 4,
                    validity: Some(vec![0b11]),
                    values: Values::DecimalText(variable(&[
                        b"0",
                        "\u{2003}+01.20\u{00a0}".as_bytes(),
                    ])),
                },
            ],
        },
        AuraV3Batch {
            schema_id,
            row_count: 1,
            columns: vec![
                AuraV3Column {
                    slot: 0,
                    validity: None,
                    values: Values::TimestampMs(vec![-1]),
                },
                AuraV3Column {
                    slot: 1,
                    validity: Some(vec![0b01]),
                    values: Values::U64(vec![0]),
                },
                AuraV3Column {
                    slot: 2,
                    validity: Some(vec![0b01]),
                    values: Values::U8(vec![0]),
                },
                AuraV3Column {
                    slot: 3,
                    validity: None,
                    values: Values::Utf8(variable(&[b"ETH"])),
                },
                AuraV3Column {
                    slot: 4,
                    validity: Some(vec![0]),
                    values: Values::DecimalText(variable(&[b""])),
                },
            ],
        },
    ]
}

fn stats() -> Vec<V3FlatColumnStats> {
    vec![
        stat(0, FieldType::TimestampMs, 4, 3, 0, 24, 8),
        stat(1, FieldType::U64, 5, 2, 1, 16, 8),
        stat(2, FieldType::U8, 5, 2, 1, 2, 1),
        stat(3, FieldType::Utf8, 2, 3, 0, 22, 7),
        stat(4, FieldType::DecimalText, 3, 2, 1, 20, 11),
    ]
}

fn stat(
    slot: u16,
    field_type: FieldType,
    flags: u8,
    present_count: u64,
    null_count: u64,
    logical_payload_bytes: u64,
    max_present_value_byte_len: u32,
) -> V3FlatColumnStats {
    V3FlatColumnStats {
        slot,
        field_type,
        flags,
        present_count,
        null_count,
        logical_payload_bytes,
        max_present_value_byte_len,
    }
}

fn build_file(empty: bool) -> (Vec<u8>, V3FlatFooter) {
    let schema = schema();
    let header = AuraHeader::new(Profile::Aura0)
        .with_container_version(AuraContainerVersion::V3)
        .with_schema_mapping(schema.compact_schema_map.clone().unwrap())
        .unwrap()
        .encode()
        .unwrap();
    let batches = if empty {
        Vec::new()
    } else {
        batches(schema.schema_id)
    };
    let record_count = batches.iter().map(|batch| batch.row_count).sum::<u32>();
    let mut global = CanonicalV3RowHasher::new(&schema, record_count, V3ValueLimits::HARD).unwrap();
    let mut body = Vec::new();
    let mut chunks = Vec::new();
    let mut first_global_row = 0u64;
    for (chunk_id, batch) in batches.iter().enumerate() {
        let block = encode_v3_value_block(&schema, batch, V3ValueLimits::HARD).unwrap();
        let offset = body.len() as u64;
        global.update_batch(&schema, batch).unwrap();
        let (first_timestamp, last_timestamp) = match &batch.columns[0].values {
            Values::TimestampMs(values) => (*values.first().unwrap(), *values.last().unwrap()),
            _ => unreachable!(),
        };
        let (sequence_flag, first_sequence, last_sequence) = match &batch.columns[1].values {
            Values::U64(values) => {
                let present = (0..batch.row_count as usize)
                    .filter(|row| batch.columns[1].value_ref(*row).unwrap().is_some())
                    .map(|row| values[row])
                    .collect::<Vec<_>>();
                if present.is_empty() {
                    (0, 0, 0)
                } else {
                    (2, present[0], *present.last().unwrap())
                }
            }
            _ => unreachable!(),
        };
        chunks.push(V3FlatChunkDescriptor {
            chunk_id: chunk_id as u32,
            flags: 1 | sequence_flag,
            first_global_row,
            row_count: batch.row_count,
            body_relative_offset: offset,
            stored_len: block.len() as u64,
            stored_sha256: Sha256::digest(&block).into(),
            chunk_logical_sha256: canonical_v3_batch_sha256(&schema, batch, V3ValueLimits::HARD)
                .unwrap(),
            first_timestamp,
            last_timestamp,
            first_sequence,
            last_sequence,
        });
        first_global_row += u64::from(batch.row_count);
        body.extend_from_slice(&block);
    }
    let footer = V3FlatFooter {
        record_count: u64::from(record_count),
        body_len: body.len() as u64,
        schema_fingerprint: canonical_v3_schema_fingerprint(&schema).unwrap(),
        header_sha256: v3_flat_header_sha256(&header).unwrap(),
        body_sha256: v3_flat_body_sha256(&body).unwrap(),
        global_logical_sha256: global.finalize().unwrap(),
        primary_timestamp_slot: 0,
        primary_sequence_slot: Some(1),
        stats: if empty {
            schema
                .fields
                .iter()
                .map(|field| {
                    let flags = match field.field_type {
                        FieldType::Utf8 | FieldType::DecimalText => 2 | u8::from(field.nullable),
                        FieldType::Opaque16 => u8::from(field.nullable),
                        _ => 4 | u8::from(field.nullable),
                    };
                    stat(field.index, field.field_type, flags, 0, 0, 0, 0)
                })
                .collect()
        } else {
            stats()
        },
        chunks,
        schema,
    };
    let footer_bytes = encode_v3_flat_footer(&footer).unwrap();
    let mut file = header;
    file.extend_from_slice(&body);
    file.extend_from_slice(&footer_bytes);
    file.extend_from_slice(&(footer_bytes.len() as u32).to_le_bytes());
    file.extend_from_slice(b"sealed:)");
    (file, footer)
}

fn build_non_time_file() -> (Vec<u8>, V3FlatFooter) {
    let schema = SchemaBuilder::new("non_time_flat_events")
        .v3()
        .field("sequence", FieldType::U64, FieldRole::Sequence)
        .finish()
        .unwrap();
    let header = AuraHeader::new(Profile::Aura0)
        .with_container_version(AuraContainerVersion::V3)
        .with_schema_mapping(schema.compact_schema_map.clone().unwrap())
        .unwrap()
        .encode()
        .unwrap();
    let batch = AuraV3Batch {
        schema_id: schema.schema_id,
        row_count: 1,
        columns: vec![AuraV3Column {
            slot: 0,
            validity: None,
            values: Values::U64(vec![42]),
        }],
    };
    let body = encode_v3_value_block(&schema, &batch, V3ValueLimits::HARD).unwrap();
    let mut global = CanonicalV3RowHasher::new(&schema, 1, V3ValueLimits::HARD).unwrap();
    global.update_batch(&schema, &batch).unwrap();
    let footer = V3FlatFooter {
        record_count: 1,
        body_len: body.len() as u64,
        schema_fingerprint: canonical_v3_schema_fingerprint(&schema).unwrap(),
        header_sha256: v3_flat_header_sha256(&header).unwrap(),
        body_sha256: v3_flat_body_sha256(&body).unwrap(),
        global_logical_sha256: global.finalize().unwrap(),
        primary_timestamp_slot: u16::MAX,
        primary_sequence_slot: Some(0),
        stats: vec![stat(0, FieldType::U64, 4, 1, 0, 8, 8)],
        chunks: vec![V3FlatChunkDescriptor {
            chunk_id: 0,
            flags: 2,
            first_global_row: 0,
            row_count: 1,
            body_relative_offset: 0,
            stored_len: body.len() as u64,
            stored_sha256: Sha256::digest(&body).into(),
            chunk_logical_sha256: canonical_v3_batch_sha256(&schema, &batch, V3ValueLimits::HARD)
                .unwrap(),
            first_timestamp: 0,
            last_timestamp: 0,
            first_sequence: 42,
            last_sequence: 42,
        }],
        schema,
    };
    let footer_bytes = encode_v3_flat_footer(&footer).unwrap();
    let mut file = header;
    file.extend_from_slice(&body);
    file.extend_from_slice(&footer_bytes);
    file.extend_from_slice(&(footer_bytes.len() as u32).to_le_bytes());
    file.extend_from_slice(b"sealed:)");
    (file, footer)
}

fn footer_range(file: &[u8]) -> std::ops::Range<usize> {
    let footer_len_offset = file.len() - 12;
    let footer_len = u32::from_le_bytes(
        file[footer_len_offset..footer_len_offset + 4]
            .try_into()
            .unwrap(),
    ) as usize;
    footer_len_offset - footer_len..footer_len_offset
}

fn resign_footer(file: &mut [u8]) {
    let range = footer_range(file);
    let hash_start = range.end - 32;
    let mut hash = Sha256::new();
    hash.update(FOOTER_DOMAIN);
    hash.update(((hash_start - range.start) as u64).to_le_bytes());
    hash.update(&file[range.start..hash_start]);
    let digest: [u8; 32] = hash.finalize().into();
    file[hash_start..range.end].copy_from_slice(&digest);
}

fn resign_standalone_footer(footer: &mut [u8]) {
    let hash_start = footer.len() - 32;
    let mut hash = Sha256::new();
    hash.update(FOOTER_DOMAIN);
    hash.update((hash_start as u64).to_le_bytes());
    hash.update(&footer[..hash_start]);
    let digest: [u8; 32] = hash.finalize().into();
    footer[hash_start..].copy_from_slice(&digest);
}

#[test]
fn exact_three_rows_two_chunks_round_trip_all_edge_semantics() {
    let (file, footer) = build_file(false);
    let decoded = decode_v3_flat_aura0(&file).unwrap();
    assert_eq!(footer, decoded.footer);
    assert_eq!(2, decoded.batches.len());
    assert_eq!(batches(footer.schema.schema_id), decoded.batches);
    assert_eq!(
        (0, i64::MAX),
        (
            footer.chunks[0].first_timestamp,
            footer.chunks[0].last_timestamp,
        )
    );
    assert_eq!(
        (-1, -1),
        (
            footer.chunks[1].first_timestamp,
            footer.chunks[1].last_timestamp,
        )
    );
    assert_eq!(
        (u64::MAX, u64::MAX),
        (
            footer.chunks[0].first_sequence,
            footer.chunks[0].last_sequence,
        )
    );
    assert_eq!(
        (0, 0),
        (
            footer.chunks[1].first_sequence,
            footer.chunks[1].last_sequence,
        )
    );
    assert_eq!(
        u64::MAX,
        match decoded.batches[0].columns[1].value_ref(1).unwrap().unwrap() {
            aura_codec::AuraV3ValueRef::U64(value) => value,
            _ => unreachable!(),
        }
    );
    assert_eq!(
        Some(aura_codec::AuraV3ValueRef::Utf8("")),
        decoded.batches[0].columns[3].value_ref(0).unwrap()
    );
    assert_eq!(
        Some(aura_codec::AuraV3ValueRef::Utf8("BTC-α\0")),
        decoded.batches[0].columns[3].value_ref(1).unwrap()
    );
    assert_eq!(
        Some(aura_codec::AuraV3ValueRef::DecimalText("0")),
        decoded.batches[0].columns[4].value_ref(0).unwrap()
    );
    assert_eq!(None, decoded.batches[1].columns[4].value_ref(0).unwrap());
    assert_eq!(
        footer,
        decode_v3_flat_footer(&encode_v3_flat_footer(&footer).unwrap()).unwrap()
    );
}

#[test]
fn zero_row_file_has_empty_body_chunks_and_one_zero_stat_per_field() {
    let (file, _) = build_file(true);
    let decoded = decode_v3_flat_aura0(&file).unwrap();
    assert!(decoded.batches.is_empty());
    assert_eq!(0, decoded.footer.body_len);
    assert!(decoded.footer.chunks.is_empty());
    assert_eq!(
        decoded.footer.schema.fields.len(),
        decoded.footer.stats.len()
    );
    assert!(decoded
        .footer
        .stats
        .iter()
        .all(|stat| stat.present_count == 0
            && stat.null_count == 0
            && stat.logical_payload_bytes == 0
            && stat.max_present_value_byte_len == 0));
}

#[test]
fn flat_schema_without_primary_timestamp_uses_no_slot_and_zero_bounds() {
    let (file, footer) = build_non_time_file();
    assert_eq!(u16::MAX, footer.primary_timestamp_slot);
    assert_eq!(2, footer.chunks[0].flags);
    let decoded = decode_v3_flat_aura0(&file).unwrap();
    assert_eq!(u16::MAX, decoded.footer.primary_timestamp_slot);
    assert_eq!(
        Some(aura_codec::AuraV3ValueRef::U64(42)),
        decoded.batches[0].columns[0].value_ref(0).unwrap()
    );
}

#[test]
fn caller_limits_accept_exact_boundaries_and_reject_plus_one_before_decode() {
    let (file, footer) = build_file(false);
    let footer_bytes = footer_range(&file).len();
    let max_block = footer
        .chunks
        .iter()
        .map(|chunk| chunk.stored_len)
        .max()
        .unwrap() as usize;
    let exact = V3FlatLimits {
        max_footer_bytes: footer_bytes,
        max_body_bytes: footer.body_len,
        max_chunks: footer.chunks.len(),
        max_rows: footer.record_count,
        value_limits: V3ValueLimits {
            max_block_bytes: max_block,
            ..V3ValueLimits::HARD
        },
    };
    decode_v3_flat_aura0_with_limits(&file, exact).unwrap();

    for lower in [
        V3FlatLimits {
            max_footer_bytes: footer_bytes - 1,
            ..exact
        },
        V3FlatLimits {
            max_body_bytes: footer.body_len - 1,
            ..exact
        },
        V3FlatLimits {
            max_chunks: footer.chunks.len() - 1,
            ..exact
        },
        V3FlatLimits {
            max_rows: footer.record_count - 1,
            ..exact
        },
        V3FlatLimits {
            value_limits: V3ValueLimits {
                max_block_bytes: max_block - 1,
                ..V3ValueLimits::HARD
            },
            ..exact
        },
    ] {
        assert!(decode_v3_flat_aura0_with_limits(&file, lower).is_err());
    }
}

#[test]
fn safe_defaults_are_lower_than_absolute_format_limits() {
    let limits = V3FlatLimits::default();
    assert_eq!(256 * 1024 * 1024, limits.max_body_bytes);
    assert_eq!(4_194_304, limits.max_rows);
    assert_eq!(4_096, limits.max_chunks);
    assert_eq!(256 * 1024 * 1024, limits.value_limits.max_block_bytes);
    assert_eq!(4_194_304, limits.value_limits.max_rows);
    assert!(limits.max_body_bytes < V3FlatLimits::HARD.max_body_bytes);
    assert!(limits.max_rows < V3FlatLimits::HARD.max_rows);
    assert!(limits.max_chunks < V3FlatLimits::HARD.max_chunks);
}

#[test]
fn footer_only_codec_rejects_impossible_stats_and_missing_required_bounds() {
    let (_, footer) = build_file(false);

    let mut bad = footer.clone();
    bad.stats[0].logical_payload_bytes -= 1;
    assert_eq!(
        encode_v3_flat_footer(&bad),
        Err(AuraError::InvalidValue("v3 flat stats"))
    );

    let mut bad = footer.clone();
    bad.stats[1].max_present_value_byte_len = 7;
    assert_eq!(
        encode_v3_flat_footer(&bad),
        Err(AuraError::InvalidValue("v3 flat stats"))
    );

    let mut bad = footer.clone();
    bad.stats[3].logical_payload_bytes = 12;
    assert_eq!(
        encode_v3_flat_footer(&bad),
        Err(AuraError::InvalidValue("v3 flat stats"))
    );

    let mut bad = footer.clone();
    bad.stats[3].logical_payload_bytes = 34;
    assert_eq!(
        encode_v3_flat_footer(&bad),
        Err(AuraError::InvalidValue("v3 flat stats"))
    );

    let encoded = encode_v3_flat_footer(&footer).unwrap();
    let mut signed_impossible = encoded.clone();
    let schema_len = 4 + u32::from_le_bytes(
        signed_impossible[V3_FLAT_FOOTER_PREFIX_BYTES..][..4]
            .try_into()
            .unwrap(),
    ) as usize;
    let stats_descriptors = V3_FLAT_FOOTER_PREFIX_BYTES + schema_len + 8;
    let symbol_payload = stats_descriptors + 3 * 36 + 20;
    signed_impossible[symbol_payload..symbol_payload + 8].copy_from_slice(&12u64.to_le_bytes());
    resign_standalone_footer(&mut signed_impossible);
    assert_eq!(
        decode_v3_flat_footer(&signed_impossible),
        Err(AuraError::InvalidValue("v3 flat stats"))
    );

    let variable_limited = V3FlatLimits {
        value_limits: V3ValueLimits {
            max_variable_value_bytes: 6,
            ..V3ValueLimits::HARD
        },
        ..V3FlatLimits::HARD
    };
    assert_eq!(
        aura_codec::experimental::decode_v3_flat_footer_with_limits(&encoded, variable_limited),
        Err(AuraError::InvalidValue("v3 flat stats"))
    );

    let mut bad = footer;
    bad.chunks[0].flags &= !1;
    bad.chunks[0].first_timestamp = 0;
    bad.chunks[0].last_timestamp = 0;
    assert_eq!(
        encode_v3_flat_footer(&bad),
        Err(AuraError::InvalidValue("v3 flat chunk descriptor"))
    );

    let (_, mut required_sequence) = build_non_time_file();
    required_sequence.chunks[0].flags = 0;
    required_sequence.chunks[0].first_sequence = 0;
    required_sequence.chunks[0].last_sequence = 0;
    assert_eq!(
        encode_v3_flat_footer(&required_sequence),
        Err(AuraError::InvalidValue("v3 flat chunk descriptor"))
    );
}

#[test]
fn every_proper_file_prefix_is_rejected_without_panicking() {
    let (file, _) = build_file(false);
    for end in 0..file.len() {
        assert!(decode_v3_flat_aura0(&file[..end]).is_err(), "prefix {end}");
    }
}

#[test]
fn every_single_byte_mutation_is_rejected_without_panicking() {
    let (file, _) = build_file(false);
    for offset in 0..file.len() {
        let mut bad = file.clone();
        bad[offset] ^= 1;
        assert!(decode_v3_flat_aura0(&bad).is_err(), "byte {offset}");
    }
}

#[test]
fn trailer_footer_header_body_chunk_stats_and_range_corruption_is_rejected() {
    let (file, _) = build_file(false);
    let mut bad = file.clone();
    *bad.last_mut().unwrap() ^= 1;
    assert!(decode_v3_flat_aura0(&bad).is_err());

    let mut bad = file.clone();
    let offset = bad.len() - 12;
    bad[offset..offset + 4].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(decode_v3_flat_aura0(&bad).is_err());

    let mut bad = file.clone();
    let footer = footer_range(&bad);
    bad[footer.start] ^= 1;
    assert!(decode_v3_flat_aura0(&bad).is_err());

    for offset in [
        0usize, 4, 6, 8, 9, 10, 11, 12, 20, 28, 32, 36, 40, 42, 44, 48, 80, 112, 144,
    ] {
        let mut bad = file.clone();
        let footer = footer_range(&bad);
        bad[footer.start + offset] ^= 1;
        resign_footer(&mut bad);
        assert!(
            decode_v3_flat_aura0(&bad).is_err(),
            "footer offset {offset}"
        );
    }

    let mut bad = file.clone();
    bad[7] ^= 1;
    assert!(decode_v3_flat_aura0(&bad).is_err());

    let header_len = AuraHeader::encoded_len(&file).unwrap();
    let mut bad = file.clone();
    bad[header_len + 70] ^= 1;
    assert!(decode_v3_flat_aura0(&bad).is_err());

    let footer = footer_range(&file);
    let schema_len = 4 + u32::from_le_bytes(
        file[footer.start + V3_FLAT_FOOTER_PREFIX_BYTES..][..4]
            .try_into()
            .unwrap(),
    ) as usize;
    let stats_start = footer.start + V3_FLAT_FOOTER_PREFIX_BYTES + schema_len + 8;
    let chunks_start = stats_start + 5 * 36 + 8;
    for offset in [
        footer.start + V3_FLAT_FOOTER_PREFIX_BYTES + 8,
        stats_start - 8,
        stats_start - 4,
        stats_start + 12,
        chunks_start - 8,
        chunks_start - 4,
        chunks_start + 8,
        chunks_start + 16,
        chunks_start + 24,
        chunks_start + 40,
        chunks_start + 72,
        chunks_start + 104,
        chunks_start + 120,
    ] {
        let mut bad = file.clone();
        bad[offset] ^= 1;
        resign_footer(&mut bad);
        assert!(
            decode_v3_flat_aura0(&bad).is_err(),
            "absolute offset {offset}"
        );
    }
}

#[test]
fn grouped_flag200_derived_and_repeated_schemas_are_rejected_by_footer_encoder() {
    let (_, mut footer) = build_file(true);
    footer.schema.groups = vec![aura_codec::GroupDescriptor::repeated(
        1,
        vec![1],
        aura_codec::RelationshipPermissions::none(),
    )];
    assert!(encode_v3_flat_footer(&footer).is_err());

    let (_, mut footer) = build_file(true);
    footer.schema.compact_schema_map.as_mut().unwrap()[1] = 200;
    assert!(encode_v3_flat_footer(&footer).is_err());

    let (_, mut footer) = build_file(true);
    footer.schema.fields[1].scope = aura_codec::FieldScope::Repeated;
    assert!(encode_v3_flat_footer(&footer).is_err());

    let (_, mut footer) = build_file(true);
    footer.schema.derived_expressions.push(
        aura_codec::DerivedExpression::new(1, 1, aura_codec::DerivedExpressionOp::Add, vec![1])
            .unwrap(),
    );
    assert!(encode_v3_flat_footer(&footer).is_err());
}

#[test]
fn any_compiled_footer_routes_v3_and_keeps_v2_fixture_behavior() {
    let (_, footer) = build_file(false);
    assert!(matches!(
        AnyCompiledFooter::decode(&encode_v3_flat_footer(&footer).unwrap()).unwrap(),
        AnyCompiledFooter::V3Flat(_)
    ));

    let v2_file = include_bytes!("fixtures/v2/plain-compact.aura0");
    let range = footer_range(v2_file);
    assert!(matches!(
        AnyCompiledFooter::decode(&v2_file[range]).unwrap(),
        AnyCompiledFooter::V2(_)
    ));
}

#[test]
fn checked_complete_file_fixtures_are_exact_and_decodable() {
    let (empty, _) = build_file(true);
    let (three_rows, footer) = build_file(false);
    assert_eq!(
        include_bytes!("fixtures/v3-container/empty-flat.aura0").as_slice(),
        empty
    );
    assert_eq!(
        include_bytes!("fixtures/v3-container/three-row-two-chunk.aura0").as_slice(),
        three_rows
    );
    assert_eq!(
        include_str!("fixtures/v3-container/anonymous.schema.json"),
        footer.schema.to_canonical_json().unwrap()
    );
    assert_eq!(
        0,
        decode_v3_flat_aura0(include_bytes!("fixtures/v3-container/empty-flat.aura0"))
            .unwrap()
            .footer
            .record_count
    );
    assert_eq!(
        3,
        decode_v3_flat_aura0(include_bytes!(
            "fixtures/v3-container/three-row-two-chunk.aura0"
        ))
        .unwrap()
        .footer
        .record_count
    );
    let manifest: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/v3-container/manifest.json")).unwrap();
    assert_eq!(manifest["schema"]["schema_id"], footer.schema.schema_id);
    assert_eq!(
        manifest["schema"]["fingerprint_sha256"],
        hex(&footer.schema_fingerprint)
    );
    let fixture = &manifest["fixtures"][1];
    assert_eq!(fixture["file_bytes"], three_rows.len());
    assert_eq!(fixture["file_sha256"], hex(&Sha256::digest(&three_rows)));
    assert_eq!(fixture["body_bytes"], footer.body_len);
    assert_eq!(fixture["footer_bytes"], footer_range(&three_rows).len());
    assert_eq!(
        fixture["global_logical_sha256"],
        hex(&footer.global_logical_sha256)
    );
    for (manifest_chunk, chunk) in fixture["chunks"]
        .as_array()
        .unwrap()
        .iter()
        .zip(&footer.chunks)
    {
        assert_eq!(manifest_chunk["chunk_id"], chunk.chunk_id);
        assert_eq!(manifest_chunk["first_global_row"], chunk.first_global_row);
        assert_eq!(manifest_chunk["row_count"], chunk.row_count);
        assert_eq!(
            manifest_chunk["body_relative_offset"],
            chunk.body_relative_offset
        );
        assert_eq!(manifest_chunk["stored_len"], chunk.stored_len);
        assert_eq!(manifest_chunk["flags"], chunk.flags);
        assert_eq!(manifest_chunk["first_timestamp"], chunk.first_timestamp);
        assert_eq!(manifest_chunk["last_timestamp"], chunk.last_timestamp);
        assert_eq!(manifest_chunk["first_sequence"], chunk.first_sequence);
        assert_eq!(manifest_chunk["last_sequence"], chunk.last_sequence);
        assert_eq!(manifest_chunk["stored_sha256"], hex(&chunk.stored_sha256));
        assert_eq!(
            manifest_chunk["logical_sha256"],
            hex(&chunk.chunk_logical_sha256)
        );
    }
}

#[test]
fn incremental_global_hash_is_identical_to_one_batch_contract() {
    let schema = schema();
    let chunks = batches(schema.schema_id);
    let mut incremental = CanonicalV3RowHasher::new(&schema, 3, V3ValueLimits::HARD).unwrap();
    for batch in &chunks {
        incremental.update_batch(&schema, batch).unwrap();
    }
    let all = AuraV3Batch {
        schema_id: schema.schema_id,
        row_count: 3,
        columns: (0..schema.fields.len())
            .map(|slot| join_columns(&chunks[0].columns[slot], &chunks[1].columns[slot]))
            .collect(),
    };
    assert_eq!(
        canonical_v3_batch_sha256(&schema, &all, V3ValueLimits::HARD).unwrap(),
        incremental.finalize().unwrap()
    );
    let mut incomplete = CanonicalV3RowHasher::new(&schema, 3, V3ValueLimits::HARD).unwrap();
    incomplete.update_batch(&schema, &chunks[0]).unwrap();
    assert_eq!(
        incomplete.finalize(),
        Err(AuraError::InvalidValue("v3 canonical hash row count"))
    );
}

fn join_columns(left: &AuraV3Column, right: &AuraV3Column) -> AuraV3Column {
    let validity = match (&left.validity, &right.validity) {
        (Some(a), Some(b)) => Some(vec![(a[0] & 0b11) | ((b[0] & 1) << 2)]),
        (None, None) => None,
        _ => unreachable!(),
    };
    let values = match (&left.values, &right.values) {
        (Values::TimestampMs(a), Values::TimestampMs(b)) => {
            Values::TimestampMs([a.as_slice(), b.as_slice()].concat())
        }
        (Values::U64(a), Values::U64(b)) => Values::U64([a.as_slice(), b.as_slice()].concat()),
        (Values::U8(a), Values::U8(b)) => Values::U8([a.as_slice(), b.as_slice()].concat()),
        (Values::Utf8(a), Values::Utf8(b)) => Values::Utf8(join_variable(a, b)),
        (Values::DecimalText(a), Values::DecimalText(b)) => {
            Values::DecimalText(join_variable(a, b))
        }
        _ => unreachable!(),
    };
    AuraV3Column {
        slot: left.slot,
        validity,
        values,
    }
}

fn join_variable(
    left: &AuraV3VariableColumn,
    right: &AuraV3VariableColumn,
) -> AuraV3VariableColumn {
    let mut data = left.data.clone();
    data.extend_from_slice(&right.data);
    let mut offsets = left.offsets.clone();
    offsets.extend(
        right
            .offsets
            .iter()
            .skip(1)
            .map(|offset| left.data.len() as u32 + offset),
    );
    AuraV3VariableColumn { offsets, data }
}

#[test]
#[ignore = "maintenance-only deterministic fixture regeneration"]
fn regenerate_checked_v3_container_fixtures() {
    let directory = std::path::Path::new("tests/fixtures/v3-container");
    std::fs::create_dir_all(directory).unwrap();
    let (empty, empty_footer) = build_file(true);
    let (three_rows, footer) = build_file(false);
    std::fs::write(directory.join("empty-flat.aura0"), &empty).unwrap();
    std::fs::write(directory.join("three-row-two-chunk.aura0"), &three_rows).unwrap();
    let schema_json = footer.schema.to_canonical_json().unwrap();
    std::fs::write(directory.join("anonymous.schema.json"), &schema_json).unwrap();
    let manifest = format!(
        "{{\n  \"version\": 1,\n  \"schema\": {{\n    \"file\": \"anonymous.schema.json\",\n    \"schema_id\": {},\n    \"fingerprint_sha256\": \"{}\",\n    \"bytes\": {},\n    \"sha256\": \"{}\"\n  }},\n  \"fixtures\": [\n{},\n{}\n  ]\n}}\n",
        footer.schema.schema_id,
        hex(&footer.schema_fingerprint),
        schema_json.len(),
        hex(&Sha256::digest(&schema_json)),
        fixture_manifest_entry("empty-flat.aura0", &empty, &empty_footer),
        fixture_manifest_entry("three-row-two-chunk.aura0", &three_rows, &footer),
    );
    std::fs::write(directory.join("manifest.json"), manifest).unwrap();
}

fn fixture_manifest_entry(name: &str, file: &[u8], footer: &V3FlatFooter) -> String {
    let header_len = AuraHeader::encoded_len(file).unwrap();
    let footer_bytes = footer_range(file);
    let body = &file[header_len..footer_bytes.start];
    let chunks = footer
        .chunks
        .iter()
        .map(|chunk| {
            format!(
                "        {{\"chunk_id\": {}, \"flags\": {}, \"first_global_row\": {}, \"row_count\": {}, \"body_relative_offset\": {}, \"stored_len\": {}, \"stored_sha256\": \"{}\", \"logical_sha256\": \"{}\", \"first_timestamp\": {}, \"last_timestamp\": {}, \"first_sequence\": {}, \"last_sequence\": {}}}",
                chunk.chunk_id,
                chunk.flags,
                chunk.first_global_row,
                chunk.row_count,
                chunk.body_relative_offset,
                chunk.stored_len,
                hex(&chunk.stored_sha256),
                hex(&chunk.chunk_logical_sha256),
                chunk.first_timestamp,
                chunk.last_timestamp,
                chunk.first_sequence,
                chunk.last_sequence,
            )
        })
        .collect::<Vec<_>>()
        .join(",\n");
    format!(
        "    {{\n      \"file\": \"{name}\",\n      \"file_bytes\": {},\n      \"file_sha256\": \"{}\",\n      \"header_bytes\": {},\n      \"header_sha256\": \"{}\",\n      \"header_domain_sha256\": \"{}\",\n      \"body_bytes\": {},\n      \"body_sha256\": \"{}\",\n      \"body_domain_sha256\": \"{}\",\n      \"footer_bytes\": {},\n      \"footer_sha256\": \"{}\",\n      \"footer_self_sha256\": \"{}\",\n      \"global_logical_sha256\": \"{}\",\n      \"record_count\": {},\n      \"chunks\": [\n{chunks}\n      ]\n    }}",
        file.len(),
        hex(&Sha256::digest(file)),
        header_len,
        hex(&Sha256::digest(&file[..header_len])),
        hex(&footer.header_sha256),
        body.len(),
        hex(&Sha256::digest(body)),
        hex(&footer.body_sha256),
        footer_bytes.len(),
        hex(&Sha256::digest(&file[footer_bytes.clone()])),
        hex(&file[footer_bytes.end - 32..footer_bytes.end]),
        hex(&footer.global_logical_sha256),
        footer.record_count,
    )
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
