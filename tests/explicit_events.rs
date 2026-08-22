use aura_codec::records::{
    aura1_fixed_layout_info, compile_i64_file_with_aura0_profile, decode_i64_events_file,
    visit_i64_rows_file_range, Aura0FileProfile,
};
use aura_codec::Aura0ByteLaneCodec;
use aura_codec::{
    decode_generic_i64_rows, generic_i64_parent_schema, AuraI64EventReader, AuraI64EventWriter,
    AuraI64Reader, AuraI64Writer, AuraReader, DerivedExpression, DerivedExpressionOp, DerivedOp,
    FieldRole, FieldType, GenericEncodedI64Rows, GenericEncodedStream, GenericGroupInstruction,
    GenericInstructionPlan, GenericStreamInstruction, GenericStreamOp, I64Event, Profile,
    SchemaBuilder,
};

fn book_schema() -> aura_codec::Result<aura_codec::SchemaDescriptor> {
    SchemaBuilder::new("explicit-book-events")
        .field("ts", FieldType::TimestampNs, FieldRole::Timestamp)
        .field("reset", FieldType::U8, FieldRole::Boolean)
        .field("sequence", FieldType::I64, FieldRole::Sequence)
        .repeated_field("side", FieldType::U8, FieldRole::Enum)
        .repeated_field("price", FieldType::I64, FieldRole::Price)
        .repeated_field("quantity", FieldType::I64, FieldRole::Quantity)
        .finish()
}

fn events() -> Vec<I64Event> {
    vec![
        I64Event {
            event_values: vec![1_000, 0, 7],
            children: vec![vec![0, 100, 8], vec![1, 101, 9]],
        },
        // A reset with no children must survive every profile. It cannot be
        // inferred from flattened level rows.
        I64Event {
            event_values: vec![1_001, 1, 8],
            children: vec![],
        },
        I64Event {
            event_values: vec![1_002, 0, 9],
            children: vec![vec![0, 102, 10]],
        },
        // Adjacent-identical message metadata remains two source events.
        I64Event {
            event_values: vec![1_002, 0, 9],
            children: vec![vec![1, 103, 11]],
        },
    ]
}

fn explicit_same_key_schema(
    op: DerivedExpressionOp,
) -> aura_codec::Result<aura_codec::SchemaDescriptor> {
    generic_i64_parent_schema("explicit-same-key", &[100, 0, 202, 101])?
        .with_derived_expressions(vec![DerivedExpression::new(1, 3, op, vec![1, 2])?])
}

fn explicit_same_key_direct_schema() -> aura_codec::Result<aura_codec::SchemaDescriptor> {
    generic_i64_parent_schema("explicit-same-key-direct", &[100, 0, 202, 0])
}

fn same_key_events() -> Vec<I64Event> {
    let key_count = 32i64;
    let base = |key: i64| {
        let mixed =
            (i128::from(key) * 6_364_136_223_846_793_005i128).rem_euclid(9_000_000_000_000i128);
        1_000_000_000_000 + i64::try_from(mixed).unwrap()
    };
    let mut current = (0..key_count).map(base).collect::<Vec<_>>();
    let mut events = vec![I64Event {
        event_values: vec![1_000, 1],
        children: (0..key_count)
            .map(|key| vec![key, current[key as usize]])
            .collect(),
    }];
    for event_index in 1..1_000i64 {
        if event_index == 500 {
            events.push(I64Event {
                event_values: vec![2_000, 1],
                children: Vec::new(),
            });
            current = (0..key_count).map(|key| base(key) + 50_000).collect();
        }
        let mut mutations = Vec::new();
        for offset in 0..4i64 {
            let key = (event_index * 17 + offset).rem_euclid(key_count);
            current[key as usize] += 1;
            mutations.push(vec![key, current[key as usize]]);
        }
        // Preserve sequential semantics for a duplicate key inside one source
        // event; the second mutation predicts from the first.
        if event_index % 31 == 0 {
            let key = event_index.rem_euclid(key_count);
            current[key as usize] += 1;
            mutations.push(vec![key, current[key as usize]]);
            current[key as usize] += 1;
            mutations.push(vec![key, current[key as usize]]);
        }
        events.push(I64Event {
            // Two adjacent source events intentionally share these values.
            event_values: vec![3_000 + event_index / 2, 0],
            children: mutations,
        });
    }
    events
}

