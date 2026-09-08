use aura_codec::body::{decode_generic_stream_body, encode_generic_stream_body};
use aura_codec::bytes::ByteReader;
use aura_codec::format::SEAL_MAGIC;
use aura_codec::generic_planner::I64SearchEffort;
use aura_codec::records::{
    compile_i64_file, compile_i64_file_with_aura0_profile, decode_i64_events_file,
    decode_i64_file_metadata, Aura0FileProfile,
};
use aura_codec::{
    generic_i64_parent_schema, Aura0ByteLaneCodec, AuraI64EventWriter, GenericStreamOp, I64Event,
    Profile,
};

fn writer() -> AuraI64EventWriter {
    let schema =
        generic_i64_parent_schema("transmutation-plan", &[100, 0, 200, 203, 0, 0]).unwrap();
    let mut writer = AuraI64EventWriter::new(schema)
        .with_stream(7, 11)
        .with_header_comment("exact source metadata");
    for i in 0..24i64 {
        writer
            .push_event(I64Event {
                event_values: vec![1000 + i / 2, i / 2],
                children: if i % 4 == 0 {
                    vec![]
                } else {
                    vec![vec![0, i64::MIN, 0], vec![1, i64::MAX, i - 10]]
                },
            })
            .unwrap();
    }
    writer
}

// Produce a valid, deliberately nonoptimal child-count stream. This prevents
// a planner rerun from accidentally passing a plan-preservation assertion.
fn alternate_count_codec(aura0: &[u8]) -> Vec<u8> {
    let metadata = decode_i64_file_metadata(aura0).unwrap();
    let mut footer = metadata.compiled_footer.unwrap();
    let plan = footer.generic_aura0_plan.as_mut().unwrap();
    let first = plan.streams.first_mut().unwrap();
    let old = first.clone();
    first.op = GenericStreamOp::BaseBitpack {
        base: 0,
        unit: 1,
        bit_width: 64,
    };
    let mut reader = ByteReader::new(&aura0[metadata.header_len..metadata.footer_start]);
    let count = reader.read_u16_le().unwrap();
    let mut body = count.to_le_bytes().to_vec();
    for _ in 0..count {
        let id = reader.read_u16_le().unwrap();
        let values = reader.read_u64_le().unwrap();
        let length = reader.read_u32_le().unwrap() as usize;
        let bytes = reader.read_exact(length).unwrap();
        let bytes = if id == first.stream_id {
            let decoded = decode_generic_stream_body(&old, bytes, values as usize).unwrap();
            encode_generic_stream_body(first, &decoded).unwrap()
        } else {
            bytes.to_vec()
        };
        body.extend_from_slice(&id.to_le_bytes());
        body.extend_from_slice(&values.to_le_bytes());
        body.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        body.extend_from_slice(&bytes);
    }
    reader.finish().unwrap();
    let footer_bytes = footer.encode().unwrap();
    let mut output = aura0[..metadata.header_len].to_vec();
    output.extend_from_slice(&body);
    output.extend_from_slice(&footer_bytes);
    output.extend_from_slice(&(footer_bytes.len() as u32).to_le_bytes());
    output.extend_from_slice(SEAL_MAGIC);
    output
}

#[test]
fn aura1_preserves_valid_stamped_plan_and_every_explicit_event() {
    let expected = writer().events().to_vec();
    for effort in [
        I64SearchEffort::Full,
        I64SearchEffort::Bounded,
        I64SearchEffort::Fast,
    ] {
        let aura0 = alternate_count_codec(&writer().finish_aura0_with_search(effort).unwrap());
        let source = decode_i64_events_file(&aura0).unwrap();
        assert_eq!(source.events, expected);
        let aura1 = compile_i64_file(&aura0, Profile::Aura1).unwrap();
        let replay = decode_i64_events_file(&aura1).unwrap();
        assert_eq!(replay.events, expected);
        assert_eq!(replay.schema, source.schema);
        assert_eq!(replay.header.stream_id, 7);
        assert_eq!(replay.header.dictionary_id, 11);
        assert_eq!(replay.header.comment, "exact source metadata");
        assert_eq!(replay.header.base_time_ns, source.header.base_time_ns);
        let source_footer = source.compiled_footer.unwrap();
        let replay_footer = replay.compiled_footer.unwrap();
        assert_eq!(
            replay_footer.generic_aura0_plan,
            source_footer.generic_aura0_plan
        );
        assert_eq!(replay_footer.aura1_program, source_footer.aura1_program);
        assert!(replay_footer.aura1_byte_lanes.is_empty());
        // Reverse conversion remains independently decodable even when its
        // compact planner selects different operations.
        let roundtrip = compile_i64_file(&aura1, Profile::Aura0).unwrap();
        assert_eq!(decode_i64_events_file(&roundtrip).unwrap().events, expected);
    }
}

#[test]
fn aura1_transmutation_still_rejects_corrupt_count_stream_and_trailer() {
    let original = alternate_count_codec(&writer().finish_profile(Profile::Aura0).unwrap());
    let metadata = decode_i64_file_metadata(&original).unwrap();
    // First stream uses exact 64-bit counts. Increase its first count while
    // keeping all framing and footer dimensions valid.
    let mut bad_count = original.clone();
    let first_value = metadata.header_len + 2 + 2 + 8 + 4;
    bad_count[first_value..first_value + 8].copy_from_slice(&1000u64.to_le_bytes());
    assert!(compile_i64_file(&bad_count, Profile::Aura1).is_err());
    let mut bad_seal = original;
    *bad_seal.last_mut().unwrap() ^= 1;
    assert!(compile_i64_file(&bad_seal, Profile::Aura1).is_err());
}

#[test]
fn explicit_hybrid_transmutation_removes_embedded_lane_metadata() {
    let expected = writer().events().to_vec();
    let compact = writer().finish_profile(Profile::Aura0).unwrap();
    let hybrid = compile_i64_file_with_aura0_profile(
        &compact,
        Aura0FileProfile::Hybrid,
        Aura0ByteLaneCodec::Lz4,
    )
    .unwrap();
    assert!(!decode_i64_file_metadata(&hybrid)
        .unwrap()
        .compiled_footer
        .unwrap()
        .aura1_byte_lanes
        .is_empty());
    let aura1 = compile_i64_file(&hybrid, Profile::Aura1).unwrap();
    let decoded = decode_i64_events_file(&aura1).unwrap();
    assert_eq!(decoded.events, expected);
    assert!(decoded.compiled_footer.unwrap().aura1_byte_lanes.is_empty());
}
