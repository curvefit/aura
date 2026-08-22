use aura_codec::footer::AuraFooter;
use aura_codec::format::SEAL_MAGIC;
use aura_codec::program::CompiledFooter;
use aura_codec::{
    decode_generic_i64_rows, encode_generic_i64_rows, plan_uuid_const_mask_stream, DerivedOp,
    GenericEncodedI64Rows, GenericEncodedStream, GenericGroupInstruction, GenericInstructionPlan,
    GenericStreamBodyValue, GenericStreamInstruction, GenericStreamOp,
};
use aura_codec::{decode_generic_stream_body, encode_generic_stream_body};
use aura_codec::{generic_i64_parent_schema, records, AuraError, Profile};
use aura_codec::{DerivedExpression, DerivedExpressionOp, FieldScope};

fn previous_snapshot_schema() -> aura_codec::SchemaDescriptor {
    generic_i64_parent_schema("full_snapshot_same_key", &[100, 203, 0, 101])
        .unwrap()
        .with_derived_expressions(vec![DerivedExpression::new(
            1,
            3,
            DerivedExpressionOp::PreviousSnapshotSameKeyResidual,
            vec![1, 2],
        )
        .unwrap()])
        .unwrap()
}

fn previous_mutation_schema() -> aura_codec::SchemaDescriptor {
    generic_i64_parent_schema("sparse_mutation_same_key", &[100, 0, 203, 0, 101])
        .unwrap()
        .with_derived_expressions(vec![DerivedExpression::new(
            1,
            4,
            DerivedExpressionOp::PreviousMutationSameKeyResidual,
            vec![1, 2, 3],
        )
        .unwrap()])
        .unwrap()
}

fn previous_output_by_key_schema() -> aura_codec::SchemaDescriptor {
    generic_i64_parent_schema("previous_output_by_key", &[100, 0, 202, 101])
        .unwrap()
        .with_derived_expressions(vec![DerivedExpression::new(
            1,
            3,
            DerivedExpressionOp::PreviousOutputByKeyResidual,
            vec![1, 2],
        )
        .unwrap()])
        .unwrap()
}

fn sparse_mutation_rows(event_count: i64, key_count: i64) -> Vec<Vec<i64>> {
    let mut rows = (0..key_count)
        .map(|key| {
            vec![
                1_000,
                1,
                key & 1,
                100_000 + key,
                mutation_quantity_base(key),
            ]
        })
        .collect::<Vec<_>>();
    for event in 1..event_count {
        for offset in 0..4 {
            let key = (event * 7 + offset).rem_euclid(key_count);
            let output = mutation_quantity_base(key) + event;
            rows.push(vec![1_000 + event, 0, key & 1, 100_000 + key, output]);
        }
    }
    rows
}

fn mutation_quantity_base(key: i64) -> i64 {
    let mixed = (i128::from(key) * 6_364_136_223_846_793_005i128).rem_euclid(9_000_000_000_000i128);
    i64::try_from(1_000_000_000_000i128 + mixed).unwrap()
}

#[test]
fn generic_planner_can_select_zstd_varint_for_previous_value_residuals() {
    let schema = generic_i64_parent_schema("zstd_previous_value", &[0]).unwrap();
    let mut deltas = Vec::new();
    for index in 0..32i64 {
        let delta = 500_003 + index * 8_192;
        deltas.push(delta);
        deltas.push(-delta);
    }
    let mut value = 10_000_000i64;
    let mut rows = vec![vec![value]];
    for index in 0..(deltas.len() * 128) {
        value = value.checked_add(deltas[index % deltas.len()]).unwrap();
        rows.push(vec![value]);
    }

    let encoded = encode_generic_i64_rows(&schema, &rows).unwrap();
    assert_eq!(rows, decode_generic_i64_rows(&encoded).unwrap());
    assert!(encoded.plan.streams.iter().any(|stream| matches!(
        stream.op,
        GenericStreamOp::PreviousValueDelta {
            ref residual_op
        } if matches!(**residual_op, GenericStreamOp::ZstdVarint { .. })
    )));

    let ingest = records::encode_ingest_i64_file(records::I64FileInput {
        schema,
        rows: rows.clone(),
        stream_id: 0,
        dictionary_id: 0,
        header_comment: None,
    })
    .unwrap();
    let aura0 = records::compile_i64_file(&ingest, Profile::Aura0).unwrap();
    assert_eq!(rows, records::decode_i64_file(&aura0).unwrap().rows);
    let aura1 = records::compile_i64_file(&aura0, Profile::Aura1).unwrap();
    assert_eq!(rows, records::decode_i64_file(&aura1).unwrap().rows);
}

fn reordered_snapshot_rows(event_count: i64, key_count: i64) -> Vec<Vec<i64>> {
    (0..event_count)
        .flat_map(|event| {
            (0..key_count).map(move |offset| {
                let key = (offset + event * 17).rem_euclid(key_count);
                vec![
                    1_000 + event * 1_000,
                    key & 1,
                    100_000 + key,
                    1_000_000 + key * 10_000 + event / 64,
                ]
            })
        })
        .collect()
}

fn encoded_i64_stream(
    instruction: &GenericStreamInstruction,
    values: Vec<i64>,
) -> GenericEncodedStream {
    let value_count = values.len();
    GenericEncodedStream {
        stream_id: instruction.stream_id,
        value_count,
        body: encode_generic_stream_body(instruction, &GenericStreamBodyValue::I64(values))
            .unwrap(),
    }
}

fn manual_previous_snapshot_encoded(
    timestamps: Vec<i64>,
    sides: Vec<i64>,
    prices: Vec<i64>,
    residuals: Vec<i64>,
    residual_op: GenericStreamOp,
) -> GenericEncodedI64Rows {
    let timestamp_run_count = 1 + timestamps
        .windows(2)
        .filter(|pair| pair[0] != pair[1])
        .count();
    let instructions = vec![
        GenericStreamInstruction {
            stream_id: 0,
            target_slot: Some(0),
            op: GenericStreamOp::Rle {
                base: *timestamps.iter().min().unwrap(),
                unit: 1,
                bit_width: 2,
                run_count: u32::try_from(timestamp_run_count).unwrap(),
            },
        },
        GenericStreamInstruction {
            stream_id: 1,
            target_slot: Some(1),
            op: GenericStreamOp::BaseBitpack {
                base: 0,
                unit: 1,
                bit_width: 1,
            },
        },
        GenericStreamInstruction {
            stream_id: 2,
            target_slot: Some(2),
            op: GenericStreamOp::BaseBitpack {
                base: *prices.iter().min().unwrap(),
                unit: 1,
                bit_width: 1,
            },
        },
        GenericStreamInstruction {
            stream_id: 3,
            target_slot: None,
            op: residual_op,
        },
    ];
    let plan = GenericInstructionPlan {
        streams: instructions.clone(),
        groups: vec![
            GenericGroupInstruction::Group {
                group_id: 0,
                event_slots: vec![0],
                repeated_slots: vec![1, 2, 3],
            },
            GenericGroupInstruction::DerivedStream {
                group_id: 1,
                parent_group_id: Some(0),
                output_slot: 3,
                op: DerivedOp::PreviousSnapshotSameKeyResidual,
                input_slots: vec![1, 2],
                stream_id: 3,
            },
        ],
    };
    plan.encode().unwrap();
    let record_count = timestamps.len();
    let streams = vec![
        encoded_i64_stream(&instructions[0], timestamps),
        encoded_i64_stream(&instructions[1], sides),
        encoded_i64_stream(&instructions[2], prices),
        encoded_i64_stream(&instructions[3], residuals),
    ];
    GenericEncodedI64Rows {
        plan,
        streams,
        record_count,
        field_count: 4,
    }
}

