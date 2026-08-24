use std::io::Cursor;

use aura_codec::{
    canonical_v3_batch_sha256, compile_v3_planned_flat, decode_any_compiled_footer,
    decode_v3_planned_flat, decode_v3_planned_flat_footer, decode_v3_selected_flat,
    encode_v3_planned_flat_footer, encode_v3_value_block, parse_schema_json, AnyCompiledFooter,
    AuraHeader, AuraV3Batch, AuraV3Column, AuraV3ColumnValues as Values, AuraV3VariableColumn,
    DecodedV3SelectedFlat, FieldRole, FieldType, FlatAuraPlanV2, PlanV2PhysicalCodec,
    SchemaBuilder, V3FlatAura0Writer, V3FlatLimits, V3FlatWriterOptions,
    FLAT_PLAN_V2_DICTIONARY_REGISTRY_VERSION, V3_PLANNED_FLAT_DICTIONARY_BLOCK_VERSION,
    V3_PLANNED_FLAT_DICTIONARY_BODY_LAYOUT_VERSION,
};
use sha2::{Digest, Sha256};

const FLAT_PLAN_HASH_DOMAIN: &[u8] = b"aura-flat-plan-v2-registry-v1\0";
const FLAT_DICTIONARY_PLAN_HASH_DOMAIN: &[u8] = b"aura-flat-plan-v2-registry-v2\0";
const FLAT_BODY_HASH_DOMAIN: &[u8] = b"aura-v3-planned-flat-body-v1\0";
const FLAT_FOOTER_HASH_DOMAIN: &[u8] = b"aura-v3-planned-flat-footer-v1\0";

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
    let mut schema = SchemaBuilder::new("anonymous_flat_codec")
        .v3()
        .field("ts", FieldType::TimestampMs, FieldRole::Timestamp)
        .field("signed", FieldType::I64, FieldRole::Value)
        .field("unsigned", FieldType::U64, FieldRole::Value)
        .nullable_field("nullable", FieldType::I32, FieldRole::Value)
        .field("opaque", FieldType::Opaque16, FieldRole::Value)
        .field("text", FieldType::Utf8, FieldRole::Value)
        .field("decimal", FieldType::DecimalText, FieldRole::Value)
        .finish()
        .unwrap();
    schema.fields[4].nullable = false;
    schema
}

fn batch(schema_id: u32, start: i64, rows: usize) -> AuraV3Batch {
    let timestamps = (0..rows).map(|i| start + i as i64).collect::<Vec<_>>();
    let signed = (0..rows)
        .map(|i| ((start as usize + i) % 5) as i64 - 2)
        .collect::<Vec<_>>();
    let unsigned = (0..rows)
        .map(|i| ((start as usize + i) % 7) as u64)
        .collect::<Vec<_>>();
    let mut validity = vec![0u8; rows.div_ceil(8)];
    for row in 0..rows {
        if !(start as usize + row).is_multiple_of(3) {
            validity[row / 8] |= 1 << (row % 8);
        }
    }
    let text = (0..rows)
        .map(|i| {
            if (start as usize + i).is_multiple_of(2) {
                b"".as_slice()
            } else {
                b"x".as_slice()
            }
        })
        .collect::<Vec<_>>();
    let decimal = (0..rows).map(|_| b"0".as_slice()).collect::<Vec<_>>();
    AuraV3Batch {
        schema_id,
        row_count: rows as u32,
        columns: vec![
            AuraV3Column {
                slot: 0,
                validity: None,
                values: Values::TimestampMs(timestamps),
            },
            AuraV3Column {
                slot: 1,
                validity: None,
                values: Values::I64(signed),
            },
            AuraV3Column {
                slot: 2,
                validity: None,
                values: Values::U64(unsigned),
            },
            AuraV3Column {
                slot: 3,
                validity: Some(validity),
                values: Values::I32(vec![0; rows]),
            },
            AuraV3Column {
                slot: 4,
                validity: None,
                values: Values::Opaque16(vec![[7; 16]; rows]),
            },
            AuraV3Column {
                slot: 5,
                validity: None,
                values: Values::Utf8(variable(&text)),
            },
            AuraV3Column {
                slot: 6,
                validity: None,
                values: Values::DecimalText(variable(&decimal)),
            },
        ],
    }
}

fn dictionary_schema() -> aura_codec::SchemaDescriptor {
    SchemaBuilder::new("anonymous_dictionary_exact_bytes")
        .v3()
        .field("ts", FieldType::TimestampMs, FieldRole::Timestamp)
        .nullable_field("text", FieldType::Utf8, FieldRole::Value)
        .nullable_field("decimal", FieldType::DecimalText, FieldRole::Value)
        .field("count", FieldType::I64, FieldRole::Count)
        .finish()
        .unwrap()
}