fn repeated_parent_events() -> Vec<I64Event> {
    (0..256i64)
        .map(|event_index| I64Event {
            event_values: vec![1_000_000 + event_index * 100, event_index],
            children: (0..64i64)
                .map(|child_index| {
                    let side = child_index & 1;
                    let price = 50_000_000 + child_index * 10 + (event_index * 17).rem_euclid(31);
                    let mixed =
                        i128::from(event_index * 64 + child_index) * 6_364_136_223_846_793_005i128;
                    let total = 1_000_000_000_000
                        + i64::try_from(mixed.rem_euclid(8_000_000_000_000i128)).unwrap();
                    let component = (event_index + child_index).rem_euclid(5);
                    let order_count = 1 + (event_index * 7 + child_index).rem_euclid(32);
                    vec![side, price, total, total - component, order_count]
                })
                .collect(),
        })
        .collect()
}

#[test]
fn explicit_event_planner_selects_repeated_parent_residual_and_beats_direct() {
    let events = repeated_parent_events();
    let related_schema =
        generic_i64_parent_schema("explicit-related-child", &[100, 0, 200, 205, 0, 0, 5, 0])
            .unwrap();
    let mut related_writer = AuraI64EventWriter::new(related_schema);
    for event in events.clone() {
        related_writer.push_event(event).unwrap();
    }
    let related_ingest = related_writer.finish().unwrap();
    let related_aura0 =
        AuraI64EventWriter::compile_profile(&related_ingest, Profile::Aura0).unwrap();
    let related_plan = decode_i64_events_file(&related_aura0)
        .unwrap()
        .compiled_footer
        .unwrap()
        .generic_aura0_plan
        .unwrap();
    assert!(
        related_plan.groups.iter().any(|group| matches!(
            group,
            GenericGroupInstruction::DerivedStream {
                parent_group_id: None,
                output_slot: 5,
                op: DerivedOp::AddResidual | DerivedOp::SubtractResidual,
                input_slots,
                ..
            } if input_slots == &[4]
        )),
        "{related_plan:#?}"
    );
    assert_eq!(
        AuraI64EventReader::open(&related_aura0).unwrap().events(),
        events
    );

    let direct_schema = generic_i64_parent_schema(
        "explicit-related-child-direct",
        &[100, 0, 200, 205, 0, 0, 0, 0],
    )
    .unwrap();
    let mut direct_writer = AuraI64EventWriter::new(direct_schema);
    for event in events.clone() {
        direct_writer.push_event(event).unwrap();
    }
    let direct_aura0 =
        AuraI64EventWriter::compile_profile(&direct_writer.finish().unwrap(), Profile::Aura0)
            .unwrap();
    assert!(related_aura0.len() * 100 < direct_aura0.len() * 75);

    let aura1 = AuraI64EventWriter::compile_profile(&related_aura0, Profile::Aura1).unwrap();
    assert_eq!(AuraI64EventReader::open(&aura1).unwrap().events(), events);
    let aura0_again = AuraI64EventWriter::compile_profile(&aura1, Profile::Aura0).unwrap();
    assert_eq!(
        AuraI64EventReader::open(&aura0_again).unwrap().events(),
        events
    );
}

#[test]
fn explicit_event_planner_keeps_small_losing_parent_relationship_direct() {
    let events = vec![I64Event {
        event_values: vec![1_000, 7],
        children: vec![vec![0, 50_000, 123_456, 123_456, 3]],
    }];
    let schema = generic_i64_parent_schema(
        "explicit-related-child-small",
        &[100, 0, 200, 205, 0, 0, 5, 0],
    )
    .unwrap();
    let mut writer = AuraI64EventWriter::new(schema);
    writer.push_event(events[0].clone()).unwrap();
    let aura0 =
        AuraI64EventWriter::compile_profile(&writer.finish().unwrap(), Profile::Aura0).unwrap();
    let decoded = decode_i64_events_file(&aura0).unwrap();
    assert!(!decoded
        .compiled_footer
        .as_ref()
        .unwrap()
        .generic_aura0_plan
        .as_ref()
        .unwrap()
        .groups
        .iter()
        .any(|group| matches!(
            group,
            GenericGroupInstruction::DerivedStream { output_slot: 5, .. }
        )));
    assert_eq!(decoded.events, events);
}