#[test]
fn generic_planner_uses_previous_snapshot_same_key_for_reordered_levels() {
    let schema = previous_snapshot_schema();
    let rows = reordered_snapshot_rows(192, 96);

    let encoded = encode_generic_i64_rows(&schema, &rows).unwrap();
    assert_eq!(rows, decode_generic_i64_rows(&encoded).unwrap());
    assert!(encoded.plan.groups.iter().any(|group| {
        matches!(
            group,
            GenericGroupInstruction::DerivedStream {
                parent_group_id: Some(_),
                output_slot: 3,
                op: DerivedOp::PreviousSnapshotSameKeyResidual,
                input_slots,
                ..
            } if input_slots.as_slice() == [1, 2]
        )
    }));

    let columns = (0..schema.fields.len())
        .map(|slot| rows.iter().map(|row| row[slot]).collect::<Vec<_>>())
        .collect::<Vec<_>>();
    let from_columns = aura_codec::generic_planner::encode_generic_i64_columns_with_plan(
        &schema,
        &columns,
        rows.len(),
        encoded.plan.clone(),
        None,
    )
    .unwrap();
    assert_eq!(rows, decode_generic_i64_rows(&from_columns).unwrap());
    let from_direct_columns =
        aura_codec::generic_planner::encode_generic_i64_columns_with_plan_direct_streams(
            &schema,
            &columns,
            rows.len(),
            encoded.plan,
            None,
        )
        .unwrap();
    assert_eq!(rows, decode_generic_i64_rows(&from_direct_columns).unwrap());
}

#[test]
fn generic_planner_does_not_infer_same_key_state_for_sparse_updates() {
    let schema = generic_i64_parent_schema("sparse_updates", &[100, 203, 0, 0]).unwrap();
    let rows = reordered_snapshot_rows(64, 32);
    let encoded = encode_generic_i64_rows(&schema, &rows).unwrap();
    assert_eq!(rows, decode_generic_i64_rows(&encoded).unwrap());
    assert!(!encoded.plan.groups.iter().any(|group| {
        matches!(
            group,
            GenericGroupInstruction::DerivedStream {
                op: DerivedOp::PreviousSnapshotSameKeyResidual,
                ..
            }
        )
    }));
}

#[test]
fn generic_planner_uses_previous_mutation_same_key_for_sparse_updates() {
    let schema = previous_mutation_schema();
    let rows = sparse_mutation_rows(1_024, 256);

    let encoded = encode_generic_i64_rows(&schema, &rows).unwrap();
    assert_eq!(rows, decode_generic_i64_rows(&encoded).unwrap());
    assert!(encoded.plan.groups.iter().any(|group| {
        matches!(
            group,
            GenericGroupInstruction::DerivedStream {
                parent_group_id: Some(_),
                output_slot: 4,
                op: DerivedOp::PreviousMutationSameKeyResidual,
                input_slots,
                ..
            } if input_slots.as_slice() == [1, 2, 3]
        )
    }));

    let columns = (0..schema.fields.len())
        .map(|slot| rows.iter().map(|row| row[slot]).collect::<Vec<_>>())
        .collect::<Vec<_>>();
    let from_columns = aura_codec::generic_planner::encode_generic_i64_columns_with_plan(
        &schema,
        &columns,
        rows.len(),
        encoded.plan.clone(),
        None,
    )
    .unwrap();
    assert_eq!(rows, decode_generic_i64_rows(&from_columns).unwrap());
    let from_direct_columns =
        aura_codec::generic_planner::encode_generic_i64_columns_with_plan_direct_streams(
            &schema,
            &columns,
            rows.len(),
            encoded.plan,
            None,
        )
        .unwrap();
    assert_eq!(rows, decode_generic_i64_rows(&from_direct_columns).unwrap());

    let ingest = records::encode_ingest_i64_file(records::I64FileInput {
        schema,
        rows: rows.clone(),
        stream_id: 7,
        dictionary_id: 11,
        header_comment: None,
    })
    .unwrap();
    let aura0 = records::compile_i64_file(&ingest, Profile::Aura0).unwrap();
    assert_eq!(rows, records::decode_i64_file(&aura0).unwrap().rows);
    let aura1 = records::compile_i64_file(&aura0, Profile::Aura1).unwrap();
    let decoded_aura1 = records::decode_i64_file(&aura1).unwrap();
    assert_eq!(Profile::Aura1, decoded_aura1.header.profile);
    assert_eq!(rows, decoded_aura1.rows);
}

#[test]
fn generic_planner_uses_previous_output_by_key_for_domain_history() {
    let schema = previous_output_by_key_schema();
    let rows = (0..4_096i64)
        .map(|event| {
            let domain = event & 1;
            let previous_in_domain = event.saturating_sub(2);
            let price = 10_000_000 + domain * 1_000 + previous_in_domain * 3;
            vec![1_000 + event, i64::from(event == 0), domain, price]
        })
        .collect::<Vec<_>>();

    let encoded = encode_generic_i64_rows(&schema, &rows).unwrap();
    assert_eq!(rows, decode_generic_i64_rows(&encoded).unwrap());
    assert!(encoded.plan.groups.iter().any(|group| matches!(
        group,
        GenericGroupInstruction::DerivedStream {
            parent_group_id: Some(_),
            output_slot: 3,
            op: DerivedOp::PreviousOutputByKeyResidual,
            input_slots,
            ..
        } if input_slots.as_slice() == [1, 2]
    )));

    let ingest = records::encode_ingest_i64_file(records::I64FileInput {
        schema,
        rows: rows.clone(),
        stream_id: 0,
        dictionary_id: 0,
        header_comment: None,
    })
    .unwrap();
    let aura0 = records::compile_i64_file(&ingest, Profile::Aura0).unwrap();
    assert_eq!(rows, records::decode_i64_file(&aura0).unwrap().rows);
    let aura1 = records::compile_i64_file(&aura0, Profile::Aura1).unwrap();
    assert_eq!(rows, records::decode_i64_file(&aura1).unwrap().rows);
}

#[test]
fn previous_mutation_same_key_handles_delete_absent_repeat_and_reset() {
    let schema = previous_mutation_schema();
    let mut rows = vec![
        vec![1, 1, 0, 100, 10],
        vec![1, 1, 1, 101, 20],
        vec![2, 0, 0, 100, 15],
        vec![2, 0, 1, 101, 0],
        vec![2, 0, 0, 99, 7],
        vec![3, 0, 1, 102, 0],
        vec![3, 0, 0, 100, 16],
        vec![3, 0, 0, 100, 18],
        vec![4, 1, 0, 200, 5],
        vec![5, 0, 0, 100, 3],
    ];
    rows.extend((0..8).map(|key| vec![262, 1, key & 1, 1_000 + key, mutation_quantity_base(key)]));
    rows.extend((263i64..2_311).map(|event| {
        let key = (event * 17).rem_euclid(8);
        vec![
            event,
            0,
            key & 1,
            1_000 + key,
            mutation_quantity_base(key) + event - 262,
        ]
    }));

    let encoded = encode_generic_i64_rows(&schema, &rows).unwrap();
    assert_eq!(rows, decode_generic_i64_rows(&encoded).unwrap());
    let (stream_id, stream_op) = encoded
        .plan
        .groups
        .iter()
        .find_map(|group| match group {
            GenericGroupInstruction::DerivedStream {
                op: DerivedOp::PreviousMutationSameKeyResidual,
                stream_id,
                ..
            } => encoded
                .plan
                .streams
                .iter()
                .find(|stream| stream.stream_id == *stream_id)
                .map(|stream| (*stream_id, stream.op.clone())),
            _ => None,
        })
        .expect("mutation residual should beat direct encoding");
    let stream = encoded
        .streams
        .iter()
        .find(|stream| stream.stream_id == stream_id)
        .unwrap();
    let instruction = GenericStreamInstruction {
        stream_id,
        target_slot: None,
        op: stream_op,
    };
    let GenericStreamBodyValue::I64(residuals) =
        decode_generic_stream_body(&instruction, &stream.body, rows.len()).unwrap()
    else {
        panic!("expected i64 mutation residuals");
    };
    assert_eq!(&[10, 20, 5, -20, 7, 0, 1, 2, 5, 3], &residuals[..10]);
    assert!(residuals[10..18]
        .iter()
        .enumerate()
        .all(|(key, residual)| *residual == mutation_quantity_base(key as i64)));
}

