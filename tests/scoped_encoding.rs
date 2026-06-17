use aura_codec::schema::{generic_i64_parent_schema, schema_parent_mapping, FieldScope};
use aura_codec::scoped::{decode_grouped_i64_rows, encode_grouped_i64_rows, plan_from_schema};

#[test]
fn scoped_plan_is_derived_from_parent_map_only() {
    let schema =
        generic_i64_parent_schema("dynamic_book_delta_v1", &[100, 0, 0, 205, 0, 0, 0, 0]).unwrap();

    let plan = plan_from_schema(&schema).unwrap();

    assert_eq!(vec![0, 1, 2], plan.event_slots);
    assert_eq!(vec![3, 4, 5, 6, 7], plan.repeated_slots);
    assert_eq!(FieldScope::Repeated, schema.fields[3].scope);
    assert_eq!(
        vec![100, 0, 0, 205, 0, 0, 0, 0],
        schema_parent_mapping(&schema).unwrap()
    );
}

#[test]
fn grouped_i64_rows_round_trip_without_field_names() {
    let schema =
        generic_i64_parent_schema("dynamic_book_delta_v1", &[100, 0, 0, 205, 0, 0, 0, 0]).unwrap();
    let rows = vec![
        vec![1_000, 10, 20, 1, 0, 100_000_000, 5_000, 0],
        vec![1_000, 10, 20, 1, 0, 100_100_000, 0, 1],
        vec![1_000, 10, 20, 1, 1, 100_200_000, 7_000, 0],
        vec![1_005, 11, 21, 1, 0, 100_100_000, 8_000, 0],
        vec![1_005, 11, 21, 1, 1, 100_200_000, 0, 1],
    ];

    let encoded = encode_grouped_i64_rows(&schema, &rows).unwrap();
    let decoded = decode_grouped_i64_rows(&schema, &encoded).unwrap();

    assert_eq!(rows, decoded);
    assert!(encoded.len() < rows.len() * rows[0].len() * 8);
}