#[test]
fn explicit_events_round_trip_all_profiles_without_ordinals() {
    let schema = book_schema().unwrap();
    let expected = events();
    let mut writer = AuraI64EventWriter::new(schema);
    for event in expected.clone() {
        writer.push_event(event).unwrap();
    }
    let ingest = writer.finish().unwrap();
    assert_eq!(
        AuraI64EventReader::open(&ingest).unwrap().events(),
        expected
    );

    let aura0 = AuraI64EventWriter::compile_profile(&ingest, Profile::Aura0).unwrap();
    assert_eq!(AuraI64EventReader::open(&aura0).unwrap().events(), expected);

    let aura1 = AuraI64EventWriter::compile_profile(&aura0, Profile::Aura1).unwrap();
    assert_eq!(AuraI64EventReader::open(&aura1).unwrap().events(), expected);

    let aura0_again = AuraI64EventWriter::compile_profile(&aura1, Profile::Aura0).unwrap();
    assert_eq!(
        AuraI64EventReader::open(&aura0_again).unwrap().events(),
        expected
    );

    // The legacy row view remains the compatible flattened child view. It has
    // no synthetic ordinal and no carrier row for the zero-child event.
    let rows = AuraI64Reader::open(&aura1).unwrap().rows().to_vec();
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[0], vec![1_000, 0, 7, 0, 100, 8]);
    assert_eq!(rows[3], vec![1_002, 0, 9, 1, 103, 11]);

    let sdk_reader = AuraReader::open_bytes(aura1).unwrap();
    let sdk_rows = sdk_reader
        .read_batches()
        .unwrap()
        .into_iter()
        .flat_map(|batch| batch.to_i64_rows().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(sdk_rows, rows);
}

#[test]
fn explicit_event_planner_selects_previous_mutation_and_beats_direct() {
    let events = same_key_events();
    let schema =
        explicit_same_key_schema(DerivedExpressionOp::PreviousMutationSameKeyResidual).unwrap();
    let mut writer = AuraI64EventWriter::new(schema);
    for event in events.clone() {
        writer.push_event(event).unwrap();
    }
    let ingest = writer.finish().unwrap();
    let aura0 = AuraI64EventWriter::compile_profile(&ingest, Profile::Aura0).unwrap();
    let plan = decode_i64_events_file(&aura0)
        .unwrap()
        .compiled_footer
        .unwrap()
        .generic_aura0_plan
        .unwrap();
    assert!(
        plan.groups.iter().any(|group| matches!(
            group,
            GenericGroupInstruction::DerivedStream {
                parent_group_id: Some(0),
                output_slot: 3,
                op: DerivedOp::PreviousMutationSameKeyResidual,
                ..
            }
        )),
        "{plan:#?}"
    );
    assert_eq!(AuraI64EventReader::open(&aura0).unwrap().events(), events);

    let direct_schema = explicit_same_key_direct_schema().unwrap();
    let mut direct_writer = AuraI64EventWriter::new(direct_schema);
    for event in events.clone() {
        direct_writer.push_event(event).unwrap();
    }
    let direct_ingest = direct_writer.finish().unwrap();
    let direct_aura0 = AuraI64EventWriter::compile_profile(&direct_ingest, Profile::Aura0).unwrap();
    let planned_bytes = aura0.len();
    let direct_bytes = direct_aura0.len();
    eprintln!(
        "explicit same-key production bytes: planned={planned_bytes} direct={direct_bytes} saved={} ({:.1}%)",
        direct_bytes - planned_bytes,
        100.0 * (direct_bytes - planned_bytes) as f64 / direct_bytes as f64
    );
    assert!(planned_bytes * 100 < direct_bytes * 80);

    let aura1 = AuraI64EventWriter::compile_profile(&aura0, Profile::Aura1).unwrap();
    assert_eq!(AuraI64EventReader::open(&aura1).unwrap().events(), events);
    let aura0_again = AuraI64EventWriter::compile_profile(&aura1, Profile::Aura0).unwrap();
    assert_eq!(
        AuraI64EventReader::open(&aura0_again).unwrap().events(),
        events
    );
}