#[test]
fn generic_planner_falls_back_on_previous_mutation_residual_overflow() {
    let schema = previous_mutation_schema();
    let rows = vec![vec![1, 1, 0, 100, i64::MIN], vec![2, 0, 0, 100, i64::MAX]];
    let encoded = encode_generic_i64_rows(&schema, &rows).unwrap();
    assert_eq!(rows, decode_generic_i64_rows(&encoded).unwrap());
    assert!(!encoded.plan.groups.iter().any(|group| matches!(
        group,
        GenericGroupInstruction::DerivedStream {
            op: DerivedOp::PreviousMutationSameKeyResidual,
            ..
        }
    )));
}

#[test]
fn generic_planner_compares_mutation_state_against_sparse_direct() {
    let schema = previous_mutation_schema();
    let key_count = 512i64;
    let mut rows = (0..key_count)
        .map(|key| vec![1, 1, key & 1, 10_000 + key, mutation_quantity_base(key)])
        .collect::<Vec<_>>();
    rows.extend((0..key_count).map(|key| vec![2 + key, 0, key & 1, 10_000 + key, 0]));

    let encoded = encode_generic_i64_rows(&schema, &rows).unwrap();
    assert_eq!(rows, decode_generic_i64_rows(&encoded).unwrap());
    assert!(!encoded.plan.groups.iter().any(|group| matches!(
        group,
        GenericGroupInstruction::DerivedStream {
            op: DerivedOp::PreviousMutationSameKeyResidual,
            ..
        }
    )));
    assert!(encoded.plan.groups.iter().any(|group| matches!(
        group,
        GenericGroupInstruction::SparseStream { output_slot: 4, .. }
    )));
}

#[test]
fn generic_planner_rejects_duplicate_key_inside_full_snapshot() {
    let schema = previous_snapshot_schema();
    let rows = vec![vec![1_000, 0, 100_000, 5], vec![1_000, 0, 100_000, 6]];
    assert!(encode_generic_i64_rows(&schema, &rows).is_err());
}

#[test]
fn generic_planner_falls_back_to_direct_on_same_key_residual_overflow() {
    let schema = previous_snapshot_schema();
    let rows = vec![
        vec![1_000, 0, 100_000, i64::MIN],
        vec![2_000, 0, 100_000, i64::MAX],
    ];
    let encoded = encode_generic_i64_rows(&schema, &rows).unwrap();
    assert_eq!(rows, decode_generic_i64_rows(&encoded).unwrap());
    assert!(!encoded.plan.groups.iter().any(|group| {
        matches!(
            group,
            GenericGroupInstruction::DerivedStream {
                op: DerivedOp::PreviousSnapshotSameKeyResidual,
                ..
            }
        )
    }));
}

#[test]
fn generic_decoder_uses_complete_previous_snapshot_across_reordered_keys() {
    let encoded = manual_previous_snapshot_encoded(
        vec![1, 1, 2, 2],
        vec![0, 1, 1, 0],
        vec![100, 101, 101, 100],
        vec![10, 20, 1, 1],
        GenericStreamOp::BaseBitpack {
            base: 1,
            unit: 1,
            bit_width: 5,
        },
    );
    assert_eq!(
        vec![
            vec![1, 0, 100, 10],
            vec![1, 1, 101, 20],
            vec![2, 1, 101, 21],
            vec![2, 0, 100, 11],
        ],
        decode_generic_i64_rows(&encoded).unwrap()
    );
}

#[test]
fn generic_decoder_rejects_duplicate_same_key_and_checked_add_overflow() {
    let duplicate = manual_previous_snapshot_encoded(
        vec![1, 1],
        vec![0, 0],
        vec![100, 100],
        vec![5, 6],
        GenericStreamOp::BaseBitpack {
            base: 5,
            unit: 1,
            bit_width: 1,
        },
    );
    assert!(decode_generic_i64_rows(&duplicate).is_err());

    let overflow = manual_previous_snapshot_encoded(
        vec![1, 2],
        vec![0, 0],
        vec![100, 100],
        vec![i64::MAX, 1],
        GenericStreamOp::BaseBitpack {
            base: 1,
            unit: 1,
            bit_width: 63,
        },
    );
    assert!(decode_generic_i64_rows(&overflow).is_err());
}

fn inject_undeclared_previous_snapshot_op(plan: &mut GenericInstructionPlan) {
    let stream_id = {
        let stream = plan
            .streams
            .iter_mut()
            .find(|stream| stream.target_slot == Some(3))
            .unwrap();
        stream.target_slot = None;
        stream.stream_id
    };
    let parent_group_id = plan
        .groups
        .iter()
        .find_map(|group| match group {
            GenericGroupInstruction::Group { group_id, .. } => Some(*group_id),
            _ => None,
        })
        .unwrap();
    let group_id = plan
        .groups
        .iter()
        .map(GenericGroupInstruction::group_id)
        .max()
        .unwrap()
        + 1;
    plan.groups.push(GenericGroupInstruction::DerivedStream {
        group_id,
        parent_group_id: Some(parent_group_id),
        output_slot: 3,
        op: DerivedOp::PreviousSnapshotSameKeyResidual,
        input_slots: vec![1, 2],
        stream_id,
    });
    plan.encode().unwrap();
}

fn inject_undeclared_previous_mutation_op(plan: &mut GenericInstructionPlan) {
    let stream_id = {
        let stream = plan
            .streams
            .iter_mut()
            .find(|stream| stream.target_slot == Some(4))
            .unwrap();
        stream.target_slot = None;
        stream.stream_id
    };
    let parent_group_id = plan
        .groups
        .iter()
        .find_map(|group| match group {
            GenericGroupInstruction::Group { group_id, .. } => Some(*group_id),
            _ => None,
        })
        .unwrap();
    let group_id = plan
        .groups
        .iter()
        .map(GenericGroupInstruction::group_id)
        .max()
        .unwrap()
        + 1;
    plan.groups.push(GenericGroupInstruction::DerivedStream {
        group_id,
        parent_group_id: Some(parent_group_id),
        output_slot: 4,
        op: DerivedOp::PreviousMutationSameKeyResidual,
        input_slots: vec![1, 2, 3],
        stream_id,
    });
    plan.encode().unwrap();
}

