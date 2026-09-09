use std::panic::{catch_unwind, AssertUnwindSafe};

use aura_codec::experimental::{
    canonical_v3_event_batch_sha256, decode_v3_event_block, encode_v3_event_block,
    validate_v3_event_batch, validate_v3_grouped_exact_subset, AuraV3Column,
    AuraV3ColumnValues as Values, AuraV3EventBatch, AuraV3VariableColumn, V3EventLimits,
    MAX_V3_EVENT_EVENTS,
};
use aura_codec::{
    DerivedExpression, DerivedExpressionOp, FieldRole, FieldScope, FieldType, GroupDescriptor,
    RelationshipPermissions, SchemaBuilder,
};
use sha2::{Digest, Sha256};

fn permissions() -> RelationshipPermissions {
    RelationshipPermissions::none()
        .with_split()
        .with_within_domain()
        .with_across_domain_same_field()
        .with_joint_same_field()
}

fn book_schema(okx_like_order_count: bool) -> aura_codec::SchemaDescriptor {
    let builder = SchemaBuilder::new(if okx_like_order_count {
        "test_only_okx_like_grouped_book"
    } else {
        "test_only_generic_grouped_book"
    })
    .field("source_ts_ms", FieldType::TimestampMs, FieldRole::Timestamp)
    .field("sequence", FieldType::U64, FieldRole::Sequence)
    .repeated_field("side", FieldType::U8, FieldRole::Side)
    .repeated_field("price", FieldType::I64, FieldRole::Price)
    .repeated_field("quantity", FieldType::I64, FieldRole::Quantity);
    let child_slots = if okx_like_order_count {
        vec![2, 3, 4, 5]
    } else {
        vec![2, 3, 4]
    };
    let mut schema = if okx_like_order_count {
        builder
            .repeated_field("order_count", FieldType::U32, FieldRole::Count)
            .dual_domain_repeated_group(1, child_slots, 2, permissions())
            .finish()
            .unwrap()
    } else {
        builder
            .dual_domain_repeated_group(1, child_slots, 2, permissions())
            .finish()
            .unwrap()
    };
    schema.fields[4].nullable = true;
    let groups = schema.groups.clone();
    schema.with_v3_groups(groups).unwrap()
}

fn base_batch(schema: &aura_codec::SchemaDescriptor) -> AuraV3EventBatch {
    let mut repeated_columns = vec![
        AuraV3Column {
            slot: 2,
            validity: None,
            values: Values::U8(vec![0, 0, 1, 0, 1]),
        },
        AuraV3Column {
            slot: 3,
            validity: None,
            values: Values::I64(vec![100, 101, 102, 103, 104]),
        },
        AuraV3Column {
            slot: 4,
            validity: Some(vec![0b0001_1101]),
            // row 1 is absent and row 2 is a present zero.
            values: Values::I64(vec![5, 0, 0, 7, 8]),
        },
    ];
    if schema.fields.len() == 6 {
        repeated_columns.push(AuraV3Column {
            slot: 5,
            validity: None,
            values: Values::U32(vec![1, 2, 3, 4, 5]),
        });
    }
    AuraV3EventBatch {
        schema_id: schema.schema_id,
        event_count: 2,
        child_offsets: vec![0, 2, 5],
        event_columns: vec![
            AuraV3Column {
                slot: 0,
                validity: None,
                values: Values::TimestampMs(vec![1_000, 2_000]),
            },
            AuraV3Column {
                slot: 1,
                validity: None,
                values: Values::U64(vec![10, 11]),
            },
        ],
        repeated_columns,
    }
}