#[test]
fn explicit_event_planner_selects_previous_output_by_key_with_zero_child_reset() {
    let events = same_key_events();
    let schema =
        explicit_same_key_schema(DerivedExpressionOp::PreviousOutputByKeyResidual).unwrap();
    let mut writer = AuraI64EventWriter::new(schema);
    for event in events.clone() {
        writer.push_event(event).unwrap();
    }
    let ingest = writer.finish().unwrap();
    let aura0 = AuraI64EventWriter::compile_profile(&ingest, Profile::Aura0).unwrap();
    let decoded = decode_i64_events_file(&aura0).unwrap();
    assert!(
        decoded
            .compiled_footer
            .as_ref()
            .unwrap()
            .generic_aura0_plan
            .as_ref()
            .unwrap()
            .groups
            .iter()
            .any(|group| matches!(
                group,
                GenericGroupInstruction::DerivedStream {
                    parent_group_id: Some(0),
                    output_slot: 3,
                    op: DerivedOp::PreviousOutputByKeyResidual,
                    ..
                }
            )),
        "{:#?}",
        decoded.compiled_footer.as_ref().unwrap().generic_aura0_plan
    );
    assert_eq!(decoded.events, events);
}

#[test]
fn explicit_event_planner_keeps_direct_when_state_footer_loses() {
    let schema =
        explicit_same_key_schema(DerivedExpressionOp::PreviousMutationSameKeyResidual).unwrap();
    let mut writer = AuraI64EventWriter::new(schema);
    writer
        .push_event(I64Event {
            event_values: vec![1_000, 1],
            children: (0..8).map(|key| vec![key, key]).collect(),
        })
        .unwrap();
    let aura0 = writer.finish_profile(Profile::Aura0).unwrap();
    let plan = decode_i64_events_file(&aura0)
        .unwrap()
        .compiled_footer
        .unwrap()
        .generic_aura0_plan
        .unwrap();
    assert!(plan.groups.iter().all(|group| !matches!(
        group,
        GenericGroupInstruction::DerivedStream {
            output_slot: 3,
            op: DerivedOp::PreviousMutationSameKeyResidual,
            ..
        }
    )));
    assert!(plan
        .streams
        .iter()
        .any(|stream| stream.target_slot == Some(3)));
}

#[test]
fn explicit_event_stats_cover_zero_child_event_ranges() {
    let schema = book_schema().unwrap();
    let mut writer = AuraI64EventWriter::new(schema);
    writer
        .push_event(I64Event {
            event_values: vec![i64::MAX - 1, 1, i64::MIN + 1],
            children: vec![],
        })
        .unwrap();
    writer
        .push_event(I64Event {
            event_values: vec![0, 0, 0],
            children: vec![vec![0, 1, 1]],
        })
        .unwrap();
    let aura1 = writer.finish_profile(Profile::Aura1).unwrap();
    let events = AuraI64EventReader::open(&aura1).unwrap();
    assert_eq!(events.events()[0].event_values[0], i64::MAX - 1);
    assert_eq!(events.events()[0].event_values[2], i64::MIN + 1);
}

#[test]
fn explicit_aura1_requires_a_valid_complete_sidecar() {
    let schema = book_schema().unwrap();
    let mut writer = AuraI64EventWriter::new(schema);
    for event in events() {
        writer.push_event(event).unwrap();
    }
    let mut aura1 = writer.finish_profile(Profile::Aura1).unwrap();
    let magic = aura1
        .windows(4)
        .rposition(|window| window == b"AUEV")
        .expect("event sidecar magic");
    aura1[magic] ^= 1;
    assert!(AuraI64EventReader::open(&aura1).is_err());
    assert!(AuraI64Reader::open(&aura1).is_err());
    assert!(AuraReader::open_bytes(aura1.clone()).is_err());
    assert!(aura1_fixed_layout_info(&aura1).is_err());
    assert!(visit_i64_rows_file_range(&aura1, 0, 0, |_| Ok(())).is_err());
}