#[test]
fn decoded_footers_reject_undeclared_previous_snapshot_state() {
    let schema = generic_i64_parent_schema("undeclared_snapshot_state", &[100, 203, 0, 0]).unwrap();
    let rows = (0..8i64)
        .flat_map(|event| {
            (0..5i64).map(move |offset| {
                let key = (offset + event * 2).rem_euclid(5);
                vec![event, key & 1, 100 + key, event * 37 + key * 11 + offset]
            })
        })
        .collect::<Vec<_>>();
    let ingest = records::encode_ingest_i64_file(records::I64FileInput {
        schema,
        rows,
        stream_id: 0,
        dictionary_id: 0,
        header_comment: None,
    })
    .unwrap();

    let mut ingest_footer = records::decode_i64_file(&ingest)
        .unwrap()
        .ingest_footer
        .unwrap();
    inject_undeclared_previous_snapshot_op(ingest_footer.generic_aura0_plan.as_mut().unwrap());
    let malformed_ingest_footer = ingest_footer.encode().unwrap();
    assert_eq!(
        Err(AuraError::InvalidValue(
            "undeclared previous same-key residual"
        )),
        AuraFooter::decode(&malformed_ingest_footer)
    );

    let mut authorized_footer = ingest_footer.clone();
    authorized_footer.schema = previous_snapshot_schema();
    assert!(AuraFooter::decode(&authorized_footer.encode().unwrap()).is_ok());

    let mut wrong_keys = authorized_footer.clone();
    let GenericGroupInstruction::DerivedStream { input_slots, .. } = wrong_keys
        .generic_aura0_plan
        .as_mut()
        .unwrap()
        .groups
        .last_mut()
        .unwrap()
    else {
        unreachable!();
    };
    input_slots.reverse();
    assert_eq!(
        Err(AuraError::InvalidValue(
            "undeclared previous same-key residual"
        )),
        AuraFooter::decode(&wrong_keys.encode().unwrap())
    );

    let mut wrong_output = authorized_footer.clone();
    let GenericGroupInstruction::DerivedStream {
        output_slot,
        input_slots,
        ..
    } = wrong_output
        .generic_aura0_plan
        .as_mut()
        .unwrap()
        .groups
        .last_mut()
        .unwrap()
    else {
        unreachable!();
    };
    *output_slot = 2;
    *input_slots = vec![1, 3];
    assert_eq!(
        Err(AuraError::InvalidValue("previous same-key slots")),
        wrong_output.encode()
    );

    let mut wrong_event_contract = authorized_footer;
    let GenericGroupInstruction::Group { event_slots, .. } = wrong_event_contract
        .generic_aura0_plan
        .as_mut()
        .unwrap()
        .groups
        .first_mut()
        .unwrap()
    else {
        unreachable!();
    };
    event_slots.clear();
    assert_eq!(
        Err(AuraError::InvalidValue("previous same-key group contract")),
        AuraFooter::decode(&wrong_event_contract.encode().unwrap())
    );

    let aura0 = records::compile_i64_file(&ingest, Profile::Aura0).unwrap();
    let mut compiled_footer = records::decode_i64_file_metadata(&aura0)
        .unwrap()
        .compiled_footer
        .unwrap();
    inject_undeclared_previous_snapshot_op(compiled_footer.generic_aura0_plan.as_mut().unwrap());
    let malformed_footer = compiled_footer.encode().unwrap();
    assert_eq!(
        Err(AuraError::InvalidValue(
            "undeclared previous same-key residual"
        )),
        CompiledFooter::decode(&malformed_footer)
    );

    let footer_len_offset = aura0.len() - SEAL_MAGIC.len() - 4;
    let old_footer_len = u32::from_le_bytes(
        aura0[footer_len_offset..footer_len_offset + 4]
            .try_into()
            .unwrap(),
    ) as usize;
    let old_footer_start = footer_len_offset - old_footer_len;
    let mut malformed_aura0 = aura0[..old_footer_start].to_vec();
    malformed_aura0.extend_from_slice(&malformed_footer);
    malformed_aura0.extend_from_slice(&(malformed_footer.len() as u32).to_le_bytes());
    malformed_aura0.extend_from_slice(SEAL_MAGIC);
    assert_eq!(
        Err(AuraError::InvalidValue(
            "undeclared previous same-key residual"
        )),
        records::decode_i64_file(&malformed_aura0)
    );
}

#[test]
fn decoded_footers_reject_undeclared_previous_mutation_state() {
    let schema =
        generic_i64_parent_schema("undeclared_mutation_state", &[100, 0, 203, 0, 0]).unwrap();
    let rows = sparse_mutation_rows(32, 8);
    let ingest = records::encode_ingest_i64_file(records::I64FileInput {
        schema,
        rows,
        stream_id: 0,
        dictionary_id: 0,
        header_comment: None,
    })
    .unwrap();

    let mut ingest_footer = records::decode_i64_file(&ingest)
        .unwrap()
        .ingest_footer
        .unwrap();
    inject_undeclared_previous_mutation_op(ingest_footer.generic_aura0_plan.as_mut().unwrap());
    assert_eq!(
        Err(AuraError::InvalidValue(
            "undeclared previous same-key residual"
        )),
        AuraFooter::decode(&ingest_footer.encode().unwrap())
    );

    let mut authorized_footer = ingest_footer;
    authorized_footer.schema = previous_mutation_schema();
    assert!(AuraFooter::decode(&authorized_footer.encode().unwrap()).is_ok());

    let aura0 = records::compile_i64_file(&ingest, Profile::Aura0).unwrap();
    let mut compiled_footer = records::decode_i64_file_metadata(&aura0)
        .unwrap()
        .compiled_footer
        .unwrap();
    inject_undeclared_previous_mutation_op(compiled_footer.generic_aura0_plan.as_mut().unwrap());
    assert_eq!(
        Err(AuraError::InvalidValue(
            "undeclared previous same-key residual"
        )),
        CompiledFooter::decode(&compiled_footer.encode().unwrap())
    );
}

#[test]
fn generic_planner_uses_fixed_order_temporal_delta_per_repeated_field() {
    let schema = generic_i64_parent_schema("percentage_depth", &[100, 203, 0, 0]).unwrap();
    let rows = (0..128)
        .flat_map(|event| {
            [-1i64, 0, 1]
                .into_iter()
                .enumerate()
                .map(move |(rank, key)| {
                    vec![
                        1_000 + i64::from(event) * 1_000,
                        key,
                        (rank as i64 + 1) * 1_000_000 + i64::from(event),
                        i64::from(event) * 3 + rank as i64,
                    ]
                })
        })
        .collect::<Vec<_>>();

    let encoded = encode_generic_i64_rows(&schema, &rows).unwrap();
    assert_eq!(rows, decode_generic_i64_rows(&encoded).unwrap());
    assert!(encoded.plan.streams.iter().any(|stream| {
        stream.target_slot == Some(2)
            && matches!(
                stream.op,
                GenericStreamOp::FixedStrideDelta { stride: 3, .. }
            )
    }));
    assert!(encoded.plan.streams.iter().any(|stream| {
        stream.target_slot == Some(3) && matches!(stream.op, GenericStreamOp::FixedStep { .. })
    }));
}

#[test]
fn generic_planner_selects_quotient_remainder_for_related_repeated_values() {
    let schema = generic_i64_parent_schema("cumulative_depth", &[100, 200, 203, 0, 3]).unwrap();
    let rows = (0..32i64)
        .flat_map(|event| {
            [-5i64, -4, -3, -2, -1, 1, 2, 3, 4, 5]
                .into_iter()
                .enumerate()
                .map(move |(rank, key)| {
                    let depth = 100_000_000 + event * 97_409 + i64::try_from(rank).unwrap() * 7_919;
                    let quotient = 60_000_000
                        + ((event * 104_729 + i64::try_from(rank).unwrap() * 13_037) % 1_000_003);
                    let remainder = (event * 31 + i64::try_from(rank).unwrap()) % 1_009;
                    vec![
                        1_000 + event * 1_000,
                        key,
                        depth,
                        depth * quotient + remainder,
                    ]
                })
        })
        .collect::<Vec<_>>();

    let encoded = encode_generic_i64_rows(&schema, &rows).unwrap();

    assert_eq!(rows, decode_generic_i64_rows(&encoded).unwrap());
    assert!(encoded.plan.groups.iter().any(|group| {
        matches!(
            group,
            GenericGroupInstruction::QuotientRemainder {
                output_slot: 3,
                divisor_slot: 2,
                ..
            }
        )
    }));
}

#[test]
fn generic_decoder_rejects_out_of_range_quotient_remainder_divisor() {
    let quotient_instruction = GenericStreamInstruction {
        stream_id: 0,
        target_slot: None,
        op: GenericStreamOp::FixedStep { base: 1, step: 0 },
    };
    let remainder_instruction = GenericStreamInstruction {
        stream_id: 1,
        target_slot: None,
        op: GenericStreamOp::FixedStep { base: 0, step: 0 },
    };
    let encoded = GenericEncodedI64Rows {
        plan: GenericInstructionPlan {
            streams: vec![quotient_instruction.clone(), remainder_instruction.clone()],
            groups: vec![GenericGroupInstruction::QuotientRemainder {
                group_id: 0,
                parent_group_id: None,
                output_slot: 1,
                divisor_slot: 99,
                quotient_stream_id: 0,
                remainder_stream_id: 1,
            }],
        },
        streams: vec![
            GenericEncodedStream {
                stream_id: 0,
                value_count: 1,
                body: encode_generic_stream_body(
                    &quotient_instruction,
                    &GenericStreamBodyValue::I64(vec![1]),
                )
                .unwrap(),
            },
            GenericEncodedStream {
                stream_id: 1,
                value_count: 1,
                body: encode_generic_stream_body(
                    &remainder_instruction,
                    &GenericStreamBodyValue::I64(vec![0]),
                )
                .unwrap(),
            },
        ],
        record_count: 1,
        field_count: 2,
    };

    assert!(decode_generic_i64_rows(&encoded).is_err());
}

