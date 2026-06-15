use aura_codec::format::SEAL_MAGIC;
use aura_codec::program::COMPILED_FOOTER_MAGIC;
use aura_codec::schema::generic_i64_parent_schema;
use aura_codec::{records, Profile};

const RICH_PARENT_MAP: &[u8] = &[255, 0, 2, 2, 2, 0, 1, 0, 0, 6, 8];

fn rich_rows() -> Vec<Vec<i64>> {
    (0..48)
        .map(|idx| {
            let open = 10_000 + i64::from(idx % 5) * 10;
            let close = open + i64::from(idx % 7) - 3;
            let high = open.max(close) + i64::from(idx % 4);
            let low = open.min(close) - i64::from(idx % 3);
            let volume = 1_000 + i64::from(idx * 10);
            let quote = volume * low + i64::from(idx % 11);
            let taker_base = volume / 3;
            let taker_quote = quote * taker_base / volume + i64::from(idx % 13);
            vec![
                i64::from(idx) * 60_000,
                open,
                high,
                low,
                close,
                volume,
                i64::from(idx) * 60_000 + 59_999,
                quote,
                i64::from(idx),
                taker_base,
                taker_quote,
            ]
        })
        .collect()
}

fn encoded_ingest() -> Vec<u8> {
    let schema = generic_i64_parent_schema("footer_preservation_v1", RICH_PARENT_MAP).unwrap();
    records::encode_ingest_i64_file(records::I64FileInput {
        schema,
        rows: rich_rows(),
        stream_id: 2,
        dictionary_id: 9,
        header_comment: None,
    })
    .unwrap()
}

fn compiled_footer_bytes(file: &[u8]) -> Vec<u8> {
    assert_eq!(SEAL_MAGIC, &file[file.len() - SEAL_MAGIC.len()..]);
    let start = footer_start(file);
    let bytes = file[start..footer_len_offset(file)].to_vec();
    assert_eq!(COMPILED_FOOTER_MAGIC, &bytes[0..4]);
    bytes
}

fn footer_start(file: &[u8]) -> usize {
    footer_len_offset(file) - footer_len(file)
}

fn footer_len_offset(file: &[u8]) -> usize {
    file.len() - SEAL_MAGIC.len() - 4
}

fn footer_len(file: &[u8]) -> usize {
    let offset = footer_len_offset(file);
    read_u32_le(&file[offset..offset + 4]) as usize
}

fn body_bytes(file: &[u8]) -> &[u8] {
    let header_len = usize::from(file[7]);
    &file[header_len..footer_start(file)]
}

fn read_u32_le(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(bytes.try_into().unwrap())
}

#[test]
fn compiled_profile_hotswaps_preserve_footer_bytes() {
    let rows = rich_rows();
    let ingest = encoded_ingest();
    let aura0 = records::compile_i64_file(&ingest, Profile::Aura0).unwrap();
    let aura1 = records::compile_i64_file(&aura0, Profile::Aura1).unwrap();
    let aura0_again = records::compile_i64_file(&aura1, Profile::Aura0).unwrap();

    let original_footer = compiled_footer_bytes(&aura0);
    assert_eq!(original_footer, compiled_footer_bytes(&aura1));
    assert_eq!(original_footer, compiled_footer_bytes(&aura0_again));
    assert_ne!(body_bytes(&aura0), body_bytes(&aura1));

    for file in [&aura0, &aura1, &aura0_again] {
        assert_eq!(rows, records::decode_i64_file(file).unwrap().rows);
    }
}

#[test]
fn compiled_hotswap_preserves_generic_aura0_plan() {
    let ingest = encoded_ingest();
    let ingest_decoded = records::decode_i64_file(&ingest).unwrap();
    let ingest_plan = ingest_decoded
        .ingest_footer
        .as_ref()
        .unwrap()
        .generic_aura0_plan
        .clone()
        .unwrap();
    assert!(!ingest_plan.streams.is_empty() || !ingest_plan.groups.is_empty());

    let aura0 = records::compile_i64_file(&ingest, Profile::Aura0).unwrap();
    let aura1 = records::compile_i64_file(&aura0, Profile::Aura1).unwrap();
    let aura0_again = records::compile_i64_file(&aura1, Profile::Aura0).unwrap();

    for file in [&aura0, &aura1, &aura0_again] {
        let decoded = records::decode_i64_file(file).unwrap();
        let footer = decoded.compiled_footer.as_ref().unwrap();
        assert_eq!(Some(&ingest_plan), footer.generic_aura0_plan.as_ref());
        assert_eq!(RICH_PARENT_MAP.len(), footer.aura0_program.fields.len());
        assert_eq!(RICH_PARENT_MAP.len(), footer.aura1_program.fields.len());
    }
}

#[test]
fn compiled_footer_identity_survives_round_trip_chain() {
    let rows = rich_rows();
    let ingest = encoded_ingest();
    let mut compiled = records::compile_i64_file(&ingest, Profile::Aura0).unwrap();
    let original_footer = compiled_footer_bytes(&compiled);

    for target in [
        Profile::Aura1,
        Profile::Aura0,
        Profile::Aura1,
        Profile::Aura0,
        Profile::Aura1,
    ] {
        compiled = records::compile_i64_file(&compiled, target).unwrap();
        assert_eq!(original_footer, compiled_footer_bytes(&compiled));
        let decoded = records::decode_i64_file(&compiled).unwrap();
        assert_eq!(target, decoded.header.profile);
        assert_eq!(rows, decoded.rows);
    }
}
