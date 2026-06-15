use aura_codec::{
    decode_generic_stream_body, encode_generic_stream_body, DerivedOp, GenericGroupInstruction,
    GenericInstructionPlan, GenericStreamBodyValue, GenericStreamInstruction, GenericStreamOp,
};

#[test]
fn generic_instruction_plan_round_trips_grouped_curvefit_shape() {
    let plan = GenericInstructionPlan {
        streams: vec![
            GenericStreamInstruction {
                stream_id: 0,
                target_slot: Some(0),
                op: GenericStreamOp::FixedStep {
                    base: 1_773_693_720_000_000_000,
                    step: 60_000_000_000,
                },
            },
            GenericStreamInstruction {
                stream_id: 1,
                target_slot: Some(1),
                op: GenericStreamOp::PatchedBitpack {
                    base: -2,
                    unit: 1_000_000,
                    low_width: 2,
                    high_width: 1,
                    exception_count: 38,
                },
            },
            GenericStreamInstruction {
                stream_id: 2,
                target_slot: Some(2),
                op: GenericStreamOp::BlockLocal {
                    block_size: 512,
                    mode_count: 254,
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
                    unit: 1_000,
                    entry_count: 8_692,
                    code_width: 14,
                },
            },
            GenericStreamInstruction {
                stream_id: 5,
                target_slot: Some(5),
                op: GenericStreamOp::UuidConstMask {
                    constant_bits: 6,
                    variable_bits: 122,
                },
            },
            GenericStreamInstruction {
                stream_id: 6,
                target_slot: None,
                op: GenericStreamOp::Rle {
                    base: 0,
                    unit: 100_000,
                    bit_width: 32,
                    run_count: 50_344,
                },
            },
            GenericStreamInstruction {
                stream_id: 7,
                target_slot: None,
                op: GenericStreamOp::Dictionary {
                    unit: 1,
                    entry_count: 32,
                    code_width: 5,
                },
            },
            GenericStreamInstruction {
                stream_id: 8,
                target_slot: None,
                op: GenericStreamOp::Rle {
                    base: 0,
                    unit: 1,
                    bit_width: 1,
                    run_count: 24,
                },
            },
            GenericStreamInstruction {
                stream_id: 9,
                target_slot: None,
                op: GenericStreamOp::BaseBitpack {
                    base: 100_000,
                    unit: 10,
                    bit_width: 12,
                },
            },
            GenericStreamInstruction {
                stream_id: 10,
                target_slot: None,
                op: GenericStreamOp::Dictionary {
                    unit: 10,
                    entry_count: 3,
                    code_width: 2,
                },
            },
            GenericStreamInstruction {
                stream_id: 11,
                target_slot: None,
                op: GenericStreamOp::BaseBitpack {
                    base: 100_000,
                    unit: 10,
                    bit_width: 12,
                },
            },
            GenericStreamInstruction {
                stream_id: 12,
                target_slot: None,
                op: GenericStreamOp::PrevDelta {
                    base: 1_773_693_720_000_000_000,
                    unit: 1_000_000,
                    bit_width: 12,
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
                event_count_stream_id: None,
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
                slots: vec![5, 6, 7],
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
                presence_index: 2,
                value: 1,
            },
        ],
    };

    let encoded = plan.encode().unwrap();
    let decoded = GenericInstructionPlan::decode(&encoded).unwrap();

    assert_eq!(plan, decoded);
}

#[test]
fn generic_instruction_plan_rejects_invalid_uuid_mask_shape() {
    let plan = GenericInstructionPlan {
        streams: vec![GenericStreamInstruction {
            stream_id: 0,
            target_slot: Some(0),
            op: GenericStreamOp::UuidConstMask {
                constant_bits: 7,
                variable_bits: 122,
            },
        }],
        groups: Vec::new(),
    };

    assert!(plan.encode().is_err());
}

#[test]
fn generic_stream_body_round_trips_core_i64_ops() {
    assert_i64_body_round_trip(
        GenericStreamOp::FixedStep { base: 100, step: 5 },
        &[100, 105, 110, 115],
    );
    assert_i64_body_round_trip(
        GenericStreamOp::BaseBitpack {
            base: 10,
            unit: 5,
            bit_width: 3,
        },
        &[10, 15, 20, 25],
    );
    assert_i64_body_round_trip(
        GenericStreamOp::PrevDelta {
            base: 100,
            unit: 10,
            bit_width: 3,
        },
        &[100, 110, 130, 120, 140],
    );
    assert_i64_body_round_trip(
        GenericStreamOp::PrevVarint { base: 100, unit: 1 },
        &[100, 101, 103, 102, 106, 107],
    );
    assert_i64_body_round_trip(
        GenericStreamOp::Rle {
            base: 0,
            unit: 1,
            bit_width: 3,
            run_count: 3,
        },
        &[2, 2, 2, 5, 5, 1],
    );
    assert_i64_body_round_trip(
        GenericStreamOp::PatchedBitpack {
            base: 0,
            unit: 1,
            low_width: 2,
            high_width: 2,
            exception_count: 2,
        },
        &[0, 1, 2, 3, 4, 7],
    );
    assert_i64_body_round_trip(
        GenericStreamOp::BitplaneRle {
            base: 0,
            unit: 1,
            bit_width: 3,
        },
        &[0, 1, 1, 3, 7, 7, 0],
    );
    assert_i64_body_round_trip(
        GenericStreamOp::Dictionary {
            unit: 10,
            entry_count: 3,
            code_width: 2,
        },
        &[10, 20, 10, 30, 20],
    );
    assert_i64_body_round_trip(
        GenericStreamOp::PackedDictionary {
            base: 10,
            unit: 10,
            entry_count: 3,
            entry_width: 2,
            code_width: 2,
        },
        &[10, 20, 10, 30, 20],
    );
    assert_i64_body_round_trip(
        GenericStreamOp::BlockLocal {
            block_size: 4,
            mode_count: 2,
        },
        &[100, 101, 102, 103, 8_000, 8_000, 8_004, 8_008],
    );
}

#[test]
fn generic_uuid_const_mask_body_round_trips_u128_values() {
    let instruction = GenericStreamInstruction {
        stream_id: 0,
        target_slot: Some(0),
        op: GenericStreamOp::UuidConstMask {
            constant_bits: 6,
            variable_bits: 122,
        },
    };
    let constant_prefix = 0b101010u128 << 122;
    let values = vec![
        constant_prefix | 1,
        constant_prefix | 2,
        constant_prefix | 3,
        constant_prefix | (1u128 << 80) | 9,
    ];

    let encoded =
        encode_generic_stream_body(&instruction, &GenericStreamBodyValue::U128(values.clone()))
            .unwrap();
    let decoded = decode_generic_stream_body(&instruction, &encoded, values.len()).unwrap();

    assert_eq!(GenericStreamBodyValue::U128(values), decoded);
}

#[test]
fn generic_stream_body_rejects_mismatched_instruction() {
    let instruction = GenericStreamInstruction {
        stream_id: 0,
        target_slot: Some(0),
        op: GenericStreamOp::BaseBitpack {
            base: 10,
            unit: 5,
            bit_width: 2,
        },
    };

    let result =
        encode_generic_stream_body(&instruction, &GenericStreamBodyValue::I64(vec![10, 12]));

    assert!(result.is_err());
}

#[test]
fn generic_bitplane_rle_rejects_values_outside_stamped_width() {
    let instruction = GenericStreamInstruction {
        stream_id: 0,
        target_slot: Some(0),
        op: GenericStreamOp::BitplaneRle {
            base: 0,
            unit: 1,
            bit_width: 2,
        },
    };

    let result =
        encode_generic_stream_body(&instruction, &GenericStreamBodyValue::I64(vec![0, 1, 2, 4]));

    assert!(result.is_err());
}

#[test]
fn generic_block_local_falls_back_when_fixed_step_probe_overflows() {
    assert_i64_body_round_trip(
        GenericStreamOp::BlockLocal {
            block_size: 2,
            mode_count: 2,
        },
        &[i64::MIN, i64::MAX, i64::MAX - 1, i64::MIN + 1],
    );
}

#[test]
fn generic_block_local_can_use_previous_delta_local_mode() {
    let instruction = GenericStreamInstruction {
        stream_id: 0,
        target_slot: Some(0),
        op: GenericStreamOp::BlockLocal {
            block_size: 8,
            mode_count: 1,
        },
    };
    let values = vec![
        1_000_000, 1_000_003, 1_000_004, 1_000_010, 1_000_012, 1_000_015, 1_000_016, 1_000_020,
    ];

    let encoded =
        encode_generic_stream_body(&instruction, &GenericStreamBodyValue::I64(values.clone()))
            .unwrap();
    let decoded = decode_generic_stream_body(&instruction, &encoded, values.len()).unwrap();

    assert_eq!(Some(&2), encoded.first());
    assert_eq!(GenericStreamBodyValue::I64(values), decoded);
}

fn assert_i64_body_round_trip(op: GenericStreamOp, values: &[i64]) {
    let instruction = GenericStreamInstruction {
        stream_id: 0,
        target_slot: Some(0),
        op,
    };
    let encoded =
        encode_generic_stream_body(&instruction, &GenericStreamBodyValue::I64(values.to_vec()))
            .unwrap();
    let decoded = decode_generic_stream_body(&instruction, &encoded, values.len()).unwrap();

    assert_eq!(GenericStreamBodyValue::I64(values.to_vec()), decoded);
}