#[test]
fn generic_decoder_rejects_non_euclidean_quotient_remainder_values() {
    let malformed = |divisor: i64, remainder: i64| {
        let instructions = [
            GenericStreamInstruction {
                stream_id: 0,
                target_slot: Some(0),
                op: GenericStreamOp::FixedStep {
                    base: divisor,
                    step: 0,
                },
            },
            GenericStreamInstruction {
                stream_id: 1,
                target_slot: None,
                op: GenericStreamOp::FixedStep { base: 1, step: 0 },
            },
            GenericStreamInstruction {
                stream_id: 2,
                target_slot: None,
                op: GenericStreamOp::FixedStep {
                    base: remainder,
                    step: 0,
                },
            },
        ];
        GenericEncodedI64Rows {
            plan: GenericInstructionPlan {
                streams: instructions.to_vec(),
                groups: vec![GenericGroupInstruction::QuotientRemainder {
                    group_id: 0,
                    parent_group_id: None,
                    output_slot: 1,
                    divisor_slot: 0,
                    quotient_stream_id: 1,
                    remainder_stream_id: 2,
                }],
            },
            streams: instructions
                .iter()
                .map(|instruction| GenericEncodedStream {
                    stream_id: instruction.stream_id,
                    value_count: 1,
                    body: encode_generic_stream_body(
                        instruction,
                        &GenericStreamBodyValue::I64(vec![match instruction.stream_id {
                            0 => divisor,
                            1 => 1,
                            _ => remainder,
                        }]),
                    )
                    .unwrap(),
                })
                .collect(),
            record_count: 1,
            field_count: 2,
        }
    };

    assert!(decode_generic_i64_rows(&malformed(0, 0)).is_err());
    assert!(decode_generic_i64_rows(&malformed(2, 2)).is_err());
    assert!(decode_generic_i64_rows(&malformed(-2, -1)).is_err());
    let mut targeted_auxiliary = malformed(2, 0);
    targeted_auxiliary.plan.streams[1].target_slot = Some(1);
    assert!(targeted_auxiliary.plan.encode().is_err());
    assert!(decode_generic_i64_rows(&targeted_auxiliary).is_err());
}

#[test]
fn generic_decoder_rejects_out_of_range_expression_input() {
    let source = GenericStreamInstruction {
        stream_id: 0,
        target_slot: Some(0),
        op: GenericStreamOp::FixedStep { base: 7, step: 0 },
    };
    let encoded = GenericEncodedI64Rows {
        plan: GenericInstructionPlan {
            streams: vec![source.clone()],
            groups: vec![GenericGroupInstruction::ExpressionValue {
                group_id: 0,
                parent_group_id: None,
                output_slot: 1,
                op: DerivedExpressionOp::Add,
                input_slots: vec![99],
                literals: Vec::new(),
                residual: 0,
            }],
        },
        streams: vec![GenericEncodedStream {
            stream_id: 0,
            value_count: 1,
            body: encode_generic_stream_body(&source, &GenericStreamBodyValue::I64(vec![7]))
                .unwrap(),
        }],
        record_count: 1,
        field_count: 2,
    };

    assert!(decode_generic_i64_rows(&encoded).is_err());
}

#[test]
fn generic_planner_rejects_fixed_stride_when_repeated_key_order_changes() {
    let schema = generic_i64_parent_schema("sparse_updates", &[100, 202, 0]).unwrap();
    let rows = (0..64)
        .flat_map(|event| {
            let keys = if event % 2 == 0 { [0, 1] } else { [1, 0] };
            keys.into_iter().map(move |key| {
                vec![
                    1_000 + i64::from(event) * 1_000,
                    key,
                    i64::from(event) + key,
                ]
            })
        })
        .collect::<Vec<_>>();

    let encoded = encode_generic_i64_rows(&schema, &rows).unwrap();
    assert_eq!(rows, decode_generic_i64_rows(&encoded).unwrap());
    assert!(!encoded
        .plan
        .streams
        .iter()
        .any(|stream| matches!(stream.op, GenericStreamOp::FixedStrideDelta { .. })));
}

#[test]
fn generic_planner_composes_related_residual_with_fixed_repeated_key() {
    let rows = (0..32u32)
        .flat_map(|event| {
            (0..16u32).map(move |level| {
                let total = 1_000_000
                    + i64::from(event) * 100_003
                    + i64::from((level * 65_537 + event * 97) % 1_000_003);
                let residual = i64::from(level) * 7_919
                    + i64::from(event >= 16 && level.is_multiple_of(5)) * 17;
                vec![
                    1_000_000 + i64::from(event),
                    10_000 + i64::from(event),
                    i64::from(level >= 8),
                    100_000 + i64::from(level),
                    total,
                    total + residual,
                    1 + i64::from((event + level) % 17),
                ]
            })
        })
        .collect::<Vec<_>>();

    let parent_schema =
        generic_i64_parent_schema("fixed_key_parent_residual", &[100, 0, 205, 0, 0, 5, 0]).unwrap();
    let encoded = encode_generic_i64_rows(&parent_schema, &rows).unwrap();
    assert_eq!(rows, decode_generic_i64_rows(&encoded).unwrap());
    let residual_stream_id = encoded
        .plan
        .groups
        .iter()
        .find_map(|group| match group {
            GenericGroupInstruction::DerivedStream {
                output_slot: 5,
                op: DerivedOp::AddResidual,
                input_slots,
                stream_id,
                ..
            } if input_slots.as_slice() == [4] => Some(*stream_id),
            _ => None,
        })
        .expect("related residual should win");
    assert!(encoded.plan.streams.iter().any(|stream| {
        stream.stream_id == residual_stream_id
            && matches!(
                stream.op,
                GenericStreamOp::FixedStrideDelta { stride: 16, .. }
            )
    }));
}

#[test]
fn generic_planner_does_not_infer_shape_math_from_parent_hints() {
    let schema = generic_i64_parent_schema("parent_only_values", &[100, 0, 2, 2, 2, 0]).unwrap();
    let rows = (0..128)
        .scan(100_000i64, |previous_close, index| {
            let open = *previous_close + i64::from(index % 3) - 1;
            let close = open + i64::from(index % 5) - 2;
            let high = open.max(close);
            let low = open.min(close);
            *previous_close = close;
            Some(vec![
                1_000 + i64::from(index) * 1_000,
                open,
                high,
                low,
                close,
                8_000 + i64::from(index % 11),
            ])
        })
        .collect::<Vec<_>>();

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
                } | GenericStreamOp::PreviousValueDelta { .. }
            )
    }));
    assert!(encoded.plan.groups.iter().any(|group| {
        matches!(
            group,
            GenericGroupInstruction::DerivedStream {
                output_slot: 2..=4,
                op: DerivedOp::AddResidual,
                input_slots,
                ..
            } if input_slots.as_slice() == [1]
        )
    }));
    assert!(!encoded.plan.groups.iter().any(|group| {
        matches!(
            group,
            GenericGroupInstruction::DerivedStream {
                op: DerivedOp::FirstOffsetThenDelta
                    | DerivedOp::MaxPlusResidual
                    | DerivedOp::MinMinusResidual,
                ..
            }
        )
    }));
}