fn bitmap(rows: usize, null_rows: &[usize]) -> Vec<u8> {
    let mut bitmap = vec![0xff; rows.div_ceil(8)];
    for row in null_rows {
        bitmap[*row / 8] &= !(1 << (*row % 8));
    }
    if !rows.is_multiple_of(8) {
        let used = rows % 8;
        *bitmap.last_mut().unwrap() &= (1 << used) - 1;
    }
    bitmap
}

fn dictionary_batch(schema_id: u32) -> AuraV3Batch {
    let text = [
        b"".as_slice(),
        "α".as_bytes(),
        b"",
        b"repeat",
        "α".as_bytes(),
        b"repeat",
        b"",
        b"repeat",
    ];
    let decimal = [
        b"+001.2300".as_slice(),
        b" 2 ",
        b"",
        b"+001.2300",
        b" 2 ",
        b"+001.2300",
        b" 2 ",
        b"+001.2300",
    ];
    AuraV3Batch {
        schema_id,
        row_count: 8,
        columns: vec![
            AuraV3Column {
                slot: 0,
                validity: None,
                values: Values::TimestampMs((0..8).map(i64::from).collect()),
            },
            AuraV3Column {
                slot: 1,
                validity: Some(bitmap(8, &[2])),
                values: Values::Utf8(variable(&text)),
            },
            AuraV3Column {
                slot: 2,
                validity: Some(bitmap(8, &[2])),
                values: Values::DecimalText(variable(&decimal)),
            },
            AuraV3Column {
                slot: 3,
                validity: None,
                values: Values::I64(vec![1; 8]),
            },
        ],
    }
}

#[test]
fn auf2_and_planned_flat_roundtrip_complete_cost() {
    let schema = schema();
    let public = schema.to_canonical_json().unwrap();
    let parsed = parse_schema_json(&public).unwrap();
    assert_eq!(parsed, schema);
    let batches = vec![
        batch(schema.schema_id, 1, 64),
        batch(schema.schema_id, 65, 64),
    ];
    let artifact = compile_v3_planned_flat(&schema, &batches, Default::default()).unwrap();
    assert_eq!(artifact.summary.file_bytes, artifact.bytes.len() as u64);
    assert_eq!(
        artifact.summary.file_bytes,
        artifact.summary.accounted_file_bytes
    );
    assert_eq!(artifact.inspection.candidates.len(), 5);
    assert!(artifact.inspection.candidates[4].selected);
    let zstd = artifact.inspection.zstd_candidate.as_ref().unwrap();
    assert_eq!(zstd.base_candidate_id, "planned-flat-variable-dictionary");
    assert_eq!(
        zstd.base_registry_version,
        FLAT_PLAN_V2_DICTIONARY_REGISTRY_VERSION
    );
    assert_eq!(zstd.stored_body_bytes, artifact.summary.body_bytes);
    assert_eq!(
        zstd.compressed_payload_bytes + zstd.wrapper_overhead_bytes,
        zstd.stored_body_bytes
    );
    assert!(artifact
        .inspection
        .codecs
        .iter()
        .any(|row| row.selected != PlanV2PhysicalCodec::FixedWidth));
    let decoded = decode_v3_planned_flat(&artifact.bytes, Default::default()).unwrap();
    assert_eq!(decoded.batches, batches);
    assert_eq!(
        decoded.summary.global_logical_sha256,
        artifact.summary.global_logical_sha256
    );
    let footer =
        decode_v3_planned_flat_footer(footer_bytes(&artifact.bytes), V3FlatLimits::HARD).unwrap();
    let plan = footer.plan.encode(&schema).unwrap();
    assert_eq!(&plan[..4], b"AUF2");
    assert_eq!(FlatAuraPlanV2::decode(&schema, &plan).unwrap(), footer.plan);
    assert!(matches!(
        decode_v3_selected_flat(&artifact.bytes, Default::default()).unwrap(),
        DecodedV3SelectedFlat::Planned(decoded) if decoded.batches == batches
    ));
    eprintln!(
        "development planned-flat bytes exact={} fixed={} mixed={} dictionary={} zstd19={}",
        artifact.inspection.candidates[0].complete_bytes.unwrap(),
        artifact.inspection.candidates[1].complete_bytes.unwrap(),
        artifact.inspection.candidates[2].complete_bytes.unwrap(),
        artifact.inspection.candidates[3].complete_bytes.unwrap(),
        artifact.inspection.candidates[4].complete_bytes.unwrap()
    );
}

