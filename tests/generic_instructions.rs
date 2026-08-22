use aura_codec::{
    decode_generic_stream_body, encode_generic_stream_body, DerivedExpressionOp, DerivedOp,
    GenericGroupInstruction, GenericInstructionPlan, GenericStreamBodyValue,
    GenericStreamInstruction, GenericStreamOp,
};

#[test]
fn previous_snapshot_same_key_instruction_is_parented_and_self_describing() {
    let stream = GenericStreamInstruction {
        stream_id: 0,
        target_slot: None,
        op: GenericStreamOp::FixedStep { base: 0, step: 0 },
    };
    let group = GenericGroupInstruction::Group {
        group_id: 0,
        event_slots: vec![0],
        repeated_slots: vec![1, 2, 3],
    };
    let derived = |parent_group_id, input_slots| GenericGroupInstruction::DerivedStream {
        group_id: 1,
        parent_group_id,
        output_slot: 3,
        op: DerivedOp::PreviousSnapshotSameKeyResidual,
        input_slots,
        stream_id: 0,
    };
    let valid_derived = derived(Some(0), vec![1, 2]);
    let plan = GenericInstructionPlan {
        streams: vec![stream.clone()],
        groups: vec![group.clone(), valid_derived],
    };
    assert_eq!(
        plan,
        GenericInstructionPlan::decode(&plan.encode().unwrap()).unwrap()
    );

    for malformed in [
        derived(None, vec![1, 2]),
        derived(Some(0), vec![0, 2]),
        derived(Some(0), vec![1, 1]),
        derived(Some(0), vec![1, 3]),
    ] {
        assert!(GenericInstructionPlan {
            streams: vec![stream.clone()],
            groups: vec![group.clone(), malformed],
        }
        .encode()
        .is_err());
    }
}

#[test]
fn previous_mutation_same_key_instruction_declares_reset_and_keys() {
    let stream = GenericStreamInstruction {
        stream_id: 0,
        target_slot: None,
        op: GenericStreamOp::FixedStep { base: 0, step: 0 },
    };
    let group = GenericGroupInstruction::Group {
        group_id: 0,
        event_slots: vec![0, 1],
        repeated_slots: vec![2, 3, 4],
    };
    let derived = |parent_group_id, input_slots| GenericGroupInstruction::DerivedStream {
        group_id: 1,
        parent_group_id,
        output_slot: 4,
        op: DerivedOp::PreviousMutationSameKeyResidual,
        input_slots,
        stream_id: 0,
    };
    let valid = GenericInstructionPlan {
        streams: vec![stream.clone()],
        groups: vec![group.clone(), derived(Some(0), vec![1, 2, 3])],
    };
    assert_eq!(
        valid,
        GenericInstructionPlan::decode(&valid.encode().unwrap()).unwrap()
    );

    for malformed in [
        derived(None, vec![1, 2, 3]),
        derived(Some(0), vec![2, 3]),
        derived(Some(0), vec![1, 0]),
        derived(Some(0), vec![1, 2, 2]),
        derived(Some(0), vec![1, 2, 4]),
    ] {
        assert!(GenericInstructionPlan {
            streams: vec![stream.clone()],
            groups: vec![group.clone(), malformed],
        }
        .encode()
        .is_err());
    }

    let targeted_residual = GenericInstructionPlan {
        streams: vec![GenericStreamInstruction {
            stream_id: 0,
            target_slot: Some(4),
            op: GenericStreamOp::FixedStep { base: 0, step: 0 },
        }],
        groups: vec![group.clone(), derived(Some(0), vec![1, 2, 3])],
    };
    assert!(targeted_residual.encode().is_err());

    let mut duplicate_output = derived(Some(0), vec![1, 2, 3]);
    let GenericGroupInstruction::DerivedStream { group_id, .. } = &mut duplicate_output else {
        unreachable!();
    };
    *group_id = 2;
    let duplicate_producer = GenericInstructionPlan {
        streams: vec![stream],
        groups: vec![group, derived(Some(0), vec![1, 2, 3]), duplicate_output],
    };
    assert!(duplicate_producer.encode().is_err());
}