#[test]
fn generic_planner_consumes_declared_derived_expressions() {
    let expressions = vec![
        DerivedExpression::new(1, 1, DerivedExpressionOp::FirstOffsetThenDelta, vec![4]).unwrap(),
        DerivedExpression::new(2, 2, DerivedExpressionOp::MaxPlusResidual, vec![1, 4]).unwrap(),
        DerivedExpression::new(3, 3, DerivedExpressionOp::MinMinusResidual, vec![1, 4]).unwrap(),
    ];
    let schema = generic_i64_parent_schema("declared_shape_math", &[100, 101, 102, 103, 2, 0])
        .unwrap()
        .with_derived_expressions(expressions)
        .unwrap();
    let rows = (0..128)
        .scan(100_000i64, |previous_close, index| {
            let open = *previous_close;
            let close = open + (i64::from(index % 7) - 3) * 10;
            let high = open.max(close) + i64::from(index % 2);
            let low = open.min(close) - i64::from(index % 3);
            *previous_close = close;
            Some(vec![
                1_000 + i64::from(index) * 1_000,
                open,
                high,
                low,
                close,
                10_000 + i64::from(index % 11),
            ])
        })
        .collect::<Vec<_>>();

    let encoded = encode_generic_i64_rows(&schema, &rows).unwrap();

    assert_eq!(rows, decode_generic_i64_rows(&encoded).unwrap());
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
fn generic_planner_consumes_arithmetic_expression_with_literals() {
    let expression =
        DerivedExpression::with_literals(3, 3, DerivedExpressionOp::Mul, vec![1, 2], vec![100], 0)
            .unwrap();
    let schema = generic_i64_parent_schema("declared_arithmetic", &[100, 0, 0, 103])
        .unwrap()
        .with_derived_expressions(vec![expression])
        .unwrap();
    let rows = (0..256)
        .map(|index| {
            let a = 10_000 + i64::from((index * 37) % 1_000);
            let b = 2 + i64::from((index * 17) % 13);
            let residual = if index % 16 == 0 {
                i64::from(index % 3)
            } else {
                0
            };
            vec![
                1_000 + i64::from(index) * 1_000,
                a,
                b,
                a * b * 100 + residual,
            ]
        })
        .collect::<Vec<_>>();

    let encoded = encode_generic_i64_rows(&schema, &rows).unwrap();

    assert_eq!(rows, decode_generic_i64_rows(&encoded).unwrap());
    assert!(encoded.plan.groups.iter().any(|group| {
        matches!(
            group,
            GenericGroupInstruction::ExpressionStream {
                output_slot: 3,
                op: DerivedExpressionOp::Mul,
                input_slots,
                literals,
                ..
            } if input_slots.as_slice() == [1, 2] && literals.as_slice() == [100]
        )
    }));
}

#[test]
fn generic_planner_uses_scaled_product_with_wide_intermediate() {
    let expression = DerivedExpression::with_literals(
        3,
        3,
        DerivedExpressionOp::MulDiv,
        vec![1, 2],
        vec![1_000_000],
        0,
    )
    .unwrap();
    let schema = generic_i64_parent_schema("scaled_product", &[100, 0, 0, 103])
        .unwrap()
        .with_derived_expressions(vec![expression])
        .unwrap();
    let rows = (0..512i64)
        .map(|index| {
            let a = 3_000_000_000 + (index * 104_729) % 20_000_003;
            let b = 4_000_000_000 + (index * 130_363) % 30_000_017;
            let output = i64::try_from(i128::from(a) * i128::from(b) / 1_000_000).unwrap();
            vec![1_000 + index * 1_000, a, b, output]
        })
        .collect::<Vec<_>>();

    let encoded = encode_generic_i64_rows(&schema, &rows).unwrap();

    assert_eq!(rows, decode_generic_i64_rows(&encoded).unwrap());
    assert!(encoded.plan.groups.iter().any(|group| {
        matches!(
            group,
            GenericGroupInstruction::ExpressionValue {
                output_slot: 3,
                op: DerivedExpressionOp::MulDiv,
                input_slots,
                literals,
                residual: 0,
                ..
            } if input_slots.as_slice() == [1, 2] && literals.as_slice() == [1_000_000]
        )
    }));
}

#[test]
fn generic_planner_rejects_overflowing_scaled_product_candidate() {
    let expression = DerivedExpression::with_literals(
        4,
        4,
        DerivedExpressionOp::MulDiv,
        vec![1, 2, 3],
        vec![1],
        0,
    )
    .unwrap();
    let schema = generic_i64_parent_schema("overflowing_scaled_product", &[100, 0, 0, 0, 104])
        .unwrap()
        .with_derived_expressions(vec![expression])
        .unwrap();
    let rows = vec![
        vec![1_000, i64::MAX, i64::MAX, i64::MAX, 7],
        vec![2_000, i64::MAX, i64::MAX, i64::MAX, 11],
    ];

    let encoded = encode_generic_i64_rows(&schema, &rows).unwrap();

    assert_eq!(rows, decode_generic_i64_rows(&encoded).unwrap());
    assert!(!encoded.plan.groups.iter().any(|group| {
        matches!(
            group,
            GenericGroupInstruction::ExpressionStream {
                output_slot: 4,
                op: DerivedExpressionOp::MulDiv,
                ..
            } | GenericGroupInstruction::ExpressionValue {
                output_slot: 4,
                op: DerivedExpressionOp::MulDiv,
                ..
            }
        )
    }));
}

#[test]
fn generic_planner_omits_stream_for_exact_arithmetic_expression() {
    let expression =
        DerivedExpression::with_literals(3, 3, DerivedExpressionOp::Add, vec![1, 2], vec![5], 0)
            .unwrap();
    let schema = generic_i64_parent_schema("declared_exact_arithmetic", &[100, 0, 0, 103])
        .unwrap()
        .with_derived_expressions(vec![expression])
        .unwrap();
    let rows = (0..256)
        .map(|index| {
            let a = 10_000 + i64::from((index * 37) % 1_000);
            let b = 2 + i64::from((index * 17) % 13);
            vec![1_000 + i64::from(index) * 1_000, a, b, a + b + 5]
        })
        .collect::<Vec<_>>();

    let encoded = encode_generic_i64_rows(&schema, &rows).unwrap();

    assert_eq!(rows, decode_generic_i64_rows(&encoded).unwrap());
    assert!(encoded.plan.groups.iter().any(|group| {
        matches!(
            group,
            GenericGroupInstruction::ExpressionValue {
                output_slot: 3,
                op: DerivedExpressionOp::Add,
                input_slots,
                literals,
                residual: 0,
                ..
            } if input_slots.as_slice() == [1, 2] && literals.as_slice() == [5]
        )
    }));
    assert!(!encoded.plan.groups.iter().any(|group| {
        matches!(
            group,
            GenericGroupInstruction::ExpressionStream { output_slot: 3, .. }
        )
    }));
}

#[test]
fn generic_planner_uses_direct_when_declared_expression_overhead_loses() {
    let expression =
        DerivedExpression::with_literals(3, 3, DerivedExpressionOp::Add, vec![1, 2], vec![5], 0)
            .unwrap();
    let schema = generic_i64_parent_schema("declared_exact_arithmetic_tiny", &[100, 0, 0, 103])
        .unwrap()
        .with_derived_expressions(vec![expression])
        .unwrap();
    let rows = vec![
        vec![1_000, 10, 2, 17],
        vec![2_000, 11, 3, 19],
        vec![3_000, 12, 4, 21],
    ];

    let encoded = encode_generic_i64_rows(&schema, &rows).unwrap();

    assert_eq!(rows, decode_generic_i64_rows(&encoded).unwrap());
    assert!(encoded
        .plan
        .streams
        .iter()
        .any(|stream| stream.target_slot == Some(3)));
    assert!(!encoded.plan.groups.iter().any(|group| {
        matches!(
            group,
            GenericGroupInstruction::ExpressionValue { output_slot: 3, .. }
                | GenericGroupInstruction::ExpressionStream { output_slot: 3, .. }
        )
    }));
}

#[test]
fn generic_planner_scores_previous_parent_residuals() {
    let schema = generic_i64_parent_schema("previous_parent", &[100, 0, 2]).unwrap();
    let mut previous_parent = 1_000_000i64;
    let rows = (0..256)
        .map(|index| {
            let parent = previous_parent + i64::from((index * 97) % 251) - 125;
            let child = if index == 0 {
                parent + 7
            } else {
                previous_parent
            };
            previous_parent = parent;
            vec![1_000 + i64::from(index) * 1_000, parent, child]
        })
        .collect::<Vec<_>>();

    let encoded = encode_generic_i64_rows(&schema, &rows).unwrap();

    assert_eq!(rows, decode_generic_i64_rows(&encoded).unwrap());
    assert!(encoded.plan.groups.iter().any(|group| {
        matches!(
            group,
            GenericGroupInstruction::DerivedStream {
                output_slot: 2,
                op: DerivedOp::FirstOffsetThenDelta,
                input_slots,
                ..
            } if input_slots.as_slice() == [1]
        )
    }));
}

#[test]
fn generic_planner_selects_tick_stream_ops_without_field_names() {
    let schema = generic_i64_parent_schema("ticks", &[100, 0, 0, 0, 0, 0]).unwrap();
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
                    | GenericStreamOp::Dictionary {
                        entry_count: 1,
                        code_width: 0,
                        ..
                    }
            )
    }));
}