#[test]
fn public_json_oi_and_trade_shapes_are_generic_and_applicable() {
    for schema in [
        SchemaBuilder::new("anonymous_oi_shape")
            .v3()
            .field("ts", FieldType::TimestampMs, FieldRole::Timestamp)
            .field("instrument", FieldType::Utf8, FieldRole::Identifier)
            .nullable_field("open_interest", FieldType::DecimalText, FieldRole::Value)
            .finish()
            .unwrap(),
        SchemaBuilder::new("anonymous_trade_shape")
            .v3()
            .field("ts", FieldType::TimestampMs, FieldRole::Timestamp)
            .field("sequence", FieldType::U64, FieldRole::Sequence)
            .field("price", FieldType::I64, FieldRole::Price)
            .field("quantity", FieldType::I64, FieldRole::Quantity)
            .field("instrument", FieldType::Utf8, FieldRole::Identifier)
            .finish()
            .unwrap(),
    ] {
        let parsed = parse_schema_json(&schema.to_canonical_json().unwrap()).unwrap();
        assert!(FlatAuraPlanV2::all_fixed(&parsed).is_ok());
        assert!(compile_v3_planned_flat(&parsed, &[], Default::default()).is_ok());
        assert!(parsed.groups.is_empty());
    }
}

#[test]
fn trade_shape_complete_ablation_is_exact() {
    let schema = SchemaBuilder::new("anonymous_trade_shape_ablation")
        .v3()
        .field("ts", FieldType::TimestampMs, FieldRole::Timestamp)
        .field("sequence", FieldType::U64, FieldRole::Sequence)
        .field("price", FieldType::I64, FieldRole::Price)
        .field("quantity", FieldType::I64, FieldRole::Quantity)
        .field("instrument", FieldType::Utf8, FieldRole::Identifier)
        .finish()
        .unwrap();
    let schema = parse_schema_json(&schema.to_canonical_json().unwrap()).unwrap();
    let rows = 128usize;
    let input = AuraV3Batch {
        schema_id: schema.schema_id,
        row_count: rows as u32,
        columns: vec![
            AuraV3Column {
                slot: 0,
                validity: None,
                values: Values::TimestampMs((0..rows).map(|i| i as i64).collect()),
            },
            AuraV3Column {
                slot: 1,
                validity: None,
                values: Values::U64((0..rows).map(|i| i as u64).collect()),
            },
            AuraV3Column {
                slot: 2,
                validity: None,
                values: Values::I64((0..rows).map(|i| 1_000_000 + i as i64).collect()),
            },
            AuraV3Column {
                slot: 3,
                validity: None,
                values: Values::I64((0..rows).map(|i| (i % 5) as i64).collect()),
            },
            AuraV3Column {
                slot: 4,
                validity: None,
                values: Values::Utf8(variable(
                    &(0..rows).map(|_| b"X".as_slice()).collect::<Vec<_>>(),
                )),
            },
        ],
    };
    let artifact = compile_v3_planned_flat(&schema, &[input], Default::default()).unwrap();
    assert!(artifact.inspection.candidates[4].selected);
    assert_eq!(
        decode_v3_planned_flat(&artifact.bytes, Default::default())
            .unwrap()
            .summary
            .file_bytes,
        artifact.summary.file_bytes
    );
    eprintln!(
        "development trade-shape planned-flat bytes exact={} fixed={} mixed={} dictionary={} zstd19={}",
        artifact.inspection.candidates[0].complete_bytes.unwrap(),
        artifact.inspection.candidates[1].complete_bytes.unwrap(),
        artifact.inspection.candidates[2].complete_bytes.unwrap(),
        artifact.inspection.candidates[3].complete_bytes.unwrap(),
        artifact.inspection.candidates[4].complete_bytes.unwrap()
    );
}