fn empty_batch(schema: &aura_codec::SchemaDescriptor, event_count: u32) -> AuraV3EventBatch {
    let events = event_count as usize;
    AuraV3EventBatch {
        schema_id: schema.schema_id,
        event_count,
        child_offsets: vec![0; events + 1],
        event_columns: vec![
            AuraV3Column {
                slot: 0,
                validity: None,
                values: Values::TimestampMs(vec![0; events]),
            },
            AuraV3Column {
                slot: 1,
                validity: None,
                values: Values::U64(vec![0; events]),
            },
        ],
        repeated_columns: vec![
            AuraV3Column {
                slot: 2,
                validity: None,
                values: Values::U8(Vec::new()),
            },
            AuraV3Column {
                slot: 3,
                validity: None,
                values: Values::I64(Vec::new()),
            },
            AuraV3Column {
                slot: 4,
                validity: Some(Vec::new()),
                values: Values::I64(Vec::new()),
            },
        ],
    }
}

fn nullable_text_schema() -> aura_codec::SchemaDescriptor {
    let mut schema = SchemaBuilder::new("test_only_grouped_null_empty_zero")
        .field("ts", FieldType::TimestampNs, FieldRole::Timestamp)
        .field("sequence", FieldType::U64, FieldRole::Sequence)
        .repeated_field("side", FieldType::U8, FieldRole::Side)
        .repeated_field("quantity", FieldType::I64, FieldRole::Quantity)
        .repeated_field("note", FieldType::Utf8, FieldRole::Value)
        .dual_domain_repeated_group(1, vec![2, 3, 4], 2, permissions())
        .finish()
        .unwrap();
    schema.fields[3].nullable = true;
    schema.fields[4].nullable = true;
    let groups = schema.groups.clone();
    schema.with_v3_groups(groups).unwrap()
}

#[test]
fn grouped_exact_roundtrip_is_deterministic_and_schema_bound() {
    for schema in [book_schema(false), book_schema(true)] {
        validate_v3_grouped_exact_subset(&schema).unwrap();
        assert_eq!(
            Some(200),
            schema.compact_schema_map.as_ref().map(|map| map[2])
        );
        let batch = base_batch(&schema);
        let first = encode_v3_event_block(&schema, &batch, V3EventLimits::default()).unwrap();
        let second = encode_v3_event_block(&schema, &batch, V3EventLimits::default()).unwrap();
        assert_eq!(first, second);
        assert_eq!(b"AURAV3EB", &first[..8]);
        assert_ne!(b"AURAV3VB", &first[..8]);
        assert_eq!(
            batch,
            decode_v3_event_block(&schema, &first, V3EventLimits::default()).unwrap()
        );
        assert_eq!(
            canonical_v3_event_batch_sha256(&schema, &batch, V3EventLimits::default()).unwrap(),
            canonical_v3_event_batch_sha256(
                &schema,
                &decode_v3_event_block(&schema, &first, V3EventLimits::default()).unwrap(),
                V3EventLimits::default(),
            )
            .unwrap()
        );
        let other = book_schema(schema.fields.len() != 6);
        assert!(decode_v3_event_block(&other, &first, V3EventLimits::default()).is_err());
        let child_slots = schema.groups[0].child_slots.clone();
        let declaration_variant = schema
            .clone()
            .with_v3_groups(vec![GroupDescriptor::dual_domain_repeated(
                1,
                child_slots,
                2,
                RelationshipPermissions::none(),
            )])
            .unwrap();
        validate_v3_grouped_exact_subset(&declaration_variant).unwrap();
        assert!(
            decode_v3_event_block(&declaration_variant, &first, V3EventLimits::default()).is_err()
        );
    }
}

#[test]
fn zero_events_and_zero_child_events_are_distinct_and_exact() {
    let schema = book_schema(false);
    let zero = empty_batch(&schema, 0);
    let one_empty = empty_batch(&schema, 1);
    for batch in [&zero, &one_empty] {
        validate_v3_event_batch(&schema, batch, V3EventLimits::default()).unwrap();
        let bytes = encode_v3_event_block(&schema, batch, V3EventLimits::default()).unwrap();
        assert_eq!(
            *batch,
            decode_v3_event_block(&schema, &bytes, V3EventLimits::default()).unwrap()
        );
    }
    assert_ne!(
        canonical_v3_event_batch_sha256(&schema, &zero, V3EventLimits::default()).unwrap(),
        canonical_v3_event_batch_sha256(&schema, &one_empty, V3EventLimits::default()).unwrap()
    );
}