#[test]
fn generic_planner_rejects_partition_hint_when_complete_cost_loses() {
    let schema = generic_i64_parent_schema("book", &[100, 0, 0, 204, 0, 0, 0]).unwrap();
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
    assert!(!encoded.plan.groups.iter().any(|group| matches!(
        group,
        GenericGroupInstruction::PartitionRuns { .. }
            | GenericGroupInstruction::PartitionRunLengths { .. }
            | GenericGroupInstruction::SegmentedDeltaStream { .. }
    )));
}

#[test]
fn generic_planner_uses_sparse_presence_for_zero_heavy_repeated_slots() {
    let schema =
        generic_i64_parent_schema("parented_repeated", &[100, 0, 0, 205, 4, 5, 5, 5]).unwrap();
    let rows = (0..256)
        .flat_map(|event| {
            (0..8).map(move |level| {
                let row_index = event * 8 + level;
                let side = i64::from(level >= 4);
                let price = 100_000 + i64::from(event * 10 + level);
                let present = row_index % 17 == 0;
                let qty1 = if present {
                    1_000_000 + i64::from(row_index * 37)
                } else {
                    0
                };
                let qty2 = if present {
                    5_000_000 + i64::from(row_index * 41)
                } else {
                    0
                };
                let delete_flag = i64::from(present);
                vec![
                    1_000_000 + i64::from(event),
                    10_000 + i64::from(event),
                    20_000 + i64::from(event / 2),
                    side,
                    price,
                    qty1,
                    qty2,
                    delete_flag,
                ]
            })
        })
        .collect::<Vec<_>>();

    let encoded = encode_generic_i64_rows(&schema, &rows).unwrap();
    assert_eq!(rows, decode_generic_i64_rows(&encoded).unwrap());
    assert!(encoded.plan.groups.iter().any(|group| {
        matches!(
            group,
            GenericGroupInstruction::PresenceMap {
                slots,
                ..
            } if slots.iter().any(|slot| matches!(slot, 5 | 6))
        )
    }));
    assert!(encoded.plan.groups.iter().any(|group| {
        matches!(
            group,
            GenericGroupInstruction::SparseStream {
                output_slot: 5 | 6,
                ..
            }
        )
    }));
    assert!(!encoded.plan.groups.iter().any(|group| {
        matches!(
            group,
            GenericGroupInstruction::DerivedStream {
                output_slot: 4,
                input_slots,
                ..
            } if input_slots.as_slice() == [3]
        )
    }));
}

#[test]
fn generic_planner_selects_sparse_set_by_total_saved_bytes() {
    let schema =
        generic_i64_parent_schema("parented_repeated", &[100, 0, 0, 204, 4, 5, 5]).unwrap();
    let rows = (0..128)
        .flat_map(|event| {
            (0..4).map(move |level| {
                let row_index = event * 4 + level;
                let slot_a = if row_index % 3 == 0 {
                    9_000_000_000_000 + i64::from(row_index) * 17
                } else {
                    0
                };
                let slot_b = if row_index % 5 == 0 {
                    8_000_000_000_000 + i64::from(row_index) * 19
                } else {
                    0
                };
                vec![
                    1_000_000 + i64::from(event),
                    10_000 + i64::from(event),
                    20_000 + i64::from(event / 2),
                    i64::from(level >= 2),
                    100_000 + i64::from(row_index),
                    slot_a,
                    slot_b,
                ]
            })
        })
        .collect::<Vec<_>>();

    let encoded = encode_generic_i64_rows(&schema, &rows).unwrap();
    assert_eq!(rows, decode_generic_i64_rows(&encoded).unwrap());
    assert!(encoded.plan.groups.iter().any(|group| {
        matches!(
            group,
            GenericGroupInstruction::PresenceMap {
                slots,
                ..
            } if slots.as_slice() == [5, 6]
        )
    }));
    assert!(!encoded.plan.groups.iter().any(|group| {
        matches!(
            group,
            GenericGroupInstruction::PresenceValue {
                output_slot: 3,
                value: 1,
                ..
            }
        )
    }));
    assert!(encoded.plan.streams.iter().any(|stream| {
        stream.target_slot == Some(3) && matches!(stream.op, GenericStreamOp::Dictionary { .. })
    }));
}

#[test]
fn generic_planner_uses_partition_runs_and_segmented_child_deltas() {
    let schema =
        generic_i64_parent_schema("parented_repeated", &[100, 0, 0, 205, 4, 5, 5, 5]).unwrap();
    let rows = (0..96)
        .flat_map(|event| {
            let run_sizes = [
                2 + usize::from(event % 3 == 0),
                3 + usize::from(event % 4 == 0),
            ];
            run_sizes
                .into_iter()
                .enumerate()
                .flat_map(move |(partition, run_len)| {
                    (0..run_len).map(move |level| {
                        let base_price = 2_000_000 + i64::from(event) * 10;
                        let first_price = if partition == 0 {
                            base_price - 100
                        } else {
                            base_price + 8_000_000 + 100
                        };
                        let price = first_price + i64::try_from(level).unwrap() * 5;
                        let has_qty = (event + level as u16 + partition as u16).is_multiple_of(11);
                        vec![
                            1_000_000 + i64::from(event / 2),
                            10_000 + i64::from(event),
                            20_000 + i64::from(event / 3),
                            partition as i64,
                            price,
                            if has_qty { 1_000 + i64::from(event) } else { 0 },
                            if has_qty {
                                2_000 + i64::from(level as u16)
                            } else {
                                0
                            },
                            i64::from(has_qty),
                        ]
                    })
                })
        })
        .collect::<Vec<_>>();

    let encoded = encode_generic_i64_rows(&schema, &rows).unwrap();

    assert_eq!(rows, decode_generic_i64_rows(&encoded).unwrap());
    assert!(encoded.plan.groups.iter().any(|group| {
        matches!(
            group,
            GenericGroupInstruction::PartitionRunLengths {
                partition_slot: 3,
                ..
            }
        )
    }));
    let first_stream_id = encoded
        .plan
        .groups
        .iter()
        .find_map(|group| match group {
            GenericGroupInstruction::SegmentedDeltaStream {
                output_slot: 4,
                first_stream_id,
                ..
            } => Some(*first_stream_id),
            _ => None,
        })
        .unwrap();
    assert!(encoded.plan.streams.iter().any(|stream| {
        stream.stream_id == first_stream_id
            && matches!(
                stream.op,
                GenericStreamOp::FixedStrideDelta { stride: 2, .. }
            )
    }));
    assert!(encoded.plan.groups.iter().any(|group| {
        matches!(
            group,
            GenericGroupInstruction::SegmentedDeltaStream {
                output_slot: 4,
                base_stream_id: Some(_),
                ..
            }
        )
    }));
    assert!(encoded.plan.groups.iter().any(|group| {
        matches!(
            group,
            GenericGroupInstruction::GroupValueStream { output_slot: 1, .. }
        )
    }));
    assert!(!encoded
        .plan
        .streams
        .iter()
        .any(|stream| matches!(stream.target_slot, Some(1 | 3 | 4))));
}