#[test]
fn previous_output_by_key_instruction_declares_reset_and_keys() {
    let stream = GenericStreamInstruction {
        stream_id: 0,
        target_slot: None,
        op: GenericStreamOp::FixedStep { base: 0, step: 0 },
    };
    let group = GenericGroupInstruction::Group {
        group_id: 0,
        event_slots: vec![0, 1],
        repeated_slots: vec![2, 3],
    };
    let derived = |parent_group_id, input_slots| GenericGroupInstruction::DerivedStream {
        group_id: 1,
        parent_group_id,
        output_slot: 3,
        op: DerivedOp::PreviousOutputByKeyResidual,
        input_slots,
        stream_id: 0,
    };
    let valid = GenericInstructionPlan {
        streams: vec![stream.clone()],
        groups: vec![group.clone(), derived(Some(0), vec![1, 2])],
    };
    assert_eq!(
        valid,
        GenericInstructionPlan::decode(&valid.encode().unwrap()).unwrap()
    );

    for malformed in [
        derived(None, vec![1, 2]),
        derived(Some(0), vec![2]),
        derived(Some(0), vec![1, 0]),
        derived(Some(0), vec![1, 2, 2]),
        derived(Some(0), vec![1, 2, 3]),
    ] {
        assert!(GenericInstructionPlan {
            streams: vec![stream.clone()],
            groups: vec![group.clone(), malformed],
        }
        .encode()
        .is_err());
    }
}

#[test]
fn fixed_stride_delta_stream_round_trips_instruction_and_values() {
    let instruction = GenericStreamInstruction {
        stream_id: 7,
        target_slot: Some(2),
        op: GenericStreamOp::FixedStrideDelta {
            stride: 3,
            residual_op: Box::new(GenericStreamOp::BaseBitpack {
                base: 1,
                unit: 1,
                bit_width: 10,
            }),
        },
    };
    let plan = GenericInstructionPlan {
        streams: vec![instruction.clone()],
        groups: Vec::new(),
    };
    assert_eq!(
        plan,
        GenericInstructionPlan::decode(&plan.encode().unwrap()).unwrap()
    );

    let values = vec![100, 200, 300, 101, 202, 303, 103, 205, 307];
    let body =
        encode_generic_stream_body(&instruction, &GenericStreamBodyValue::I64(values.clone()))
            .unwrap();
    assert_eq!(
        GenericStreamBodyValue::I64(values),
        decode_generic_stream_body(&instruction, &body, 9).unwrap()
    );

    let invalid = GenericInstructionPlan {
        streams: vec![GenericStreamInstruction {
            stream_id: 0,
            target_slot: Some(0),
            op: GenericStreamOp::FixedStrideDelta {
                stride: 0,
                residual_op: Box::new(GenericStreamOp::FixedStep { base: 0, step: 0 }),
            },
        }],
        groups: Vec::new(),
    };
    assert!(invalid.encode().is_err());
}

#[test]
fn composed_temporal_delta_streams_round_trip_instructions_and_values() {
    let values = vec![1_000, 1_010, 1_020, 1_035, 1_050, 1_070];
    for op in [
        GenericStreamOp::PreviousValueDelta {
            residual_op: Box::new(GenericStreamOp::BaseBitpack {
                base: 10,
                unit: 5,
                bit_width: 8,
            }),
        },
        GenericStreamOp::DeltaOfDelta {
            residual_op: Box::new(GenericStreamOp::PatchedBitpack {
                base: 0,
                unit: 5,
                low_width: 2,
                high_width: 8,
                exception_count: 1,
            }),
        },
    ] {
        let instruction = GenericStreamInstruction {
            stream_id: 4,
            target_slot: Some(0),
            op,
        };
        let plan = GenericInstructionPlan {
            streams: vec![instruction.clone()],
            groups: Vec::new(),
        };
        assert_eq!(
            plan,
            GenericInstructionPlan::decode(&plan.encode().unwrap()).unwrap()
        );
        let body =
            encode_generic_stream_body(&instruction, &GenericStreamBodyValue::I64(values.clone()))
                .unwrap();
        assert_eq!(
            GenericStreamBodyValue::I64(values.clone()),
            decode_generic_stream_body(&instruction, &body, values.len()).unwrap()
        );
    }
}