#[test]
fn dictionary_registry_roundtrips_both_exact_byte_types_and_accounts_complete_cost() {
    let schema = dictionary_schema();
    let input = dictionary_batch(schema.schema_id);
    let artifact =
        compile_v3_planned_flat(&schema, std::slice::from_ref(&input), V3FlatLimits::HARD).unwrap();
    assert!(artifact.inspection.candidates[3].selected);
    assert!(
        artifact.inspection.candidates[3].complete_bytes
            < artifact.inspection.candidates[2].complete_bytes
    );
    assert_eq!(artifact.summary.file_bytes, artifact.bytes.len() as u64);
    assert_eq!(
        artifact.summary.file_bytes,
        artifact.summary.accounted_file_bytes
    );
    let decoded = decode_v3_planned_flat(&artifact.bytes, V3FlatLimits::HARD).unwrap();
    assert_eq!(decoded.batches, vec![input]);
    assert_eq!(
        decoded.footer.plan.registry_version,
        FLAT_PLAN_V2_DICTIONARY_REGISTRY_VERSION
    );
    assert_eq!(
        decoded.footer.plan.codecs[1],
        PlanV2PhysicalCodec::VariableByteDictionaryBitpacked
    );
    assert_eq!(
        decoded.footer.plan.codecs[2],
        PlanV2PhysicalCodec::VariableByteDictionaryBitpacked
    );
    let header_len = AuraHeader::encoded_len(&artifact.bytes).unwrap();
    assert_eq!(&artifact.bytes[header_len..header_len + 8], b"AUFPVB02");
    let footer = footer_bytes(&artifact.bytes);
    assert_eq!(
        u16::from_le_bytes(footer[36..38].try_into().unwrap()),
        V3_PLANNED_FLAT_DICTIONARY_BODY_LAYOUT_VERSION
    );
    assert_eq!(
        u16::from_le_bytes(footer[38..40].try_into().unwrap()),
        V3_PLANNED_FLAT_DICTIONARY_BLOCK_VERSION
    );
    let text = &artifact.inspection.dictionary_candidate_codecs[1];
    assert!(text.dictionary_selected);
    assert_eq!(text.present_count, Some(7));
    assert_eq!(text.null_count, Some(1));
    assert_eq!(text.present_empty_count, Some(2));
    assert_eq!(text.dictionary_entries, Some(3));
    let decimal = &artifact.inspection.dictionary_candidate_codecs[2];
    assert!(decimal.dictionary_selected);
    assert_eq!(decimal.dictionary_entries, Some(2));
    assert_eq!(decimal.present_empty_count, Some(0));
    let repeat = compile_v3_planned_flat(
        &schema,
        &[dictionary_batch(schema.schema_id)],
        V3FlatLimits::HARD,
    )
    .unwrap();
    assert_eq!(artifact.bytes, repeat.bytes);
}

#[test]
fn dictionary_candidate_mixes_eligible_lanes_and_high_cardinality_falls_back() {
    let schema = dictionary_schema();
    let rows = 64usize;
    let unique_decimal = (0..rows)
        .map(|row| format!("{row}.00000000000000000001"))
        .collect::<Vec<_>>();
    let unique_decimal_refs = unique_decimal
        .iter()
        .map(|value| value.as_bytes())
        .collect::<Vec<_>>();
    let repeated_text = vec![b"same-long-text-value".as_slice(); rows];
    let input = AuraV3Batch {
        schema_id: schema.schema_id,
        row_count: rows as u32,
        columns: vec![
            AuraV3Column {
                slot: 0,
                validity: None,
                values: Values::TimestampMs((0..rows).map(|row| row as i64).collect()),
            },
            AuraV3Column {
                slot: 1,
                validity: Some(bitmap(rows, &[])),
                values: Values::Utf8(variable(&repeated_text)),
            },
            AuraV3Column {
                slot: 2,
                validity: Some(bitmap(rows, &[])),
                values: Values::DecimalText(variable(&unique_decimal_refs)),
            },
            AuraV3Column {
                slot: 3,
                validity: None,
                values: Values::I64(vec![0; rows]),
            },
        ],
    };
    let artifact = compile_v3_planned_flat(&schema, &[input], V3FlatLimits::HARD).unwrap();
    assert!(artifact.inspection.candidates[3].applicable);
    assert!(artifact.inspection.candidates[4].selected);
    let decoded = decode_v3_planned_flat(&artifact.bytes, V3FlatLimits::HARD).unwrap();
    assert_eq!(
        decoded.footer.plan.codecs[1],
        PlanV2PhysicalCodec::VariableByteDictionaryBitpacked
    );
    assert_eq!(
        decoded.footer.plan.codecs[2],
        PlanV2PhysicalCodec::FixedWidth
    );
    assert_eq!(
        artifact.inspection.dictionary_candidate_codecs[2]
            .dictionary_rejection
            .as_deref(),
        Some("dictionary lane bytes did not win")
    );

    let tiny_schema = SchemaBuilder::new("anonymous_dictionary_tiny")
        .v3()
        .field("ts", FieldType::TimestampMs, FieldRole::Timestamp)
        .field("text", FieldType::Utf8, FieldRole::Value)
        .finish()
        .unwrap();
    let tiny = AuraV3Batch {
        schema_id: tiny_schema.schema_id,
        row_count: 1,
        columns: vec![
            AuraV3Column {
                slot: 0,
                validity: None,
                values: Values::TimestampMs(vec![0]),
            },
            AuraV3Column {
                slot: 1,
                validity: None,
                values: Values::Utf8(variable(&[b"unique-value"])),
            },
        ],
    };
    let fallback = compile_v3_planned_flat(
        &tiny_schema,
        std::slice::from_ref(&tiny),
        V3FlatLimits::HARD,
    )
    .unwrap();
    assert!(!fallback.inspection.candidates[3].applicable);
    assert_eq!(fallback.inspection.candidates[3].complete_bytes, None);
    assert_eq!(
        fallback.inspection.candidates[3].rejection.as_deref(),
        Some("invalid value for planned flat dictionary no lane win")
    );
    assert_eq!(
        fallback
            .inspection
            .zstd_candidate
            .as_ref()
            .unwrap()
            .base_candidate_id,
        "planned-flat-integer-codecs"
    );
    assert!(!fallback.inspection.candidates[4].selected);
    let zstd = fallback.inspection.zstd_candidate.as_ref().unwrap();
    assert!(zstd.stored_body_bytes > zstd.inner_body_bytes);
    let limited = compile_v3_planned_flat(
        &tiny_schema,
        &[tiny],
        V3FlatLimits {
            value_limits: aura_codec::V3ValueLimits {
                max_block_bytes: zstd.inner_body_bytes as usize,
                ..V3FlatLimits::HARD.value_limits
            },
            ..V3FlatLimits::HARD
        },
    )
    .unwrap();
    assert!(!limited.inspection.candidates[4].applicable);
    assert!(limited.inspection.candidates[..4]
        .iter()
        .any(|candidate| candidate.selected));
}