#[test]
fn generic_planner_rejects_fixed_stride_for_unrelated_segment_bases() {
    let schema = generic_i64_parent_schema("unrelated_bases", &[100, 200, 203, 2, 0]).unwrap();
    let rows = (0..256u32)
        .flat_map(|event| {
            (0..2u32).flat_map(move |partition| {
                let mut scrambled = u64::from(event) + u64::from(partition) * 1_000_003;
                scrambled = (scrambled ^ (scrambled >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
                scrambled = (scrambled ^ (scrambled >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
                scrambled ^= scrambled >> 31;
                let base = 10_000_000 + i64::try_from(scrambled % 1_000_003).unwrap() * 10;
                (0..8u32).map(move |level| {
                    let direction = if partition == 0 { -1 } else { 1 };
                    vec![
                        1_000_000 + i64::from(event),
                        i64::from(partition),
                        base + direction * i64::from(level) * 10,
                        1_000 + i64::from((event + level + partition) % 31),
                    ]
                })
            })
        })
        .collect::<Vec<_>>();

    let encoded = encode_generic_i64_rows(&schema, &rows).unwrap();

    assert_eq!(rows, decode_generic_i64_rows(&encoded).unwrap());
    let first_stream_id = encoded
        .plan
        .groups
        .iter()
        .find_map(|group| match group {
            GenericGroupInstruction::SegmentedDeltaStream {
                output_slot: 2,
                first_stream_id,
                ..
            } => Some(*first_stream_id),
            _ => None,
        })
        .unwrap();
    assert!(encoded.plan.streams.iter().any(|stream| {
        stream.stream_id == first_stream_id
            && !matches!(stream.op, GenericStreamOp::FixedStrideDelta { .. })
    }));
}

#[test]
fn generic_planner_uses_fixed_partition_order_with_variable_run_counts() {
    let schema = generic_i64_parent_schema("parented_repeated", &[100, 0, 0, 202, 4]).unwrap();
    let rows = (0..128)
        .flat_map(|event| {
            let first_len = 1 + usize::from(event % 3 == 0);
            let second_len = 2 + usize::from(event % 4 == 0);
            [(0, first_len), (1, second_len)]
                .into_iter()
                .flat_map(move |(partition, run_len)| {
                    (0..run_len).map(move |level| {
                        let base = 1_000_000 + i64::from(event) * 100;
                        let first = if partition == 0 { base } else { base + 10_000 };
                        vec![
                            1_000 + i64::from(event),
                            10_000 + i64::from(event / 2),
                            20_000 + i64::from(event / 3),
                            partition,
                            first + i64::try_from(level).unwrap(),
                        ]
                    })
                })
        })
        .collect::<Vec<_>>();

    let encoded = encode_generic_i64_rows(&schema, &rows).unwrap();

    assert_eq!(rows, decode_generic_i64_rows(&encoded).unwrap());
    assert!(encoded.plan.groups.iter().any(|group| {
        matches!(
            group,
            GenericGroupInstruction::PartitionRunLengths {
                partition_slot: 3,
                fixed_order: true,
                event_count_stream_id: None,
                ..
            }
        )
    }));
}

#[test]
fn generic_planner_selects_a_previous_delta_representation_for_skewed_values() {
    let schema = generic_i64_parent_schema("skewed", &[100, 0]).unwrap();
    let rows = (0..12)
        .scan(1_000_000i64, |value, index| {
            if index == 6 {
                *value += 1_000_000;
            } else {
                *value += i64::from(index % 3);
            }
            Some(vec![i64::from(index), *value])
        })
        .collect::<Vec<_>>();

    let encoded = encode_generic_i64_rows(&schema, &rows).unwrap();

    assert_eq!(rows, decode_generic_i64_rows(&encoded).unwrap());
    assert!(encoded.plan.streams.iter().any(|stream| {
        stream.target_slot == Some(1)
            && matches!(
                stream.op,
                GenericStreamOp::PrevVarint { .. } | GenericStreamOp::PreviousValueDelta { .. }
            )
    }));
}

#[test]
fn generic_planner_uses_packed_dictionary_for_repeated_wide_values() {
    let schema = generic_i64_parent_schema("wide_repeats", &[0]).unwrap();
    let buckets = [
        0,
        1_000_000_000_000,
        17,
        999_999_999_937,
        2_000_000_000_003,
        3_000_000_000_019,
        4_000_000_000_031,
        5_000_000_000_041,
    ];
    let rows = (0..64)
        .map(|index| {
            let bucket = buckets[index % buckets.len()];
            vec![9_000_000_000_000 + bucket]
        })
        .collect::<Vec<_>>();

    let encoded = encode_generic_i64_rows(&schema, &rows).unwrap();

    assert_eq!(rows, decode_generic_i64_rows(&encoded).unwrap());
    assert!(encoded.plan.streams.iter().any(|stream| {
        stream.target_slot == Some(0)
            && matches!(stream.op, GenericStreamOp::PackedDictionary { .. })
    }));
}

#[test]
fn generic_planner_selects_huffman_when_complete_stream_cost_wins() {
    let schema = generic_i64_parent_schema("large_skewed_repeats", &[0]).unwrap();
    let rows = (0usize..8_192)
        .map(|index| {
            let pseudo = ((index as u64)
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407)
                >> 32) as usize;
            let bucket = if pseudo % 10 < 8 {
                0
            } else {
                1 + ((pseudo / 10) % 389) as i64
            };
            vec![9_000_000_000_000 + bucket * 1_000_000_003]
        })
        .collect::<Vec<_>>();

    let encoded = encode_generic_i64_rows(&schema, &rows).unwrap();

    assert_eq!(rows, decode_generic_i64_rows(&encoded).unwrap());
    assert!(encoded.plan.streams.iter().any(|stream| {
        stream.target_slot == Some(0)
            && matches!(stream.op, GenericStreamOp::HuffmanDictionary { .. })
    }));
}

#[test]
fn generic_planner_composes_previous_delta_with_residual_codec() {
    let schema = generic_i64_parent_schema("skewed_random_walk", &[0]).unwrap();
    let mut value = 9_000_000_000_000i64;
    let rows = (0usize..16_384)
        .map(|index| {
            let pseudo = (index as u64)
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let delta = if pseudo % 10 < 8 {
                0
            } else {
                1 + i64::try_from((pseudo >> 17) % 389).unwrap() * 1_000_000_003
            };
            value += delta;
            vec![value]
        })
        .collect::<Vec<_>>();

    let encoded = encode_generic_i64_rows(&schema, &rows).unwrap();

    assert_eq!(rows, decode_generic_i64_rows(&encoded).unwrap());
    assert!(encoded
        .plan
        .streams
        .iter()
        .any(|stream| { matches!(stream.op, GenericStreamOp::PreviousValueDelta { .. }) }));
}

#[test]
fn generic_planner_composes_delta_of_delta_with_residual_codec() {
    let schema = generic_i64_parent_schema("skewed_delta_changes", &[100]).unwrap();
    let mut value = 1_700_000_000_000_000_000i64;
    let mut delta = 1_000_000i64;
    let rows = (0usize..16_384)
        .map(|index| {
            let pseudo = (index as u64)
                .wrapping_mul(2_862_933_555_777_941_757)
                .wrapping_add(3_037_000_493);
            if pseudo % 20 == 0 {
                delta += 1 + i64::try_from((pseudo >> 21) % 257).unwrap() * 10_003;
            }
            value += delta;
            vec![value]
        })
        .collect::<Vec<_>>();

    let encoded = encode_generic_i64_rows(&schema, &rows).unwrap();

    assert_eq!(rows, decode_generic_i64_rows(&encoded).unwrap());
    assert!(encoded
        .plan
        .streams
        .iter()
        .any(|stream| { matches!(stream.op, GenericStreamOp::DeltaOfDelta { .. }) }));
}

#[test]
fn generic_planner_avoids_huffman_when_footer_overhead_loses() {
    let schema = generic_i64_parent_schema("small_skewed_repeats", &[0]).unwrap();
    let rows = [0, 1_000_000_000_000, 0, 17, 0, 999_999_999_937]
        .into_iter()
        .cycle()
        .take(30)
        .map(|bucket| vec![9_000_000_000_000 + bucket])
        .collect::<Vec<_>>();

    let encoded = encode_generic_i64_rows(&schema, &rows).unwrap();

    assert_eq!(rows, decode_generic_i64_rows(&encoded).unwrap());
    assert!(!encoded.plan.streams.iter().any(|stream| {
        stream.target_slot == Some(0)
            && matches!(stream.op, GenericStreamOp::HuffmanDictionary { .. })
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