#[test]
fn nullable_child_null_empty_and_zero_are_distinct() {
    let schema = nullable_text_schema();
    let batch = AuraV3EventBatch {
        schema_id: schema.schema_id,
        event_count: 1,
        child_offsets: vec![0, 3],
        event_columns: vec![
            AuraV3Column {
                slot: 0,
                validity: None,
                values: Values::TimestampNs(vec![1]),
            },
            AuraV3Column {
                slot: 1,
                validity: None,
                values: Values::U64(vec![1]),
            },
        ],
        repeated_columns: vec![
            AuraV3Column {
                slot: 2,
                validity: None,
                values: Values::U8(vec![0, 1, 0]),
            },
            AuraV3Column {
                slot: 3,
                validity: Some(vec![0b0000_0110]),
                // absent, present zero, present nonzero
                values: Values::I64(vec![0, 0, 5]),
            },
            AuraV3Column {
                slot: 4,
                validity: Some(vec![0b0000_0110]),
                // absent empty placeholder, present empty, present "x"
                values: Values::Utf8(AuraV3VariableColumn {
                    offsets: vec![0, 0, 0, 1],
                    data: b"x".to_vec(),
                }),
            },
        ],
    };
    let bytes = encode_v3_event_block(&schema, &batch, V3EventLimits::default()).unwrap();
    assert_eq!(
        batch,
        decode_v3_event_block(&schema, &bytes, V3EventLimits::default()).unwrap()
    );
    let hash = canonical_v3_event_batch_sha256(&schema, &batch, V3EventLimits::default()).unwrap();
    let mut null_to_empty = batch.clone();
    null_to_empty.repeated_columns[2].validity = Some(vec![0b0000_0111]);
    let mut zero_to_null = batch.clone();
    zero_to_null.repeated_columns[1].validity = Some(vec![0b0000_0100]);
    for changed in [null_to_empty, zero_to_null] {
        validate_v3_event_batch(&schema, &changed, V3EventLimits::default()).unwrap();
        assert_ne!(
            hash,
            canonical_v3_event_batch_sha256(&schema, &changed, V3EventLimits::default()).unwrap()
        );
    }
}

#[test]
fn side_shapes_source_order_boundaries_and_null_presence_affect_hash() {
    let schema = book_schema(false);
    let base = base_batch(&schema);
    let base_hash =
        canonical_v3_event_batch_sha256(&schema, &base, V3EventLimits::default()).unwrap();

    let mut boundary = base.clone();
    boundary.child_offsets = vec![0, 3, 5];
    let mut reordered = base.clone();
    if let Values::I64(values) = &mut reordered.repeated_columns[1].values {
        values.swap(0, 1);
    }
    let mut present_zero = base.clone();
    present_zero.repeated_columns[2].validity = Some(vec![0b0001_1111]);
    let mut bid_only = base.clone();
    bid_only.repeated_columns[0].values = Values::U8(vec![0; 5]);
    let mut ask_only = base.clone();
    ask_only.repeated_columns[0].values = Values::U8(vec![1; 5]);
    let mut asymmetric = base.clone();
    asymmetric.child_offsets = vec![0, 1, 5];

    for candidate in [
        boundary,
        reordered,
        present_zero,
        bid_only,
        ask_only,
        asymmetric,
    ] {
        validate_v3_event_batch(&schema, &candidate, V3EventLimits::default()).unwrap();
        assert_ne!(
            base_hash,
            canonical_v3_event_batch_sha256(&schema, &candidate, V3EventLimits::default()).unwrap()
        );
    }
}