#[test]
fn zstd_varint_round_trips_footer_values_and_composed_residuals() {
    let raw_len = |values: &[i64]| {
        values
            .iter()
            .map(|value| {
                let mut bytes = Vec::new();
                aura_codec::varint::encode_i64(*value, &mut bytes);
                bytes.len() as u64
            })
            .sum()
    };
    let values = vec![i64::MIN, -16_384, -1, 0, 1, 127, 128, 16_384, i64::MAX];
    let composed_values = vec![100, 103, 107, 112, 118, 125];
    let previous_residuals = vec![100, 3, 4, 5, 6, 7];
    let delta2_residuals = vec![100, 3, 1, 1, 1, 1];
    let stride_residuals = vec![100, 103, 7, 9, 11, 13];
    for (values, op) in [
        (
            Vec::new(),
            GenericStreamOp::ZstdVarint {
                unit: 1,
                raw_len: 0,
            },
        ),
        (
            values,
            GenericStreamOp::ZstdVarint {
                unit: 1,
                raw_len: raw_len(&[i64::MIN, -16_384, -1, 0, 1, 127, 128, 16_384, i64::MAX]),
            },
        ),
        (
            composed_values.clone(),
            GenericStreamOp::PreviousValueDelta {
                residual_op: Box::new(GenericStreamOp::ZstdVarint {
                    unit: 1,
                    raw_len: raw_len(&previous_residuals),
                }),
            },
        ),
        (
            composed_values.clone(),
            GenericStreamOp::DeltaOfDelta {
                residual_op: Box::new(GenericStreamOp::ZstdVarint {
                    unit: 1,
                    raw_len: raw_len(&delta2_residuals),
                }),
            },
        ),
        (
            composed_values,
            GenericStreamOp::FixedStrideDelta {
                stride: 2,
                residual_op: Box::new(GenericStreamOp::ZstdVarint {
                    unit: 1,
                    raw_len: raw_len(&stride_residuals),
                }),
            },
        ),
    ] {
        let instruction = GenericStreamInstruction {
            stream_id: 31,
            target_slot: Some(0),
            op,
        };
        let plan = GenericInstructionPlan {
            streams: vec![instruction.clone()],
            groups: Vec::new(),
        };
        assert_eq!(
            plan,
            GenericInstructionPlan::decode(&plan.encode().unwrap()).unwrap()
        );
        let body =
            encode_generic_stream_body(&instruction, &GenericStreamBodyValue::I64(values.clone()))
                .unwrap();
        assert_eq!(
            GenericStreamBodyValue::I64(values.clone()),
            decode_generic_stream_body(&instruction, &body, values.len()).unwrap()
        );
    }
}