#[test]
fn huge_sidecar_event_count_is_rejected_before_allocation() {
    let mut writer = AuraI64EventWriter::new(book_schema().unwrap());
    for event in events() {
        writer.push_event(event).unwrap();
    }
    let mut aura1 = writer.finish_profile(Profile::Aura1).unwrap();
    let magic = aura1
        .windows(4)
        .rposition(|window| window == b"AUEV")
        .unwrap();
    let length_offset = magic - 8;
    let length = u64::from_le_bytes(aura1[length_offset..magic].try_into().unwrap()) as usize;
    let sidecar_start = length_offset - length;
    let mut malicious = vec![0xff; 9];
    malicious.push(0x01); // canonical u64::MAX varint terminator
    malicious.push(3); // schema event-field count
    aura1.splice(sidecar_start..length_offset, malicious);
    let new_length_offset = sidecar_start + 11;
    aura1[new_length_offset..new_length_offset + 8].copy_from_slice(&11u64.to_le_bytes());
    let result = std::panic::catch_unwind(|| AuraReader::open_bytes(aura1));
    assert!(result.is_ok());
    assert!(result.unwrap().is_err());
}

#[test]
fn empty_and_all_zero_child_event_streams_round_trip_aura1() {
    let schema = book_schema().unwrap();
    let empty = AuraI64EventWriter::new(schema.clone())
        .finish_profile(Profile::Aura1)
        .unwrap();
    assert!(AuraI64EventReader::open(&empty)
        .unwrap()
        .events()
        .is_empty());

    let expected = vec![
        I64Event {
            event_values: vec![11, 1, 21],
            children: vec![],
        },
        I64Event {
            event_values: vec![12, 0, 22],
            children: vec![],
        },
    ];
    let mut writer = AuraI64EventWriter::new(schema);
    for event in expected.clone() {
        writer.push_event(event).unwrap();
    }
    let aura1 = writer.finish_profile(Profile::Aura1).unwrap();
    assert_eq!(AuraI64EventReader::open(&aura1).unwrap().events(), expected);
}

#[test]
fn fast_lz4_aura0_preserves_explicit_event_sidecar() {
    let expected = events();
    let mut writer = AuraI64EventWriter::new(book_schema().unwrap());
    for event in expected.clone() {
        writer.push_event(event).unwrap();
    }
    let ingest = writer.finish().unwrap();
    let fast = compile_i64_file_with_aura0_profile(
        &ingest,
        Aura0FileProfile::Fast,
        Aura0ByteLaneCodec::Lz4,
    )
    .unwrap();
    assert_eq!(decode_i64_events_file(&fast).unwrap().events, expected);
    let aura1 = AuraI64EventWriter::compile_profile(&fast, Profile::Aura1).unwrap();
    assert_eq!(decode_i64_events_file(&aura1).unwrap().events, expected);
}

#[test]
fn legacy_aura1_still_rejects_trailing_body_junk() {
    let schema = SchemaBuilder::new("legacy-row-body")
        .field("ts", FieldType::TimestampNs, FieldRole::Timestamp)
        .field("value", FieldType::I64, FieldRole::Value)
        .finish()
        .unwrap();
    let mut writer = AuraI64Writer::new(schema);
    writer.push_row(vec![1, 2]).unwrap();
    let ingest = writer.finish().unwrap();
    let mut aura1 = AuraI64Writer::compile_profile(&ingest, Profile::Aura1).unwrap();
    let footer_offset = aura1_fixed_layout_info(&aura1).unwrap().footer_offset;
    aura1.insert(footer_offset, 0xaa);
    assert!(AuraReader::open_bytes(aura1).is_err());
}