#[test]
fn grouped_subset_rejects_nonexact_schema_shapes() {
    let valid = book_schema(false);
    let repeated = vec![2, 3, 4];
    let cases = vec![
        valid.clone().with_v3_groups(vec![
            GroupDescriptor::dual_domain_repeated(1, vec![2], 2, permissions()),
            GroupDescriptor::repeated(2, vec![3, 4], permissions()),
        ]),
        valid
            .clone()
            .with_v3_groups(vec![GroupDescriptor::dual_domain_repeated(
                1,
                vec![2, 3],
                2,
                permissions(),
            )]),
        valid
            .clone()
            .with_v3_groups(vec![GroupDescriptor::dual_domain_repeated(
                1,
                repeated.clone(),
                3,
                permissions(),
            )]),
    ];
    for schema in cases {
        assert!(
            schema.is_err() || validate_v3_grouped_exact_subset(schema.as_ref().unwrap()).is_err()
        );
    }

    let mut nullable_side = valid.clone();
    nullable_side.fields[2].nullable = true;
    nullable_side = nullable_side
        .with_v3_groups(vec![GroupDescriptor::dual_domain_repeated(
            1,
            repeated.clone(),
            2,
            permissions(),
        )])
        .unwrap();
    assert!(validate_v3_grouped_exact_subset(&nullable_side).is_err());

    let mutations: [fn(&mut aura_codec::SchemaDescriptor); 2] = [
        |schema: &mut aura_codec::SchemaDescriptor| schema.fields[2].field_type = FieldType::I16,
        |schema: &mut aura_codec::SchemaDescriptor| schema.fields[2].role = FieldRole::Value,
    ];
    for mutate in mutations {
        let mut schema = valid.clone();
        mutate(&mut schema);
        let groups = schema.groups.clone();
        let schema = schema.with_v3_groups(groups).unwrap();
        assert!(validate_v3_grouped_exact_subset(&schema).is_err());
    }

    let mut wrong_scope = valid.clone();
    wrong_scope.fields[2].scope = FieldScope::Event;
    let groups = wrong_scope.groups.clone();
    assert!(wrong_scope.with_v3_groups(groups).is_err());

    let mut overlap = valid.clone();
    overlap.groups = vec![
        GroupDescriptor::dual_domain_repeated(1, vec![2, 3], 2, permissions()),
        GroupDescriptor::repeated(2, vec![3, 4], permissions()),
    ];
    assert!(validate_v3_grouped_exact_subset(&overlap).is_err());

    let mut wrong_domain_count = valid.clone();
    wrong_domain_count.groups[0]
        .dual_domain
        .as_mut()
        .unwrap()
        .domain_count = 1;
    assert!(validate_v3_grouped_exact_subset(&wrong_domain_count).is_err());

    let mut derived = valid.clone();
    derived.derived_expressions =
        vec![DerivedExpression::new(1, 1, DerivedExpressionOp::AddResidual, vec![0]).unwrap()];
    assert!(validate_v3_grouped_exact_subset(&derived).is_err());

    let mut bad_marker = valid;
    bad_marker.compact_schema_map.as_mut().unwrap()[2] = 0;
    assert!(validate_v3_grouped_exact_subset(&bad_marker).is_err());
}