#[test]
fn zstd_varint_rejects_noncanonical_or_malformed_frames_without_allocating_from_stamp() {
    let instruction = |unit, raw_len| GenericStreamInstruction {
        stream_id: 0,
        target_slot: Some(0),
        op: GenericStreamOp::ZstdVarint { unit, raw_len },
    };
    let compressed = |raw: &[u8]| zstd::bulk::compress(raw, 3).unwrap();

    for raw in [&[0x80, 0x00][..], &[0xff; 10][..]] {
        assert!(
            decode_generic_stream_body(&instruction(1, raw.len() as u64), &compressed(raw), 1,)
                .is_err()
        );
    }
    let two_values = compressed(&[0, 0]);
    assert!(decode_generic_stream_body(&instruction(1, 2), &two_values, 1).is_err());

    let frame = compressed(&[0]);
    let mut trailing = frame.clone();
    trailing.push(0);
    assert!(decode_generic_stream_body(&instruction(1, 1), &trailing, 1).is_err());
    let mut concatenated = frame.clone();
    concatenated.extend_from_slice(&frame);
    assert!(decode_generic_stream_body(&instruction(1, 1), &concatenated, 1).is_err());

    let large_value_count = isize::MAX as usize / std::mem::size_of::<i64>();
    let large_stamp = u64::try_from(large_value_count).unwrap();
    let tiny_frame_large_stamp = std::panic::catch_unwind(|| {
        decode_generic_stream_body(&instruction(1, large_stamp), &frame, large_value_count)
    });
    assert!(tiny_frame_large_stamp.is_ok());
    assert!(tiny_frame_large_stamp.unwrap().is_err());

    let empty_skippable_frame = [0x50, 0x2a, 0x4d, 0x18, 0, 0, 0, 0];
    assert!(decode_generic_stream_body(&instruction(1, 0), &empty_skippable_frame, 0).is_err());

    let huge_stamp = instruction(1, u64::MAX);
    let result = std::panic::catch_unwind(|| decode_generic_stream_body(&huge_stamp, &[], 0));
    assert!(result.is_ok());
    assert!(result.unwrap().is_err());

    let overflow_raw = {
        let mut raw = Vec::new();
        aura_codec::varint::encode_i64(i64::MAX, &mut raw);
        raw
    };
    assert!(decode_generic_stream_body(
        &instruction(2, overflow_raw.len() as u64),
        &compressed(&overflow_raw),
        1,
    )
    .is_err());
    assert!(
        encode_generic_stream_body(&instruction(2, 1), &GenericStreamBodyValue::I64(vec![3]),)
            .is_err()
    );
    assert!(
        encode_generic_stream_body(&instruction(1, 2), &GenericStreamBodyValue::I64(vec![0]),)
            .is_err()
    );
}

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
            GenericStreamInstruction {
                stream_id: 13,
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
fn generic_instruction_plan_round_trips_expression_stream() {
    let plan = GenericInstructionPlan {
        streams: vec![GenericStreamInstruction {
            stream_id: 0,
            target_slot: None,
            op: GenericStreamOp::BaseBitpack {
                base: 0,
                unit: 1,
                bit_width: 3,
            },
        }],
        groups: vec![GenericGroupInstruction::ExpressionStream {
            group_id: 0,
            parent_group_id: None,
            output_slot: 3,
            op: DerivedExpressionOp::Mul,
            input_slots: vec![1, 2],
            literals: vec![100],
            stream_id: 0,
        }],
    };

    let encoded = plan.encode().unwrap();
    let decoded = GenericInstructionPlan::decode(&encoded).unwrap();

    assert_eq!(plan, decoded);
}

#[test]
fn generic_instruction_plan_round_trips_scaled_product_expression() {
    let plan = GenericInstructionPlan {
        streams: Vec::new(),
        groups: vec![GenericGroupInstruction::ExpressionValue {
            group_id: 0,
            parent_group_id: None,
            output_slot: 3,
            op: DerivedExpressionOp::MulDiv,
            input_slots: vec![1, 2],
            literals: vec![100_000_000],
            residual: 0,
        }],
    };

    assert_eq!(
        plan,
        GenericInstructionPlan::decode(&plan.encode().unwrap()).unwrap()
    );

    let invalid = GenericInstructionPlan {
        streams: Vec::new(),
        groups: vec![GenericGroupInstruction::ExpressionValue {
            group_id: 0,
            parent_group_id: None,
            output_slot: 3,
            op: DerivedExpressionOp::MulDiv,
            input_slots: vec![1, 2],
            literals: vec![0],
            residual: 0,
        }],
    };
    assert!(invalid.encode().is_err());
}

#[test]
fn generic_instruction_plan_round_trips_quotient_remainder_group() {
    let plan = GenericInstructionPlan {
        streams: vec![
            GenericStreamInstruction {
                stream_id: 0,
                target_slot: Some(0),
                op: GenericStreamOp::FixedStep { base: 10, step: 1 },
            },
            GenericStreamInstruction {
                stream_id: 1,
                target_slot: None,
                op: GenericStreamOp::BaseBitpack {
                    base: 100,
                    unit: 1,
                    bit_width: 8,
                },
            },
            GenericStreamInstruction {
                stream_id: 2,
                target_slot: None,
                op: GenericStreamOp::BaseBitpack {
                    base: 0,
                    unit: 1,
                    bit_width: 8,
                },
            },
        ],
        groups: vec![GenericGroupInstruction::QuotientRemainder {
            group_id: 0,
            parent_group_id: None,
            output_slot: 1,
            divisor_slot: 0,
            quotient_stream_id: 1,
            remainder_stream_id: 2,
        }],
    };

    assert_eq!(
        plan,
        GenericInstructionPlan::decode(&plan.encode().unwrap()).unwrap()
    );
}

#[test]
fn generic_instruction_plan_round_trips_expression_value() {
    let plan = GenericInstructionPlan {
        streams: Vec::new(),
        groups: vec![GenericGroupInstruction::ExpressionValue {
            group_id: 0,
            parent_group_id: None,
            output_slot: 3,
            op: DerivedExpressionOp::Add,
            input_slots: vec![1, 2],
            literals: vec![5],
            residual: 0,
        }],
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
fn generic_instruction_plan_rejects_duplicate_ids_and_invalid_refs() {
    let duplicate_streams = GenericInstructionPlan {
        streams: vec![
            GenericStreamInstruction {
                stream_id: 0,
                target_slot: Some(0),
                op: GenericStreamOp::FixedStep { base: 0, step: 1 },
            },
            GenericStreamInstruction {
                stream_id: 0,
                target_slot: Some(1),
                op: GenericStreamOp::FixedStep { base: 0, step: 1 },
            },
        ],
        groups: Vec::new(),
    };
    assert!(duplicate_streams.encode().is_err());

    let invalid_group_ref = GenericInstructionPlan {
        streams: vec![GenericStreamInstruction {
            stream_id: 0,
            target_slot: Some(0),
            op: GenericStreamOp::FixedStep { base: 0, step: 1 },
        }],
        groups: vec![GenericGroupInstruction::PartitionRuns {
            group_id: 1,
            parent_group_id: 99,
            partition_slot: 0,
            count_stream_id: 0,
            fixed_order: true,
        }],
    };
    assert!(invalid_group_ref.encode().is_err());

    let invalid_stream_ref = GenericInstructionPlan {
        streams: vec![GenericStreamInstruction {
            stream_id: 0,
            target_slot: Some(0),
            op: GenericStreamOp::FixedStep { base: 0, step: 1 },
        }],
        groups: vec![
            GenericGroupInstruction::Group {
                group_id: 0,
                event_slots: vec![0],
                repeated_slots: vec![1],
            },
            GenericGroupInstruction::PartitionRuns {
                group_id: 1,
                parent_group_id: 0,
                partition_slot: 1,
                count_stream_id: 99,
                fixed_order: true,
            },
        ],
    };
    assert!(invalid_stream_ref.encode().is_err());

    let duplicate_groups = GenericInstructionPlan {
        streams: Vec::new(),
        groups: vec![
            GenericGroupInstruction::Group {
                group_id: 0,
                event_slots: vec![0],
                repeated_slots: vec![1],
            },
            GenericGroupInstruction::Group {
                group_id: 0,
                event_slots: vec![0],
                repeated_slots: vec![1],
            },
        ],
    };
    assert!(duplicate_groups.encode().is_err());
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
        GenericStreamOp::HuffmanDictionary {
            base: 10,
            unit: 10,
            entry_count: 4,
            entry_width: 2,
            code_lengths: vec![1, 2, 3, 3],
        },
        &[10, 10, 10, 20, 10, 30, 40, 20, 10],
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
fn huffman_instruction_packs_code_lengths_in_footer_plan() {
    let code_lengths = vec![4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 15, 15, 15];
    let huffman = GenericStreamOp::HuffmanDictionary {
        base: 10,
        unit: 1,
        entry_count: code_lengths.len() as u32,
        entry_width: 6,
        code_lengths,
    };
    let legacy_len = 1 + 8 + 8 + 4 + 1 + 16;

    assert!(huffman.encoded_len().unwrap() < legacy_len);

    let plan = GenericInstructionPlan {
        streams: vec![GenericStreamInstruction {
            stream_id: 0,
            target_slot: Some(0),
            op: huffman,
        }],
        groups: Vec::new(),
    };

    assert_eq!(
        plan,
        GenericInstructionPlan::decode(&plan.encode().unwrap()).unwrap()
    );
}

#[test]
fn legacy_huffman_instruction_raw_code_lengths_still_decodes() {
    let mut encoded = Vec::new();
    encoded.extend_from_slice(b"AURI");
    encoded.push(1);
    encoded.extend_from_slice(&1u16.to_le_bytes());
    encoded.extend_from_slice(&0u16.to_le_bytes());
    encoded.extend_from_slice(&7u16.to_le_bytes());
    encoded.extend_from_slice(&u16::MAX.to_le_bytes());
    encoded.push(11);
    encoded.extend_from_slice(&10i64.to_le_bytes());
    encoded.extend_from_slice(&1i64.to_le_bytes());
    encoded.extend_from_slice(&4u32.to_le_bytes());
    encoded.push(4);
    encoded.extend_from_slice(&[1, 2, 3, 3]);

    let expected = GenericInstructionPlan {
        streams: vec![GenericStreamInstruction {
            stream_id: 7,
            target_slot: None,
            op: GenericStreamOp::HuffmanDictionary {
                base: 10,
                unit: 1,
                entry_count: 4,
                entry_width: 4,
                code_lengths: vec![1, 2, 3, 3],
            },
        }],
        groups: Vec::new(),
    };

    assert_eq!(expected, GenericInstructionPlan::decode(&encoded).unwrap());
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
