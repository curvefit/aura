use std::io::Cursor;

use aura_codec::{
    canonical_v3_batch_sha256, compile_v3_planned_flat, decode_any_compiled_footer,
    decode_v3_planned_flat, decode_v3_planned_flat_footer, decode_v3_selected_flat,
    encode_v3_planned_flat_footer, parse_schema_json, AnyCompiledFooter, AuraError, AuraHeader,
    AuraV3Batch, AuraV3Column, AuraV3ColumnValues as Values, AuraV3VariableColumn,
    DecodedV3SelectedFlat, FieldRole, FieldType, FlatAuraPlanV2, PlanV2PhysicalCodec,
    SchemaBuilder, V3FlatAura0Writer, V3FlatLimits, V3FlatWriterOptions,
};
use sha2::{Digest, Sha256};

const FLAT_PLAN_HASH_DOMAIN: &[u8] = b"aura-flat-plan-v2-registry-v1\0";
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
    assert_eq!(artifact.inspection.candidates.len(), 3);
    assert!(artifact.inspection.candidates[2].selected);
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
        "development planned-flat bytes exact={} fixed={} mixed={}",
        artifact.inspection.candidates[0].complete_bytes.unwrap(),
        artifact.inspection.candidates[1].complete_bytes.unwrap(),
        artifact.inspection.candidates[2].complete_bytes.unwrap()
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
    assert!(artifact.inspection.candidates[2].selected);
    assert_eq!(
        decode_v3_planned_flat(&artifact.bytes, Default::default())
            .unwrap()
            .summary
            .file_bytes,
        artifact.summary.file_bytes
    );
    eprintln!(
        "development trade-shape planned-flat bytes exact={} fixed={} mixed={}",
        artifact.inspection.candidates[0].complete_bytes.unwrap(),
        artifact.inspection.candidates[1].complete_bytes.unwrap(),
        artifact.inspection.candidates[2].complete_bytes.unwrap()
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
    let block_limits = V3FlatLimits {
        value_limits: aura_codec::V3ValueLimits {
            max_block_bytes: artifact.summary.body_bytes as usize,
            ..defaults.value_limits
        },
        ..defaults
    };
    let fallback =
        compile_v3_planned_flat(&schema, &[batch(schema.schema_id, 0, 32)], block_limits).unwrap();
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
fn planned_flat_rehashed_footer_and_noncanonical_varint_reject() {
    let schema = schema();
    let input = batch(schema.schema_id, 0, 32);
    let artifact = compile_v3_planned_flat(&schema, &[input], Default::default()).unwrap();
    assert!(artifact.inspection.candidates[2].selected);
    let mut footer = footer_bytes(&artifact.bytes).to_vec();
    footer[8] ^= 1;
    resign(&mut footer, FLAT_FOOTER_HASH_DOMAIN);
    assert!(decode_v3_planned_flat_footer(&footer, V3FlatLimits::HARD).is_err());

    let header_len = AuraHeader::encoded_len(&artifact.bytes).unwrap();
    let body_end = artifact.bytes.len() - 12 - footer_bytes(&artifact.bytes).len();
    let mut body = artifact.bytes[header_len..body_end].to_vec();
    assert_eq!(body[64], 0);
    body.splice(64..65, [0x80, 0x00]);
    let body_len = body.len() as u64;
    body[56..64].copy_from_slice(&body_len.to_le_bytes());
    let mut decoded_footer =
        decode_v3_planned_flat_footer(footer_bytes(&artifact.bytes), V3FlatLimits::HARD).unwrap();
    decoded_footer.body_len = body_len;
    decoded_footer.body_sha256 = domain_hash(FLAT_BODY_HASH_DOMAIN, &body);
    decoded_footer.chunks[0].stored_len = body_len;
    decoded_footer.chunks[0].stored_sha256 = Sha256::digest(&body).into();
    let footer = encode_v3_planned_flat_footer(&decoded_footer, V3FlatLimits::HARD).unwrap();
    let mut rebuilt = Vec::new();
    rebuilt.extend_from_slice(&artifact.bytes[..header_len]);
    rebuilt.extend_from_slice(&body);
    rebuilt.extend_from_slice(&footer);
    rebuilt.extend_from_slice(&(footer.len() as u32).to_le_bytes());
    rebuilt.extend_from_slice(b"sealed:)");
    assert!(decode_v3_planned_flat(&rebuilt, V3FlatLimits::HARD).is_err());
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
    body[48..52].copy_from_slice(&u32::MAX.to_le_bytes());
    let malicious = rebuild_with_body(&artifact.bytes, &body);
    let result = std::panic::catch_unwind(|| decode_v3_planned_flat(&malicious, at_boundary));
    assert!(matches!(
        result,
        Ok(Err(AuraError::InvalidValue("planned flat row count")))
    ));
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
