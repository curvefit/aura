use aura_codec::instructions::{
    DerivedOp, GenericGroupInstruction, GenericInstructionPlan, GenericStreamInstruction,
    GenericStreamOp,
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
