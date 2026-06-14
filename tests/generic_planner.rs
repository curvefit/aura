use aura_codec::{
    decode_generic_i64_rows, encode_generic_i64_rows, plan_uuid_const_mask_stream, DerivedOp,
    GenericGroupInstruction, GenericStreamBodyValue, GenericStreamOp,
};
use aura_codec::{decode_generic_stream_body, encode_generic_stream_body};
use aura_codec::{generic_i64_parent_schema, FieldScope};

#[test]
fn generic_planner_derives_candle_shape_from_parent_hints() {
    let schema = generic_i64_parent_schema("candles", &[255, 0, 2, 2, 2, 0]).unwrap();
    let rows = vec![
        vec![1_000, 100_000, 100_250, 99_900, 100_120, 8_000],
        vec![2_000, 100_130, 100_400, 100_050, 100_300, 8_100],
        vec![3_000, 100_290, 100_310, 100_000, 100_050, 8_050],
        vec![4_000, 100_060, 100_500, 99_990, 100_450, 8_090],
    ];

    let encoded = encode_generic_i64_rows(&schema, &rows).unwrap();
    let decoded = decode_generic_i64_rows(&encoded).unwrap();

    assert_eq!(rows, decoded);
    assert!(encoded.encoded_body_len() < rows.len() * rows[0].len() * 8);
    assert!(encoded.plan.streams.iter().any(|stream| {
        stream.target_slot == Some(0)
            && matches!(
                stream.op,
                GenericStreamOp::FixedStep {
                    base: 1_000,
                    step: 1_000
                }
            )
    }));
    assert!(encoded.plan.groups.iter().any(|group| {
        matches!(
            group,
            GenericGroupInstruction::DerivedStream {
                output_slot: 1,
                op: DerivedOp::FirstOffsetThenDelta,
                input_slots,
                ..
            } if input_slots.as_slice() == [4]
        )
    }));
    assert!(encoded.plan.groups.iter().any(|group| {
        matches!(
            group,
            GenericGroupInstruction::DerivedStream {
                output_slot: 2,
                op: DerivedOp::MaxPlusResidual,
                input_slots,
                ..
            } if input_slots.as_slice() == [1, 4]
        )
    }));
    assert!(encoded.plan.groups.iter().any(|group| {
        matches!(
            group,
            GenericGroupInstruction::DerivedStream {
                output_slot: 3,
                op: DerivedOp::MinMinusResidual,
                input_slots,
                ..
            } if input_slots.as_slice() == [1, 4]
        )
    }));
}

#[test]
fn generic_planner_selects_tick_stream_ops_without_field_names() {
    let schema = generic_i64_parent_schema("ticks", &[255, 0, 0, 0, 0, 0]).unwrap();
    let rows = (0..300)
        .map(|index| {
            vec![
                1_000_000 + (index / 100) * 100,
                10 + index,
                100_000 + (index % 7) * 10,
                5 + (index % 3),
                index % 2,
                1,
            ]
        })
        .collect::<Vec<_>>();

    let encoded = encode_generic_i64_rows(&schema, &rows).unwrap();
    let decoded = decode_generic_i64_rows(&encoded).unwrap();

    assert_eq!(rows, decoded);
    assert!(encoded.plan.streams.iter().any(|stream| {
        stream.target_slot == Some(0) && matches!(stream.op, GenericStreamOp::Rle { .. })
    }));
    assert!(encoded.plan.streams.iter().any(|stream| {
        stream.target_slot == Some(5)
            && matches!(
                stream.op,
                GenericStreamOp::BaseBitpack { bit_width: 0, .. }
                    | GenericStreamOp::FixedStep { step: 0, .. }
            )
    }));
}

#[test]
fn generic_planner_emits_group_hints_for_repeated_slots() {
    let schema = generic_i64_parent_schema("book", &[255, 0, 0, 128, 128, 128, 128]).unwrap();
    let rows = vec![
        vec![1_000, 10, 1, 0, 100_000, 5, 0],
        vec![1_000, 10, 1, 1, 100_010, 0, 1],
        vec![1_001, 11, 1, 0, 100_020, 7, 0],
        vec![1_001, 11, 1, 1, 100_030, 4, 1],
    ];

    let encoded = encode_generic_i64_rows(&schema, &rows).unwrap();

    assert_eq!(rows, decode_generic_i64_rows(&encoded).unwrap());
    assert_eq!(FieldScope::Repeated, schema.fields[3].scope);
    assert!(encoded.plan.groups.iter().any(|group| {
        matches!(
            group,
            GenericGroupInstruction::Group {
                event_slots,
                repeated_slots,
                ..
            } if event_slots.as_slice() == [0, 1, 2]
                && repeated_slots.as_slice() == [3, 4, 5, 6]
        )
    }));
    assert!(encoded.plan.groups.iter().any(|group| {
        matches!(
            group,
            GenericGroupInstruction::PartitionRuns {
                partition_slot: 3,
                fixed_order: true,
                ..
            }
        )
    }));
}

#[test]
fn uuid_const_mask_is_planned_and_executable() {
    let prefix = 0xabcdu128 << 112;
    let values = vec![
        prefix | 1,
        prefix | 2,
        prefix | 3,
        prefix | (1u128 << 64) | 4,
    ];
    let instruction = plan_uuid_const_mask_stream(7, Some(2), &values).unwrap();

    assert!(matches!(
        instruction.op,
        GenericStreamOp::UuidConstMask {
            constant_bits: 124,
            variable_bits: 4
        }
    ));

    let encoded =
        encode_generic_stream_body(&instruction, &GenericStreamBodyValue::U128(values.clone()))
            .unwrap();
    let decoded = decode_generic_stream_body(&instruction, &encoded, values.len()).unwrap();

    assert_eq!(GenericStreamBodyValue::U128(values), decoded);
}