#[test]
fn zstd_candidate_uses_preselected_registry1_mixed_base_without_dictionary() {
    let schema = SchemaBuilder::new("anonymous_zstd_registry1_base")
        .v3()
        .field("ts", FieldType::TimestampMs, FieldRole::Timestamp)
        .field("value", FieldType::I64, FieldRole::Value)
        .finish()
        .unwrap();
    let rows = 1024usize;
    let input = AuraV3Batch {
        schema_id: schema.schema_id,
        row_count: rows as u32,
        columns: vec![
            AuraV3Column {
                slot: 0,
                validity: None,
                values: Values::TimestampMs((0..rows).map(|row| row as i64).collect()),
            },
            AuraV3Column {
                slot: 1,
                validity: None,
                values: Values::I64(vec![7; rows]),
            },
        ],
    };
    let artifact = compile_v3_planned_flat(&schema, &[input], V3FlatLimits::HARD).unwrap();
    assert!(!artifact.inspection.candidates[3].applicable);
    assert!(artifact.inspection.candidates[4].selected);
    let zstd = artifact.inspection.zstd_candidate.as_ref().unwrap();
    assert_eq!(zstd.base_candidate_id, "planned-flat-integer-codecs");
    assert_eq!(zstd.base_registry_version, 1);
    let decoded = decode_v3_planned_flat(&artifact.bytes, V3FlatLimits::HARD).unwrap();
    assert_eq!(decoded.footer.body_layout_version, 3);
    assert_eq!(decoded.footer.block_version, 3);
    assert_eq!(decoded.footer.plan.registry_version, 1);
}

#[test]
fn dictionary_plan_registry_is_distinct_and_wrong_type_rehash_rejects() {
    let schema = dictionary_schema();
    let mut plan = FlatAuraPlanV2::all_fixed(&schema).unwrap();
    plan.registry_version = FLAT_PLAN_V2_DICTIONARY_REGISTRY_VERSION;
    plan.codecs[1] = PlanV2PhysicalCodec::VariableByteDictionaryBitpacked;
    let encoded = plan.encode(&schema).unwrap();
    assert_eq!(FlatAuraPlanV2::decode(&schema, &encoded).unwrap(), plan);
    let mut wrong_type = encoded;
    wrong_type[52] = PlanV2PhysicalCodec::VariableByteDictionaryBitpacked as u8;
    resign(&mut wrong_type, FLAT_DICTIONARY_PLAN_HASH_DOMAIN);
    assert!(FlatAuraPlanV2::decode(&schema, &wrong_type).is_err());
}