#[test]
fn structured_stats_observe_same_scope_related_fields() {
    let schema = SchemaBuilder::new("related-explicit-events")
        .field("ts", FieldType::TimestampNs, FieldRole::Timestamp)
        .field_related_to("recv", FieldType::TimestampNs, FieldRole::Timestamp, "ts")
        .repeated_field("price", FieldType::I64, FieldRole::Price)
        .repeated_field_related_to("quantity", FieldType::I64, FieldRole::Quantity, "price")
        .finish()
        .unwrap();
    let mut writer = AuraI64EventWriter::new(schema);
    writer
        .push_event(I64Event {
            event_values: vec![100, 103],
            children: vec![],
        })
        .unwrap();
    writer
        .push_event(I64Event {
            event_values: vec![200, 205],
            children: vec![vec![50, 57]],
        })
        .unwrap();
    let ingest = writer.finish().unwrap();
    let decoded = decode_i64_events_file(&ingest).unwrap();
    let stats = &decoded.ingest_footer.unwrap().stats;
    assert_eq!(stats.related_field(1).unwrap().observed, 2);
    assert_eq!(stats.related_field(3).unwrap().observed, 1);
}

fn minimal_explicit_plan(extra_event_producer: bool) -> GenericInstructionPlan {
    let mut streams = vec![
        GenericStreamInstruction {
            stream_id: 0,
            target_slot: None,
            op: GenericStreamOp::FixedStep { base: 2, step: 0 },
        },
        GenericStreamInstruction {
            stream_id: 1,
            target_slot: None,
            op: GenericStreamOp::FixedStep { base: 7, step: 0 },
        },
        GenericStreamInstruction {
            stream_id: 2,
            target_slot: Some(1),
            op: GenericStreamOp::FixedStep { base: 9, step: 0 },
        },
    ];
    if extra_event_producer {
        streams.push(GenericStreamInstruction {
            stream_id: 3,
            target_slot: Some(0),
            op: GenericStreamOp::FixedStep { base: 7, step: 0 },
        });
    }
    GenericInstructionPlan {
        streams,
        groups: vec![
            GenericGroupInstruction::Group {
                group_id: 0,
                event_slots: vec![0],
                repeated_slots: vec![1],
            },
            GenericGroupInstruction::ExplicitEvents {
                group_id: 1,
                parent_group_id: 0,
                child_count_stream_id: 0,
            },
            GenericGroupInstruction::GroupValueStream {
                group_id: 2,
                parent_group_id: 1,
                output_slot: 0,
                stream_id: 1,
            },
        ],
    }
}

#[test]
fn explicit_plan_rejects_conflicting_event_producers() {
    assert!(minimal_explicit_plan(true).encode().is_err());
}

#[test]
fn malformed_explicit_counts_fail_before_materialization() {
    let plan = minimal_explicit_plan(false);
    let encoded = GenericEncodedI64Rows {
        plan,
        streams: vec![
            GenericEncodedStream {
                stream_id: 0,
                value_count: 1,
                body: vec![],
            },
            GenericEncodedStream {
                stream_id: 1,
                value_count: 1,
                body: vec![],
            },
            GenericEncodedStream {
                stream_id: 2,
                value_count: 1,
                body: vec![],
            },
        ],
        record_count: 1,
        field_count: 2,
    };
    let result = std::panic::catch_unwind(|| decode_generic_i64_rows(&encoded));
    assert!(result.is_ok());
    assert!(result.unwrap().is_err());
}

#[test]
fn malformed_explicit_bitpack_count_cannot_overflow_length() {
    let mut plan = minimal_explicit_plan(false);
    plan.streams[0].op = GenericStreamOp::BaseBitpack {
        base: 0,
        unit: 1,
        bit_width: 128,
    };
    let encoded = GenericEncodedI64Rows {
        plan,
        streams: vec![GenericEncodedStream {
            stream_id: 0,
            value_count: usize::MAX,
            body: vec![],
        }],
        record_count: 0,
        field_count: 2,
    };
    let result = std::panic::catch_unwind(|| decode_generic_i64_rows(&encoded));
    assert!(result.is_ok());
    assert!(result.unwrap().is_err());
}

#[test]
fn malformed_explicit_fixed_step_count_cannot_allocate_usize_max() {
    let encoded = GenericEncodedI64Rows {
        plan: minimal_explicit_plan(false),
        streams: vec![GenericEncodedStream {
            stream_id: 0,
            value_count: usize::MAX,
            body: vec![],
        }],
        record_count: 0,
        field_count: 2,
    };
    let result = std::panic::catch_unwind(|| decode_generic_i64_rows(&encoded));
    assert!(result.is_ok());
    assert!(result.unwrap().is_err());
}
