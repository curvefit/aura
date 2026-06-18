use aura_codec::{
    decode_generic_i64_rows, encode_generic_i64_rows, plan_uuid_const_mask_stream, DerivedOp,
    GenericGroupInstruction, GenericStreamBodyValue, GenericStreamOp,
};
use aura_codec::{decode_generic_stream_body, encode_generic_stream_body};
use aura_codec::{generic_i64_parent_schema, DerivedExpression, DerivedExpressionOp, FieldScope};

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
                }
            )
    }));
    assert!(encoded.plan.groups.iter().any(|group| {
        matches!(
            group,
            GenericGroupInstruction::DerivedStream {
                output_slot: 2 | 3 | 4,
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
fn generic_planner_emits_group_hints_for_repeated_slots() {
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
            } if slots.as_slice() == [5, 6, 3]
        )
    }));
    assert!(encoded.plan.groups.iter().any(|group| {
        matches!(
            group,
            GenericGroupInstruction::PresenceValue {
                output_slot: 3,
                value: 1,
                ..
            }
        )
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
fn generic_planner_uses_fixed_partition_order_with_variable_run_counts() {
    let schema = generic_i64_parent_schema("parented_repeated", &[100, 0, 0, 202, 4]).unwrap();
    let rows = (0..16)
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
fn generic_planner_uses_prev_varint_for_skewed_previous_deltas() {
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
        stream.target_slot == Some(1) && matches!(stream.op, GenericStreamOp::PrevVarint { .. })
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
fn generic_planner_prefers_blocklocal_when_huffman_speed_gate_fails() {
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
        stream.target_slot == Some(0) && matches!(stream.op, GenericStreamOp::BlockLocal { .. })
    }));
    assert!(!encoded.plan.streams.iter().any(|stream| {
        stream.target_slot == Some(0)
            && matches!(stream.op, GenericStreamOp::HuffmanDictionary { .. })
    }));
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