#[test]
fn chunk_local_dictionaries_preserve_rechunked_logical_identity() {
    let schema = SchemaBuilder::new("anonymous_dictionary_rechunk")
        .v3()
        .field("ts", FieldType::TimestampMs, FieldRole::Timestamp)
        .field("text", FieldType::Utf8, FieldRole::Value)
        .finish()
        .unwrap();
    let make = |start: usize, rows: usize| AuraV3Batch {
        schema_id: schema.schema_id,
        row_count: rows as u32,
        columns: vec![
            AuraV3Column {
                slot: 0,
                validity: None,
                values: Values::TimestampMs((start..start + rows).map(|row| row as i64).collect()),
            },
            AuraV3Column {
                slot: 1,
                validity: None,
                values: Values::Utf8(variable(
                    &(start..start + rows)
                        .map(|row| {
                            if row.is_multiple_of(2) {
                                b"even-value".as_slice()
                            } else {
                                b"odd-value".as_slice()
                            }
                        })
                        .collect::<Vec<_>>(),
                )),
            },
        ],
    };
    let whole_batch = make(0, 512);
    let split_batches = vec![make(0, 256), make(256, 256)];
    let whole = compile_v3_planned_flat(
        &schema,
        std::slice::from_ref(&whole_batch),
        V3FlatLimits::HARD,
    )
    .unwrap();
    let split = compile_v3_planned_flat(&schema, &split_batches, V3FlatLimits::HARD).unwrap();
    assert!(whole.inspection.candidates[4].selected);
    assert!(split.inspection.candidates[4].selected);
    assert_eq!(
        whole.summary.global_logical_sha256,
        split.summary.global_logical_sha256
    );
    assert_eq!(
        whole.summary.global_logical_sha256,
        canonical_v3_batch_sha256(&schema, &whole_batch, V3FlatLimits::HARD.value_limits).unwrap()
    );
    assert_ne!(whole.bytes, split.bytes);
    assert_eq!(
        decode_v3_planned_flat(&whole.bytes, V3FlatLimits::HARD)
            .unwrap()
            .batches,
        vec![whole_batch]
    );
    assert_eq!(
        decode_v3_planned_flat(&split.bytes, V3FlatLimits::HARD)
            .unwrap()
            .batches,
        split_batches
    );
}

#[test]
fn exact_empty_fallback_is_byte_identical() {
    let schema = SchemaBuilder::new("anonymous_empty_exact")
        .v3()
        .field("ts", FieldType::TimestampMs, FieldRole::Timestamp)
        .finish()
        .unwrap();
    let artifact = compile_v3_planned_flat(&schema, &[], Default::default()).unwrap();
    let writer = V3FlatAura0Writer::try_new(
        Cursor::new(Vec::new()),
        schema,
        V3FlatWriterOptions::default(),
    )
    .unwrap();
    let exact = writer.finish().unwrap().0.into_inner();
    assert_eq!(
        artifact.inspection.candidates[0].complete_bytes,
        Some(exact.len() as u64)
    );
    assert!(artifact.inspection.candidates[0].selected);
    assert_eq!(artifact.bytes, exact);
    assert!(matches!(
        decode_v3_selected_flat(&artifact.bytes, Default::default()).unwrap(),
        DecodedV3SelectedFlat::Exact(decoded) if decoded.batches.is_empty()
    ));
}

#[test]
fn current_flat_fixture_hash_and_dispatch_are_unchanged() {
    let bytes = include_bytes!("fixtures/v3-container/three-row-two-chunk.aura0");
    let hash: [u8; 32] = Sha256::digest(bytes).into();
    assert_eq!(
        hash.iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "796c25ab0ff1108c1460bd115fb5158f975c6f8c47bb1f2f6bfbc39ecdc859b0"
    );
    assert!(matches!(
        decode_any_compiled_footer(footer_bytes(bytes)).unwrap(),
        AnyCompiledFooter::V3Flat(_)
    ));
}

#[test]
fn planned_flat_rechunking_and_logical_hash_are_stable() {
    let schema = schema();
    let whole = batch(schema.schema_id, 0, 128);
    let split = vec![
        batch(schema.schema_id, 0, 64),
        batch(schema.schema_id, 64, 64),
    ];
    let one =
        compile_v3_planned_flat(&schema, std::slice::from_ref(&whole), Default::default()).unwrap();
    let many = compile_v3_planned_flat(&schema, &split, Default::default()).unwrap();
    assert_eq!(one.summary.plan_sha256, many.summary.plan_sha256);
    assert_eq!(
        one.summary.global_logical_sha256,
        many.summary.global_logical_sha256
    );
    assert_eq!(
        canonical_v3_batch_sha256(&schema, &whole, Default::default()).unwrap(),
        one.summary.global_logical_sha256
    );
}

