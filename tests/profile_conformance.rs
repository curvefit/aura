use aura_codec::schema::{generic_i64_parent_schema, ohlcv_schema, SchemaDescriptor};
use aura_codec::{records, Profile};

const FLAT_PARENT_MAP: &[u8] = &[255, 0, 2, 2, 2, 0];
const REPEATED_PARENT_MAP: &[u8] = &[255, 0, 0, 128, 132, 133, 133];

fn flat_rows() -> Vec<Vec<i64>> {
    vec![
        vec![1_000_000_000, 10_000, 10_100, 9_900, 10_050, 500],
        vec![61_000_000_000, 20_000, 20_100, 19_900, 20_050, 525],
        vec![121_000_000_000, 10_000, 10_100, 9_900, 10_050, 510],
    ]
}

fn repeated_rows() -> Vec<Vec<i64>> {
    vec![
        vec![1_000, 10, 20, 0, 100_000, 5, 0],
        vec![1_000, 10, 20, 0, 100_010, 0, 1],
        vec![2_000, 11, 21, 1, 100_020, 8, 0],
        vec![2_000, 11, 21, 1, 100_030, 0, 1],
    ]
}

fn assert_profiles_preserve_rows(
    schema: SchemaDescriptor,
    rows: Vec<Vec<i64>>,
    schema_mapping: &[u8],
) {
    let ingest = records::encode_ingest_i64_file(records::I64FileInput {
        schema,
        rows: rows.clone(),
        stream_id: 41,
        dictionary_id: 9,
        header_comment: Some("profile conformance".to_owned()),
    })
    .unwrap();

    let ingest_decoded = records::decode_i64_file(&ingest).unwrap();
    let aura0 = records::compile_i64_file(&ingest, Profile::Aura0).unwrap();
    let aura1 = records::compile_i64_file(&ingest, Profile::Aura1).unwrap();
    let aura0_decoded = records::decode_i64_file(&aura0).unwrap();
    let aura1_decoded = records::decode_i64_file(&aura1).unwrap();

    for decoded in [&ingest_decoded, &aura0_decoded, &aura1_decoded] {
        assert_eq!(rows, decoded.rows);
        assert_eq!(ingest_decoded.schema.fields, decoded.schema.fields);
        assert_eq!(schema_mapping, decoded.header.schema_mapping.as_slice());
        assert_eq!(41, decoded.header.stream_id);
        assert_eq!(9, decoded.header.dictionary_id);
        assert_eq!("profile conformance", decoded.header.comment);
    }

    assert_eq!(Profile::Ingest, ingest_decoded.header.profile);
    assert!(ingest_decoded.ingest_footer.is_some());
    assert!(ingest_decoded.compiled_footer.is_none());

    assert_eq!(Profile::Aura0, aura0_decoded.header.profile);
    assert!(aura0_decoded.ingest_footer.is_none());
    assert!(aura0_decoded.compiled_footer.is_some());

    assert_eq!(Profile::Aura1, aura1_decoded.header.profile);
    assert!(aura1_decoded.ingest_footer.is_none());
    assert!(aura1_decoded.compiled_footer.is_some());
}

#[test]
fn all_i64_profiles_preserve_baseline_logical_rows() {
    let flat_schema = generic_i64_parent_schema("profile_flat_v1", FLAT_PARENT_MAP).unwrap();
    assert_profiles_preserve_rows(flat_schema, flat_rows(), FLAT_PARENT_MAP);

    let repeated_schema =
        generic_i64_parent_schema("profile_repeated_v1", REPEATED_PARENT_MAP).unwrap();
    assert_profiles_preserve_rows(repeated_schema, repeated_rows(), REPEATED_PARENT_MAP);

    assert_profiles_preserve_rows(ohlcv_schema().unwrap(), flat_rows(), FLAT_PARENT_MAP);
}

#[test]
fn compiled_profiles_reject_truncated_or_trailing_body_bytes() {
    let schema = generic_i64_parent_schema("malformed_compiled_v1", FLAT_PARENT_MAP).unwrap();
    let rows = flat_rows();
    let ingest = records::encode_ingest_i64_file(records::I64FileInput {
        schema,
        rows,
        stream_id: 7,
        dictionary_id: 5,
        header_comment: None,
    })
    .unwrap();

    for profile in [Profile::Aura0, Profile::Aura1] {
        let compiled = records::compile_i64_file(&ingest, profile).unwrap();
        assert!(records::decode_i64_file(&compiled).is_ok());

        let truncated = without_last_body_byte(compiled.clone());
        assert!(records::decode_i64_file(&truncated).is_err());

        let with_trailing = with_trailing_body_byte(compiled);
        assert!(records::decode_i64_file(&with_trailing).is_err());
    }
}

#[test]
fn profile_metadata_survives_compile() {
    let schema = generic_i64_parent_schema("metadata_survival_v1", FLAT_PARENT_MAP).unwrap();
    let rows = flat_rows();
    let ingest = records::encode_ingest_i64_file(records::I64FileInput {
        schema,
        rows,
        stream_id: 123,
        dictionary_id: 456,
        header_comment: Some("ts,open,high,low,close,volume".to_owned()),
    })
    .unwrap();
    let ingest_decoded = records::decode_i64_file(&ingest).unwrap();

    for profile in [Profile::Aura0, Profile::Aura1] {
        let compiled = records::compile_i64_file(&ingest, profile).unwrap();
        let decoded = records::decode_i64_file(&compiled).unwrap();

        assert_eq!(profile, decoded.header.profile);
        assert_eq!(ingest_decoded.header.stream_id, decoded.header.stream_id);
        assert_eq!(
            ingest_decoded.header.dictionary_id,
            decoded.header.dictionary_id
        );
        assert_eq!(
            ingest_decoded.header.base_time_ns,
            decoded.header.base_time_ns
        );
        assert_eq!(
            ingest_decoded.header.schema_mapping,
            decoded.header.schema_mapping
        );
        assert_eq!(ingest_decoded.header.comment, decoded.header.comment);
    }
}

fn without_last_body_byte(mut file: Vec<u8>) -> Vec<u8> {
    let body_end = footer_start(&file);
    assert!(usize::from(file[7]) < body_end);
    file.remove(body_end - 1);
    file
}

fn with_trailing_body_byte(mut file: Vec<u8>) -> Vec<u8> {
    let body_end = footer_start(&file);
    file.insert(body_end, 0x42);
    file
}

fn footer_start(file: &[u8]) -> usize {
    let footer_len_offset = file.len() - 12;
    footer_len_offset - read_u32_le(&file[footer_len_offset..footer_len_offset + 4]) as usize
}

fn read_u32_le(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(bytes.try_into().unwrap())
}
