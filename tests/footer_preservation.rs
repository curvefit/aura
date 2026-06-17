use aura_codec::footer::AuraFooter;
use aura_codec::format::SEAL_MAGIC;
use aura_codec::instructions::{
    DerivedOp, GenericGroupInstruction, GenericInstructionPlan, GenericStreamInstruction,
    GenericStreamOp,
};
use aura_codec::plan::{Aura0Plan, Aura1Plan};
use aura_codec::program::COMPILED_FOOTER_MAGIC;
use aura_codec::program::{CompiledFooter, DecodeProgram};
use aura_codec::schema::generic_i64_parent_schema;
use aura_codec::{records, IngestStats, Profile};

const RICH_PARENT_MAP: &[u8] = &[100, 0, 2, 2, 2, 0, 1, 0, 0, 6, 8];

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

fn all_variant_generic_plan() -> GenericInstructionPlan {
    GenericInstructionPlan {
        streams: vec![
            GenericStreamInstruction {
                stream_id: 0,
                target_slot: Some(0),
                op: GenericStreamOp::FixedStep {
                    base: 1_000,
                    step: 1,
                },
            },
            GenericStreamInstruction {
                stream_id: 1,
                target_slot: Some(1),
                op: GenericStreamOp::PatchedBitpack {
                    base: -2,
                    unit: 1,
                    low_width: 2,
                    high_width: 3,
                    exception_count: 4,
                },
            },
            GenericStreamInstruction {
                stream_id: 2,
                target_slot: Some(2),
                op: GenericStreamOp::BlockLocal {
                    block_size: 8,
                    mode_count: 2,
                },
            },
            GenericStreamInstruction {
                stream_id: 3,
                target_slot: Some(3),
                op: GenericStreamOp::BitplaneRle {
                    base: 0,
                    unit: 1,
                    bit_width: 2,
                },
            },
            GenericStreamInstruction {
                stream_id: 4,
                target_slot: Some(4),
                op: GenericStreamOp::Dictionary {
                    unit: 1,
                    entry_count: 3,
                    code_width: 2,
                },
            },
            GenericStreamInstruction {
                stream_id: 5,
                target_slot: Some(5),
                op: GenericStreamOp::UuidConstMask {
                    constant_bits: 8,
                    variable_bits: 120,
                },
            },
            GenericStreamInstruction {
                stream_id: 6,
                target_slot: None,
                op: GenericStreamOp::Rle {
                    base: 0,
                    unit: 1,
                    bit_width: 3,
                    run_count: 2,
                },
            },
            GenericStreamInstruction {
                stream_id: 7,
                target_slot: None,
                op: GenericStreamOp::Dictionary {
                    unit: 1,
                    entry_count: 2,
                    code_width: 1,
                },
            },
            GenericStreamInstruction {
                stream_id: 8,
                target_slot: None,
                op: GenericStreamOp::Rle {
                    base: 0,
                    unit: 1,
                    bit_width: 1,
                    run_count: 2,
                },
            },
            GenericStreamInstruction {
                stream_id: 9,
                target_slot: None,
                op: GenericStreamOp::BaseBitpack {
                    base: 100,
                    unit: 1,
                    bit_width: 8,
                },
            },
            GenericStreamInstruction {
                stream_id: 10,
                target_slot: None,
                op: GenericStreamOp::Dictionary {
                    unit: 1,
                    entry_count: 4,
                    code_width: 2,
                },
            },
            GenericStreamInstruction {
                stream_id: 11,
                target_slot: None,
                op: GenericStreamOp::BaseBitpack {
                    base: 100,
                    unit: 1,
                    bit_width: 8,
                },
            },
            GenericStreamInstruction {
                stream_id: 12,
                target_slot: None,
                op: GenericStreamOp::PrevDelta {
                    base: 1_000,
                    unit: 1,
                    bit_width: 8,
                },
            },
            GenericStreamInstruction {
                stream_id: 13,
                target_slot: None,
                op: GenericStreamOp::PrevVarint {
                    base: 1_000,
                    unit: 1,
                },
            },
            GenericStreamInstruction {
                stream_id: 14,
                target_slot: None,
                op: GenericStreamOp::PackedDictionary {
                    base: 10,
                    unit: 1,
                    entry_count: 4,
                    entry_width: 4,
                    code_width: 2,
                },
            },
            GenericStreamInstruction {
                stream_id: 15,
                target_slot: None,
                op: GenericStreamOp::HuffmanDictionary {
                    base: 10,
                    unit: 1,
                    entry_count: 4,
                    entry_width: 4,
                    code_lengths: vec![1, 2, 3, 3],
                },
            },
        ],
        groups: vec![
            GenericGroupInstruction::Group {
                group_id: 0,
                event_slots: vec![0, 1, 2],
                repeated_slots: vec![3, 4, 5, 6],
            },
            GenericGroupInstruction::PartitionRuns {
                group_id: 1,
                parent_group_id: 0,
                partition_slot: 3,
                count_stream_id: 6,
                fixed_order: true,
            },
            GenericGroupInstruction::PartitionRunLengths {
                group_id: 8,
                parent_group_id: 0,
                partition_slot: 3,
                fixed_order: true,
                value_stream_id: 8,
                count_stream_id: 6,
                event_count_stream_id: Some(13),
            },
            GenericGroupInstruction::SegmentedDeltaStream {
                group_id: 9,
                parent_group_id: 8,
                output_slot: 4,
                base_stream_id: Some(11),
                first_stream_id: 9,
                delta_stream_id: 10,
            },
            GenericGroupInstruction::GroupValueStream {
                group_id: 10,
                parent_group_id: 8,
                output_slot: 0,
                stream_id: 12,
            },
            GenericGroupInstruction::PresenceMap {
                group_id: 2,
                parent_group_id: 0,
                slots: vec![5, 6],
                stream_id: 3,
            },
            GenericGroupInstruction::DerivedStream {
                group_id: 3,
                parent_group_id: Some(1),
                output_slot: 4,
                op: DerivedOp::FirstOffsetThenDelta,
                input_slots: vec![3],
                stream_id: 2,
            },
            GenericGroupInstruction::DerivedStream {
                group_id: 4,
                parent_group_id: None,
                output_slot: 8,
                op: DerivedOp::MaxPlusResidual,
                input_slots: vec![1, 2],
                stream_id: 1,
            },
            GenericGroupInstruction::DerivedStream {
                group_id: 5,
                parent_group_id: None,
                output_slot: 9,
                op: DerivedOp::MinMinusResidual,
                input_slots: vec![1, 2],
                stream_id: 1,
            },
            GenericGroupInstruction::SparseStream {
                group_id: 6,
                parent_group_id: 0,
                presence_group_id: 2,
                output_slot: 5,
                presence_index: 0,
                stream_id: 7,
            },
            GenericGroupInstruction::PresenceValue {
                group_id: 7,
                parent_group_id: 0,
                presence_group_id: 2,
                output_slot: 7,
                presence_index: 1,
                value: 1,
            },
        ],
    }
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

#[test]
fn generic_instruction_footer_preserves_all_stream_and_group_variants() {
    let schema =
        generic_i64_parent_schema("all_variant_footer", &[100, 0, 0, 204, 0, 0, 0]).unwrap();
    let mut stats = IngestStats::new_for_schema(&schema).unwrap();
    stats
        .observe_i64_record(&schema, &[1_000, 10, 20, 0, 100, 5, 1])
        .unwrap();
    let aura0_plan = Aura0Plan::from_schema_stats(&schema, &stats).unwrap();
    let aura1_plan = Aura1Plan::from_stats(&stats, 4);
    let plan = all_variant_generic_plan();
    let footer = AuraFooter::new(schema.clone(), stats.clone())
        .with_aura0_plan(aura0_plan.clone())
        .with_aura1_plan(aura1_plan.clone())
        .with_generic_aura0_plan(plan.clone());

    let decoded_footer = AuraFooter::decode(&footer.encode().unwrap()).unwrap();
    assert_eq!(Some(&plan), decoded_footer.generic_aura0_plan.as_ref());

    let compiled = CompiledFooter::new(
        schema.clone(),
        stats.record_count,
        aura1_plan.block_capacity,
        DecodeProgram::from_aura0_plan(&aura0_plan, schema.fields.len()).unwrap(),
        DecodeProgram::from_aura1_plan(&aura1_plan, schema.fields.len()).unwrap(),
    )
    .unwrap()
    .with_generic_aura0_plan(plan.clone());

    let decoded_compiled = CompiledFooter::decode(&compiled.encode().unwrap()).unwrap();
    assert_eq!(Some(&plan), decoded_compiled.generic_aura0_plan.as_ref());
}