#[test]
fn planned_flat_dispatch_corruption_prefixes_and_limits_fail_closed() {
    let schema = schema();
    let input = batch(schema.schema_id, 0, 32);
    let artifact = compile_v3_planned_flat(&schema, &[input], Default::default()).unwrap();
    assert!(matches!(
        decode_any_compiled_footer(footer_bytes(&artifact.bytes)).unwrap(),
        AnyCompiledFooter::V3PlannedFlat(_)
    ));
    for len in 0..artifact.bytes.len() {
        assert!(decode_v3_planned_flat(&artifact.bytes[..len], V3FlatLimits::HARD).is_err());
    }
    for index in 0..artifact.bytes.len() {
        let mut b = artifact.bytes.clone();
        b[index] ^= 1;
        assert!(decode_v3_planned_flat(&b, V3FlatLimits::HARD).is_err());
    }
    let limits = V3FlatLimits {
        max_body_bytes: artifact.summary.body_bytes - 1,
        ..Default::default()
    };
    assert!(compile_v3_planned_flat(&schema, &[batch(schema.schema_id, 0, 32)], limits).is_err());
    let defaults = V3FlatLimits::default();
    let input = batch(schema.schema_id, 0, 32);
    let logical_cap = V3FlatLimits {
        value_limits: aura_codec::V3ValueLimits {
            max_block_bytes: artifact.summary.body_bytes as usize,
            ..defaults.value_limits
        },
        ..defaults
    };
    assert!(compile_v3_planned_flat(&schema, std::slice::from_ref(&input), logical_cap).is_err());
    let exact_block_bytes = encode_v3_value_block(&schema, &input, aura_codec::V3ValueLimits::HARD)
        .unwrap()
        .len();
    let compact_limit = V3FlatLimits {
        value_limits: aura_codec::V3ValueLimits {
            max_block_bytes: exact_block_bytes - 1,
            ..defaults.value_limits
        },
        ..defaults
    };
    let fallback = compile_v3_planned_flat(&schema, &[input], compact_limit).unwrap();
    assert!(!fallback.inspection.candidates[0].applicable);
    assert!(fallback.inspection.candidates[1..]
        .iter()
        .any(|candidate| candidate.selected));
}

#[test]
fn planned_flat_extremes_nulls_and_property_roundtrip() {
    let schema = schema();
    let mut edge = batch(schema.schema_id, 0, 3);
    edge.columns[1].values = Values::I64(vec![i64::MIN, 0, i64::MAX]);
    edge.columns[2].values = Values::U64(vec![u64::MAX, 0, u64::MAX]);
    let artifact =
        compile_v3_planned_flat(&schema, std::slice::from_ref(&edge), Default::default()).unwrap();
    assert_eq!(
        decode_v3_planned_flat(&artifact.bytes, Default::default())
            .unwrap()
            .batches,
        vec![edge]
    );
    for seed in 0..24i64 {
        let b = batch(schema.schema_id, seed, ((seed as usize) % 17) + 1);
        let a =
            compile_v3_planned_flat(&schema, std::slice::from_ref(&b), Default::default()).unwrap();
        assert_eq!(
            decode_v3_planned_flat(&a.bytes, Default::default())
                .unwrap()
                .batches,
            vec![b]
        );
    }
}

#[test]
fn auf2_rehashed_codec_and_layout_mutations_reject() {
    let schema = schema();
    let encoded = FlatAuraPlanV2::all_fixed(&schema)
        .unwrap()
        .encode(&schema)
        .unwrap();
    for (offset, value) in [(52usize, 99u8), (50, 1)] {
        let mut corrupted = encoded.clone();
        corrupted[offset] = value;
        resign(&mut corrupted, FLAT_PLAN_HASH_DOMAIN);
        assert!(FlatAuraPlanV2::decode(&schema, &corrupted).is_err());
    }
}

#[test]
fn planned_flat_rehashed_footer_and_wrapper_mutation_reject() {
    let schema = schema();
    let input = batch(schema.schema_id, 0, 32);
    let artifact = compile_v3_planned_flat(&schema, &[input], Default::default()).unwrap();
    assert!(artifact.inspection.candidates[4].selected);
    let mut footer = footer_bytes(&artifact.bytes).to_vec();
    footer[8] ^= 1;
    resign(&mut footer, FLAT_FOOTER_HASH_DOMAIN);
    assert!(decode_v3_planned_flat_footer(&footer, V3FlatLimits::HARD).is_err());

    let header_len = AuraHeader::encoded_len(&artifact.bytes).unwrap();
    let body_end = artifact.bytes.len() - 12 - footer_bytes(&artifact.bytes).len();
    let body = artifact.bytes[header_len..body_end].to_vec();
    assert_eq!(&body[..8], b"AUFPZB01");
    for offset in [8usize, 10, 11, 12, 13, 14, 16, 18, 20, 28, 36] {
        let mut corrupted = body.clone();
        corrupted[offset] ^= 1;
        let rebuilt = rebuild_with_body(&artifact.bytes, &corrupted);
        assert!(decode_v3_planned_flat(&rebuilt, V3FlatLimits::HARD).is_err());
    }
}