#[test]
fn malformed_offsets_counts_side_lengths_and_bounds_fail_closed() {
    let schema = book_schema(false);
    let base = base_batch(&schema);
    for offsets in [vec![1, 2, 5], vec![0, 3, 2], vec![0, 2, 4], vec![0, 2]] {
        let mut candidate = base.clone();
        candidate.child_offsets = offsets;
        assert!(validate_v3_event_batch(&schema, &candidate, V3EventLimits::default()).is_err());
    }
    let mut side_two = base.clone();
    side_two.repeated_columns[0].values = Values::U8(vec![0, 2, 1, 0, 1]);
    assert!(validate_v3_event_batch(&schema, &side_two, V3EventLimits::default()).is_err());

    let bytes = encode_v3_event_block(&schema, &base, V3EventLimits::default()).unwrap();
    for end in 0..bytes.len() {
        let result = catch_unwind(AssertUnwindSafe(|| {
            decode_v3_event_block(&schema, &bytes[..end], V3EventLimits::default())
        }));
        assert!(result.is_ok());
        assert!(result.unwrap().is_err());
    }
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert!(decode_v3_event_block(&schema, &trailing, V3EventLimits::default()).is_err());
    for (offset, value) in [(48usize, u32::MAX), (52, u32::MAX), (64, u32::MAX)] {
        let mut hostile = bytes.clone();
        hostile[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        let result = catch_unwind(AssertUnwindSafe(|| {
            decode_v3_event_block(&schema, &hostile, V3EventLimits::default())
        }));
        assert!(result.is_ok());
        assert!(result.unwrap().is_err());
    }
    let mut nonmonotonic = bytes.clone();
    nonmonotonic[80..84].copy_from_slice(&3u32.to_le_bytes());
    nonmonotonic[84..88].copy_from_slice(&2u32.to_le_bytes());
    assert!(decode_v3_event_block(&schema, &nonmonotonic, V3EventLimits::default()).is_err());

    let mut wrong_type = bytes.clone();
    let first_column_type = 76 + (base.child_offsets.len() * 4) + 2;
    wrong_type[first_column_type] = FieldType::U8 as u8;
    assert!(decode_v3_event_block(&schema, &wrong_type, V3EventLimits::default()).is_err());

    for limits in [
        V3EventLimits {
            max_events: 1,
            ..V3EventLimits::default()
        },
        V3EventLimits {
            max_children: 4,
            ..V3EventLimits::default()
        },
        V3EventLimits {
            max_values: 1,
            ..V3EventLimits::default()
        },
        V3EventLimits {
            max_block_bytes: bytes.len() - 1,
            ..V3EventLimits::default()
        },
    ] {
        assert!(validate_v3_event_batch(&schema, &base, limits).is_err());
        assert!(decode_v3_event_block(&schema, &bytes, limits).is_err());
    }
}

#[test]
fn grouped_defaults_are_conservative_and_hard_clamped() {
    let defaults = V3EventLimits::default();
    let hard = V3EventLimits::HARD;
    assert_eq!(defaults, V3EventLimits::DEFAULT_IN_MEMORY);
    assert!(defaults.max_block_bytes < hard.max_block_bytes);
    assert!(defaults.max_events < hard.max_events);
    assert!(defaults.max_children < hard.max_children);
    assert!(defaults.max_values < hard.max_values);

    let schema = book_schema(false);
    let batch = base_batch(&schema);
    let bytes = encode_v3_event_block(&schema, &batch, defaults).unwrap();
    let exact = V3EventLimits {
        max_block_bytes: bytes.len(),
        max_events: batch.event_count as usize,
        max_children: batch.child_count() as usize,
        max_values: defaults.max_values,
        ..defaults
    };
    assert!(validate_v3_event_batch(&schema, &batch, exact).is_ok());
    assert!(decode_v3_event_block(&schema, &bytes, exact).is_ok());
    assert!(decode_v3_event_block(
        &schema,
        &bytes,
        V3EventLimits {
            max_block_bytes: bytes.len() - 1,
            ..exact
        },
    )
    .is_err());

    let mut hostile = empty_batch(&schema, 0);
    hostile.event_count = u32::try_from(MAX_V3_EVENT_EVENTS + 1).unwrap();
    let raised = V3EventLimits {
        max_block_bytes: usize::MAX,
        max_variable_value_bytes: usize::MAX,
        max_events: usize::MAX,
        max_children: usize::MAX,
        max_values: usize::MAX,
    };
    assert!(validate_v3_event_batch(&schema, &hostile, raised).is_err());
}

#[test]
fn flat_v3_reference_fixture_sha_is_unchanged() {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/v3");
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(format!("{root}/manifest.json")).unwrap()).unwrap();
    let hex = std::fs::read_to_string(format!("{root}/flat-source-order.aurav3vb.hex")).unwrap();
    let compact = hex
        .chars()
        .filter(|value| !value.is_whitespace())
        .collect::<String>();
    let bytes = (0..compact.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&compact[index..index + 2], 16).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        manifest["block_sha256"].as_str().unwrap(),
        hex_string(&Sha256::digest(bytes))
    );
}

fn hex_string(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
