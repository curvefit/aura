use aura_codec::format::{AURA_MAGIC, FORMAT_VERSION, SEAL_MAGIC};
use aura_codec::header::{AuraHeader, HEADER_PREFIX_SIZE};
use aura_codec::schema::{
    decode_schema_map, generic_i64_parent_schema, ohlcv_schema, FieldRelation, FieldScope,
};
use aura_codec::{records, Profile};

const GENERIC_PARENT_MAP: &[u8] = &[255, 0, 2, 2, 2, 0];
const REPEATED_PARENT_MAP: &[u8] = &[255, 0, 0, 128, 132, 133, 133];

fn sample_rows() -> Vec<Vec<i64>> {
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

fn encoded_generic_file() -> Vec<u8> {
    let schema = generic_i64_parent_schema("container_contract_v1", GENERIC_PARENT_MAP).unwrap();
    records::encode_ingest_i64_file(records::I64FileInput {
        schema,
        rows: sample_rows(),
        stream_id: 3,
        dictionary_id: 11,
        header_comment: None,
    })
    .unwrap()
}

fn read_u32_le(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(bytes.try_into().unwrap())
}

fn read_u64_le(bytes: &[u8]) -> u64 {
    u64::from_le_bytes(bytes.try_into().unwrap())
}

fn write_u32_le(bytes: &mut [u8], value: u32) {
    bytes.copy_from_slice(&value.to_le_bytes());
}

fn footer_len(file: &[u8]) -> usize {
    let seal_offset = file.len() - SEAL_MAGIC.len();
    read_u32_le(&file[seal_offset - 4..seal_offset]) as usize
}

fn footer_len_offset(file: &[u8]) -> usize {
    file.len() - SEAL_MAGIC.len() - 4
}

fn footer_start(file: &[u8]) -> usize {
    footer_len_offset(file) - footer_len(file)
}

fn footer_bytes(file: &[u8]) -> &[u8] {
    &file[footer_start(file)..footer_len_offset(file)]
}

fn mutated(mut file: Vec<u8>, change: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
    change(&mut file);
    file
}

#[test]
fn container_header_body_footer_trailer_offsets_match_contract() {
    let file = encoded_generic_file();
    let header_len = usize::from(file[7]);
    let original_footer_len_offset = footer_len_offset(&file);
    let footer_start = footer_start(&file);

    assert_eq!(AURA_MAGIC, &file[0..4]);
    assert_eq!(FORMAT_VERSION.to_le_bytes(), file[4..6]);
    assert_eq!(Profile::Ingest as u8, file[6]);
    assert_eq!(HEADER_PREFIX_SIZE + GENERIC_PARENT_MAP.len(), header_len);
    assert_eq!(1_000_000_000i64.to_le_bytes(), file[8..16]);
    assert_eq!(3u16.to_le_bytes(), file[16..18]);
    assert_eq!(11u16.to_le_bytes(), file[18..20]);
    assert_eq!(GENERIC_PARENT_MAP.len() as u8, file[20]);
    assert_eq!(0, file[21]);
    assert_eq!(GENERIC_PARENT_MAP, &file[22..header_len]);
    assert_eq!(3, read_u64_le(&file[header_len..header_len + 8]));
    assert_eq!(file.len() - 12, footer_start + footer_len(&file));
    assert_eq!(original_footer_len_offset, footer_start + footer_len(&file));
    assert_eq!(b"AURF", &footer_bytes(&file)[0..4]);
    assert_eq!(SEAL_MAGIC, &file[file.len() - SEAL_MAGIC.len()..]);
}

#[test]
fn schema_map_byte_contract_round_trips_event_and_repeated_slots() {
    let flat_entries = decode_schema_map(GENERIC_PARENT_MAP).unwrap();
    assert!(flat_entries[0].is_timestamp);
    assert_eq!(FieldScope::Event, flat_entries[1].scope);
    assert_eq!(FieldRelation::None, flat_entries[1].relation);
    assert_eq!(FieldRelation::DeltaFromField(1), flat_entries[2].relation);
    assert_eq!(FieldRelation::DeltaFromField(1), flat_entries[3].relation);
    assert_eq!(FieldRelation::DeltaFromField(1), flat_entries[4].relation);
    assert_eq!(FieldRelation::None, flat_entries[5].relation);

    let repeated_entries = decode_schema_map(REPEATED_PARENT_MAP).unwrap();
    assert!(repeated_entries[0].is_timestamp);
    assert_eq!(FieldScope::Event, repeated_entries[1].scope);
    assert_eq!(FieldScope::Event, repeated_entries[2].scope);
    assert_eq!(FieldScope::Repeated, repeated_entries[3].scope);
    assert_eq!(FieldRelation::None, repeated_entries[3].relation);
    assert_eq!(FieldScope::Repeated, repeated_entries[4].scope);
    assert_eq!(FieldRelation::DeltaFromField(3), repeated_entries[4].relation);
    assert_eq!(FieldRelation::DeltaFromField(4), repeated_entries[5].relation);
    assert_eq!(FieldRelation::DeltaFromField(4), repeated_entries[6].relation);

    let schema = generic_i64_parent_schema("schema_map_repeated_v1", REPEATED_PARENT_MAP).unwrap();
    assert_eq!(REPEATED_PARENT_MAP, schema_parent_map_from_file(schema, repeated_rows()));

    assert!(decode_schema_map(&[]).is_err());
    assert!(decode_schema_map(&[0, 255]).is_err());
    assert!(decode_schema_map(&[255, 255]).is_err());
    assert!(decode_schema_map(&[255, 2]).is_err());
    assert!(decode_schema_map(&[255, 3, 0]).is_err());
}

fn schema_parent_map_from_file(
    schema: aura_codec::schema::SchemaDescriptor,
    rows: Vec<Vec<i64>>,
) -> Vec<u8> {
    let file = records::encode_ingest_i64_file(records::I64FileInput {
        schema,
        rows,
        stream_id: 0,
        dictionary_id: 0,
        header_comment: None,
    })
    .unwrap();
    records::decode_i64_file(&file).unwrap().header.schema_mapping
}

#[test]
fn footer_schema_archive_matches_header_slots_and_decoded_rows() {
    let generic_schema =
        generic_i64_parent_schema("footer_archive_generic_v1", GENERIC_PARENT_MAP).unwrap();
    assert_schema_archive_agreement(generic_schema, sample_rows());

    let typed_schema = ohlcv_schema().unwrap();
    assert_schema_archive_agreement(typed_schema, sample_rows());
}

fn assert_schema_archive_agreement(
    schema: aura_codec::schema::SchemaDescriptor,
    rows: Vec<Vec<i64>>,
) {
    let ingest = records::encode_ingest_i64_file(records::I64FileInput {
        schema,
        rows: rows.clone(),
        stream_id: 5,
        dictionary_id: 13,
        header_comment: Some("archive contract".to_owned()),
    })
    .unwrap();

    for file in [
        ingest.clone(),
        records::compile_i64_file(&ingest, Profile::Aura0).unwrap(),
        records::compile_i64_file(&ingest, Profile::Aura1).unwrap(),
    ] {
        let decoded = records::decode_i64_file(&file).unwrap();
        assert_eq!(decoded.header.schema_mapping.len(), decoded.schema.fields.len());
        assert!(decoded
            .rows
            .iter()
            .all(|row| row.len() == decoded.schema.fields.len()));
        assert_eq!(rows, decoded.rows);
    }
}

#[test]
fn invalid_container_boundaries_are_rejected() {
    let file = encoded_generic_file();
    let header_len = usize::from(file[7]);

    let bad_magic = mutated(file.clone(), |bytes| bytes[0..4].copy_from_slice(b"NOPE"));
    assert!(records::decode_i64_file(&bad_magic).is_err());

    let bad_seal = mutated(file.clone(), |bytes| {
        let seal_offset = bytes.len() - SEAL_MAGIC.len();
        bytes[seal_offset..].copy_from_slice(b"unsealed");
    });
    assert!(records::decode_i64_file(&bad_seal).is_err());

    let oversized_footer = mutated(file.clone(), |bytes| {
        let offset = footer_len_offset(bytes);
        write_u32_le(&mut bytes[offset..offset + 4], u32::MAX);
    });
    assert!(records::decode_i64_file(&oversized_footer).is_err());

    let short_header = mutated(file.clone(), |bytes| {
        bytes[7] = (HEADER_PREFIX_SIZE - 1) as u8;
    });
    assert!(records::decode_i64_file(&short_header).is_err());

    let overlapping_footer = mutated(file.clone(), |bytes| {
        let offset = footer_len_offset(bytes);
        let overlapping_len = offset - (header_len - 1);
        write_u32_le(&mut bytes[offset..offset + 4], overlapping_len as u32);
    });
    assert!(records::decode_i64_file(&overlapping_footer).is_err());

    let with_comment = records::encode_ingest_i64_file(records::I64FileInput {
        schema: generic_i64_parent_schema("comment_contract_v1", GENERIC_PARENT_MAP).unwrap(),
        rows: sample_rows(),
        stream_id: 3,
        dictionary_id: 11,
        header_comment: Some("valid comment".to_owned()),
    })
    .unwrap();
    let invalid_utf8_comment = mutated(with_comment, |bytes| {
        let comment_start = HEADER_PREFIX_SIZE + usize::from(bytes[20]);
        bytes[comment_start] = 0xff;
    });
    let invalid_header_len = usize::from(invalid_utf8_comment[7]);
    assert!(AuraHeader::decode(&invalid_utf8_comment[..invalid_header_len]).is_err());
    assert!(records::decode_i64_file(&invalid_utf8_comment).is_err());
}