#[test]
fn planned_flat_row_limit_precedes_variable_lane_arithmetic() {
    let schema = SchemaBuilder::new("anonymous_variable_first")
        .v3()
        .field("text", FieldType::Utf8, FieldRole::Value)
        .field("ts", FieldType::TimestampMs, FieldRole::Timestamp)
        .finish()
        .unwrap();
    let rows = 512usize;
    let input = AuraV3Batch {
        schema_id: schema.schema_id,
        row_count: rows as u32,
        columns: vec![
            AuraV3Column {
                slot: 0,
                validity: None,
                values: Values::Utf8(variable(&vec![b"".as_slice(); rows])),
            },
            AuraV3Column {
                slot: 1,
                validity: None,
                values: Values::TimestampMs(vec![0; rows]),
            },
        ],
    };
    let artifact = compile_v3_planned_flat(&schema, &[input], V3FlatLimits::HARD).unwrap();
    assert!(matches!(
        decode_any_compiled_footer(footer_bytes(&artifact.bytes)).unwrap(),
        AnyCompiledFooter::V3PlannedFlat(_)
    ));

    let at_boundary = V3FlatLimits {
        max_rows: rows as u64,
        value_limits: aura_codec::V3ValueLimits {
            max_rows: rows,
            ..V3FlatLimits::HARD.value_limits
        },
        ..V3FlatLimits::HARD
    };
    assert!(decode_v3_planned_flat(&artifact.bytes, at_boundary).is_ok());
    let below_boundary = V3FlatLimits {
        max_rows: rows as u64 - 1,
        value_limits: aura_codec::V3ValueLimits {
            max_rows: rows - 1,
            ..V3FlatLimits::HARD.value_limits
        },
        ..V3FlatLimits::HARD
    };
    assert!(decode_v3_planned_flat(&artifact.bytes, below_boundary).is_err());

    let header_len = AuraHeader::encoded_len(&artifact.bytes).unwrap();
    let body_end = artifact.bytes.len() - 12 - footer_bytes(&artifact.bytes).len();
    let mut body = artifact.bytes[header_len..body_end].to_vec();
    body[20..28].copy_from_slice(&u64::MAX.to_le_bytes());
    let malicious = rebuild_with_body(&artifact.bytes, &body);
    let result = std::panic::catch_unwind(|| decode_v3_planned_flat(&malicious, at_boundary));
    assert!(matches!(result, Ok(Err(_))));
}

fn footer_bytes(file: &[u8]) -> &[u8] {
    let off = file.len() - 12;
    let len = u32::from_le_bytes(file[off..off + 4].try_into().unwrap()) as usize;
    &file[off - len..off]
}

fn resign(bytes: &mut [u8], domain: &[u8]) {
    let hash_start = bytes.len() - 32;
    let mut hasher = Sha256::new();
    hasher.update(domain);
    hasher.update((hash_start as u64).to_le_bytes());
    hasher.update(&bytes[..hash_start]);
    let hash: [u8; 32] = hasher.finalize().into();
    bytes[hash_start..].copy_from_slice(&hash);
}

fn domain_hash(domain: &[u8], bytes: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(domain);
    hasher.update((bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
    hasher.finalize().into()
}

fn rebuild_with_body(file: &[u8], body: &[u8]) -> Vec<u8> {
    let header_len = AuraHeader::encoded_len(file).unwrap();
    let mut footer = decode_v3_planned_flat_footer(footer_bytes(file), V3FlatLimits::HARD).unwrap();
    assert_eq!(footer.chunks.len(), 1);
    footer.body_len = body.len() as u64;
    footer.body_sha256 = domain_hash(FLAT_BODY_HASH_DOMAIN, body);
    footer.chunks[0].stored_len = body.len() as u64;
    footer.chunks[0].stored_sha256 = Sha256::digest(body).into();
    let footer = encode_v3_planned_flat_footer(&footer, V3FlatLimits::HARD).unwrap();
    let mut rebuilt = Vec::new();
    rebuilt.extend_from_slice(&file[..header_len]);
    rebuilt.extend_from_slice(body);
    rebuilt.extend_from_slice(&footer);
    rebuilt.extend_from_slice(&(footer.len() as u32).to_le_bytes());
    rebuilt.extend_from_slice(b"sealed:)");
    rebuilt
}
